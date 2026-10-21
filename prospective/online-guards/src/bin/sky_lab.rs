//! Real-target comparison on Skytable 0.8.4.
//!
//! One cut/cancel is injected into an operation, then four clients are asked
//! to serve a successor on the same resource:
//!
//! * B0 raw strategy model (not the upstream client): keep the stream and read whatever arrives;
//! * B1 poison-retire: one boolean, reconnect before the next use;
//! * B2 drain-only: try to consume one pending reply, reconnect on failure;
//! * B3 guarded: ledger + exclusive recovery + quarantine + epoch witness.
//!
//! The successor selects a key that was primed before the cut.  A reply for
//! the *cut* operation has a different value, so a misassociated successor is
//! visible as a wrong value, an Empty reply to a select, or a timeout.

use online_guards::lab::policy::ByteStatePolicy;
use online_guards::lab::proxy::{DelayProxy, ProxyMode};
use online_guards::lab::raw::{RawSky, ReuseRule};
use online_guards::lab::record::{Aggregate, TrialRecord, aggregate, write_jsonl};
use online_guards::pool::{Lease, Pool, PoolConfig};
use online_guards::proto::sky::{
    CutPoint, Reply, SkyConfig, SkyConn, SkyManager, SkyValue, encode_param_str, encode_param_uint,
};
use online_guards::recovery::EffectContract;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const EXPECTED: u64 = 111;
const CUT_VALUE: u64 = 222;
const SIDE_VALUE: u64 = 333;

#[derive(Clone, Copy, Debug)]
enum CutOp {
    SelectCut,
    InsertSide,
}

struct Spec {
    name: &'static str,
    cut: Option<CutPoint>,
    write_chunk: Option<usize>,
    natural: Option<(ProxyMode, Duration)>,
    contract: EffectContract,
    cut_op: CutOp,
    /// Try both a conservative and an effect-authorised guarded policy.
    compare_finish: bool,
}

fn specs() -> Vec<Spec> {
    let may = EffectContract::AtomicEffectMayApply;
    vec![
        Spec {
            name: "S1_cut_before_read",
            cut: Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            }),
            write_chunk: None,
            natural: None,
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
        Spec {
            name: "S2_cut_mid_reply",
            cut: Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(1),
            }),
            write_chunk: None,
            natural: None,
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
        Spec {
            name: "S3_partial_insert",
            cut: Some(CutPoint {
                after_write_bytes: Some(5),
                after_read_bytes: None,
            }),
            write_chunk: Some(1),
            natural: None,
            contract: may,
            cut_op: CutOp::InsertSide,
            compare_finish: true,
        },
        Spec {
            name: "S4_insert_before_read",
            cut: Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            }),
            write_chunk: None,
            natural: None,
            contract: may,
            cut_op: CutOp::InsertSide,
            compare_finish: false,
        },
        Spec {
            name: "S5_nothing_written",
            cut: Some(CutPoint {
                after_write_bytes: Some(0),
                after_read_bytes: None,
            }),
            write_chunk: None,
            natural: None,
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
        Spec {
            name: "S6_late_reply_caught",
            cut: None,
            write_chunk: None,
            natural: Some((
                ProxyMode::Delay(Duration::from_millis(20)),
                Duration::from_millis(2),
            )),
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
        Spec {
            name: "S7_late_reply_beyond_first_timeout",
            cut: None,
            write_chunk: None,
            natural: Some((
                ProxyMode::Delay(Duration::from_millis(300)),
                Duration::from_millis(2),
            )),
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
        Spec {
            name: "S8_reply_lost",
            cut: None,
            write_chunk: None,
            natural: Some((ProxyMode::LoseReply, Duration::from_millis(2))),
            contract: EffectContract::MustNotApply,
            cut_op: CutOp::SelectCut,
            compare_finish: false,
        },
    ]
}

fn key(space: &str, trial: usize, which: char) -> String {
    format!("{space}_{which}{trial}")
}

fn select_sql(space: &str, k: &str) -> (String, Vec<u8>) {
    (
        format!("select * from {space}.kv where id = ?"),
        encode_param_str(k),
    )
}

fn insert_sql(space: &str, k: &str, v: u64) -> (String, Vec<u8>) {
    let mut p = encode_param_str(k);
    p.extend(encode_param_uint(v));
    (format!("insert into {space}.kv(?, ?)"), p)
}

fn classify(res: Result<Reply, Box<dyn std::error::Error + Send + Sync>>) -> (String, i64) {
    match res {
        Ok(Reply::Row(vals)) => match vals.get(1).and_then(SkyValue::as_u64) {
            Some(v) if v == EXPECTED => ("ok".into(), v as i64),
            Some(v) => ("stale".into(), v as i64),
            None => ("error".into(), -1),
        },
        Ok(Reply::Empty) => ("empty".into(), -1),
        Ok(Reply::Error(code)) => ("error".into(), -(code as i64)),
        Ok(_) => ("error".into(), -2),
        Err(_) => ("error".into(), -3),
    }
}

async fn prime_raw(r: &mut RawSky, space: &str, trial: usize) -> Result<(), String> {
    let (s, p) = insert_sql(space, &key(space, trial, 'a'), EXPECTED);
    r.query(&s, &p).await.map_err(|e| format!("prime a: {e}"))?;
    let (s, p) = insert_sql(space, &key(space, trial, 'b'), CUT_VALUE);
    r.query(&s, &p).await.map_err(|e| format!("prime b: {e}"))?;
    Ok(())
}

async fn prime_pool(
    lease: &mut Lease<SkyConn, SkyManager>,
    space: &str,
    trial: usize,
) -> Result<(), String> {
    let (s, p) = insert_sql(space, &key(space, trial, 'a'), EXPECTED);
    lease
        .query_with(&s, &p)
        .await
        .map_err(|e| format!("prime a: {e}"))?;
    let (s, p) = insert_sql(space, &key(space, trial, 'b'), CUT_VALUE);
    lease
        .query_with(&s, &p)
        .await
        .map_err(|e| format!("prime b: {e}"))?;
    Ok(())
}

fn side_effect(raw_c: &mut Option<i64>) -> String {
    match raw_c {
        Some(v) if *v == SIDE_VALUE as i64 => "present".into(),
        Some(v) => format!("wrong({v})"),
        None => "missing".into(),
    }
}

async fn side_check_raw(r: &mut RawSky, space: &str, trial: usize) -> Option<i64> {
    let (s, p) = select_sql(space, &key(space, trial, 'c'));
    match tokio::time::timeout(Duration::from_millis(600), r.query(&s, &p)).await {
        Ok(Ok(Reply::Row(vals))) => vals.get(1).and_then(SkyValue::as_u64).map(|v| v as i64),
        _ => None,
    }
}

async fn side_check_pool(
    lease: &mut Lease<SkyConn, SkyManager>,
    space: &str,
    trial: usize,
) -> Option<i64> {
    let (s, p) = select_sql(space, &key(space, trial, 'c'));
    match tokio::time::timeout(Duration::from_millis(600), lease.query_with(&s, &p)).await {
        Ok(Ok(Reply::Row(vals))) => vals.get(1).and_then(SkyValue::as_u64).map(|v| v as i64),
        _ => None,
    }
}

async fn run_raw(
    spec: &Spec,
    trial: usize,
    space: &str,
    server: SocketAddr,
    pass: &str,
    rule: ReuseRule,
    baseline: &'static str,
) -> TrialRecord {
    let mut rec = TrialRecord {
        scenario: spec.name.into(),
        mode: if spec.natural.is_some() {
            "proxy_timer"
        } else {
            "scripted_cut"
        }
        .into(),
        baseline: baseline.into(),
        trial,
        contract: format!("{:?}", spec.contract),
        expected: EXPECTED as i64,
        got: -1,
        ..Default::default()
    };
    let proxy = match spec.natural {
        None => None,
        Some((mode, _)) => match DelayProxy::start(server, mode).await {
            Ok(p) => Some(p),
            Err(e) => {
                rec.successor = "error".into();
                rec.note = format!("proxy: {e}");
                return rec;
            }
        },
    };
    let port = proxy
        .as_ref()
        .map(|p| p.addr.port())
        .unwrap_or(server.port());
    let cfg = SkyConfig::loopback(port, "root", pass);
    let mut raw = match RawSky::connect(trial as u64, cfg, rule).await {
        Ok(r) => r,
        Err(e) => {
            rec.successor = "error".into();
            rec.note = format!("connect: {e}");
            return rec;
        }
    };
    eprintln!("  primed raw");
    // For natural faults the tested connection must be the one the proxy
    // affects; prime over a separate direct connection so setup replies are
    // never the ones being delayed or lost.
    let prime_result = if spec.natural.is_some() {
        let direct_cfg = SkyConfig::loopback(server.port(), "root", pass);
        match RawSky::connect(0, direct_cfg, ReuseRule::Immediate).await {
            Ok(mut direct) => prime_raw(&mut direct, space, trial).await,
            Err(e) => Err(format!("direct prime connect: {e}")),
        }
    } else {
        prime_raw(&mut raw, space, trial).await
    };
    if let Err(e) = prime_result {
        rec.successor = "error".into();
        rec.note = e;
        return rec;
    }
    let (cut_sql, cut_params) = match spec.cut_op {
        CutOp::SelectCut => select_sql(space, &key(space, trial, 'b')),
        CutOp::InsertSide => insert_sql(space, &key(space, trial, 'c'), SIDE_VALUE),
    };
    raw.write_chunk = spec.write_chunk;
    if spec.natural.is_none() {
        raw.cut = spec.cut;
    }
    let cut_started = Instant::now();
    let cut_res = match spec.natural {
        None => Some(raw.query(&cut_sql, &cut_params).await),
        Some((_, cancel_after)) => {
            let fut = raw.query(&cut_sql, &cut_params);
            tokio::pin!(fut);
            tokio::select! {
                r = &mut fut => Some(r),
                _ = tokio::time::sleep(cancel_after) => None,
            }
        }
    };
    eprintln!("  cut done: {:?}", raw.last_cut);
    rec.latency_us = cut_started.elapsed().as_micros() as u64;
    if let Some(snap) = raw.last_cut.clone() {
        rec.cut_write = snap.write_bytes as i64;
        rec.cut_read = snap.read_bytes as i64;
        rec.write_state = snap.write_state.into();
        rec.read_state = snap.read_state.into();
    } else {
        let (written, total, read) = raw.io_state();
        rec.cut_write = written as i64;
        rec.cut_read = read as i64;
        rec.write_state = if written == 0 {
            "none"
        } else if written == total {
            "complete"
        } else {
            "partial"
        }
        .into();
        rec.read_state = if read == 0 { "none" } else { "partial" }.into();
    }
    rec.suspect = raw.poisoned;
    rec.valid_cut = match (&cut_res, spec.natural) {
        (None, Some(_)) => rec.write_state == "complete" && rec.read_state == "none",
        (Some(Err(online_guards::proto::sky::SkyError::Cut(_))), None) => true,
        _ => false,
    };
    if !rec.valid_cut {
        rec.successor = "skipped".into();
        rec.note = "unrealized cut or unexpected operation failure".into();
        return rec;
    }
    rec.note = match cut_res {
        None => "cancelled while future pending".into(),
        Some(Ok(_)) => "cut operation completed before the fault".into(),
        Some(Err(e)) => format!("cut: {e}"),
    };

    let reuse_started = Instant::now();
    eprintln!("  reusing");
    raw.prepare_reuse(Duration::from_millis(120)).await;
    eprintln!("  reuse decision done");
    eprintln!("  pooling: successor");
    let (succ_sql, succ_params) = select_sql(space, &key(space, trial, 'a'));
    let succ = tokio::time::timeout(
        Duration::from_millis(800),
        raw.query(&succ_sql, &succ_params),
    )
    .await;
    eprintln!("  successor done");
    let (kind, got) = match succ {
        Ok(r) => classify(r.map_err(|e| Box::new(e) as Box<_>)),
        Err(_) => ("timeout".into(), -4),
    };
    rec.successor = kind;
    rec.got = got;
    rec.reconnects = raw.reconnects as i64;
    rec.conns = raw.reconnects as i64 + 1;
    rec.drains_ok = raw.drains_ok as i64;
    rec.drains_failed = raw.drains_failed as i64;
    rec.plan = match rule {
        ReuseRule::Immediate => "none".into(),
        ReuseRule::PoisonRetire => "retire".into(),
        ReuseRule::DrainOnly => "drain".into(),
    };
    rec.recovery = if raw.drains_ok > 0 {
        "drained".into()
    } else if raw.drains_failed > 0 {
        "drain-failed".into()
    } else {
        "none".into()
    };
    rec.recovered = raw.drains_ok > 0;
    rec.latency_us = reuse_started.elapsed().as_micros() as u64;
    if spec.cut_op as u8 == CutOp::InsertSide as u8 {
        let c = side_check_raw(&mut raw, space, trial).await;
        rec.note = format!(
            "{}; side_effect={}",
            rec.note,
            side_effect(&mut Some(c.unwrap_or(-1)))
        );
    }
    drop(proxy);
    rec
}

async fn run_pool(
    spec: &Spec,
    trial: usize,
    space: &str,
    server: SocketAddr,
    pass: &str,
    allow_finish: bool,
    baseline: &'static str,
) -> TrialRecord {
    let mut rec = TrialRecord {
        scenario: spec.name.into(),
        mode: if spec.natural.is_some() {
            "proxy_timer"
        } else {
            "scripted_cut"
        }
        .into(),
        baseline: baseline.into(),
        trial,
        contract: format!("{:?}", spec.contract),
        expected: EXPECTED as i64,
        got: -1,
        ..Default::default()
    };
    let proxy = match spec.natural {
        None => None,
        Some((mode, _)) => match DelayProxy::start(server, mode).await {
            Ok(p) => Some(p),
            Err(e) => {
                rec.successor = "error".into();
                rec.note = format!("proxy: {e}");
                return rec;
            }
        },
    };
    let port = proxy
        .as_ref()
        .map(|p| p.addr.port())
        .unwrap_or(server.port());
    let cfg = SkyConfig::loopback(port, "root", pass);
    let mgr = SkyManager::new(cfg, spec.contract);
    let policy = ByteStatePolicy {
        allow_finish_write: allow_finish,
    };
    let pool = Pool::new(
        mgr,
        PoolConfig {
            name: "sky-lab",
            max_size: 1,
            recovery_capacity: 2,
            limits: online_guards::guard::RecoveryLimits {
                max_attempts: 2,
                deadline: Duration::from_secs(2),
            },
            validate_on_checkout: false,
            policy: std::sync::Arc::new(policy),
            acquire_timeout: Some(Duration::from_secs(5)),
            retry_delay: Duration::from_millis(5),
            ownership_grace: Duration::from_millis(250),
        },
    );
    if spec.natural.is_some() {
        let direct_cfg = SkyConfig::loopback(server.port(), "root", pass);
        let prime = match RawSky::connect(0, direct_cfg, ReuseRule::Immediate).await {
            Ok(mut direct) => prime_raw(&mut direct, space, trial).await,
            Err(e) => Err(format!("direct prime connect: {e}")),
        };
        if let Err(e) = prime {
            rec.successor = "error".into();
            rec.note = e;
            return rec;
        }
    }
    eprintln!("  pooling: acquire");
    let mut lease = match pool.acquire().await {
        Ok(l) => l,
        Err(e) => {
            rec.successor = "error".into();
            rec.note = format!("acquire: {e}");
            return rec;
        }
    };
    if spec.natural.is_none()
        && let Err(e) = prime_pool(&mut lease, space, trial).await
    {
        rec.successor = "error".into();
        rec.note = e;
        let _ = pool.shutdown(Duration::from_millis(50)).await;
        return rec;
    }
    lease.write_chunk = spec.write_chunk;
    if spec.natural.is_none() {
        lease.cut = spec.cut;
    }
    let guard = lease.guard().clone();
    let (cut_sql, cut_params) = match spec.cut_op {
        CutOp::SelectCut => select_sql(space, &key(space, trial, 'b')),
        CutOp::InsertSide => insert_sql(space, &key(space, trial, 'c'), SIDE_VALUE),
    };
    let cut_res = match spec.natural {
        None => Some(lease.query_with(&cut_sql, &cut_params).await),
        Some((_, cancel_after)) => {
            let fut = lease.query_with(&cut_sql, &cut_params);
            tokio::pin!(fut);
            tokio::select! {
                r = &mut fut => Some(r),
                _ = tokio::time::sleep(cancel_after) => None,
            }
        }
    };
    if let Some(sus) = guard.suspect() {
        rec.suspect = true;
        rec.write_state = sus.features.name("write").unwrap_or("unknown").into();
        rec.read_state = sus.features.name("read").unwrap_or("unknown").into();
        rec.cut_write = sus.features.int("write_bytes").unwrap_or(-1);
        rec.cut_read = sus.features.int("read_bytes").unwrap_or(-1);
    } else {
        rec.write_state = "clean".into();
        rec.read_state = "clean".into();
    }
    rec.valid_cut = match (&cut_res, spec.natural) {
        (None, Some(_)) => rec.write_state == "complete" && rec.read_state == "none",
        (Some(Err(online_guards::proto::sky::SkyError::Cut(_))), None) => true,
        _ => false,
    };
    if !rec.valid_cut {
        rec.successor = "skipped".into();
        rec.note = "unrealized cut or unexpected operation failure".into();
        return rec;
    }
    rec.note = match cut_res {
        None => "cancelled while future pending".into(),
        Some(Ok(_)) => "cut operation completed before the fault".into(),
        Some(Err(e)) => format!("cut: {e}"),
    };
    eprintln!("  pooling: drop lease; wait recovery");
    drop(lease);
    let reuse_started = Instant::now();
    let mut lease = match tokio::time::timeout(Duration::from_secs(5), pool.acquire()).await {
        Ok(Ok(l)) => l,
        Ok(Err(e)) => {
            rec.successor = "error".into();
            rec.note = format!("{}; reacquire: {e}", rec.note);
            finish_pool(&pool, &mut rec, reuse_started);
            return rec;
        }
        Err(_) => {
            rec.successor = "timeout".into();
            rec.note = format!("{}; reacquire timeout", rec.note);
            finish_pool(&pool, &mut rec, reuse_started);
            return rec;
        }
    };
    eprintln!("  pooling: successor");
    let (succ_sql, succ_params) = select_sql(space, &key(space, trial, 'a'));
    let succ = tokio::time::timeout(
        Duration::from_millis(900),
        lease.query_with(&succ_sql, &succ_params),
    )
    .await;
    eprintln!("  successor done");
    let (kind, got) = match succ {
        Ok(r) => classify(r.map_err(|e| Box::new(e) as Box<_>)),
        Err(_) => ("timeout".into(), -4),
    };
    rec.successor = kind;
    rec.got = got;
    if spec.cut_op as u8 == CutOp::InsertSide as u8 {
        let c = side_check_pool(&mut lease, space, trial).await;
        rec.note = format!(
            "{}; side_effect={}",
            rec.note,
            side_effect(&mut Some(c.unwrap_or(-1)))
        );
    }
    drop(lease);
    finish_pool(&pool, &mut rec, reuse_started);
    let _ = pool.shutdown(Duration::from_millis(100)).await;
    rec
}

fn finish_pool(pool: &Pool<SkyConn, SkyManager>, rec: &mut TrialRecord, started: Instant) {
    let s = pool.snapshot();
    rec.latency_us = started.elapsed().as_micros() as u64;
    rec.plan = s.last_plan.into();
    rec.recovered = s.recovery_succeeded > 0;
    rec.recovery = if s.recovery_cancelled > 0 {
        "cancelled".into()
    } else if s.recovery_failed > 0 {
        "failed".into()
    } else if s.recovery_succeeded > 0 {
        "ok".into()
    } else if s.recovery_unresolved > 0 {
        "unresolved".into()
    } else {
        "none".into()
    };
    rec.reconnects = s.connects.saturating_sub(1) as i64;
    rec.conns = s.connects as i64;
    if s.retired_suspect > 0 || s.retired_cleanup > 0 || s.retired_forced > 0 {
        rec.note = format!(
            "{}; retired={}",
            rec.note,
            s.retired_suspect + s.retired_cleanup + s.retired_forced
        );
    }
}

fn print_summary(
    records: &[TrialRecord],
    aggs: &std::collections::BTreeMap<(String, String), Aggregate>,
) {
    println!(
        "\n| scenario | baseline | n | unsafe | ok | err | timeout | recovered | reconnects | p50 us | p95 us |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for ((scenario, baseline), a) in aggs {
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            scenario,
            baseline,
            a.n,
            a.unsafe_successors,
            a.successor_ok,
            a.successor_error,
            a.successor_timeout,
            a.recovered,
            a.reconnects,
            a.p50(),
            a.p95()
        );
    }
    let _ = records;
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SKY_PORT").ok())
        .expect("usage: sky_lab <port> [out.jsonl] [trials]")
        .parse()
        .expect("port");
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "research/online/data/sky-trials.jsonl".into());
    let trials: usize = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "5".into())
        .parse()
        .unwrap();
    let pass = std::env::var("SKY_PASS").unwrap_or_else(|_| "online-guards-pass".into());
    let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let space = format!("lab{}", std::process::id());
    let cfg = SkyConfig::loopback(port, "root", pass.clone());
    {
        let mut setup = SkyConn::connect(&cfg).await.expect("setup connect");
        setup
            .query(&format!("create space {space}"))
            .await
            .expect("create space");
        setup
            .query(&format!("create model {space}.kv(id: string, v: uint64)"))
            .await
            .expect("create model");
    }
    eprintln!("setup complete; space={space}");
    let mut records = Vec::new();
    for spec in specs() {
        if let Ok(only) = std::env::var("SKY_ONLY")
            && !spec.name.contains(&only)
        {
            continue;
        }
        let mut baselines: Vec<&'static str> =
            vec!["B0_raw_reuse", "B1_raw_retire", "B2_raw_drain", "B3_guard"];
        if spec.compare_finish {
            baselines.push("B3f_finish");
        }
        if let Ok(only) = std::env::var("SKY_BASE") {
            baselines.retain(|b| b.contains(&only));
        }
        for trial in 0..trials {
            for b in &baselines {
                eprintln!("--> {} {} trial {}", spec.name, b, trial);
                let rec = match *b {
                    "B0_raw_reuse" => {
                        run_raw(&spec, trial, &space, server, &pass, ReuseRule::Immediate, b).await
                    }
                    "B1_raw_retire" => {
                        run_raw(
                            &spec,
                            trial,
                            &space,
                            server,
                            &pass,
                            ReuseRule::PoisonRetire,
                            b,
                        )
                        .await
                    }
                    "B2_raw_drain" => {
                        run_raw(&spec, trial, &space, server, &pass, ReuseRule::DrainOnly, b).await
                    }
                    "B3_guard" => run_pool(&spec, trial, &space, server, &pass, false, b).await,
                    "B3f_finish" => run_pool(&spec, trial, &space, server, &pass, true, b).await,
                    _ => unreachable!(),
                };
                eprintln!(
                    "{} {} t{} succ={} got={} plan={} rec={} reconn={} lat={}us",
                    rec.scenario,
                    rec.baseline,
                    rec.trial,
                    rec.successor,
                    rec.got,
                    rec.plan,
                    rec.recovery,
                    rec.reconnects,
                    rec.latency_us
                );
                records.push(rec);
            }
        }
    }
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_jsonl(&out, &records).expect("write jsonl");
    assert!(!records.is_empty(), "no trials selected");
    assert!(
        records.iter().all(|r| r.valid_cut),
        "invalid/unrealized trials: inspect records; no aggregate claim"
    );
    let aggs = aggregate(&records);
    print_summary(&records, &aggs);
    println!("\nwrote {} records to {out}", records.len());
}

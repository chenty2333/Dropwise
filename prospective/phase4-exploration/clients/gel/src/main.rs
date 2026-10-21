//! Gel 7.1 / gel-tokio 0.11.0 cancellation ladder (round 3).
//!
//! `Client` is a pool; gel-tokio 0.11.0 exposes no direct connection handle, so
//! the tested client is built with `max_concurrency(1usize)`. That bounds the
//! pool to one connection at a time but is **not** a persistent direct handle:
//! a cancelled request leaves the pooled connection in `Mode::Dirty`,
//! `PoolConnection::drop` discards it (it is not returned to the queue), and the
//! next operation opens a fresh connection through the proxy.
//!
//! Setup (all outside `Ctx::target`, never counted as a cut): one independent
//! observer `Client` direct to the server creates `DwGel` once and inserts a
//! fresh seed row `(v := 0, marker := 'seed')` with a process-unique `k`; the
//! tested `Client` goes through `dropwise::net::Proxy` with `reply_delay = 50ms`
//! and is warmed with `ensure_connected()`.
//!
//! Target: `Client::transaction` whose body issues one
//! `update DwGel ... set { v := 1, marker := 'target' } select (v, marker)`.
//! gel-tokio starts a transaction lazily, so the first `Pending` boundary of the
//! whole target future is the `START TRANSACTION` round trip, before the body's
//! update/select is sent; both races cut there. That is the honest cancellation
//! point for this client shape and is recorded as such.
//!
//! B1 stops after the cut. B2 retries the same transaction once. B3 runs a
//! different operation (a plain read) once on the same client. M1 runs a
//! survivor transaction `(v := 7, marker := 'survivor')` on the same client,
//! reads it back with the same client, and reads it with the independent
//! observer before and after; it is judged with `Expect::Atomic` against
//! auto-calibration, and `calibrate-pre`/`calibrate-post` print two manual
//! samples per side for the concrete evidence.
//!
//! Prints `GEL-*` lines and the shared `LADDER`/`LADDER-VIOLATION` lines.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::net::SocketAddr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::support::{bounded, unique};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Report, Settle};
use gel_tokio::{Builder, Client, TlsSecurity};

const REPLY_DELAY: Duration = Duration::from_millis(50);
const BOUND: Duration = Duration::from_secs(2);
const SEED_V: i64 = 0;
const TARGET_V: i64 = 1;
const SURVIVOR_V: i64 = 7;

const CREATE: &str = "create type default::DwGel { create required property k -> str; create required property v -> int64; create required property marker -> str; };";
const INSERT: &str = "insert DwGel { k := <str>$0, v := 0, marker := 'seed' }";
const QUERY: &str = "with u := (update DwGel filter .k = <str>$0 set { v := <int64>$1, marker := <str>$2 }) select (u.v, u.marker)";
const READ: &str = "select (DwGel.v, DwGel.marker) filter DwGel.k = <str>$0";

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn config() -> Config {
    Config {
        max_runs: 64,
        max_cancellations: 1,
        races: vec![Race::Immediate, Race::AfterWake],
        repeat_at_site: false,
        gap_choices: Vec::new(),
        flavor: Flavor::MultiThread { workers: 1 },
        settle: Settle {
            tokio_time: Duration::ZERO,
            real_time: Duration::ZERO,
            watchdog: Duration::from_secs(10),
        },
        scenario_timeout: Duration::from_secs(20),
    }
}

fn server_addr() -> Result<SocketAddr, String> {
    std::env::var("DW_GEL_SERVER")
        .map_err(|_| "DW_GEL_SERVER is required".to_string())?
        .parse()
        .map_err(|err| format!("bad DW_GEL_SERVER: {err}"))
}

fn client_for(addr: SocketAddr) -> Result<Client, String> {
    let cfg = Builder::new()
        .host(addr.ip())
        .port(addr.port())
        .user("admin")
        .branch("main")
        .tls_security(TlsSecurity::Insecure)
        .max_concurrency(1usize)
        .build()
        .map_err(e)?;
    Ok(Client::new(&cfg))
}

async fn update_tx(
    client: &Client,
    key: &str,
    v: i64,
    marker: &str,
) -> Result<(i64, String), String> {
    let key = key.to_string();
    let marker = marker.to_string();
    let rows: Vec<(i64, String)> = client
        .transaction(move |mut tx| {
            let key = key.clone();
            let marker = marker.clone();
            async move { tx.query(QUERY, &(key.as_str(), v, marker.as_str())).await }
        })
        .await
        .map_err(e)?;
    rows.into_iter()
        .next()
        .ok_or_else(|| "transaction update returned no row".to_string())
}

async fn read(client: &Client, key: &str) -> Result<Vec<(i64, String)>, String> {
    client.query(READ, &(key,)).await.map_err(e)
}

struct Env {
    /// Independent reader/writer: its own `Client` straight to the server.
    observer: Client,
    /// Pending mechanism for the tested client only.
    proxy: Proxy,
    /// The pooled client under test; `max_concurrency(1)`.
    tested: Client,
    /// Fresh seed row for this scenario run.
    key: String,
    run: usize,
}

impl Env {
    async fn setup(run: usize) -> Result<Self, String> {
        let server = server_addr()?;
        let observer = client_for(server)?;
        observer
            .ensure_connected()
            .await
            .map_err(|err| format!("observer connect: {err}"))?;
        // Idempotent-ish: an existing type returns DuplicateNameError, which is
        // ignored; setup is not a cut and is outside every target future.
        let _ = observer.execute(CREATE, &()).await;
        let key = unique("dw-gel");
        observer
            .execute(INSERT, &(key.as_str(),))
            .await
            .map_err(|err| format!("seed insert: {err}"))?;
        let proxy = Proxy::start_with(server, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let tested = client_for(proxy.addr())?;
        tested
            .ensure_connected()
            .await
            .map_err(|err| format!("tested warm-up connect: {err}"))?;
        Ok(Self { observer, proxy, tested, key, run })
    }

    fn snapshot(&self, label: &str) {
        let conns = self
            .proxy
            .connections()
            .iter()
            .map(|c| format!("{}/{}/{}", c.client, c.to_upstream, c.to_client))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "GEL-SNAPSHOT run={} stage={label} proxy_connections={} conn=[{conns}]",
            self.run,
            self.proxy.connections().len(),
        );
    }

    async fn observer_read(&self) -> Result<Vec<(i64, String)>, String> {
        read(&self.observer, &self.key).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// Independent observer read straight after the cut, before the survivor.
    before: Result<Vec<(i64, String)>, String>,
    /// Survivor transaction on the same tested client: v=7, marker=survivor.
    write: Result<(i64, String), String>,
    /// Same-client read-back after the survivor transaction.
    own: Result<Vec<(i64, String)>, String>,
    /// Independent observer read after the survivor.
    visible: Result<Vec<(i64, String)>, String>,
}

fn summary(res: &Option<Result<(), String>>) -> String {
    match res {
        None => "cancelled".into(),
        Some(Ok(())) => "Ok(())".into(),
        Some(Err(err)) => format!("Err({err})"),
    }
}

async fn observed_after_cut(env: &Env) -> Obs {
    let before = bounded(BOUND, "observer read before survivor", async {
        env.observer_read().await
    })
    .await;
    let write = bounded(BOUND, "survivor transaction", async {
        update_tx(&env.tested, &env.key, SURVIVOR_V, "survivor").await
    })
    .await;
    env.snapshot("after_survivor_write");
    let own = bounded(BOUND, "same-client read-back", async {
        read(&env.tested, &env.key).await
    })
    .await;
    let visible = bounded(BOUND, "observer read after survivor", async {
        env.observer_read().await
    })
    .await;
    let obs = Obs { before, write, own, visible };
    println!(
        "GEL-OBS run={} before={:?} write={:?} own={:?} visible={:?} proxy_connections={}",
        env.run,
        obs.before,
        obs.write,
        obs.own,
        obs.visible,
        env.proxy.connections().len(),
    );
    obs
}

/// B1: cancel the target, then stop. Judged by explore (panic/liveness/leak).
fn run_b1(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let env = Env::setup(run).await?;
        env.snapshot("after_setup");
        let res = ctx
            .target(update_tx(&env.tested, &env.key, TARGET_V, "target"))
            .await
            .map(|res| res.map(|_| ()));
        println!("B1-TARGET run={} result={}", env.run, summary(&res));
        env.snapshot("after_target");
        if let Some(res) = res {
            res?;
        }
        Ok(())
    });
    ladder::simple("B1", "TX_UPDATE", &report);
    gel_report("B1", run, &report);
}

/// B2: after a cancelled target, repeat the same transaction once.
fn run_b2(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let env = Env::setup(run).await?;
        let res = ctx
            .target(update_tx(&env.tested, &env.key, TARGET_V, "target"))
            .await
            .map(|res| res.map(|_| ()));
        if let Some(res) = res {
            res?;
            println!("B2-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let retry = bounded(BOUND, "B2 retry", async {
            update_tx(&env.tested, &env.key, TARGET_V, "target").await
        })
        .await;
        let visible = bounded(BOUND, "B2 observer after retry", async {
            env.observer_read().await
        })
        .await;
        println!(
            "B2-RESULT run={} retry={retry:?} observer={visible:?} proxy_connections={}",
            env.run,
            env.proxy.connections().len()
        );
        env.snapshot("after_retry");
        match &retry {
            Err(err) => Err(format!("B2 same transaction failed after cancellation: {err}")),
            Ok((v, marker)) if *v != TARGET_V || marker != "target" => Err(format!(
                "B2 retry returned ({v},{marker:?}), expected ({TARGET_V},\"target\")"
            )),
            Ok(_) => Ok(()),
        }
    });
    ladder::simple("B2", "TX_UPDATE", &report);
    gel_report("B2", run, &report);
}

/// B3: after a cancelled target, a different operation (plain read) once.
fn run_b3(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let env = Env::setup(run).await?;
        let res = ctx
            .target(update_tx(&env.tested, &env.key, TARGET_V, "target"))
            .await
            .map(|res| res.map(|_| ()));
        if let Some(res) = res {
            res?;
            println!("B3-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let read = bounded(BOUND, "B3 read", async {
            read(&env.tested, &env.key).await
        })
        .await;
        println!(
            "B3-RESULT run={} read={read:?} proxy_connections={}",
            env.run,
            env.proxy.connections().len()
        );
        env.snapshot("after_read");
        if let Err(err) = &read {
            return Err(format!("B3 different read failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B3", "READ_ROW", &report);
    gel_report("B3", run, &report);
}

/// M1: `Expect::Atomic` with an independent observer and survivor transaction.
fn run_m1(run: usize) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| async move {
        let env = Env::setup(run).await?;
        let res = ctx
            .target(update_tx(&env.tested, &env.key, TARGET_V, "target"))
            .await;
        let cancelled = res.is_none();
        if let Some(res) = res {
            res.map_err(|err| format!("baseline target: {err}"))?;
        }
        env.snapshot("after_target");
        println!("M1-TARGET run={} cancelled={cancelled}", env.run);
        Ok(observed_after_cut(&env).await)
    });
    ladder::observed("M1", "TX_UPDATE", &report);
    gel_report("M1", run, &report);
}

/// Manual calibration evidence, independent of explore_observed's cache:
/// skip (`pre`) or complete (`post`) the target, then run the same survivor.
/// Each call makes two identical samples and requires them to agree.
async fn calibrate(complete: bool, rep: usize) -> Result<Obs, String> {
    let side = if complete { "post" } else { "pre" };
    let mut samples = Vec::new();
    for sample in 0..2 {
        let env = Env::setup(rep).await?;
        if complete {
            update_tx(&env.tested, &env.key, TARGET_V, "target").await?;
        }
        let obs = observed_after_cut(&env).await;
        println!("GEL-CAL side={side} rep={rep} sample={sample} obs={obs:?}");
        samples.push(obs);
    }
    if samples[0] != samples[1] {
        return Err(format!(
            "calibration side {side} unstable: sample0={:?} sample1={:?}",
            samples[0], samples[1]
        ));
    }
    println!("GEL-CAL-OK side={side} rep={rep} identical=true samples=2");
    Ok(samples.remove(0))
}

/// Uncanceled target through the proxy, same-client reuse, observer effect.
async fn preflight() -> Result<(), String> {
    let env = Env::setup(0).await?;
    env.snapshot("preflight_setup");
    let target = bounded(BOUND, "uncancelled target", async {
        update_tx(&env.tested, &env.key, TARGET_V, "target").await
    })
    .await?;
    let observer = bounded(BOUND, "observer after uncancelled target", async {
        env.observer_read().await
    })
    .await?;
    let own = bounded(BOUND, "same-client readback", async {
        read(&env.tested, &env.key).await
    })
    .await?;
    let second = bounded(BOUND, "same-client second read", async {
        read(&env.tested, &env.key).await
    })
    .await?;
    env.snapshot("preflight_after");
    let conns = env.proxy.connections().len();
    println!(
        "GEL-PREFLIGHT server={} target={target:?} observer={observer:?} own={own:?} second={second:?} proxy_connections={conns}",
        server_addr()?
    );
    let expected = vec![(TARGET_V, "target".to_string())];
    if target != (TARGET_V, "target".to_string())
        || observer != expected
        || own != expected
        || second != expected
    {
        return Err(format!(
            "preflight mismatch: target={target:?} observer={observer:?} own={own:?} second={second:?}"
        ));
    }
    if observer[0].0 == SEED_V {
        return Err("uncancelled target did not change the observed row".into());
    }
    if conns != 1 {
        return Err(format!(
            "uncancelled target used {conns} proxy connections, expected 1 (no reconnect)"
        ));
    }
    println!("GEL-PREFLIGHT-OK observer_effect=v:{SEED_V}->{TARGET_V} same_client=reused proxy_connections=1");
    Ok(())
}

/// Aggregate counts for the results fragment. The shared `ladder` helper prints
/// the standard line; this adds unknown/unrealized/realized counts and a
/// `GEL-VIOLATION` line with the concrete comparison.
fn gel_report(level: &str, run: usize, report: &Report) {
    let trials = report.trials.len();
    let realized: usize = report.trials.iter().map(|t| t.cancelled).sum();
    println!(
        "GEL-LADDER {level} rep={run} plans={trials} trials={trials} violations={} unknown={} \
         unrealized={} unsettled={} realized_cancellations={realized} baseline_errors={} \
         baseline_targets={} exhaustive={}",
        report.violations().count(),
        report.unknown().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.baseline_targets,
        report.exhaustive,
    );
    for trial in &report.trials {
        println!(
            "GEL-TRIAL {level} rep={run} plan={} cancelled={} realized={} verdict={:?}",
            ladder::plan(trial),
            trial.cancelled,
            trial.is_realized(),
            trial.verdict,
        );
    }
    for gap in report.coverage_gaps() {
        println!("GEL-GAP {level} rep={run}: {gap}");
    }
    for trial in report.violations() {
        println!(
            "GEL-VIOLATION {level} rep={run} plan={} cancelled={} verdict={:?} detail={} comparison={}",
            ladder::plan(trial),
            trial.cancelled,
            trial.verdict,
            trial.invariant.as_ref().err().map(String::as_str).unwrap_or("obligation leak"),
            trial.comparison.as_deref().unwrap_or("none"),
        );
    }
}

fn usage() -> ! {
    eprintln!("usage: p4e-gel <preflight|calibrate-pre|calibrate-post|ladder <rep> <B1|B2|B3|M1>>");
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| usage());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    match cmd.as_str() {
        "preflight" => {
            if let Err(err) = rt.block_on(preflight()) {
                eprintln!("GEL-PREFLIGHT-ERR {err}");
                std::process::exit(1);
            }
        }
        "calibrate-pre" | "calibrate-post" => {
            let complete = cmd == "calibrate-post";
            let rep = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if let Err(err) = rt.block_on(calibrate(complete, rep)) {
                eprintln!("GEL-CAL-ERR {err}");
                std::process::exit(1);
            }
        }
        "ladder" => {
            let rep: u32 = args
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            let level = args.next().unwrap_or_else(|| usage());
            println!(
                "=== GEL LADDER REPETITION {rep} LEVEL {level} START (run {}) ===",
                unique("gel")
            );
            println!("CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0 proxy_reply_delay=50ms client=gel-tokio/0.11.0 max_concurrency=1 target=transaction");
            match level.as_str() {
                "B1" => run_b1(rep as usize),
                "B2" => run_b2(rep as usize),
                "B3" => run_b3(rep as usize),
                "M1" => run_m1(rep as usize),
                other => {
                    eprintln!("unknown level {other}");
                    usage();
                }
            }
            println!("=== GEL LADDER REPETITION {rep} LEVEL {level} END ===");
        }
        other => {
            eprintln!("unknown command {other}");
            usage();
        }
    }
}

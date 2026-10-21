//! Live Skytable probe for the adapter and the ledger.
//!
//! This is the smallest real-target harness: it cuts an exchange at exact byte
//! offsets, shows what the ledger records, and runs the adapter's recovery
//! plans against the real server.  It is intentionally separate from the pool
//! lab so adapter behaviour can be debugged without pool scheduling.

use online_guards::features::Features;
use online_guards::proto::sky::{Reply, SkyConfig, SkyConn, encode_param_str, encode_param_uint};
use online_guards::recovery::{EffectContract, RecoveryAttempt, RecoveryContext};
use std::time::{Duration, Instant};

fn ctx_for(conn: &SkyConn, plan: &'static str) -> RecoveryContext {
    let g = conn.guard();
    let suspect = g.suspect();
    let mut features = Features::new();
    let mut cause = online_guards::guard::Cause::Cancelled;
    let mut contract = online_guards::EffectContract::MustNotApply;
    if let Some(s) = &suspect {
        features = s.features.clone();
        cause = s.cause;
        if s.features.name("contract") == Some("may_apply") {
            contract = online_guards::EffectContract::AtomicEffectMayApply;
        }
    }
    RecoveryContext {
        obj: g.obj(),
        epoch: g.epoch(),
        attempt: g.attempts() + 1,
        plan,
        limits: online_guards::guard::RecoveryLimits::default(),
        features,
        remaining: Duration::from_secs(2),
        write_state: "unknown",
        read_state: "unknown",
        cause,
        contract,
    }
}

async fn recover(conn: &mut SkyConn, plan: &'static str) -> Result<String, String> {
    let g = conn.guard().clone();
    let permit = match g.claim_recovery() {
        Ok(p) => p,
        Err(e) => return Err(format!("claim refused: {e:?}")),
    };
    let ctx = ctx_for(conn, plan);
    match conn.recover(&ctx).await {
        RecoveryAttempt::Recovered(w) => match permit.recovered(w) {
            Ok(()) => Ok(format!("plan={plan} recovered epoch={:?}", g.epoch())),
            Err(e) => Err(format!("stale witness: {e:?}")),
        },
        RecoveryAttempt::Failed(cause) => {
            permit.failed(cause);
            Err(format!("plan={plan} failed {cause:?}"))
        }
        RecoveryAttempt::Unresolved(cause) => {
            permit.unresolved(cause);
            Err(format!("plan={plan} unresolved {cause:?}"))
        }
    }
}

async fn successor(conn: &mut SkyConn, space: &str, key: &str) -> Result<Option<u64>, String> {
    let sql = format!("select * from {space}.kv where id = ?");
    match conn.query_with(&sql, &encode_param_str(key)).await {
        Ok(Reply::Row(vals)) => Ok(vals.get(1).and_then(|v| v.as_u64())),
        Ok(Reply::Error(code)) => Err(format!("successor server error {code}")),
        Ok(other) => Err(format!("successor unexpected {other:?}")),
        Err(e) => Err(format!("successor error {e}")),
    }
}

async fn insert(conn: &mut SkyConn, space: &str, key: &str, v: u64) -> Result<(), String> {
    let sql = format!("insert into {space}.kv(?, ?)");
    let mut params = encode_param_str(key);
    params.extend(encode_param_uint(v));
    match conn.query_with(&sql, &params).await {
        Ok(Reply::Empty) => Ok(()),
        other => Err(format!("insert -> {other:?}")),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SKY_PORT").ok())
        .expect("usage: sky_probe <port> (or SKY_PORT)")
        .parse()
        .expect("port");
    let pass = std::env::var("SKY_PASS").unwrap_or_else(|_| "online-guards-pass".into());
    let cfg = SkyConfig::loopback(port, "root", pass);
    let space = format!("og{}", std::process::id());

    println!("== setup ==");
    let mut c = SkyConn::connect(&cfg).await.expect("connect");
    println!(
        "create space: {:?}",
        c.query(&format!("create space {space}")).await
    );
    println!(
        "create model: {:?}",
        c.query(&format!("create model {space}.kv(id: string, v: uint64)"))
            .await
    );
    println!("insert: {:?}", insert(&mut c, &space, "k0", 7).await);
    println!("select: {:?}", successor(&mut c, &space, "k0").await);
    drop(c);

    println!("\n== scenario: cut before first write byte ==");
    let mut c = SkyConn::connect(&cfg).await.unwrap();
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: Some(0),
        after_read_bytes: None,
    });
    let sql = format!("insert into {space}.kv(?, ?)");
    let mut params = encode_param_str("cut0");
    params.extend(encode_param_uint(1));
    println!("query: {:?}", c.query_with(&sql, &params).await);
    report(&c);
    println!(
        "guard refuses successor: {:?}",
        successor(&mut c, &space, "k0").await
    );
    println!("drain: {:?}", recover(&mut c, "drain").await);
    println!(
        "retire after failure: qualification={:?}",
        c.guard().qualification()
    );
    drop(c);

    println!("\n== scenario: cut mid-write, drain (must fail) ==");
    let mut c = SkyConn::connect(&cfg).await.unwrap();
    c.write_chunk = Some(1);
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: Some(3),
        after_read_bytes: None,
    });
    let mut params = encode_param_str("cut1a");
    params.extend(encode_param_uint(2));
    println!("query: {:?}", c.query_with(&sql, &params).await);
    report(&c);
    println!(
        "drain (must fail: partial write): {:?}",
        recover(&mut c, "drain").await
    );
    drop(c);

    println!("\n== scenario: cut mid-write, finish write then drain ==");
    let mut c = SkyConn::connect(&cfg)
        .await
        .unwrap()
        .with_contract(EffectContract::AtomicEffectMayApply);
    c.write_chunk = Some(1);
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: Some(3),
        after_read_bytes: None,
    });
    let mut params = encode_param_str("cut1b");
    params.extend(encode_param_uint(9));
    println!("query: {:?}", c.query_with(&sql, &params).await);
    report(&c);
    println!(
        "finish_write_drain: {:?}",
        recover(&mut c, "finish_write_drain").await
    );
    println!(
        "successor sees value: {:?}",
        successor(&mut c, &space, "cut1b").await
    );
    drop(c);

    println!("\n== scenario: cut after full write, before any read ==");
    let mut c = SkyConn::connect(&cfg).await.unwrap();
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(0),
    });
    let mut params = encode_param_str("cut2");
    params.extend(encode_param_uint(3));
    println!("query: {:?}", c.query_with(&sql, &params).await);
    report(&c);
    println!("drain: {:?}", recover(&mut c, "drain").await);
    println!(
        "successor sees value: {:?}",
        successor(&mut c, &space, "cut2").await
    );
    println!(
        "next insert: {:?}",
        insert(&mut c, &space, "after2", 4).await
    );
    drop(c);

    println!("\n== scenario: cut mid-reply ==");
    let mut c = SkyConn::connect(&cfg).await.unwrap();
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(1),
    });
    let mut params = encode_param_str("cut3");
    params.extend(encode_param_uint(5));
    println!("query: {:?}", c.query_with(&sql, &params).await);
    report(&c);
    println!("drain: {:?}", recover(&mut c, "drain").await);
    println!(
        "successor sees value: {:?}",
        successor(&mut c, &space, "cut3").await
    );
    drop(c);

    println!("\n== scenario: reply arrived in one read, cut before parse ==");
    let mut c = SkyConn::connect(&cfg).await.unwrap();
    c.cut = Some(online_guards::proto::sky::CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(1),
    });
    let mut params = encode_param_str("cut4");
    params.extend(encode_param_uint(6));
    let _ = c.query_with(&sql, &params).await;
    // The first read may have produced the whole 3-byte Empty frame; the cut
    // fires before parsing, so drain must consume the buffered frame.
    println!("snapshot = {:?}", c.cut_snapshot());
    report(&c);
    println!("drain: {:?}", recover(&mut c, "drain").await);
    println!(
        "successor sees value: {:?}",
        successor(&mut c, &space, "cut4").await
    );
    drop(c);

    println!("\n== scenario: natural select-cancel (no scripted cut) ==");
    let mut hits = 0;
    for delay_us in [0u64, 10, 50, 100, 250, 500, 1000, 2000] {
        let mut c = SkyConn::connect(&cfg).await.unwrap();
        let mut params = encode_param_str(&format!("nat{delay_us}"));
        params.extend(encode_param_uint(8));
        let started = Instant::now();
        let r = tokio::select! {
            r = c.query_with(&sql, &params) => Some(r),
            _ = tokio::time::sleep(Duration::from_micros(delay_us)) => None,
        };
        let state = c.guard().qualification();
        let phase = c.guard().phase();
        println!(
            "delay={delay_us}us result={:?} phase={phase:?} qual={state:?} elapsed={:?}",
            r.as_ref().map(|x| x.is_ok()),
            started.elapsed()
        );
        if !matches!(state, online_guards::guard::Qualification::Clean) {
            hits += 1;
        }
        drop(c);
    }
    println!("natural cancellations (suspect/other): {hits}/8");

    println!("\n== done ==");
}

fn report(c: &SkyConn) {
    println!(
        "qualification={:?} suspect={:?} snapshot={:?}",
        c.guard().qualification(),
        c.guard().suspect().map(|s| (s.cause, s.features.render())),
        c.cut_snapshot()
    );
}

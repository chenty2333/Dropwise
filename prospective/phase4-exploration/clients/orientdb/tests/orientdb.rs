//! orientdb-client 0.6.0 (`aio::SessionPool`, mobc 0.7) cancellation ladder.
//!
//! Target: one pooled request — session checkout plus one complete SQL script
//! (`UPDATE DwOri ...; SELECT v, marker ...`). The script reply carries the
//! written value and a marker, so a stale reply is distinguishable from the
//! survivor's own reply. Pending comes from `dropwise::net::Proxy` holding every
//! server->client chunk for 50 ms on the binary port.
//!
//! Survivor: the same pool, the same record, a different value (7) and marker.
//! Observer: a second direct `OrientDB` client that does not go through the
//! proxy and does not use the tested SessionPool.
//!
//! Run with the environment produced by run-round3.sh; prints R3-ORI-* and
//! LADDER* lines. It does not assert outcomes, so logs may contain violations.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::time::Duration;

use dropwise::{Ctx, Expect, Flavor, Race, Report, Settle, Verdict, explore, explore_observed};
use p4e_orientdb::*;

const LABEL: &str = "primary";

fn config() -> dropwise::Config {
    dropwise::Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, real_time: Duration::ZERO, watchdog: Duration::from_secs(10) },
        ..dropwise::Config::default()
    }
}

async fn cut(ctx: &Ctx, env: &Env) -> Result<bool, String> {
    let record = env.record(LABEL)?.to_string();
    let cancelled = match ctx
        .target(env.pooled_update(&record, TARGET_V, "target"))
        .await
    {
        Some(reply) => {
            let reply = reply?;
            println!("R3-ORI-TARGET-DONE reply={reply:?}");
            false
        }
        None => true,
    };
    Ok(cancelled)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    before: Result<Reply, String>,
    write: Result<Reply, String>,
    own: Result<Reply, String>,
    visible: Result<Reply, String>,
    pool: (u64, u64, u64, u64),
}

fn print_report(level: &str, report: &Report) {
    println!(
        "R3-ORI-RESULT level={level} plans={} violations={} unrealized={} unsettled={} \
         baseline_errors={} calibration_errors={} exhaustive={} truncated={} pre={} post={} \
         mixed={} neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.calibration_errors.len(),
        report.exhaustive,
        report.truncated,
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    );
    for trial in &report.trials {
        println!(
            "R3-ORI-TRIAL level={level} plan={} sites={:?} outcomes={:?} cancelled={} \
             verdict={:?} settled={} realized={} invariant={:?}",
            trial.plan_label(),
            trial.sites,
            trial.outcomes,
            trial.cancelled,
            trial.verdict,
            trial.settled,
            trial.is_realized(),
            trial.invariant
        );
    }
    for error in &report.baseline_errors {
        println!("R3-ORI-BASELINE-ERROR level={level} {error}");
    }
    for error in &report.calibration_errors {
        println!("R3-ORI-CALIBRATION-ERROR level={level} {error}");
    }
    for gap in report.coverage_gaps() {
        println!("R3-ORI-COVERAGE level={level} {gap}");
    }
}

fn b1() {
    let report = explore(&config(), |ctx| async move {
        let env = setup(&[LABEL]).await?;
        let _ = cut(&ctx, &env).await?;
        Ok(())
    });
    ladder::simple("B1", "pooled", &report);
    print_report("B1", &report);
}

fn b2() {
    let report = explore(&config(), |ctx| async move {
        let env = setup(&[LABEL]).await?;
        let cancelled = cut(&ctx, &env).await?;
        if cancelled {
            let record = env.record(LABEL)?.to_string();
            match bounded(
                "retry of the same request",
                env.pooled_update(&record, TARGET_V, "target"),
            )
            .await
            {
                Ok(reply) => println!("R3-ORI-VALUE level=B2 retry=Ok({reply:?})"),
                Err(err) => {
                    println!("R3-ORI-VALUE level=B2 retry=Err({err:?})");
                    return Err(format!("retry of the same operation failed: {err}"));
                }
            }
        }
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(())
    });
    ladder::simple("B2", "pooled", &report);
    print_report("B2", &report);
}

fn b3() {
    let report = explore(&config(), |ctx| async move {
        let env = setup(&[LABEL]).await?;
        let cancelled = cut(&ctx, &env).await?;
        if cancelled {
            let record = env.record(LABEL)?.to_string();
            match bounded("different read", env.pooled_read(&record)).await {
                Ok(reply) => println!("R3-ORI-VALUE level=B3 read=Ok({reply:?})"),
                Err(err) => {
                    println!("R3-ORI-VALUE level=B3 read=Err({err:?})");
                    return Err(format!("different operation failed: {err}"));
                }
            }
        }
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(())
    });
    ladder::simple("B3", "pooled", &report);
    print_report("B3", &report);
}

fn m1() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup(&[LABEL]).await?;
        cut(&ctx, &env).await?;
        let record = env.record(LABEL)?.to_string();
        let before = bounded("observer before survivor", env.observer_read(&record)).await;
        let write = bounded(
            "survivor update",
            env.pooled_update(&record, SURVIVOR_V, "survivor"),
        )
        .await;
        let own = bounded("survivor read-back", env.pooled_read(&record)).await;
        let visible = bounded("observer after survivor", env.observer_read(&record)).await;
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        let pool = env.pooled_stats().await;
        let obs = Obs { before, write, own, visible, pool };
        println!(
            "R3-ORI-OBS level=M1 before={:?} write={:?} own={:?} visible={:?} pool={:?} \
             proxy_connections={}",
            obs.before,
            obs.write,
            obs.own,
            obs.visible,
            obs.pool,
            env.proxy.connections().len()
        );
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(obs)
    });
    ladder::observed("M1", "pooled", &report);
    print_report("M1", &report);
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn b1_pooled() {
    b1();
}

#[test]
fn b2_pooled() {
    b2();
}

#[test]
fn b3_pooled() {
    b3();
}

#[test]
fn m1_pooled() {
    m1();
}

/// Uncancelled control: target, survivor, same-pool read and independent
/// observer all work before the ladder is interpreted.
#[tokio::test]
async fn preflight() -> Result<(), String> {
    let env = setup(&[LABEL]).await?;
    let record = env.record(LABEL)?.to_string();
    let target = env.pooled_update(&record, TARGET_V, "target").await?;
    let survivor = env.pooled_update(&record, SURVIVOR_V, "survivor").await?;
    let own = env.pooled_read(&record).await?;
    let visible = env.observer_read(&record).await?;
    println!(
        "R3-ORI-PREFLIGHT target={target:?} survivor={survivor:?} own={own:?} \
         visible={visible:?} proxy_connections={} pool={:?}",
        env.proxy.connections().len(),
        env.pooled_stats().await
    );
    env.cleanup().await?;
    Ok(())
}

/// Reuse diagnostic: five sequential no-cancellation requests on one pool of
/// size 1. Records the logical session id and physical proxy connections.
#[tokio::test]
async fn reuse() -> Result<(), String> {
    let env = setup(&[LABEL]).await?;
    let record = env.record(LABEL)?.to_string();
    println!(
        "R3-ORI-REUSE after_setup proxy_connections={} pool={:?}",
        env.proxy.connections().len(),
        env.pooled_stats().await
    );
    for step in 0..5 {
        let reply = env.pooled_update(&record, step, "reuse").await?;
        let session = env.pooled_session_id().await?;
        println!(
            "R3-ORI-REUSE step={step} reply={reply:?} session_id={session} \
             proxy_connections={} pool={:?}",
            env.proxy.connections().len(),
            env.pooled_stats().await
        );
    }
    env.cleanup().await?;
    Ok(())
}

/// Explicit recovery contrast. This does NOT use a Dropwise cut: it drops a
/// real request future with `tokio::time::timeout` during the reply delay, then
/// compares (a) the stale pool and (b) a fresh client/pool through the same
/// proxy. A fresh `OrientDB` client is the documented recovery action available
/// (a new session/client); there is no documented cancellation recovery on the
/// pool itself.
#[tokio::test]
async fn recovery_contrast() -> Result<(), String> {
    for rep in 0..3 {
        let env = setup(&[LABEL]).await?;
        let record = env.record(LABEL)?.to_string();
        let cancelled = tokio::time::timeout(
            Duration::from_millis(5),
            env.pooled_update(&record, TARGET_V, "target"),
        )
        .await
        .is_err();
        let after_cancel = env.observer_read(&record).await;
        println!(
            "R3-ORI-RECOVERY rep={rep} phase=cancelled cancelled={cancelled} \
             observer={after_cancel:?} pool={:?} proxy_connections={}",
            env.pooled_stats().await,
            env.proxy.connections().len()
        );
        let stale = bounded(
            "stale pool survivor",
            env.pooled_update(&record, SURVIVOR_V, "survivor"),
        )
        .await;
        let stale_visible = env.observer_read(&record).await;
        println!("R3-ORI-RECOVERY rep={rep} stale_survivor={stale:?} observer={stale_visible:?}");

        let fresh_pool = pooled_client(env.proxy.addr(), 1).await?;
        let session = fresh_pool.get().await.map_err(e)?;
        let fresh = bounded(
            "fresh pool survivor",
            update_session(&session, &record, SURVIVOR_V, "survivor"),
        )
        .await;
        let fresh_visible = env.observer_read(&record).await;
        println!(
            "R3-ORI-RECOVERY rep={rep} fresh_survivor={fresh:?} observer={fresh_visible:?} \
             pool={:?} proxy_connections={}",
            env.pooled_stats().await,
            env.proxy.connections().len()
        );
        let _ = bounded("cleanup", env.cleanup()).await;
    }
    Ok(())
}

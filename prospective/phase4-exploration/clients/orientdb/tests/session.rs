//! Supplementary session-held ladder for orientdb-client 0.6.0.
//!
//! Setup checks out one pooled session and holds it. The target is the first
//! complete SQL script on that session (no checkout in the target future), so
//! cancelling it drops only the query future and the survivor uses the *same*
//! session guard. This is the session-level reuse unit; the main ladder uses
//! the pool-level unit (checkout included in the target).

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::time::Duration;

use dropwise::{Expect, Flavor, Race, Report, Settle, Verdict, explore, explore_observed};
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

async fn cut(ctx: &dropwise::Ctx, env: &SessionEnv) -> Result<bool, String> {
    let record = env.record(LABEL)?.to_string();
    let cancelled = match ctx.target(env.update(&record, TARGET_V, "target")).await {
        Some(reply) => {
            let reply = reply?;
            println!("R3-ORI-SESSION-TARGET-DONE reply={reply:?}");
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
        "R3-ORI-SESSION-RESULT level={level} plans={} violations={} unrealized={} unsettled={} \
         baseline_errors={} calibration_errors={} exhaustive={} pre={} post={} mixed={} \
         neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.calibration_errors.len(),
        report.exhaustive,
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    );
    for trial in &report.trials {
        println!(
            "R3-ORI-SESSION-TRIAL level={level} plan={} verdict={:?} realized={} invariant={:?}",
            trial.plan_label(),
            trial.verdict,
            trial.is_realized(),
            trial.invariant
        );
    }
    for error in &report.baseline_errors {
        println!("R3-ORI-SESSION-BASELINE-ERROR level={level} {error}");
    }
    for error in &report.calibration_errors {
        println!("R3-ORI-SESSION-CALIBRATION-ERROR level={level} {error}");
    }
}

fn b1() {
    let report = explore(&config(), |ctx| async move {
        let env = session_setup(&[LABEL]).await?;
        let _ = cut(&ctx, &env).await?;
        Ok(())
    });
    ladder::simple("S-B1", "session", &report);
    print_report("S-B1", &report);
}

fn b2() {
    let report = explore(&config(), |ctx| async move {
        let env = session_setup(&[LABEL]).await?;
        let cancelled = cut(&ctx, &env).await?;
        if cancelled {
            let record = env.record(LABEL)?.to_string();
            match bounded("retry on same session", env.update(&record, TARGET_V, "target")).await {
                Ok(reply) => println!("R3-ORI-SESSION-VALUE level=S-B2 retry=Ok({reply:?})"),
                Err(err) => {
                    println!("R3-ORI-SESSION-VALUE level=S-B2 retry=Err({err:?})");
                    return Err(format!("retry on the same session failed: {err}"));
                }
            }
        }
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(())
    });
    ladder::simple("S-B2", "session", &report);
    print_report("S-B2", &report);
}

fn b3() {
    let report = explore(&config(), |ctx| async move {
        let env = session_setup(&[LABEL]).await?;
        let cancelled = cut(&ctx, &env).await?;
        if cancelled {
            let record = env.record(LABEL)?.to_string();
            match bounded("different read on same session", env.read(&record)).await {
                Ok(reply) => println!("R3-ORI-SESSION-VALUE level=S-B3 read=Ok({reply:?})"),
                Err(err) => {
                    println!("R3-ORI-SESSION-VALUE level=S-B3 read=Err({err:?})");
                    return Err(format!("different operation on the same session failed: {err}"));
                }
            }
        }
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(())
    });
    ladder::simple("S-B3", "session", &report);
    print_report("S-B3", &report);
}

fn m1() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = session_setup(&[LABEL]).await?;
        cut(&ctx, &env).await?;
        let record = env.record(LABEL)?.to_string();
        let before = bounded("observer before survivor", env.observer_read(&record)).await;
        let write = bounded("survivor update", env.update(&record, SURVIVOR_V, "survivor")).await;
        let own = bounded("survivor read-back", env.read(&record)).await;
        let visible = bounded("observer after survivor", env.observer_read(&record)).await;
        let pool = env.stats().await;
        let obs = Obs { before, write, own, visible, pool };
        println!(
            "R3-ORI-SESSION-OBS before={:?} write={:?} own={:?} visible={:?} pool={:?} \
             proxy_connections={}",
            obs.before,
            obs.write,
            obs.own,
            obs.visible,
            obs.pool,
            env.env.proxy.connections().len()
        );
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(obs)
    });
    ladder::observed("S-M1", "session", &report);
    print_report("S-M1", &report);
}

#[test]
fn b1_session() { b1(); }
#[test]
fn b2_session() { b2(); }
#[test]
fn b3_session() { b3(); }
#[test]
fn m1_session() { m1(); }

// Round 2: bb8 0.8.6 original vs the Invalid-until-validated copy.
//
// The scenario shape is copied from the first-round tests/bolt.rs: one
// loopback proxy with a 50 ms reply delay, one Neo4j 5.26.0 container, one
// pooled bb8 handle (max size 1), MERGE ... SET ... RETURN, and the same
// Config/race set. The original project resolves bb8 0.8.6 from the registry;
// the fixed project uses a local path patch of the same 0.8.6 source.
//
// This harness prints R2-* lines. It does not assert outcomes, so the logs
// contain both clean and dirty runs.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use async_trait::async_trait;
use bb8_bolt::Manager;
use bb8_bolt::bb8::{ManageConnection, Pool};
use bolt_client::bolt_proto::Value;
use bolt_client::bolt_proto::version::*;
use bolt_client::error::{CommunicationError, Error as ClientError};
use bolt_client::Params;
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{Ctx, Expect, Report, Verdict, explore, explore_observed, replay_observed};

use crate::bolt::*;

async fn cut(ctx: &Ctx, env: &mut Env) -> Result<bool, String> {
    let cancelled = match ctx.target(env.write(TARGET_V)).await {
        Some(result) => {
            result?;
            false
        }
        None => true,
    };
    println!(
        "R2-CUT variant={VARIANT} level={} run={} cancelled={cancelled}",
        env.level, env.run
    );
    Ok(cancelled)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    before: Result<i64, String>,
    write: Result<i64, String>,
    own: Result<i64, String>,
    tag: Result<String, String>,
    visible: Result<i64, String>,
}

async fn observed_after_cut(env: &mut Env) -> Result<Obs, String> {
    let before = bounded("observer read before survivor", env.observer_value()).await;
    let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
    env.snapshot("after_survivor_write");
    let own = bounded("survivor read", env.read()).await;
    env.snapshot("after_own_read");
    let tag = bounded(
        "survivor tag read",
        env.read_tag(&format!("{}-survivor", env.tag)),
    )
    .await
    .map(|value| normalize(&env.tag, &value));
    let visible = bounded("observer read after survivor", env.observer_value()).await;
    let _ = bounded("cleanup", env.cleanup()).await;
    let obs = Obs { before, write, own, tag, visible };
    println!(
        "R2-OBS variant={VARIANT} level={} run={} before={:?} write={:?} own={:?} tag={:?} visible={:?}",
        env.level, env.run, obs.before, obs.write, obs.own, obs.tag, obs.visible
    );
    Ok(obs)
}

async fn checkout_scenario(ctx: Ctx) -> Result<Obs, String> {
    let mut env = setup("checkout").await?;
    env.snapshot("setup");
    let cancelled = match ctx.target(env.checkout()).await {
        Some(result) => {
            result?;
            false
        }
        None => true,
    };
    println!(
        "R2-CUT variant={VARIANT} level=checkout run={} cancelled={cancelled}",
        env.run
    );
    env.snapshot("after_target");
    observed_after_cut(&mut env).await
}

async fn pooled_m_scenario(ctx: Ctx) -> Result<Obs, String> {
    let mut env = setup("pooled_m").await?;
    env.snapshot("setup");
    cut(&ctx, &mut env).await?;
    env.snapshot("after_target");
    observed_after_cut(&mut env).await
}

fn report_summary(level: &str, report: &Report) {
    let plans = report.trials.len();
    let violations = report.violations().count();
    let unrealized = report.unrealized().count();
    let unsettled = report.unsettled().count();
    let unknown = report.unknown().count();
    println!(
        "R2-RESULT variant={VARIANT} level={level} plans={plans} violations={violations} \
         unrealized={unrealized} unsettled={unsettled} baseline_errors={} \
         calibration_errors={} exhaustive={} truncated={} baseline_targets={} \
         baseline_settled={} pre={} post={} mixed={} neither={} unknown={unknown}",
        report.baseline_errors.len(),
        report.calibration_errors.len(),
        report.exhaustive,
        report.truncated,
        report.baseline_targets,
        report.baseline_settled,
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
    );
    for trial in &report.trials {
        println!(
            "R2-TRIAL variant={VARIANT} level={level} plan={} sites={:?} outcomes={:?} \
             cancelled={} verdict={:?} settled={} realized={} invariant={:?}",
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
    for trial in report.violations() {
        println!(
            "R2-VIOLATION variant={VARIANT} level={level} plan={} detail={:?}",
            trial.plan_label(),
            trial.invariant
        );
    }
    for error in &report.baseline_errors {
        println!("R2-BASELINE-ERROR variant={VARIANT} level={level} {error}");
    }
    for error in &report.calibration_errors {
        println!("R2-CALIBRATION-ERROR variant={VARIANT} level={level} {error}");
    }
    for gap in report.coverage_gaps() {
        println!("R2-COVERAGE variant={VARIANT} level={level} {gap}");
    }
}

#[test]
fn checkout_only() {
    let report = explore_observed(&config(), Expect::Atomic, checkout_scenario);
    report_summary("checkout", &report);
}

#[test]
fn pooled_m() {
    let report = explore_observed(&config(), Expect::Atomic, pooled_m_scenario);
    report_summary("pooled_m", &report);
}

#[test]
fn pooled_b2() {
    let report = explore(&config(), |ctx| async move {
        let mut env = setup("pooled_b2").await?;
        env.snapshot("setup");
        let cancelled = cut(&ctx, &mut env).await?;
        env.snapshot("after_target");
        if cancelled {
            match bounded("retry of the same write", env.write(TARGET_V)).await {
                Ok(value) => {
                    println!("R2-VALUE variant={VARIANT} level=pooled_b2 retry=Ok({value})");
                }
                Err(err) => {
                    println!("R2-VALUE variant={VARIANT} level=pooled_b2 retry=Err({err:?})");
                    return Err(format!("retry of the same operation failed: {err}"));
                }
            }
        }
        env.snapshot("after_survivor");
        bounded("cleanup", env.cleanup()).await?;
        Ok(())
    });
    report_summary("pooled_b2", &report);
}

#[test]
fn pooled_b3() {
    let report = explore(&config(), |ctx| async move {
        let mut env = setup("pooled_b3").await?;
        env.snapshot("setup");
        let cancelled = cut(&ctx, &mut env).await?;
        env.snapshot("after_target");
        if cancelled {
            match bounded("different read", env.read()).await {
                Ok(value) => {
                    println!("R2-VALUE variant={VARIANT} level=pooled_b3 read=Ok({value})");
                }
                Err(err) => {
                    println!("R2-VALUE variant={VARIANT} level=pooled_b3 read=Err({err:?})");
                    return Err(format!("different operation failed: {err}"));
                }
            }
        }
        env.snapshot("after_survivor");
        bounded("cleanup", env.cleanup()).await?;
        Ok(())
    });
    report_summary("pooled_b3", &report);
}

#[tokio::test]
async fn normal_checkout() -> Result<(), String> {
    let mut env = setup("normal_checkout").await?;
    env.snapshot("setup");
    let mut values = Vec::new();
    for value in 0..3 {
        values.push(env.write(value).await?);
    }
    env.snapshot("after_3_writes");
    env.checkout().await?;
    env.snapshot("after_checkout_drop");
    let last = env.write(9).await?;
    env.snapshot("after_reuse");
    let state = format!("{:?}", env.pool.state());
    let connections = env.proxy.connections().len();
    println!(
        "R2-NORMAL variant={VARIANT} level=normal_checkout writes={values:?} last={last} \
         proxy_connections={connections} state={state}"
    );
    env.cleanup().await?;
    Ok(())
}

struct FlakyManager {
    inner: Manager,
    fail_next: Arc<AtomicUsize>,
}

#[async_trait]
impl ManageConnection for FlakyManager {
    type Connection = DirectClient;
    type Error = ClientError;

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        self.inner.connect().await
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        if self
            .fail_next
            .fetch_update(SeqCst, SeqCst, |n| if n > 0 { Some(n - 1) } else { None })
            .is_ok()
        {
            return Err(ClientError::from(CommunicationError::IoError(
                io::Error::new(
                    io::ErrorKind::Other,
                    "injected is_valid failure (round2 path check)",
                ),
            )));
        }
        self.inner.is_valid(conn).await
    }

    fn has_broken(&self, conn: &mut Self::Connection) -> bool {
        self.inner.has_broken(conn)
    }
}

#[tokio::test]
async fn is_valid_failure() -> Result<(), String> {
    let direct = url()?;
    let proxy = Proxy::start_with(
        upstream(&direct)?,
        ProxyOptions { reply_delay: REPLY_DELAY },
    )
    .await
    .map_err(e)?;
    let fail_next = Arc::new(AtomicUsize::new(0));
    let inner = Manager::new(
        proxy.addr().to_string(),
        None::<String>,
        [V4_4, V4_3, 0, 0],
        meta(),
    )
    .await
    .map_err(e)?;
    let manager = FlakyManager { inner, fail_next: Arc::clone(&fail_next) };
    let pool = Pool::builder()
        .max_size(1)
        .test_on_check_out(true)
        .build(manager)
        .await
        .map_err(e)?;
    let n = NEXT.fetch_add(1, SeqCst);
    let id = format!("dw-p4e-bolt-r2-isvalid-{}-{n}", std::process::id());
    let mut observer = connect_direct(&direct).await?;
    let params = Params::from_iter(vec![
        ("id", Value::from(id.as_str())),
        ("v", Value::from(0)),
    ]);
    run_pull(
        &mut observer,
        "CREATE (k:DwBolt {id: $id}) SET k.v = $v",
        Some(params),
    )
    .await?;

    {
        let mut conn = pool.get().await.map_err(e)?;
        write_on(&mut *conn, &id, 0).await?;
    }
    println!(
        "R2-ISVALID variant={VARIANT} level=is_valid_failure phase=warm state={:?}",
        pool.state()
    );

    fail_next.store(1, SeqCst);
    let mut conn = match pool.get().await {
        Ok(conn) => conn,
        Err(err) => {
            println!(
                "R2-ISVALID variant={VARIANT} level=is_valid_failure get=Err({err}) state={:?}",
                pool.state()
            );
            return Err(format!("pool.get after injected is_valid failure: {err}"));
        }
    };
    let value = write_on(&mut *conn, &id, SURVIVOR_V).await?;
    drop(conn);
    println!(
        "R2-ISVALID variant={VARIANT} level=is_valid_failure injected=1 get=Ok write=Ok({value}) \
         proxy_connections={} state={:?}",
        proxy.connections().len(),
        pool.state()
    );

    let params = Params::from_iter(vec![("id", Value::from(id.as_str()))]);
    run_pull(
        &mut observer,
        "MATCH (k:DwBolt {id: $id}) DETACH DELETE k",
        Some(params),
    )
    .await?;
    Ok(())
}

#[test]
fn replay_checkout() {
    let report = explore_observed(&config(), Expect::Atomic, checkout_scenario);
    report_summary("checkout-explore", &report);
    let failed = report
        .violations()
        .next()
        .cloned()
        .expect("round2 checkout exploration produced no violation to replay");
    let plan = failed.replay_plan();
    println!(
        "R2-REPLAY source_plan={} source_sites={:?} source_outcomes={:?} \
         source_verdict={:?} source_invariant={:?}",
        failed.plan_label(),
        failed.sites,
        failed.outcomes,
        failed.verdict,
        failed.invariant
    );
    for attempt in 1..=3 {
        match replay_observed(&config(), Expect::Atomic, &plan, checkout_scenario) {
            Ok(replay) => {
                report_summary("checkout-replay", &replay);
                for line in replay.diagnostics().lines() {
                    println!("R2-REPLAY-DIAG attempt={attempt} {line}");
                }
                let trial = replay
                    .trials
                    .first()
                    .expect("replay report has no trial");
                println!(
                    "R2-REPLAY attempt={attempt} realized={} plan={} sites={:?} outcomes={:?} \
                     cancelled={} verdict={:?} settled={} comparison={:?} invariant={:?}",
                    trial.is_realized(),
                    trial.plan_label(),
                    trial.sites,
                    trial.outcomes,
                    trial.cancelled,
                    trial.verdict,
                    trial.settled,
                    trial.comparison,
                    trial.invariant
                );
                if trial.is_realized() {
                    break;
                }
            }
            Err(err) => {
                println!("R2-REPLAY attempt={attempt} error={err:?}");
                break;
            }
        }
    }
}

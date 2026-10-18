//! Does enumerating real query Pending boundaries reach committed/unacknowledged?
//! The gate makes the window reproducible; it does not model natural frequency.
#[path = "support/reply_gate.rs"]
mod reply_gate;
use dropwise::{Config, Flavor, Race, Settle, explore};
use reply_gate::ReplyGate;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, SqlSafeStr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
};
use std::time::Duration;
static NEXT: AtomicUsize = AtomicUsize::new(0);
type Error = Box<dyn std::error::Error + Send + Sync>;

// Sample before dropping the original query, not after target().await returns:
// on a multithread runtime a post-return sample could certify a later commit.
struct ObserveDrop<F> {
    future: std::pin::Pin<Box<F>>,
    certified: Arc<AtomicBool>,
    at_drop: Arc<AtomicBool>,
}
impl<F: std::future::Future> std::future::Future for ObserveDrop<F> {
    type Output = F::Output;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}
impl<F> Drop for ObserveDrop<F> {
    fn drop(&mut self) {
        self.at_drop.store(self.certified.load(SeqCst), SeqCst);
    }
}

#[derive(Debug)]
struct Observation {
    cancelled: bool,
    certified_at_drop: bool,
}

async fn scenario(ctx: dropwise::Ctx, gated: bool) -> Result<Observation, Error> {
    let options: PgConnectOptions = std::env::var("DATABASE_URL")?.parse()?;
    if options.get_host() != "127.0.0.1" {
        return Err("loopback only".into());
    }
    let options = options.ssl_mode(PgSslMode::Disable);
    let table = format!(
        "dw_pending_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, SeqCst)
    );
    let mut observer = PgConnection::connect_with(&options).await?;
    sqlx::query(AssertSqlSafe(format!("CREATE TABLE {table} (n INT)")))
        .execute(&mut observer)
        .await?;
    let gate = Arc::new(ReplyGate::start(options.get_port()).await?);
    let result: Result<Observation, Error> = async {
        let mut client = PgConnection::connect_with(&options.port(gate.port)).await?;
        let operation = format!("INSERT INTO {table} VALUES (1) RETURNING n");
        client
            .prepare(AssertSqlSafe(operation.as_str()).into_sql_str())
            .await?;
        gate.hold(gated);
        let certified = Arc::new(AtomicBool::new(false));
        let certified_driver = certified.clone();
        let driver_gate = gate.clone();
        let driver_table = table.clone();
        let driver = tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_millis(300), async {
                loop {
                    let committed: bool = sqlx::query_scalar(AssertSqlSafe(format!(
                        "SELECT EXISTS(SELECT 1 FROM {driver_table})"
                    )))
                    .fetch_one(&mut observer)
                    .await?;
                    if committed && (!gated || driver_gate.withheld.load(SeqCst) > 0) {
                        certified_driver.store(true, SeqCst);
                        return Ok::<_, sqlx::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
            driver_gate.hold(false); // release even when the query never reached the DB
            (observer, result)
        });
        let at_drop = Arc::new(AtomicBool::new(false));
        let result = ctx
            .target(ObserveDrop {
                future: Box::pin(
                    sqlx::query_scalar::<_, i32>(AssertSqlSafe(operation)).fetch_one(&mut client),
                ),
                certified,
                at_drop: at_drop.clone(),
            })
            .await;
        let observed = Observation {
            cancelled: result.is_none(),
            certified_at_drop: at_drop.load(SeqCst),
        };
        // A false certificate means UNKNOWN, not proof of pre-commit cancellation.
        let (mut observer, driver_result) = driver.await?;
        if let Ok(result) = driver_result {
            result?;
        }
        if let Some(result) = result {
            result?;
        }
        // Drain old protocol messages on the same survivor before table cleanup.
        let _: i32 = sqlx::query_scalar("SELECT 42")
            .fetch_one(&mut client)
            .await?;
        sqlx::query(AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(&mut observer)
            .await?;
        client.close().await?;
        Ok(observed)
    }
    .await;
    let mut gate = Arc::try_unwrap(gate).map_err(|_| "driver still owns gate")?;
    gate.stop().await;
    result
}

#[test]
fn pending_enumeration_compared_with_a_reply_gate() {
    for gated in [false, true] {
        for race in [Race::Immediate, Race::AfterWake] {
            let observations = Arc::new(Mutex::new(Vec::new()));
            let output = observations.clone();
            let report = explore(
                &Config {
                    flavor: Flavor::MultiThread { workers: 1 },
                    races: vec![race],
                    max_runs: 64,
                    scenario_timeout: Duration::from_secs(5),
                    settle: Settle {
                        tokio_time: Duration::ZERO,
                        ..Settle::default()
                    },
                    ..Config::default()
                },
                move |ctx| {
                    let output = output.clone();
                    async move {
                        let observed = scenario(ctx, gated).await.map_err(|e| e.to_string())?;
                        output.lock().unwrap().push(observed);
                        Ok(())
                    }
                },
            );
            assert!(report.is_clean(), "{report}");
            let observations = observations.lock().unwrap();
            let hits = observations
                .iter()
                .filter(|o| o.cancelled && o.certified_at_drop)
                .count();
            let unknown = observations
                .iter()
                .filter(|o| o.cancelled && !o.certified_at_drop)
                .count();
            println!(
                "gated={gated} race={race:?}: certified committed/unacknowledged={hits}, cancelled-unknown={unknown}, unrealized={}, explored={}, exhaustive={}",
                report.unrealized().count(),
                report.trials.len(),
                report.exhaustive
            );
            if gated && race == Race::AfterWake {
                assert!(
                    hits > 0,
                    "gate + Pending/AfterWake should reach the window; observations={observations:?}, {report}"
                );
            }
        }
    }
}

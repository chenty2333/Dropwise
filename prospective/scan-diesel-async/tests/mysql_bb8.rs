//! diesel-async 0.9.2 `AsyncMysqlConnection` behind its bb8 manager.
//! Prints one RESCAN line per experiment; no outcome asserts.
//!
//! mysql_async marks a connection `disconnected` while a query runs and clears
//! the flag only when the query finishes. diesel-async's pooled `is_broken`
//! does not look at that flag, so a connection whose query was cancelled goes
//! back to the pool. Survivor: through the same pool, a query whose result is
//! known (`SELECT 67890`) and an autocommit write counted by an independent
//! connection. Environment: DW_MYSQL_URL (loopback server).

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use diesel::sql_types::BigInt;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::bb8::Pool;
use diesel_async::{AsyncConnection, AsyncMysqlConnection, RunQueryDsl};
use dropwise::{Config, Expect, Flavor, Race, Report, Settle, Verdict, explore_observed};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn url() -> Result<String, String> {
    std::env::var("DW_MYSQL_URL").map_err(e)
}

/// What the survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// Result of `SELECT 67890` (as BIGINT) through the pool.
    known: Result<i64, String>,
    /// Rows the survivor's write made visible to an independent connection.
    visible: Result<i64, String>,
}

struct Env {
    pool: Pool<AsyncMysqlConnection>,
    observer: AsyncMysqlConnection,
    table: String,
}

async fn setup(test_on_check_out: bool) -> Result<Env, String> {
    let url = url()?;
    let mut observer = AsyncMysqlConnection::establish(&url).await.map_err(e)?;
    let table = format!("dw_diesel_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    diesel::sql_query(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB"))
        .execute(&mut observer)
        .await
        .map_err(e)?;
    let manager = AsyncDieselConnectionManager::<AsyncMysqlConnection>::new(url);
    let pool = Pool::builder()
        .max_size(1)
        .test_on_check_out(test_on_check_out)
        .connection_timeout(Duration::from_secs(3))
        .build(manager)
        .await
        .map_err(e)?;
    // Warm the single pooled connection.
    select_int(&pool, 1).await?;
    Ok(Env { pool, observer, table })
}

/// `SELECT n` as BIGINT, the same column type as the cancelled query's, so a
/// stale result would decode instead of failing on a type mismatch.
async fn select_int(pool: &Pool<AsyncMysqlConnection>, n: i64) -> Result<i64, String> {
    let mut conn = pool.get().await.map_err(e)?;
    diesel::select(diesel::dsl::sql::<BigInt>(&format!("CAST({n} AS SIGNED)")))
        .get_result(&mut *conn)
        .await
        .map_err(e)
}

impl Env {
    async fn survive(mut self) -> Result<Obs, String> {
        let known = select_int(&self.pool, 67890).await;
        let write = async {
            let mut conn = self.pool.get().await.map_err(e)?;
            diesel::sql_query(format!("INSERT INTO {} VALUES (1)", self.table))
                .execute(&mut *conn)
                .await
                .map_err(e)
        }
        .await;
        let visible = match write {
            Ok(_) => diesel::select(diesel::dsl::sql::<BigInt>(&format!("(SELECT COUNT(*) FROM {})", self.table)))
                .get_result(&mut self.observer)
                .await
                .map_err(e),
            Err(err) => Err(format!("write: {err}")),
        };
        drop(self.pool);
        diesel::sql_query(format!("DROP TABLE {}", self.table)).execute(&mut self.observer).await.map_err(e)?;
        Ok(Obs { known, visible })
    }
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(15),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

fn summary(id: &str, report: &Report) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} pre={} post={} neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        report.baseline_errors.len(),
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    )
}

/// A borrower's query is cancelled while it waits for the result. The server
/// delays that result by 50 ms (`SLEEP`), so the query returns `Pending`; a
/// loopback server otherwise answers before the client polls.
fn query_cancelled(id: &str, test_on_check_out: bool) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup(test_on_check_out).await?;
        let query = async {
            let mut conn = env.pool.get().await.map_err(e)?;
            diesel::select(diesel::dsl::sql::<BigInt>("CAST(12345 + SLEEP(0.05) AS SIGNED)"))
                .get_result::<i64>(&mut *conn)
                .await
                .map_err(e)
        };
        if let Some(r) = ctx.target(query).await {
            r?;
        }
        env.survive().await
    });
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("{id} [{variant}]"), &report));
}

/// DA1: bb8 defaults (the pool validates a connection on checkout).
#[test]
fn da1_query_cancelled() {
    query_cancelled("DA1 diesel-async mysql query cancelled", true);
}

/// DA2: the same with `test_on_check_out(false)`, so the next borrower's own
/// query is the first to read from the connection.
#[test]
fn da2_query_cancelled_no_checkout_test() {
    query_cancelled("DA2 diesel-async mysql query cancelled, no checkout test", false);
}

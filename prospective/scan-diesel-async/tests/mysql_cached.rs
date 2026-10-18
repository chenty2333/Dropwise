//! Follow-up to mysql_bb8.rs with the statement shape diesel code usually has:
//! one parameterized statement, prepared once and then served from the
//! connection's statement cache. The cancelled query and the survivor run the
//! same statement with different parameters, so the survivor's request is a
//! plain execute of a cached statement. Prints one RESCAN line per experiment;
//! no outcome asserts. Environment: DW_MYSQL_URL (loopback server).

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use diesel::sql_types::{BigInt, Double};
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncConnection, AsyncMysqlConnection, RunQueryDsl};
use dropwise::{Config, Expect, Flavor, Race, Report, Settle, Verdict, explore_observed};

type Bb8Pool = diesel_async::pooled_connection::bb8::Pool<AsyncMysqlConnection>;
type DeadPool = diesel_async::pooled_connection::deadpool::Pool<AsyncMysqlConnection>;

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

#[derive(Clone, Copy)]
enum Kind {
    Bb8 { test_on_check_out: bool },
    Deadpool,
}

enum Pool {
    Bb8(Bb8Pool),
    Deadpool(DeadPool),
}

/// `CAST(? + SLEEP(?) AS SIGNED)`: one cached statement for every call.
async fn value(conn: &mut AsyncMysqlConnection, n: i64, delay: f64) -> Result<i64, String> {
    diesel::select(
        diesel::dsl::sql::<BigInt>("CAST(")
            .bind::<BigInt, _>(n)
            .sql(" + SLEEP(")
            .bind::<Double, _>(delay)
            .sql(") AS SIGNED)"),
    )
    .get_result(conn)
    .await
    .map_err(e)
}

impl Pool {
    async fn value(&self, n: i64, delay: f64) -> Result<i64, String> {
        match self {
            Pool::Bb8(pool) => value(&mut *pool.get().await.map_err(e)?, n, delay).await,
            Pool::Deadpool(pool) => value(&mut *pool.get().await.map_err(e)?, n, delay).await,
        }
    }

    async fn write(&self, sql: String) -> Result<usize, String> {
        match self {
            Pool::Bb8(pool) => diesel::sql_query(sql).execute(&mut *pool.get().await.map_err(e)?).await.map_err(e),
            Pool::Deadpool(pool) => diesel::sql_query(sql).execute(&mut *pool.get().await.map_err(e)?).await.map_err(e),
        }
    }
}

/// What the survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The cached statement with parameter 67890.
    own: Result<i64, String>,
    /// Rows the survivor's write made visible to an independent connection.
    visible: Result<i64, String>,
}

struct Env {
    pool: Pool,
    observer: AsyncMysqlConnection,
    table: String,
}

async fn setup(kind: Kind) -> Result<Env, String> {
    let url = std::env::var("DW_MYSQL_URL").map_err(e)?;
    let mut observer = AsyncMysqlConnection::establish(&url).await.map_err(e)?;
    let table = format!("dw_diesel_c_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    diesel::sql_query(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB"))
        .execute(&mut observer)
        .await
        .map_err(e)?;
    let manager = AsyncDieselConnectionManager::<AsyncMysqlConnection>::new(url);
    let pool = match kind {
        Kind::Bb8 { test_on_check_out } => Pool::Bb8(
            Bb8Pool::builder()
                .max_size(1)
                .test_on_check_out(test_on_check_out)
                .connection_timeout(Duration::from_secs(3))
                .build(manager)
                .await
                .map_err(e)?,
        ),
        Kind::Deadpool => Pool::Deadpool(DeadPool::builder(manager).max_size(1).build().map_err(e)?),
    };
    // Warm the single pooled connection and its statement cache.
    pool.value(1, 0.0).await?;
    Ok(Env { pool, observer, table })
}

impl Env {
    async fn survive(mut self) -> Result<Obs, String> {
        let own = self.pool.value(67890, 0.0).await;
        let visible = match self.pool.write(format!("INSERT INTO {} VALUES (1)", self.table)).await {
            Ok(_) => diesel::select(diesel::dsl::sql::<BigInt>(&format!("(SELECT COUNT(*) FROM {})", self.table)))
                .get_result(&mut self.observer)
                .await
                .map_err(e),
            Err(err) => Err(format!("write: {err}")),
        };
        drop(self.pool);
        diesel::sql_query(format!("DROP TABLE {}", self.table)).execute(&mut self.observer).await.map_err(e)?;
        Ok(Obs { own, visible })
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

/// The cancelled borrower runs the cached statement with parameter 12345 and a
/// 50 ms server-side delay, so it returns `Pending` while waiting.
fn cached_query_cancelled(id: &str, kind: Kind) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup(kind).await?;
        if let Some(r) = ctx.target(env.pool.value(12345, 0.05)).await {
            r?;
        }
        env.survive().await
    });
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("{id} [{variant}]"), &report));
}

/// DA3: bb8 defaults (validation on checkout).
#[test]
fn da3_cached_bb8() {
    cached_query_cancelled("DA3 cached statement, bb8", Kind::Bb8 { test_on_check_out: true });
}

/// DA4: bb8 without validation on checkout.
#[test]
fn da4_cached_bb8_no_checkout_test() {
    cached_query_cancelled("DA4 cached statement, bb8, no checkout test", Kind::Bb8 { test_on_check_out: false });
}

/// DA5: deadpool defaults (recycle before reuse).
#[test]
fn da5_cached_deadpool() {
    cached_query_cancelled("DA5 cached statement, deadpool", Kind::Deadpool);
}

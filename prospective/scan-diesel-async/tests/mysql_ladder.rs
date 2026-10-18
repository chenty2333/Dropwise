//! Ablation ladder for DA1-DA5 (RESCAN.md, section 6.7): the same cancellation
//! scenario with only what runs after the cancelled operation changed.
//!
//! B1 stops after the cancellation and is judged by `dropwise::explore` (panic,
//! liveness, obligation leak); B2 retries the same operation with the same
//! parameters through the same pool; B3 runs a different operation of the same
//! kind and checks only `Ok`; M is the `explore_observed(Expect::Atomic)`
//! survivor plus independent observer copied from mysql_bb8.rs and
//! mysql_cached.rs. Prints one LADDER line per experiment and one
//! LADDER-VIOLATION line per violating plan; no outcome asserts.
//! Environment: DW_MYSQL_URL (loopback server).

#[path = "../../support/ladder.rs"]
mod ladder;

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use diesel::sql_types::{BigInt, Double};
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncConnection, AsyncMysqlConnection, RunQueryDsl};
use dropwise::{Config, Expect, Flavor, Race, Settle, explore, explore_observed};

type Bb8Pool = diesel_async::pooled_connection::bb8::Pool<AsyncMysqlConnection>;
type DeadPool = diesel_async::pooled_connection::deadpool::Pool<AsyncMysqlConnection>;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// B2 and B3 give up after this long.
const RETRY: Duration = Duration::from_secs(3);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn url() -> Result<String, String> {
    std::env::var("DW_MYSQL_URL").map_err(e)
}

/// DA1/DA2 send their own SQL text on every call; DA3-DA5 share one cached
/// parameterized statement and change only its parameters.
#[derive(Clone, Copy)]
enum Kind {
    /// bb8, with the given `test_on_check_out`.
    Query { test_on_check_out: bool },
    /// bb8, with the given `test_on_check_out`.
    Cached { test_on_check_out: bool },
    /// deadpool, defaults.
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
    /// One parameterless SQL text, the shape DA1/DA2 send.
    async fn text(&self, sql: &str) -> Result<i64, String> {
        match self {
            Pool::Bb8(pool) => diesel::select(diesel::dsl::sql::<BigInt>(sql))
                .get_result(&mut *pool.get().await.map_err(e)?)
                .await
                .map_err(e),
            Pool::Deadpool(pool) => diesel::select(diesel::dsl::sql::<BigInt>(sql))
                .get_result(&mut *pool.get().await.map_err(e)?)
                .await
                .map_err(e),
        }
    }

    /// The cached statement with parameters `n` and `delay`.
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

/// `SELECT n` as BIGINT, the same column type as the cancelled query's, so a
/// stale result would decode instead of failing on a type mismatch.
async fn select_int(pool: &Pool, n: i64) -> Result<i64, String> {
    pool.text(&format!("CAST({n} AS SIGNED)")).await
}

/// What the M survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The survivor's known-value query through the pool.
    own: Result<i64, String>,
    /// Rows the survivor's write made visible to an independent connection.
    visible: Result<i64, String>,
}

struct Env {
    kind: Kind,
    pool: Pool,
    observer: AsyncMysqlConnection,
    table: String,
}

async fn setup(kind: Kind) -> Result<Env, String> {
    let url = url()?;
    let mut observer = AsyncMysqlConnection::establish(&url).await.map_err(e)?;
    let table = format!("dw_diesel_l_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    diesel::sql_query(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB"))
        .execute(&mut observer)
        .await
        .map_err(e)?;
    let manager = AsyncDieselConnectionManager::<AsyncMysqlConnection>::new(url);
    let pool = match kind {
        Kind::Query { test_on_check_out } | Kind::Cached { test_on_check_out } => Pool::Bb8(
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
    // Warm the single pooled connection; DA3-DA5 also cache the statement here.
    match kind {
        Kind::Query { .. } => select_int(&pool, 1).await?,
        Kind::Cached { .. } | Kind::Deadpool => pool.value(1, 0.0).await?,
    };
    Ok(Env { kind, pool, observer, table })
}

impl Env {
    /// The operation every level starts with. The server delays its result by
    /// 50 ms (`SLEEP`), so the query returns `Pending` while waiting.
    async fn query(&self) -> Result<i64, String> {
        match self.kind {
            Kind::Query { .. } => self.pool.text("CAST(12345 + SLEEP(0.05) AS SIGNED)").await,
            Kind::Cached { .. } | Kind::Deadpool => self.pool.value(12345, 0.05).await,
        }
    }

    /// B3: a different operation of the same kind through the same pool.
    async fn other(&self) -> Result<i64, String> {
        match self.kind {
            Kind::Query { .. } => select_int(&self.pool, 67890).await,
            Kind::Cached { .. } | Kind::Deadpool => self.pool.value(67890, 0.0).await,
        }
    }

    async fn survive(mut self) -> Result<Obs, String> {
        let own = match self.kind {
            Kind::Query { .. } => select_int(&self.pool, 67890).await,
            Kind::Cached { .. } | Kind::Deadpool => self.pool.value(67890, 0.0).await,
        };
        let write = self.pool.write(format!("INSERT INTO {} VALUES (1)", self.table)).await;
        let visible = match write {
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

/// B1: setup and the cancellation, nothing after it. The query is autocommit
/// and needs no cleanup, so its result is dropped either way.
fn b1(id: &str, kind: Kind) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(kind).await?;
        let _ = ctx.target(env.query()).await;
        Ok(())
    });
    ladder::simple("B1", id, &report);
}

/// B2: after the cancellation, the same operation with the same parameters
/// through the same pool. Only `Ok` is checked.
fn b2(id: &str, kind: Kind) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(kind).await?;
        match ctx.target(env.query()).await {
            Some(result) => {
                result?;
            }
            None => match tokio::time::timeout(RETRY, env.query()).await {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => return Err(format!("retry of the same operation failed: {err}")),
                Err(_) => return Err("retry of the same operation timed out".into()),
            },
        }
        Ok(())
    });
    ladder::simple("B2", id, &report);
}

/// B3: after the cancellation, a different operation of the same kind through
/// the same pool. Only `Ok` is checked.
fn b3(id: &str, kind: Kind) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(kind).await?;
        let cancelled = match ctx.target(env.query()).await {
            Some(result) => {
                result?;
                false
            }
            None => true,
        };
        if cancelled {
            match tokio::time::timeout(RETRY, env.other()).await {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => return Err(format!("different operation failed: {err}")),
                Err(_) => return Err("different operation timed out".into()),
            }
        }
        Ok(())
    });
    ladder::simple("B3", id, &report);
}

/// M: the full template, copied from mysql_bb8.rs and mysql_cached.rs.
fn m(id: &str, kind: Kind) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup(kind).await?;
        if let Some(r) = ctx.target(env.query()).await {
            r?;
        }
        env.survive().await
    });
    ladder::observed("M", id, &report);
}

const DA1: Kind = Kind::Query { test_on_check_out: true };
const DA2: Kind = Kind::Query { test_on_check_out: false };
const DA3: Kind = Kind::Cached { test_on_check_out: true };
const DA4: Kind = Kind::Cached { test_on_check_out: false };
const DA5: Kind = Kind::Deadpool;

/// DA1/B1: bb8 defaults, plain SQL text, stop after the cancellation.
#[test]
fn da1_b1() {
    b1("DA1", DA1);
}

/// DA1/B2: bb8 defaults, retry the same SQL text after the cancellation.
#[test]
fn da1_b2() {
    b2("DA1", DA1);
}

/// DA1/B3: bb8 defaults, a different SQL text after the cancellation.
#[test]
fn da1_b3() {
    b3("DA1", DA1);
}

/// DA1/M: bb8 defaults, survivor and independent observer.
#[test]
fn da1_m() {
    m("DA1", DA1);
}

/// DA2/B1: bb8 without the checkout test, plain SQL text.
#[test]
fn da2_b1() {
    b1("DA2", DA2);
}

/// DA2/B2: bb8 without the checkout test, retry the same SQL text.
#[test]
fn da2_b2() {
    b2("DA2", DA2);
}

/// DA2/B3: bb8 without the checkout test, a different SQL text.
#[test]
fn da2_b3() {
    b3("DA2", DA2);
}

/// DA2/M: bb8 without the checkout test, survivor and independent observer.
#[test]
fn da2_m() {
    m("DA2", DA2);
}

/// DA3/B1: cached statement, bb8 defaults.
#[test]
fn da3_b1() {
    b1("DA3", DA3);
}

/// DA3/B2: cached statement, retry with the same parameters.
#[test]
fn da3_b2() {
    b2("DA3", DA3);
}

/// DA3/B3: cached statement, a different parameter.
#[test]
fn da3_b3() {
    b3("DA3", DA3);
}

/// DA3/M: cached statement, survivor and independent observer.
#[test]
fn da3_m() {
    m("DA3", DA3);
}

/// DA4/B1: cached statement, bb8 without the checkout test.
#[test]
fn da4_b1() {
    b1("DA4", DA4);
}

/// DA4/B2: cached statement without the checkout test, same parameters.
#[test]
fn da4_b2() {
    b2("DA4", DA4);
}

/// DA4/B3: cached statement without the checkout test, a different parameter.
#[test]
fn da4_b3() {
    b3("DA4", DA4);
}

/// DA4/M: cached statement without the checkout test, survivor and observer.
#[test]
fn da4_m() {
    m("DA4", DA4);
}

/// DA5/B1: cached statement, deadpool defaults.
#[test]
fn da5_b1() {
    b1("DA5", DA5);
}

/// DA5/B2: cached statement on deadpool, retry with the same parameters.
#[test]
fn da5_b2() {
    b2("DA5", DA5);
}

/// DA5/B3: cached statement on deadpool, a different parameter.
#[test]
fn da5_b3() {
    b3("DA5", DA5);
}

/// DA5/M: cached statement on deadpool, survivor and independent observer.
#[test]
fn da5_m() {
    m("DA5", DA5);
}

#![cfg(feature = "net")]
//! DA1 and DA3 with a 50 ms reply delay in `dropwise::net::Proxy` instead of
//! `SLEEP(0.05)`: the server answers at once and the proxy holds every chunk
//! read from it, so the query returns `Pending` while the reply is outstanding.
//!
//! The observer connects directly to DW_MYSQL_URL; the pool connects through
//! the proxy, which stays alive for the whole scenario. Every test fails if
//! DW_MYSQL_URL is unset. Prints one RESCAN line per experiment; no outcome
//! asserts. Environment: DW_MYSQL_URL (loopback server).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use diesel::sql_types::BigInt;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::bb8::Pool;
use diesel_async::{AsyncConnection, AsyncMysqlConnection, RunQueryDsl};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{Config, Expect, Flavor, Race, Report, Settle, Verdict, explore_observed};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// How long the proxy holds every chunk read from the server.
const REPLY_DELAY: Duration = Duration::from_millis(50);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// The proxy tests must fail instead of skipping when the server is missing.
fn url() -> Result<String, String> {
    std::env::var("DW_MYSQL_URL").map_err(|err| format!("DW_MYSQL_URL is required: {err}"))
}

/// The host:port that follows the last '@'.
fn authority(rest: &str) -> &str {
    let end = rest.find('/').unwrap_or(rest.len());
    &rest[..end]
}

/// `mysql://user:pass@host:port/db`: the server address.
fn upstream(url: &str) -> Result<SocketAddr, String> {
    let (_, rest) = url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    authority(rest).parse().map_err(e)
}

/// The same URL with host:port after the last '@' replaced by `proxy`.
fn through(url: &str, proxy: SocketAddr) -> Result<String, String> {
    let (head, rest) = url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    let tail = &rest[authority(rest).len()..];
    Ok(format!("{head}@{proxy}{tail}"))
}

/// DA1 sends its own SQL text on every call; DA3 shares one cached
/// parameterized statement and changes only its parameter.
#[derive(Clone, Copy)]
enum Kind {
    Query,
    Cached,
}

/// What the survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The survivor's known-value query through the pool.
    own: Result<i64, String>,
    /// Rows the survivor's write made visible to an independent connection.
    visible: Result<i64, String>,
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

/// `CAST(? AS SIGNED)`: one cached statement with one parameter and no
/// server-side delay; the held reply supplies the `Pending`.
async fn value(conn: &mut AsyncMysqlConnection, n: i64) -> Result<i64, String> {
    diesel::select(diesel::dsl::sql::<BigInt>("CAST(").bind::<BigInt, _>(n).sql(" AS SIGNED)"))
        .get_result(conn)
        .await
        .map_err(e)
}

struct Env {
    kind: Kind,
    pool: Pool<AsyncMysqlConnection>,
    observer: AsyncMysqlConnection,
    table: String,
}

async fn setup(url: &str, pooled_url: &str, kind: Kind) -> Result<Env, String> {
    let mut observer = AsyncMysqlConnection::establish(url).await.map_err(e)?;
    let table = format!("dw_diesel_p_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    diesel::sql_query(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB"))
        .execute(&mut observer)
        .await
        .map_err(e)?;
    let manager = AsyncDieselConnectionManager::<AsyncMysqlConnection>::new(pooled_url.to_string());
    let pool = Pool::builder()
        .max_size(1)
        .test_on_check_out(true)
        .connection_timeout(Duration::from_secs(3))
        .build(manager)
        .await
        .map_err(e)?;
    // Warm the single pooled connection; DA3 also caches the statement here.
    match kind {
        Kind::Query => select_int(&pool, 1).await?,
        Kind::Cached => value(&mut *pool.get().await.map_err(e)?, 1).await?,
    };
    Ok(Env { kind, pool, observer, table })
}

impl Env {
    /// The cancelled operation: DA1's own SQL text, DA3's cached statement.
    async fn query(&self) -> Result<i64, String> {
        match self.kind {
            Kind::Query => select_int(&self.pool, 12345).await,
            Kind::Cached => value(&mut *self.pool.get().await.map_err(e)?, 12345).await,
        }
    }

    async fn survive(mut self) -> Result<Obs, String> {
        let own = match self.kind {
            Kind::Query => select_int(&self.pool, 67890).await,
            Kind::Cached => value(&mut *self.pool.get().await.map_err(e)?, 67890).await,
        };
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
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} \
         pre={} post={} mixed={} neither={} unknown={} reply_delay_ms=50",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        report.baseline_errors.len(),
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    )
}

fn proxy_observed(id: &str, kind: Kind) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let url = url()?;
        // One proxy per run, held until the survivor's observations are done.
        let proxy =
            Proxy::start_with(upstream(&url)?, ProxyOptions { reply_delay: REPLY_DELAY }).await.map_err(e)?;
        let pooled = through(&url, proxy.addr())?;
        let env = setup(&url, &pooled, kind).await?;
        if let Some(r) = ctx.target(env.query()).await {
            r?;
        }
        let observed = env.survive().await;
        drop(proxy);
        observed
    });
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("{id} [{variant}]"), &report));
}

/// DA1 with a held reply: bb8 defaults, a plain SQL text without `SLEEP`.
#[test]
fn da1_proxy() -> Result<(), String> {
    url()?;
    proxy_observed("DA1-proxy", Kind::Query);
    Ok(())
}

/// DA3 with a held reply: bb8 defaults, the cached statement without `SLEEP`.
#[test]
fn da3_proxy() -> Result<(), String> {
    url()?;
    proxy_observed("DA3-proxy", Kind::Cached);
    Ok(())
}

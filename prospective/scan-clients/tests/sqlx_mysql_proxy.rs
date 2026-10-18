//! sqlx 0.9.0 MySQL S1 with the BEGIN round trip held by
//! `dropwise::net::Proxy` instead of a server-side `SLEEP` (RESCAN.md,
//! section 6.6's held-reply technique, applied to 6.1's S1).
//!
//! S1 has no `SLEEP`: the target is exactly `Pool::begin()`, and the proxy
//! holds every upstream reply for 50 ms of real time so the BEGIN reply is
//! still in the proxy when the target is cancelled. The observer connects
//! directly to `DW_MYSQL_URL`; only the pool's connections go through the
//! proxy. Survivor: the same autocommit write as `sqlx_mysql.rs`, counted by
//! the independent connection. Environment: DW_MYSQL_URL (loopback server),
//! SCAN_VARIANT (label only). Linux only, and only with the `net` feature.

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore_observed, Config, Ctx, Expect, Flavor, Race, Report, Settle, Verdict};
use sqlx::mysql::{MySqlConnection, MySqlPool, MySqlPoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// `mysql://user:pass@HOST:PORT/db` -> `mysql://user:pass@PROXY/db`.
fn proxy_url(url: &str, proxy: SocketAddr) -> Result<String, String> {
    let (credentials, rest) =
        url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    let path = rest.find('/').ok_or_else(|| format!("DW_MYSQL_URL has no path: {url}"))?;
    Ok(format!("{credentials}@{proxy}{}", &rest[path..]))
}

/// The host:port after the last '@', parsed as the proxy's upstream address.
fn upstream_addr(url: &str) -> Result<SocketAddr, String> {
    let (_, rest) = url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    authority.parse().map_err(|e| format!("DW_MYSQL_URL authority {authority:?}: {e}"))
}

struct Env {
    pool: MySqlPool,
    observer: MySqlConnection,
    table: String,
}

async fn setup(url: &str, proxy: SocketAddr) -> Result<Env, String> {
    let mut observer = MySqlConnection::connect(url).await.map_err(e)?;
    let table = format!("dw_scan_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    observer
        .execute(AssertSqlSafe(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB")))
        .await
        .map_err(e)?;
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&proxy_url(url, proxy)?)
        .await
        .map_err(e)?;
    Ok(Env { pool, observer, table })
}

impl Env {
    fn insert(&self, n: i32) -> String {
        format!("INSERT INTO {} VALUES ({n})", self.table)
    }

    /// Survivor write through the pool, counted by the independent connection.
    /// Closing the pool ends any stale server transaction before the table is dropped.
    async fn survive(mut self) -> Result<i64, String> {
        self.pool.execute(AssertSqlSafe(self.insert(1))).await.map_err(e)?;
        let visible: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {}", self.table)))
            .fetch_one(&mut self.observer)
            .await
            .map_err(e)?;
        self.pool.close().await;
        self.observer.execute(AssertSqlSafe(format!("DROP TABLE {}", self.table))).await.map_err(e)?;
        Ok(visible)
    }
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(10),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

/// The S1 scenario with the proxy holding every reply for 50 ms.
async fn scenario(ctx: Ctx, url: String, upstream: SocketAddr) -> Result<i64, String> {
    let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: Duration::from_millis(50) })
        .await
        .map_err(e)?;
    let env = setup(&url, proxy.addr()).await?;
    if let Some(tx) = ctx.target(env.pool.begin()).await {
        tx.map_err(e)?.rollback().await.map_err(e)?;
    }
    env.survive().await
}

/// One line per experiment, for the rescan log. The field names keep the old
/// `allowed.rs` format; the counts now come from `Report` (trial verdicts only).
fn summary(id: &str, report: &Report) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} verdicts(all runs) pre={} post={} mixed={} neither={} unknown={} reply_delay_ms=50",
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

#[test]
fn s1_held_begin_reply() {
    // Fail loudly rather than silently skipping when the environment is missing.
    let url = std::env::var("DW_MYSQL_URL").expect("DW_MYSQL_URL must be set");
    let upstream = upstream_addr(&url).expect("DW_MYSQL_URL must be mysql://user:pass@HOST:PORT/db");
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario(ctx, url.clone(), upstream));
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!(
        "{}",
        summary(
            &format!("S1-proxy sqlx-mysql Pool::begin, 50 ms held BEGIN reply [{variant}]"),
            &report
        )
    );
}

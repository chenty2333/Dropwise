//! Small mysql_async primitives shared by the historical MY2 ladder and the
//! generated target/successor cases. Experiment-local, not Dropwise API.
//!
//! The direct-`Conn` shape is deliberate: the cancelled target future only
//! borrows the `Conn`, the handle stays in the scenario, and the successor uses
//! the same handle unless the caller explicitly enables the guarded recovery
//! path (`DW_MYSQL_GUARD=1`). Nothing here reconnects silently.
#![allow(dead_code)]

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, Pool};

/// Real time the proxy holds every server->client chunk.
pub const REPLY_DELAY: Duration = Duration::from_millis(50);
/// How long a retry/successor step may take before it is recorded as an error.
pub const BOUND: Duration = Duration::from_secs(3);

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub fn e(err: impl fmt::Display) -> String {
    err.to_string()
}

pub fn url() -> Result<String, String> {
    std::env::var("DW_MYSQL_URL").map_err(|err| format!("DW_MYSQL_URL is required: {err}"))
}

/// The host:port after the last '@'.
fn authority(rest: &str) -> &str {
    let end = rest.find('/').unwrap_or(rest.len());
    &rest[..end]
}

pub fn upstream(url: &str) -> Result<SocketAddr, String> {
    let (_, rest) =
        url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    authority(rest).parse().map_err(e)
}

/// The same URL with host:port after the last '@' replaced by the proxy.
pub fn through(url: &str, proxy: SocketAddr) -> Result<String, String> {
    let (head, rest) =
        url.rsplit_once('@').ok_or_else(|| format!("DW_MYSQL_URL has no '@': {url}"))?;
    let tail = &rest[authority(rest).len()..];
    Ok(format!("{head}@{proxy}{tail}"))
}

/// A pool of exactly one connection above the proxy URL.
pub fn one_conn_pool(url: &str) -> Result<Pool, String> {
    let separator = if url.contains('?') { '&' } else { '?' };
    let opts = Opts::from_url(&format!("{url}{separator}pool_min=1&pool_max=1")).map_err(e)?;
    Ok(Pool::new(opts))
}

pub fn table_name() -> String {
    format!("dw_p3_my_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

pub async fn connect(url: &str) -> Result<Conn, String> {
    Conn::new(Opts::from_url(url).map_err(e)?).await.map_err(e)
}

pub async fn create_table(conn: &mut Conn, table: &str) -> Result<(), String> {
    conn.query_drop(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB"))
        .await
        .map_err(e)
}

pub async fn drop_table(conn: &mut Conn, table: &str) -> Result<(), String> {
    conn.query_drop(format!("DROP TABLE {table}")).await.map_err(e)
}

/// The MY2 target: a known-value query on the directly held `Conn`.
pub async fn known(conn: &mut Conn) -> Result<i64, String> {
    select_value(conn, 12345).await
}

/// One concrete-value read. Kept as a value, not `is_ok()`.
pub async fn select_value(conn: &mut Conn, value: i64) -> Result<i64, String> {
    let rows: Vec<i64> = conn.query(format!("SELECT {value}")).await.map_err(e)?;
    rows.into_iter().next().ok_or_else(|| "query returned no row".to_string())
}

pub fn insert_sql(table: &str, n: i32) -> String {
    format!("INSERT INTO {table} VALUES ({n})")
}

pub async fn insert_row(conn: &mut Conn, table: &str, n: i32) -> Result<(), String> {
    conn.query_drop(insert_sql(table, n)).await.map_err(e)
}

pub async fn count_rows(conn: &mut Conn, table: &str) -> Result<i64, String> {
    conn.query_first::<i64, _>(format!("SELECT COUNT(*) FROM {table}"))
        .await
        .map_err(e)
        .and_then(|row| row.ok_or_else(|| "count returned no row".to_string()))
}

/// The historical usage contrast: a caller that explicitly checks the
/// public `Conn::is_disconnected()` flag and replaces the handle. This is a
/// separate configuration, not something the generator inserts implicitly.
pub async fn recover_direct(conn: &mut Option<Conn>, direct_url: &str) -> Result<bool, String> {
    let broken = conn.as_ref().map(Conn::is_disconnected).unwrap_or(false);
    if !broken {
        return Ok(false);
    }
    conn.take();
    *conn = Some(connect(direct_url).await?);
    Ok(true)
}

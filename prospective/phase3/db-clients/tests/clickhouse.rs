//! clickhouse-arrow 0.2.1 cancellation-ablation ladder (phase 3, database targets).
//!
//! Two operations, five levels each. Every level uses the same scenario
//! skeleton and the same `Config`; only what runs after a cancelled target
//! changes. The frozen target is the crate's bb8 manager (the `pool` feature,
//! `inner_pool` disabled by the manifest, so one `Client` is one native TCP
//! connection):
//!
//! - `CH1` (bb8 pool, session state): `execute("USE dw_p3_ch_<pid>_<n>")`, the
//!   first application round trip that establishes session state. The setup
//!   creates that database and a `Memory` table in it, and the pooled client
//!   starts in the server's `default` database. The survivor's write uses the
//!   **unqualified** table name, so it can only work if its connection is in
//!   the intended database.
//! - `CH2` (bb8 pool, ordinary request): `query_rows("SELECT 12345")` with the
//!   first row collected.
//!
//! Pending comes from `dropwise::net::Proxy` holding every server reply for
//! 50 ms of real time. The observer is a separate direct `ArrowClient` on the
//! same upstream address; the pool connects through the proxy, which stays
//! alive for the whole scenario. The pool is `max_size(1)` with a 3 s connection
//! timeout. `.with_check()` is **not** used: bb8's checkout validation then
//! only checks the client's local channel/status (`health_check(false)`), i.e.
//! no extra ping round trip through the proxy; a connection with a reply still
//! outstanding is handed to the survivor, which is what the experiment measures.
//!
//! `B1` stops after the cancellation, `B2` retries the same operation with the
//! same arguments through the same pool, `B3` runs a different operation of the
//! same kind and checks only `Ok`, `M` is `explore_observed(Expect::Atomic)`
//! with a survivor write confirmed by the independent observer, and
//! `M-residue` adds the bytes still unread in the client socket to the same
//! observation (after a 100 ms real-time settle).
//!
//! Environment: the pinned ClickHouse 25.5.2.47 native TCP address in
//! `DW_CLICKHOUSE_ADDR` (`DW_CLICKHOUSE_URL` is an alias); `run-db.sh` starts
//! that server. The 24.8.14.39 server is left running but cannot be used with
//! clickhouse-arrow 0.2.1:
//! revision 54469 is below `DBMS_MIN_PROTOCOL_VERSION_WITH_CHUNKED_PACKETS`
//! (54470), and the crate then hard-codes `ChunkedProtocolMode::default()` =
//! `ChunkedOptional` in `read_hello` instead of using the configured mode, so
//! the client chunks packets the server does not understand (server error
//! `UNKNOWN_PACKET_FROM_CLIENT`) and the caller hangs. 25.5.2.47 is the version
//! in the crate's own README benchmarks and tests.
//!
//! Prints one LADDER line per experiment and one LADDER-VIOLATION line per
//! violating plan. No outcome asserts.

#[path = "../../../support/ladder.rs"]
mod ladder;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use clickhouse_arrow::prelude::*;
use clickhouse_arrow::{ArrowClient, ArrowConnectionPoolBuilder, ConnectionPool, Value};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Settle};
use futures_util::StreamExt;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Diagnostics for the M-residue variant: largest unread-byte count any run
/// saw before the survivor started, and how many runs saw a non-zero count.
static RESIDUE_MAX: AtomicUsize = AtomicUsize::new(0);
static RESIDUE_NONZERO: AtomicUsize = AtomicUsize::new(0);

/// Real time the proxy holds every server->client chunk.
const REPLY_DELAY: Duration = Duration::from_millis(50);

/// How long B2/B3, the survivor, the observer and cleanup wait before giving up.
const BOUND: Duration = Duration::from_secs(3);

/// Real time M-residue lets the client's reader consume what the wire has
/// already delivered, before reading `Proxy::residue()`.
const RESIDUE_SETTLE: Duration = Duration::from_millis(100);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn upstream() -> Result<SocketAddr, String> {
    std::env::var("DW_CLICKHOUSE_ADDR")
        .or_else(|_| std::env::var("DW_CLICKHOUSE_URL"))
        .map_err(|err| format!("DW_CLICKHOUSE_ADDR is required (native TCP): {err}"))?
        .parse()
        .map_err(e)
}

/// The two target operations; they differ only in what is cancelled and what
/// the survivor writes.
#[derive(Clone, Copy)]
enum Op {
    /// CH1: a session-state statement (`USE <db>`).
    Session,
    /// CH2: an ordinary known-value read.
    Ordinary,
}

fn id(op: Op) -> &'static str {
    match op {
        Op::Session => "CH1",
        Op::Ordinary => "CH2",
    }
}

/// The cancelled operation. CH1's `USE` leaves the connection in the unique
/// database if it reaches the server; CH2's read has no side effect but leaves
/// a reply in flight if the client has stopped reading.
async fn cancelled(op: Op, env: &Env, client: &ArrowClient) -> Result<(), String> {
    match op {
        Op::Session => client.execute(format!("USE {}", env.db), None).await.map_err(e),
        Op::Ordinary => first_int(client, "SELECT 12345").await.map(|_| ()),
    }
}

/// B3's different operation of the same kind: another session-state statement,
/// or another known-value read.
async fn different(op: Op, client: &ArrowClient) -> Result<(), String> {
    match op {
        Op::Session => client.execute("SET max_threads = 1", None).await.map_err(e),
        Op::Ordinary => first_int(client, "SELECT 67890").await.map(|_| ()),
    }
}

/// `query_rows` until the first row's first value, decoded as an integer.
async fn first_int(client: &ArrowClient, sql: &str) -> Result<i64, String> {
    let mut stream = client.query_rows(sql, None).await.map_err(e)?;
    let rows = stream
        .next()
        .await
        .ok_or_else(|| "query returned no batch".to_string())?
        .map_err(e)?;
    let value = rows.into_iter().next().ok_or_else(|| "query returned no row".to_string())?;
    int_of(&value).ok_or_else(|| format!("unexpected value {value:?}"))
}

fn int_of(value: &Value) -> Option<i64> {
    match value {
        Value::Int8(v) => Some(i64::from(*v)),
        Value::Int16(v) => Some(i64::from(*v)),
        Value::Int32(v) => Some(i64::from(*v)),
        Value::Int64(v) => Some(*v),
        Value::UInt8(v) => Some(i64::from(*v)),
        Value::UInt16(v) => Some(i64::from(*v)),
        Value::UInt32(v) => Some(i64::from(*v)),
        Value::UInt64(v) => i64::try_from(*v).ok(),
        _ => None,
    }
}

/// Run `fut` with `BOUND`, flattening both the timeout and the future's error
/// into strings.
async fn within<T, F>(fut: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(BOUND, fut).await {
        Ok(result) => result,
        Err(_) => Err("timed out".to_string()),
    }
}

/// Map a driver error to a small deterministic category so the judged value
/// does not contain run-specific table names, query ids, or stack traces.
fn classify(err: &str) -> &'static str {
    if err.contains("UnknownTable") || err.contains("UNKNOWN_TABLE")
        || err.contains("does not exist") || err.contains("doesn't exist")
    {
        "unknown table"
    } else if err.contains("timed out") || err.contains("Timeout") || err.contains("TIMEOUT") {
        "timeout"
    } else {
        "other"
    }
}

/// Classify a survivor step's failure for the judged value. The pre calibration
/// of CH1 writes unqualified from the `default` database and legitimately gets
/// "unknown table"; that one is expected and not printed. Any other failure is
/// printed (one line) because it is triage material.
fn failed(step: &str, err: String) -> String {
    let category = classify(&err);
    if category != "unknown table" {
        eprintln!("CH {step} failed ({category}): {}", err.lines().next().unwrap_or(""));
    }
    format!("{step} failed: {category}")
}

/// What the M survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The survivor's own known-value read through the same pool.
    own: Result<i64, String>,
    /// Rows the survivor's write made visible to the independent client.
    visible: Result<i64, String>,
    /// Bytes still unread in the client socket before the survivor starts;
    /// `None` for M, `Some` for M-residue.
    residue: Option<usize>,
}

struct Env {
    op: Op,
    pool: ConnectionPool<ArrowFormat>,
    observer: ArrowClient,
    db: String,
    table: String,
    proxy: Proxy,
}

impl Env {
    /// Survivor: a known-value read and a side-effecting write through the
    /// same pooled client, counted by the independent client. A write failure
    /// is part of the observation, not a scenario error, because the `pre`
    /// calibration of CH1 leaves the connection outside the intended database.
    async fn survive(self, residue: Option<usize>) -> Result<Obs, String> {
        let Env { op, pool, observer, db, table, proxy: _ } = self;
        let write_sql = match op {
            Op::Session => format!("INSERT INTO {table} SELECT 1"),
            Op::Ordinary => format!("INSERT INTO {db}.{table} SELECT 1"),
        };
        let count_sql = format!("SELECT count() FROM {db}.{table}");
        let (own, write) = match within(async { pool.get().await.map_err(e) }).await {
            Ok(client) => {
                let own = match within(async { first_int(&client, "SELECT 67890").await }).await {
                    Ok(value) => Ok(value),
                    Err(err) => Err(failed("own read", err)),
                };
                let write = match within(async { client.execute(write_sql, None).await.map_err(e) }).await {
                    Ok(()) => Ok(()),
                    Err(err) => Err(failed("write", err)),
                };
                (own, write)
            }
            Err(err) => (Err(failed("own read", err.clone())), Err(failed("write", err))),
        };
        // The pooled connection is no longer needed and must be closed before
        // the observer drops the database it may still point at.
        drop(pool);
        let visible = match write {
            Ok(()) => match within(async { first_int(&observer, &count_sql).await }).await {
                Ok(count) => Ok(count),
                Err(err) => Err(failed("count", err)),
            },
            Err(err) => Err(err),
        };
        let drop_sql = format!("DROP DATABASE IF EXISTS {db}");
        match within(async { observer.execute(drop_sql, None).await.map_err(e) }).await {
            Ok(()) => {}
            Err(err) => eprintln!("CH cleanup of {db} failed: {err}"),
        }
        Ok(Obs { own, visible, residue })
    }
}

/// Start one run's proxy, open the observer directly, create the run's
/// database and table, and open the one-connection pool through the proxy.
async fn setup(op: Op) -> Result<Env, String> {
    let upstream = upstream()?;
    let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: REPLY_DELAY })
        .await
        .map_err(e)?;
    let db = format!("dw_p3_ch_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    let observer = within(async {
        ClientBuilder::new().with_endpoint(upstream.to_string()).build_arrow().await.map_err(e)
    })
    .await
    .map_err(|err| format!("observer connect: {err}"))?;
    within(async { observer.execute(format!("DROP DATABASE IF EXISTS {db}"), None).await.map_err(e) })
        .await
        .map_err(|err| format!("DROP DATABASE: {err}"))?;
    within(async { observer.execute(format!("CREATE DATABASE {db}"), None).await.map_err(e) })
        .await
        .map_err(|err| format!("CREATE DATABASE: {err}"))?;
    within(async {
        observer
            .execute(format!("CREATE TABLE {db}.t (n UInt8) ENGINE = Memory"), None)
            .await
            .map_err(e)
    })
    .await
    .map_err(|err| format!("CREATE TABLE: {err}"))?;
    let pool = ArrowConnectionPoolBuilder::new(proxy.addr())
        .configure_pool(|builder| builder.max_size(1).connection_timeout(BOUND))
        .build()
        .await
        .map_err(|err| format!("pool build: {}", e(err)))?;
    // bb8 is lazy: force the one native connection (and its handshake through
    // the proxy) now, so the target operation is the first application round trip.
    let client = within(async { pool.get().await.map_err(e) })
        .await
        .map_err(|err| format!("first checkout: {err}"))?;
    drop(client);
    Ok(Env { op, pool, observer, db, table: "t".to_string(), proxy })
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

/// B1: setup and the cancellation, nothing after it. A completed target is
/// dropped too; B1 judges only the cancellation.
fn b1(op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(op).await?;
        let client = env.pool.get().await.map_err(e)?;
        if let Some(result) = ctx.target(cancelled(op, &env, &client)).await {
            result?;
        }
        Ok(())
    });
    ladder::simple("B1", id(op), &report);
}

/// B2: after the cancellation, the same operation with the same arguments
/// through the same pool. Only `Ok` is checked; 3 s is failure.
fn b2(op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(op).await?;
        let client = env.pool.get().await.map_err(e)?;
        match ctx.target(cancelled(op, &env, &client)).await {
            Some(result) => result?,
            None => match within(cancelled(op, &env, &client)).await {
                Ok(()) => {}
                Err(err) => return Err(format!("retry of the same operation failed: {err}")),
            },
        }
        Ok(())
    });
    ladder::simple("B2", id(op), &report);
}

/// B3: after the cancellation, a different operation of the same kind through
/// the same pool. Only `Ok` is checked.
fn b3(op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = setup(op).await?;
        let client = env.pool.get().await.map_err(e)?;
        let cancelled = match ctx.target(cancelled(op, &env, &client)).await {
            Some(result) => {
                result?;
                false
            }
            None => true,
        };
        if cancelled {
            match within(different(op, &client)).await {
                Ok(()) => {}
                Err(err) => return Err(format!("different operation failed: {err}")),
            }
        }
        Ok(())
    });
    ladder::simple("B3", id(op), &report);
}

/// M or M-residue: the full template with the survivor and independent
/// observer. `residue` selects the M-residue variant.
fn m(op: Op, residue: bool) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let env = setup(op).await?;
        {
            let client = env.pool.get().await.map_err(e)?;
            if let Some(result) = ctx.target(cancelled(op, &env, &client)).await {
                result?;
            }
        }
        let residue = if residue {
            // Let a reply already on the wire reach the client's receive
            // buffer, then inspect it before the survivor reuses the pool.
            tokio::time::sleep(RESIDUE_SETTLE).await;
            let bytes = env
                .proxy
                .residue()
                .map_err(e)?
                .into_iter()
                .map(|entry| entry.bytes)
                .max()
                .unwrap_or(0);
            RESIDUE_MAX.fetch_max(bytes, SeqCst);
            if bytes > 0 {
                RESIDUE_NONZERO.fetch_add(1, SeqCst);
            }
            Some(bytes)
        } else {
            None
        };
        env.survive(residue).await
    });
    let level = if residue { "M-residue" } else { "M" };
    ladder::observed(level, id(op), &report);
    for note in &report.notes {
        println!("LADDER-NOTE {level} {}: {note}", id(op));
    }
    if residue {
        println!(
            "CH-RESIDUE {}: max={} runs_nonzero={}",
            id(op),
            RESIDUE_MAX.swap(0, SeqCst),
            RESIDUE_NONZERO.swap(0, SeqCst),
        );
    }
}

#[test]
fn ch1_b1() {
    b1(Op::Session);
}

#[test]
fn ch1_b2() {
    b2(Op::Session);
}

#[test]
fn ch1_b3() {
    b3(Op::Session);
}

#[test]
fn ch1_m() {
    m(Op::Session, false);
}

#[test]
fn ch1_m_residue() {
    m(Op::Session, true);
}

#[test]
fn ch2_b1() {
    b1(Op::Ordinary);
}

#[test]
fn ch2_b2() {
    b2(Op::Ordinary);
}

#[test]
fn ch2_b3() {
    b3(Op::Ordinary);
}

#[test]
fn ch2_m() {
    m(Op::Ordinary, false);
}

#[test]
fn ch2_m_residue() {
    m(Op::Ordinary, true);
}


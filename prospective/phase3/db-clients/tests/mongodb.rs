//! mongodb 3.9.1 cancellation-ablation ladder (phase 3, target 3).
//!
//! Two operations, five levels each. Every level uses the same scenario
//! skeleton and the same `Config`; only what runs after a cancelled target
//! changes:
//!
//! - `MG1` (session state): a new session, `start_transaction()`, then the
//!   first in-transaction command, an `insert_one` carrying
//!   `startTransaction: true`. The target future aborts the transaction if the
//!   insert completes. Cancelling it may leave a server-side transaction and/or
//!   an unread reply.
//! - `MG2` (ordinary request): a known-value `find_one` through the pooled
//!   client on a document the observer wrote during setup (`known`, 12345).
//!   The survivor reads a *different* document (`own`, 67890), so a reply
//!   carried over from the cancelled read is visible as the wrong value.
//!
//! Pending comes from `dropwise::net::Proxy` holding every server reply for
//! 50 ms of real time; the official `mongo:7.0.21` server is a single-node
//! replica set on 127.0.0.1:32878. The observer connects directly, the pooled
//! client goes through the proxy with `maxPoolSize=1&minPoolSize=0` and a
//! bounded server-selection timeout. Environment: DW_MONGO_URL (a loopback
//! single-node replica set; `run-db.sh` starts one per repetition). `B1` stops after the cancellation, `B2`
//! retries the same operation with the same arguments, `B3` runs a different
//! operation of the same kind and checks only `Ok`, `M` is
//! `explore_observed(Expect::Atomic)` with a survivor write confirmed by the
//! independent client, and `M-residue` adds the bytes still unread in the
//! client socket to the same observation.
//!
//! Prints one LADDER line per experiment and one LADDER-VIOLATION line per
//! violating plan. No outcome asserts.

#[path = "../../../support/ladder.rs"]
mod ladder;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Settle};
use mongodb::bson::{doc, Document};
use mongodb::{Client, Collection};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Real time the proxy holds every server->client chunk.
const REPLY_DELAY: Duration = Duration::from_millis(50);

/// How long B2/B3 and the survivor wait before giving up.
const BOUND: Duration = Duration::from_secs(3);

/// How long setup waits for the observer's write and the warm pooled read.
const SETUP_BOUND: Duration = Duration::from_secs(10);

const KNOWN: i64 = 12345;
const OWN: i64 = 67890;
const SURVIVOR: i64 = 424242;

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn url() -> Result<String, String> {
    std::env::var("DW_MONGO_URL")
        .map_err(|err| format!("DW_MONGO_URL is required (loopback single-node replica set): {err}"))
}

/// The `host:port` between the scheme and the first `/` or `?`.
fn authority(uri: &str) -> Result<&str, String> {
    let (_, rest) = uri.split_once("://").ok_or_else(|| format!("URL has no scheme: {uri}"))?;
    let end = rest.find(|c| c == '/' || c == '?').unwrap_or(rest.len());
    Ok(&rest[..end])
}

fn upstream(uri: &str) -> Result<SocketAddr, String> {
    authority(uri)?.parse().map_err(e)
}

/// The same URL with the server address replaced by the proxy.
fn through(uri: &str, proxy: SocketAddr) -> Result<String, String> {
    let (scheme, rest) = uri.split_once("://").ok_or_else(|| format!("URL has no scheme: {uri}"))?;
    let tail = &rest[authority(uri)?.len()..];
    Ok(format!("{scheme}://{proxy}{tail}"))
}

/// The proxy URL plus a one-connection pool and a bounded server selection.
fn pooled(uri: &str, proxy: SocketAddr) -> Result<String, String> {
    let mut uri = through(uri, proxy)?;
    uri.push(if uri.contains('?') { '&' } else { '?' });
    uri.push_str("maxPoolSize=1&minPoolSize=0&serverSelectionTimeoutMS=3000&connectTimeoutMS=3000");
    Ok(uri)
}

/// What the M survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// The survivor's own known-value read (document `own`, 67890) through
    /// the same pooled client.
    own: Result<i64, String>,
    /// Survivor documents the independent client can see with the expected
    /// value (`1` when the write is there, `0` when it is not).
    visible: Result<i64, String>,
    /// Bytes still unread in a client socket before the survivor starts;
    /// `None` for M, `Some` for M-residue.
    residue: Option<usize>,
}

/// The two target operations, chosen by the `docs/choosing-targets.md` order:
/// session state first, then an ordinary request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `MG1`: start a session and transaction, then insert in it.
    SessionTxn,
    /// `MG2`: a known-value read through the pooled client.
    OrdinaryRead,
}

impl Op {
    fn id(self) -> &'static str {
        match self {
            Op::SessionTxn => "MG1",
            Op::OrdinaryRead => "MG2",
        }
    }
}

/// The target future for each operation. For `MG1` it is the first command
/// that carries `startTransaction: true`; on completion it aborts the
/// transaction, as the mysql_async skeleton rolls back.
async fn target(op: Op, env: &Env) -> Result<(), String> {
    match op {
        Op::SessionTxn => txn_insert(&env.client, &env.coll, "mg1", 1).await,
        Op::OrdinaryRead => known_read(&env.coll).await.map(|_| ()),
    }
}

/// B3: a different operation of the same kind through the same pool.
async fn other(op: Op, env: &Env) -> Result<(), String> {
    match op {
        Op::SessionTxn => txn_upsert(&env.client, &env.coll).await,
        Op::OrdinaryRead => count_all(&env.coll).await.map(|_| ()),
    }
}

/// `MG1`'s cancelled operation: the transaction's first command. Aborting after
/// completion keeps the observation's "completed" side free of the insert, so
/// pre and post calibrations stay comparable.
async fn txn_insert(client: &Client, coll: &Collection<Document>, id: &str, value: i64) -> Result<(), String> {
    let mut session = client.start_session().await.map_err(e)?;
    session.start_transaction().await.map_err(e)?;
    coll.insert_one(doc! { "_id": id, "v": value })
        .session(&mut session)
        .await
        .map_err(e)?;
    session.abort_transaction().await.map_err(e)
}

/// `MG1`'s B3 operation: a different in-transaction write (upsert), aborted.
async fn txn_upsert(client: &Client, coll: &Collection<Document>) -> Result<(), String> {
    let mut session = client.start_session().await.map_err(e)?;
    session.start_transaction().await.map_err(e)?;
    coll.find_one_and_update(doc! { "_id": "mg1-other" }, doc! { "$set": { "v": 67890i64 } })
        .upsert(true)
        .session(&mut session)
        .await
        .map_err(e)?;
    session.abort_transaction().await.map_err(e)
}

/// One known-value read through the pooled client. The expected value is
/// checked so a stale reply carried over from another request is visible as a
/// mismatch rather than as a valid result.
async fn read_value(coll: &Collection<Document>, id: &str, expected: i64) -> Result<i64, String> {
    let value = coll
        .find_one(doc! { "_id": id })
        .await
        .map_err(e)?
        .and_then(|document| document.get_i64("v").ok())
        .ok_or_else(|| format!("document {id} is missing or has no i64 v"))?;
    if value != expected {
        return Err(format!("read of {id} returned {value}, expected {expected}"));
    }
    Ok(value)
}

/// `MG2`'s cancelled operation: a known-value read through the pooled client.
async fn known_read(coll: &Collection<Document>) -> Result<i64, String> {
    read_value(coll, "known", KNOWN).await
}

/// The survivor's own known-value read: a *different* document and value from
/// the cancelled read, so a carried-over reply shows up as the wrong value.
async fn own_read(coll: &Collection<Document>) -> Result<i64, String> {
    read_value(coll, "own", OWN).await
}

/// `MG2`'s B3 operation: a different read (a count) through the same pool.
async fn count_all(coll: &Collection<Document>) -> Result<u64, String> {
    coll.count_documents(doc! {}).await.map_err(e)
}

struct Env {
    client: Client,
    observer: Client,
    db: String,
    coll: Collection<Document>,
    proxy: Proxy,
}

impl Env {
    /// Survivor: a known-value read and a side-effecting write through the
    /// scenario's pool, checked by the independent client. The per-run
    /// database is left in place: dropping it would wait on the lock of a
    /// transaction left open by a cancelled MG1, and the disposable server is
    /// restarted between repetitions.
    async fn survive(self, residue: Option<usize>) -> Result<Obs, String> {
        let own = own_read(&self.coll).await;
        let write = self
            .coll
            .insert_one(doc! { "_id": "survivor", "v": SURVIVOR })
            .await
            .map_err(e);
        let visible = match write {
            Ok(_) => self
                .observer
                .database(&self.db)
                .collection::<Document>("c")
                .count_documents(doc! { "_id": "survivor", "v": SURVIVOR })
                .await
                .map_err(e)
                .map(|count| count as i64),
            Err(err) => Err(format!("write: {err}")),
        };
        Ok(Obs { own, visible, residue })
    }
}

/// Open the observer directly, write the known document, then open the pooled
/// client through the held-reply proxy and warm its single connection.
async fn setup(url: &str, proxy: Proxy) -> Result<Env, String> {
    let observer = Client::with_uri_str(url).await.map_err(e)?;
    let db = format!("dw_p3_mg_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    let observed = observer.database(&db).collection::<Document>("c");
    tokio::time::timeout(
        SETUP_BOUND,
        observed.insert_many(vec![
            doc! { "_id": "known", "v": KNOWN },
            doc! { "_id": "own", "v": OWN },
        ]),
    )
    .await
    .map_err(|_| "observer setup insert timed out".to_string())?
    .map_err(e)?;
    let pooled = pooled(url, proxy.addr())?;
    let client = Client::with_uri_str(&pooled).await.map_err(e)?;
    let coll = client.database(&db).collection::<Document>("c");
    // Warm the single pooled connection so the target's first Pending is the
    // in-transaction command, not the handshake.
    tokio::time::timeout(SETUP_BOUND, known_read(&coll))
        .await
        .map_err(|_| "warm pooled read timed out".to_string())??;
    Ok(Env { client, observer, db, coll, proxy })
}

/// One fresh proxy and one fresh database per scenario run.
async fn scenario_env(url: &str) -> Result<Env, String> {
    let upstream = upstream(url)?;
    let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: REPLY_DELAY })
        .await
        .map_err(e)?;
    setup(url, proxy).await
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

/// B1: setup and the cancellation, nothing after it. A completed target is
/// aborted by the target future itself; B1 judges only the cancellation.
fn b1(id: &str, op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = scenario_env(&url()?).await?;
        let _ = ctx.target(target(op, &env)).await;
        Ok(())
    });
    ladder::simple("B1", id, &report);
}

/// B2: after the cancellation, the same operation with the same arguments
/// through the same pool. Only `Ok` is checked; 3 s counts as failure.
fn b2(id: &str, op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = scenario_env(&url()?).await?;
        match ctx.target(target(op, &env)).await {
            Some(result) => result?,
            None => match tokio::time::timeout(BOUND, target(op, &env)).await {
                Ok(Ok(())) => {}
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
fn b3(id: &str, op: Op) {
    let report = explore(&config(), |ctx| async move {
        let env = scenario_env(&url()?).await?;
        let cancelled = match ctx.target(target(op, &env)).await {
            Some(result) => {
                result?;
                false
            }
            None => true,
        };
        if cancelled {
            match tokio::time::timeout(BOUND, other(op, &env)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(format!("different operation failed: {err}")),
                Err(_) => return Err("different operation timed out".into()),
            }
        }
        Ok(())
    });
    ladder::simple("B3", id, &report);
}

/// M or M-residue: the full template with a survivor and independent observer.
/// `residue` selects the M-residue variant.
fn m(id: &str, op: Op, residue: bool) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let env = scenario_env(&url()?).await?;
        if let Some(result) = ctx.target(target(op, &env)).await {
            result?;
        }
        let residue = if residue {
            // Let a reply already on the wire reach the client's receive
            // buffer, then inspect it before the survivor reuses the pool.
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some(
                env.proxy
                    .residue()
                    .map_err(e)?
                    .into_iter()
                    .map(|entry| entry.bytes)
                    .max()
                    .unwrap_or(0),
            )
        } else {
            None
        };
        env.survive(residue).await
    });
    let level = if residue { "M-residue" } else { "M" };
    ladder::observed(level, id, &report);
    for note in &report.notes {
        println!("LADDER-NOTE {level} {id}: {note}");
    }
}

const MG1: Op = Op::SessionTxn;
const MG2: Op = Op::OrdinaryRead;

#[test]
fn mg1_b1() {
    b1(MG1.id(), MG1);
}

#[test]
fn mg1_b2() {
    b2(MG1.id(), MG1);
}

#[test]
fn mg1_b3() {
    b3(MG1.id(), MG1);
}

#[test]
fn mg1_m() {
    m(MG1.id(), MG1, false);
}

#[test]
fn mg1_m_residue() {
    m(MG1.id(), MG1, true);
}

#[test]
fn mg2_b1() {
    b1(MG2.id(), MG2);
}

#[test]
fn mg2_b2() {
    b2(MG2.id(), MG2);
}

#[test]
fn mg2_b3() {
    b3(MG2.id(), MG2);
}

#[test]
fn mg2_m() {
    m(MG2.id(), MG2, false);
}

#[test]
fn mg2_m_residue() {
    m(MG2.id(), MG2, true);
}

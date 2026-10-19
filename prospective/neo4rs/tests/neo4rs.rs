//! neo4rs 0.8.0 cancellation-ablation ladder (paired-scan follow-up).
//!
//! Two pooled operations, four levels each. Every level uses the same scenario
//! skeleton and the same `Config`; only what runs after a cancelled target
//! changes:
//!
//! - `N1` (pooled write): `MATCH (k:DwK {id: $id}) SET k.v = $v` through
//!   `Graph::run`, i.e. RUN + DISCARD on a one-connection pool.
//! - `N2` (pooled read): `UNWIND [1, 2] ... RETURN $tag + ':' + toString(i)`
//!   through `Graph::execute`, drained one row per PULL (`fetch_size = 1`).
//!   The two rows make a cut inside a PULL exchange reachable; the tag makes a
//!   reply from another exchange recognizable.
//!
//! Pending comes from `dropwise::net::Proxy` holding every server reply for
//! 50 ms of real time. `B1` stops after the cancellation, `B2` repeats the same
//! operation with the same arguments, `B3` runs a different operation of the
//! same kind and checks only `Ok`, `M` is `explore_observed(Expect::Atomic)`
//! with a survivor write on the same node confirmed by an independent `Graph`,
//! and `M-residue` adds the bytes still unread in the client socket to the same
//! observation. Environment: DW_NEO4J_URL (loopback Neo4j community 5.26.0).
//!
//! Prints one LADDER line per experiment and one LADDER-VIOLATION line per
//! violating plan. No outcome asserts.

#[path = "../../support/ladder.rs"]
mod ladder;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use neo4rs::{query, ConfigBuilder, Graph};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Real time the proxy holds every server->client chunk.
const REPLY_DELAY: Duration = Duration::from_millis(50);

/// How long a retry or survivor step may take before it counts as hanging.
const BOUND: Duration = Duration::from_secs(5);

/// Value the target write sets.
const TARGET_V: i64 = 1;

/// Value the survivor write sets, and the value the survivor must read back.
const SURVIVOR_V: i64 = 7;

/// Rows `N2` returns per PULL (one record per PULL exchange).
const READ_FETCH_SIZE: usize = 1;

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn env(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.into())
}

fn url() -> Result<String, String> {
    std::env::var("DW_NEO4J_URL").map_err(|err| format!("DW_NEO4J_URL is required: {err}"))
}

/// The host:port of a `bolt://host:port` URL.
fn upstream(url: &str) -> Result<SocketAddr, String> {
    url.strip_prefix("bolt://")
        .unwrap_or(url)
        .trim_end_matches('/')
        .parse()
        .map_err(e)
}

/// The same URL pointed at the proxy.
fn through(proxy: SocketAddr) -> String {
    format!("bolt://{proxy}")
}

async fn connect(uri: &str, max: usize, fetch: usize) -> Result<Graph, String> {
    Graph::connect(
        ConfigBuilder::new()
            .uri(uri)
            .user(env("DW_NEO4J_USER", "neo4j"))
            .password(env("DW_NEO4J_PASSWORD", "dropwise"))
            .max_connections(max)
            .fetch_size(fetch)
            .build()
            .map_err(e)?,
    )
    .await
    .map_err(e)
}

/// The M survivor's observation. `write`, `own` and `visible` are the caller's
/// side-effecting write, the caller's own read through the same pool, and an
/// independent `Graph`'s read of the same node. `residue` is `None` for M and
/// `Some` for M-residue.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    write: Result<(), String>,
    own: Result<i64, String>,
    /// A known-value read of a tag that only this run knows. This is where a
    /// stale RECORD from the cancelled read would show up as a row, because the
    /// cancelled read returns the same `t` field.
    own_tag: Result<Vec<String>, String>,
    visible: Result<i64, String>,
    residue: Option<usize>,
}

struct Env {
    /// The pooled `Graph` under test, connected through the proxy.
    graph: Graph,
    /// Independent observer, not connected through the proxy or the pool.
    observer: Graph,
    id: String,
    /// A value only this share of the driver should be able to return.
    tag: String,
    proxy: Proxy,
}

impl Env {
    fn write(&self, value: i64) -> impl Future<Output = Result<(), String>> + '_ {
        let q = query("MATCH (k:DwK {id: $id}) SET k.v = $v")
            .param("id", self.id.clone())
            .param("v", value);
        async move { self.graph.run(q).await.map_err(e) }
    }

    async fn read(&self) -> Result<i64, String> {
        let q = query("MATCH (k:DwK {id: $id}) RETURN k.v AS v").param("id", self.id.clone());
        let mut rows = self.graph.execute(q).await.map_err(e)?;
        match rows.next().await.map_err(e)? {
            Some(row) => row.get::<i64>("v").map_err(e),
            None => Err("value query returned no row".into()),
        }
    }

    async fn read_tag(&self, tag: &str) -> Result<Vec<String>, String> {
        let q = query("UNWIND [1, 2] AS i RETURN $tag + ':' + toString(i) AS t ORDER BY t")
            .param("tag", tag.to_string());
        let mut rows = self.graph.execute(q).await.map_err(e)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(e)? {
            out.push(row.get::<String>("t").map_err(e)?);
        }
        Ok(out)
    }

    async fn observer_value(&self) -> Result<i64, String> {
        let q = query("MATCH (k:DwK {id: $id}) RETURN k.v AS v").param("id", self.id.clone());
        let mut rows = self.observer.execute(q).await.map_err(e)?;
        match rows.next().await.map_err(e)? {
            Some(row) => row.get::<i64>("v").map_err(e),
            None => Err("observer query returned no row".into()),
        }
    }

    async fn cleanup(&self) {
        let q = query("MATCH (k:DwK {id: $id}) DETACH DELETE k").param("id", self.id.clone());
        let _ = self.observer.run(q).await;
    }
}

/// Open the observer directly, create the node, then open the shared `Graph`
/// with exactly one connection above the held-reply proxy and warm it.
async fn setup() -> Result<Env, String> {
    let direct = url()?;
    let proxy = Proxy::start_with(
        upstream(&direct)?,
        ProxyOptions { reply_delay: REPLY_DELAY },
    )
    .await
    .map_err(e)?;
    let observer = connect(&direct, 2, 10).await?;
    let n = NEXT.fetch_add(1, SeqCst);
    let id = format!("dw-neo4rs-{}-{n}", std::process::id());
    let tag = format!("secret-{}-{n}", std::process::id());
    observer
        .run(query("CREATE (k:DwK {id: $id, v: 0})").param("id", id.clone()))
        .await
        .map_err(e)?;
    let graph = connect(&through(proxy.addr()), 1, READ_FETCH_SIZE).await?;
    // Warm the single connection so the first Pending of the target is its own
    // exchange or its pool recycle, not the HELLO handshake.
    graph
        .run(query("MATCH (k:DwK {id: $id}) SET k.v = 0").param("id", id.clone()))
        .await
        .map_err(e)?;
    Ok(Env { graph, observer, id, tag, proxy })
}

/// Strip the per-run tag so the observation is comparable across scenario
/// runs. A row from the cancelled read (`<tag>:1`) becomes `:1`; a row from the
/// survivor's own read becomes `-survivor:1`.
fn normalize(tag: &str, value: &str) -> String {
    value.strip_prefix(tag).unwrap_or(value).to_string()
}

async fn bounded<T>(
    label: &str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(BOUND, future).await {
        Ok(result) => result,
        Err(_) => Err(format!("{label} timed out after {BOUND:?}")),
    }
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

#[derive(Clone, Copy)]
enum Op {
    Write,
    Read,
}

impl Op {
    fn id(self) -> &'static str {
        match self {
            Op::Write => "N1",
            Op::Read => "N2",
        }
    }
}

/// Run the target and return whether it was cancelled.
async fn cut(ctx: &Ctx, env: &Env, op: Op) -> Result<bool, String> {
    let cancelled = match op {
        Op::Write => match ctx.target(env.write(TARGET_V)).await {
            Some(result) => {
                result?;
                false
            }
            None => true,
        },
        Op::Read => match ctx.target(env.read_tag(&env.tag)).await {
            Some(result) => {
                result?;
                false
            }
            None => true,
        },
    };
    Ok(cancelled)
}

/// B1: setup and the cancellation, nothing after it.
fn b1(op: Op) {
    let report = explore(&config(), move |ctx| async move {
        let env = setup().await?;
        let _ = cut(&ctx, &env, op).await;
        // No observer work after the cut: B1 judges the cancellation only. A
        // cancelled write can leave an open server transaction holding a write
        // lock, and a blocking observer DELETE would report that as a timeout.
        // The pooled connection closes when the scenario drops, which rolls the
        // transaction back.
        Ok(())
    });
    ladder::simple("B1", op.id(), &report);
    cleanup_prefix();
}

/// Delete this process's B1 nodes after the run, when no cancelled transaction
/// can hold their locks any more.
fn cleanup_prefix() {
    let prefix = format!("dw-neo4rs-{}-", std::process::id());
    let body = async {
        let observer = connect(&url()?, 2, 10).await?;
        let q = query("MATCH (k:DwK) WHERE k.id STARTS WITH $prefix DETACH DELETE k")
            .param("prefix", prefix);
        observer.run(q).await.map_err(e)
    };
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), body).await });
}

/// B2: after the cancellation, the same operation with the same arguments
/// through the same pool. Only `Ok` is checked.
fn b2(op: Op) {
    let report = explore(&config(), move |ctx| async move {
        let env = setup().await?;
        let cancelled = cut(&ctx, &env, op).await?;
        if cancelled {
            let retry = match op {
                Op::Write => bounded("retry of the same write", env.write(TARGET_V))
                    .await
                    .map(|()| String::new()),
                Op::Read => bounded("retry of the same read", env.read_tag(&env.tag))
                    .await
                    .map(|rows| format!("{rows:?}")),
            };
            match retry {
                Ok(detail) => {
                    if std::env::var("DW_DIAG").is_ok() {
                        eprintln!("{} B2 retry returned {detail}", op.id());
                    }
                }
                Err(err) => return Err(format!("retry of the same operation failed: {err}")),
            }
        }
        env.cleanup().await;
        Ok(())
    });
    ladder::simple("B2", op.id(), &report);
}

/// B3: after the cancellation, a different operation of the same kind through
/// the same pool. Only `Ok` is checked.
fn b3(op: Op) {
    let report = explore(&config(), move |ctx| async move {
        let env = setup().await?;
        let cancelled = cut(&ctx, &env, op).await?;
        if cancelled {
            let other = match op {
                Op::Write => bounded("different write", env.write(TARGET_V + 1))
                    .await
                    .map(|()| String::new()),
                Op::Read => {
                    let other = format!("{}-other", env.tag);
                    bounded("different read", env.read_tag(&other))
                        .await
                        .map(|rows| format!("{rows:?}"))
                }
            };
            match other {
                Ok(detail) => {
                    if std::env::var("DW_DIAG").is_ok() {
                        eprintln!("{} B3 different operation returned {detail}", op.id());
                    }
                }
                Err(err) => return Err(format!("different operation failed: {err}")),
            }
        }
        env.cleanup().await;
        Ok(())
    });
    ladder::simple("B3", op.id(), &report);
}

/// The survivor: a side-effecting write on the same node through the same
/// pool, the caller's own read of that node, and the independent observer's
/// read. Every step is bounded and its failure is part of the observation.
async fn survive(env: Env, residue: Option<usize>) -> Result<Obs, String> {
    let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
    let own = bounded("survivor read", env.read()).await;
    let own_tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
        .await
        .map(|rows| rows.iter().map(|value| normalize(&env.tag, value)).collect());
    let visible = bounded("observer read", env.observer_value()).await;
    env.cleanup().await;
    Ok(Obs { write, own, own_tag, visible, residue })
}

/// M or M-residue: the full template.
fn m(op: Op, residue: bool) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let env = setup().await?;
        cut(&ctx, &env, op).await?;
        let residue = if residue {
            // Let the replies for the cancelled exchange reach the client's
            // receive buffer before the survivor reuses the connection.
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
        let obs = survive(env, residue).await?;
        if std::env::var("DW_DIAG").is_ok() {
            eprintln!("{} survivor observation: {obs:?}", op.id());
        }
        Ok(obs)
    });
    let level = if residue { "M-residue" } else { "M" };
    ladder::observed(level, op.id(), &report);
    for note in &report.notes {
        println!("LADDER-NOTE {level} {}: {note}", op.id());
    }
}

#[test]
fn n1_b1() {
    b1(Op::Write);
}

#[test]
fn n1_b2() {
    b2(Op::Write);
}

#[test]
fn n1_b3() {
    b3(Op::Write);
}

#[test]
fn n1_m() {
    m(Op::Write, false);
}

#[test]
fn n1_m_residue() {
    m(Op::Write, true);
}

#[test]
fn n2_b1() {
    b1(Op::Read);
}

#[test]
fn n2_b2() {
    b2(Op::Read);
}

#[test]
fn n2_b3() {
    b3(Op::Read);
}

#[test]
fn n2_m() {
    m(Op::Read, false);
}

#[test]
fn n2_m_residue() {
    m(Op::Read, true);
}

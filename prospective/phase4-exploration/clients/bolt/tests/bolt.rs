//! bolt-client 0.11.0 cancellation ladder: pooled (bb8-bolt) and direct
//! handle, same Neo4j server and same operation.
//!
//! Target: RUN + PULL of one auto-commit write on a unique node. The survivor
//! writes the same node through the same pool/handle, reads it back, and an
//! independent Client (not through the proxy or the tested pool) reads the
//! server-side value. Pending comes from a loopback proxy holding every
//! server->client chunk for 50 ms. Prints LADDER lines; no outcome asserts.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use bb8_bolt::bb8::Pool;
use bb8_bolt::Manager;
use bolt_client::bolt_proto::message::Success;
use bolt_client::bolt_proto::version::*;
use bolt_client::bolt_proto::{Message, Value};
use bolt_client::{Client, Metadata, Params, Stream};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use tokio::io::BufStream;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const REPLY_DELAY: Duration = Duration::from_millis(50);
const BOUND: Duration = Duration::from_secs(5);
const TARGET_V: i64 = 1;
const SURVIVOR_V: i64 = 7;

type DirectClient = Client<Compat<BufStream<Stream>>>;
type BoltPool = Pool<Manager>;

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn url() -> Result<String, String> {
    std::env::var("DW_NEO4J_URL").map_err(|err| format!("DW_NEO4J_URL is required: {err}"))
}

fn upstream(url: &str) -> Result<SocketAddr, String> {
    url.strip_prefix("bolt://")
        .unwrap_or(url)
        .trim_end_matches('/')
        .parse()
        .map_err(e)
}

fn meta() -> Metadata {
    let user = std::env::var("DW_NEO4J_USER").unwrap_or_else(|_| "neo4j".into());
    let password = std::env::var("DW_NEO4J_PASSWORD").unwrap_or_else(|_| "dropwise".into());
    Metadata::from_iter(vec![
        ("user_agent", "dropwise-p4e/0.1"),
        ("scheme", "basic"),
        ("principal", user.as_str()),
        ("credentials", password.as_str()),
    ])
}

async fn connect_direct(addr: &str) -> Result<DirectClient, String> {
    let addr = addr.strip_prefix("bolt://").unwrap_or(addr).trim_end_matches('/');
    let stream = Stream::connect(addr, None::<&str>).await.map_err(e)?;
    let stream = BufStream::new(stream).compat();
    let mut client = Client::new(stream, &[V4_4, V4_3, 0, 0]).await.map_err(e)?;
    let hello = client.hello(meta()).await.map_err(e)?;
    Success::try_from(hello).map_err(|m| format!("HELLO failed: {m:?}"))?;
    Ok(client)
}

async fn connect_pooled(addr: &str, validate: bool) -> Result<BoltPool, String> {
    let manager = Manager::new(addr.to_string(), None::<String>, [V4_4, V4_3, 0, 0], meta())
        .await
        .map_err(e)?;
    Pool::builder()
        .max_size(1)
        .test_on_check_out(validate)
        .build(manager)
        .await
        .map_err(e)
}

fn first_int(values: Vec<Value>) -> Result<i64, String> {
    match values.into_iter().next() {
        Some(Value::Integer(value)) => Ok(value),
        Some(other) => Err(format!("expected integer, got {other:?}")),
        None => Err("query returned no row".into()),
    }
}

fn first_string(values: Vec<Value>) -> Result<String, String> {
    match values.into_iter().next() {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(format!("expected string, got {other:?}")),
        None => Err("query returned no row".into()),
    }
}

async fn run_pull(
    client: &mut DirectClient,
    query: &str,
    params: Option<Params>,
) -> Result<Vec<Value>, String> {
    let response: Message = client.run(query, params, None).await.map_err(e)?;
    Success::try_from(response).map_err(|m| format!("RUN failed: {m:?}"))?;
    let (records, summary) = client
        .pull(Some(Metadata::from_iter(vec![("n", -1)])))
        .await
        .map_err(e)?;
    Success::try_from(summary).map_err(|m| format!("PULL failed: {m:?}"))?;
    Ok(records
        .into_iter()
        .map(|record| record.fields()[0].clone())
        .collect())
}

async fn write_on(client: &mut DirectClient, id: &str, value: i64) -> Result<i64, String> {
    let params = Params::from_iter(vec![
        ("id", Value::from(id)),
        ("v", Value::from(value)),
    ]);
    let rows = run_pull(
        client,
        "MERGE (k:DwBolt {id: $id}) SET k.v = $v RETURN k.v AS v",
        Some(params),
    )
    .await?;
    first_int(rows)
}

async fn read_on(client: &mut DirectClient, id: &str) -> Result<i64, String> {
    let params = Params::from_iter(vec![("id", Value::from(id))]);
    let rows = run_pull(
        client,
        "MATCH (k:DwBolt {id: $id}) RETURN k.v AS v",
        Some(params),
    )
    .await?;
    first_int(rows)
}

async fn tag_on(client: &mut DirectClient, tag: &str) -> Result<String, String> {
    let params = Params::from_iter(vec![("tag", Value::from(tag))]);
    let rows = run_pull(client, "RETURN $tag AS t", Some(params)).await?;
    first_string(rows)
}

enum Handle {
    Pooled(BoltPool),
    Direct(DirectClient),
}

struct Env {
    handle: Handle,
    observer: DirectClient,
    id: String,
    tag: String,
    proxy: Proxy,
}

impl Env {
    async fn write(&mut self, value: i64) -> Result<i64, String> {
        let id = self.id.clone();
        match &mut self.handle {
            Handle::Pooled(pool) => {
                let mut conn = pool.get().await.map_err(e)?;
                write_on(&mut *conn, &id, value).await
            }
            Handle::Direct(client) => write_on(client, &id, value).await,
        }
    }

    async fn read(&mut self) -> Result<i64, String> {
        let id = self.id.clone();
        match &mut self.handle {
            Handle::Pooled(pool) => {
                let mut conn = pool.get().await.map_err(e)?;
                read_on(&mut *conn, &id).await
            }
            Handle::Direct(client) => read_on(client, &id).await,
        }
    }

    async fn read_tag(&mut self, tag: &str) -> Result<String, String> {
        match &mut self.handle {
            Handle::Pooled(pool) => {
                let mut conn = pool.get().await.map_err(e)?;
                tag_on(&mut *conn, tag).await
            }
            Handle::Direct(client) => tag_on(client, tag).await,
        }
    }

    async fn observer_value(&mut self) -> Result<i64, String> {
        let id = self.id.clone();
        read_on(&mut self.observer, &id).await
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let id = self.id.clone();
        let params = Params::from_iter(vec![("id", Value::from(id))]);
        run_pull(
            &mut self.observer,
            "MATCH (k:DwBolt {id: $id}) DETACH DELETE k",
            Some(params),
        )
        .await
        .map(|_| ())
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Pooled,
    Direct,
}

impl Mode {
    fn id(self) -> &'static str {
        match self {
            Mode::Pooled => "P1",
            Mode::Direct => "D1",
        }
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

async fn setup(mode: Mode) -> Result<Env, String> {
    setup_with(mode, true).await
}

async fn setup_with(mode: Mode, validate_checkout: bool) -> Result<Env, String> {
    let direct = url()?;
    let proxy = Proxy::start_with(
        upstream(&direct)?,
        ProxyOptions { reply_delay: REPLY_DELAY },
    )
    .await
    .map_err(e)?;
    let mut observer = connect_direct(&direct).await?;
    let n = NEXT.fetch_add(1, SeqCst);
    let id = format!("dw-p4e-bolt-{}-{n}", std::process::id());
    let tag = format!("p4e-tag-{}-{n}", std::process::id());
    let params = Params::from_iter(vec![
        ("id", Value::from(id.as_str())),
        ("v", Value::from(0)),
    ]);
    run_pull(
        &mut observer,
        "CREATE (k:DwBolt {id: $id}) SET k.v = $v",
        Some(params),
    )
    .await?;
    let handle = match mode {
        Mode::Pooled => {
            Handle::Pooled(connect_pooled(&proxy.addr().to_string(), validate_checkout).await?)
        }
        Mode::Direct => Handle::Direct(connect_direct(&proxy.addr().to_string()).await?),
    };
    let mut env = Env { handle, observer, id, tag, proxy };
    // Warm-up: consume connection setup/HELLO and make the first target
    // Pending its own exchange.
    env.write(0).await?;
    Ok(env)
}

async fn cut(ctx: &Ctx, env: &mut Env) -> Result<bool, String> {
    match ctx.target(env.write(TARGET_V)).await {
        Some(result) => {
            result?;
            Ok(false)
        }
        None => Ok(true),
    }
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

fn b1(mode: Mode) {
    let report = explore(&config(), move |ctx| async move {
        let mut env = setup(mode).await?;
        let _ = cut(&ctx, &mut env).await?;
        // B1 judges the cancellation only. The scenario drops the pool/handle;
        // a leftover server transaction is rolled back when the socket closes.
        Ok(())
    });
    ladder::simple("B1", mode.id(), &report);
    cleanup_prefix();
}

fn cleanup_prefix() {
    let prefix = format!("dw-p4e-bolt-{}-", std::process::id());
    let body = async {
        let direct = url()?;
        let mut observer = connect_direct(&direct).await?;
        let params = Params::from_iter(vec![("prefix", Value::from(prefix))]);
        run_pull(
            &mut observer,
            "MATCH (k:DwBolt) WHERE k.id STARTS WITH $prefix DETACH DELETE k",
            Some(params),
        )
        .await
        .map(|_| ())
    };
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), body).await });
}

fn b2(mode: Mode) {
    let report = explore(&config(), move |ctx| async move {
        let mut env = setup(mode).await?;
        let cancelled = cut(&ctx, &mut env).await?;
        if cancelled {
            match bounded("retry of the same write", env.write(TARGET_V)).await {
                Ok(value) => {
                    if std::env::var("DW_DIAG").is_ok() {
                        eprintln!("{} B2 retry returned Ok({value})", mode.id());
                    }
                }
                Err(err) => return Err(format!("retry of the same operation failed: {err}")),
            }
        }
        bounded("cleanup", env.cleanup()).await?;
        Ok(())
    });
    ladder::simple("B2", mode.id(), &report);
}

fn b3(mode: Mode) {
    let report = explore(&config(), move |ctx| async move {
        let mut env = setup(mode).await?;
        let cancelled = cut(&ctx, &mut env).await?;
        if cancelled {
            match bounded("different read", env.read()).await {
                Ok(value) => {
                    if std::env::var("DW_DIAG").is_ok() {
                        eprintln!("{} B3 different read returned Ok({value})", mode.id());
                    }
                }
                Err(err) => return Err(format!("different operation failed: {err}")),
            }
        }
        bounded("cleanup", env.cleanup()).await?;
        Ok(())
    });
    ladder::simple("B3", mode.id(), &report);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    before: Result<i64, String>,
    write: Result<i64, String>,
    own: Result<i64, String>,
    tag: Result<String, String>,
    visible: Result<i64, String>,
}

fn normalize(tag: &str, value: &str) -> String {
    value.strip_prefix(tag).unwrap_or(value).to_string()
}

fn m(mode: Mode) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup(mode).await?;
        cut(&ctx, &mut env).await?;
        let before = bounded("observer read before survivor", env.observer_value()).await;
        let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
        let own = bounded("survivor read", env.read()).await;
        let tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
            .await
            .map(|value| normalize(&env.tag, &value));
        let visible = bounded("observer read after survivor", env.observer_value()).await;
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(Obs { before, write, own, tag, visible })
    });
    ladder::observed("M", mode.id(), &report);
    for note in &report.notes {
        println!("LADDER-NOTE M {}: {note}", mode.id());
    }
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn pooled_b1() { b1(Mode::Pooled); }
#[test]
fn pooled_b2() { b2(Mode::Pooled); }
#[test]
fn pooled_b3() { b3(Mode::Pooled); }
#[test]
fn pooled_m() { m(Mode::Pooled); }

#[test]
fn direct_b1() { b1(Mode::Direct); }
#[test]
fn direct_b2() { b2(Mode::Direct); }
#[test]
fn direct_b3() { b3(Mode::Direct); }
#[test]
fn direct_m() { m(Mode::Direct); }
// Preflight: no cancellation. Confirms that setup, both writes, the same-handle
// read and the independent observer work before the ladder is interpreted.
async fn normal(mode: Mode) -> Result<(), String> {
    let mut env = setup(mode).await?;
    let target = env.write(TARGET_V).await?;
    let survivor = env.write(SURVIVOR_V).await?;
    let own = env.read().await?;
    let visible = env.observer_value().await?;
    let connections = env.proxy.connections().len();
    let pooled_state = match &env.handle {
        Handle::Pooled(pool) => format!("{:?}", pool.state()),
        Handle::Direct(_) => "direct".to_string(),
    };
    println!(
        "PREFLIGHT {} target={target} survivor={survivor} own={own} visible={visible} \
         proxy_connections={connections} state={pooled_state}",
        mode.id()
    );
    env.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn preflight_pooled() -> Result<(), String> {
    normal(Mode::Pooled).await
}

#[tokio::test]
async fn preflight_direct() -> Result<(), String> {
    normal(Mode::Direct).await
}

/// Reuse diagnostic: three sequential no-cancellation writes on the same
/// pool/handle. The proxy connection count and the pool state are printed.
async fn reuse(mode: Mode) -> Result<(), String> {
    let mut env = setup(mode).await?;
    let mut values = Vec::new();
    for value in 0..3 {
        values.push(env.write(value).await?);
    }
    let connections = env.proxy.connections().len();
    let pooled_state = match &env.handle {
        Handle::Pooled(pool) => format!("{:?}", pool.state()),
        Handle::Direct(_) => "direct".to_string(),
    };
    println!(
        "REUSE {} writes={values:?} proxy_connections={connections} state={pooled_state}",
        mode.id()
    );
    env.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn reuse_pooled() -> Result<(), String> {
    reuse(Mode::Pooled).await
}

#[tokio::test]
async fn reuse_direct() -> Result<(), String> {
    reuse(Mode::Direct).await
}
impl Env {
    /// Explicit caller recovery after a cancelled exchange: send one RESET on
    /// the same handle and report what it returned. Only used by the added
    /// direct-handle exploration, not by the original ladder.
    async fn reset(&mut self) -> Result<String, String> {
        match &mut self.handle {
            Handle::Direct(client) => {
                let response = client.reset().await.map_err(e)?;
                Success::try_from(response).map_err(|m| format!("RESET failed: {m:?}"))?;
                Ok("ok".into())
            }
            Handle::Pooled(_) => Err("reset is only used on the direct handle".into()),
        }
    }
}

/// Added exploration after the original direct results: the caller sends one
/// explicit RESET on the same handle before the survivor. This is the minimal
/// caller recovery the public API offers; it is not part of the original
/// ladder and does not replace it.
fn direct_m_reset() {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup(Mode::Direct).await?;
        cut(&ctx, &mut env).await?;
        let reset = bounded("explicit RESET", env.reset()).await;
        eprintln!("D1-RESET result: {reset:?}");
        let before = bounded("observer read before survivor", env.observer_value()).await;
        let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
        let own = bounded("survivor read", env.read()).await;
        let tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
            .await
            .map(|value| normalize(&env.tag, &value));
        let visible = bounded("observer read after survivor", env.observer_value()).await;
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(Obs { before, write, own, tag, visible })
    });
    ladder::observed("M-reset", "D1", &report);
    for note in &report.notes {
        println!("LADDER-NOTE M-reset D1: {note}");
    }
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn direct_m_reset_test() { direct_m_reset(); }
/// Added timing contrast: wait long enough for the held target reply to arrive
/// in the client socket, then send the same explicit RESET. This tests whether
/// the pool's failure is explained by *when* the reset runs, not by the pool
/// itself.
fn direct_m_reset_delayed() {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup(Mode::Direct).await?;
        cut(&ctx, &mut env).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let reset = bounded("delayed RESET", env.reset()).await;
        eprintln!("D1-RESET-DELAYED result: {reset:?}");
        let before = bounded("observer read before survivor", env.observer_value()).await;
        let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
        let own = bounded("survivor read", env.read()).await;
        let tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
            .await
            .map(|value| normalize(&env.tag, &value));
        let visible = bounded("observer read after survivor", env.observer_value()).await;
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(Obs { before, write, own, tag, visible })
    });
    ladder::observed("M-reset-delayed", "D1", &report);
    for note in &report.notes {
        println!("LADDER-NOTE M-reset-delayed D1: {note}");
    }
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn direct_m_reset_delayed_test() { direct_m_reset_delayed(); }
impl Env {
    /// Added checkout-only target: ask the pool for a connection and drop it.
    /// bb8 runs the manager's is_valid on checkout; with the proxy delay this
    /// is a real Pending point even when one connection is idle.
    async fn checkout(&mut self) -> Result<(), String> {
        match &mut self.handle {
            Handle::Pooled(pool) => {
                let conn = pool.get().await.map_err(e)?;
                drop(conn);
                Ok(())
            }
            Handle::Direct(_) => Err("checkout is pooled only".into()),
        }
    }
}

/// Added exploration after the original pooled result: cancel only the pool
/// checkout (the manager's validation reset), then let the next borrower write
/// on the same pool. This isolates which Pending point produced the error.
fn pooled_m_checkout() {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup(Mode::Pooled).await?;
        if let Some(result) = ctx.target(env.checkout()).await {
            result?;
        }
        let before = bounded("observer read before survivor", env.observer_value()).await;
        let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
        let own = bounded("survivor read", env.read()).await;
        let tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
            .await
            .map(|value| normalize(&env.tag, &value));
        let visible = bounded("observer read after survivor", env.observer_value()).await;
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(Obs { before, write, own, tag, visible })
    });
    ladder::observed("M-checkout", "P1", &report);
    for note in &report.notes {
        println!("LADDER-NOTE M-checkout P1: {note}");
    }
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn pooled_m_checkout_test() { pooled_m_checkout(); }

/// Added configuration contrast: the same checkout-only target with bb8's
/// checkout validation disabled. If the error disappears, the manager's
/// is_valid RESET is the carrier; this is a diagnostic contrast, not a fix
/// recommendation (a dirty connection can now reach the next borrower).
fn pooled_m_checkout_nocheck() {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup_with(Mode::Pooled, false).await?;
        if let Some(result) = ctx.target(env.checkout()).await {
            result?;
        }
        let before = bounded("observer read before survivor", env.observer_value()).await;
        let write = bounded("survivor write", env.write(SURVIVOR_V)).await;
        let own = bounded("survivor read", env.read()).await;
        let tag = bounded("survivor tag read", env.read_tag(&format!("{}-survivor", env.tag)))
            .await
            .map(|value| normalize(&env.tag, &value));
        let visible = bounded("observer read after survivor", env.observer_value()).await;
        let _ = bounded("cleanup", env.cleanup()).await;
        Ok(Obs { before, write, own, tag, visible })
    });
    ladder::observed("M-checkout-nocheck", "P1", &report);
    for note in &report.notes {
        println!("LADDER-NOTE M-checkout-nocheck P1: {note}");
    }
    if std::env::var("DW_DIAG").is_ok() {
        println!("{}", report.diagnostics());
    }
}

#[test]
fn pooled_m_checkout_nocheck_test() { pooled_m_checkout_nocheck(); }

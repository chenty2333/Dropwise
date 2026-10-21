//! Small BOLT/bb8 primitives shared by the round-2 comparison scenarios and
//! the generated target/successor cases. Experiment-local; the original/fixed
//! projects still include the same shared test files and differ only by the
//! `[patch.crates-io]` bb8 source.
//!
//! Nothing here decides which target or successor to use. It only opens the
//! same loopback proxy/server/pool shape, performs one write/read/tag exchange
//! on a named node, and reads the independent observer.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use bb8_bolt::Manager;
use bb8_bolt::bb8::Pool;
use bolt_client::bolt_proto::message::Success;
use bolt_client::bolt_proto::version::*;
use bolt_client::bolt_proto::{Message, Value};
use bolt_client::{Client, Metadata, Params, Stream};
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{Config, Flavor, Race, Settle};
use tokio::io::BufStream;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

pub const VARIANT: &str = env!("CARGO_PKG_NAME");
pub static NEXT: AtomicUsize = AtomicUsize::new(0);
pub static RUN: AtomicUsize = AtomicUsize::new(0);
pub const REPLY_DELAY: Duration = Duration::from_millis(50);
pub const BOUND: Duration = Duration::from_secs(5);
pub const TARGET_V: i64 = 1;
pub const SURVIVOR_V: i64 = 7;

pub type DirectClient = Client<Compat<BufStream<Stream>>>;
pub type BoltPool = Pool<Manager>;

pub fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

pub fn url() -> Result<String, String> {
    std::env::var("DW_NEO4J_URL").map_err(|err| format!("DW_NEO4J_URL is required: {err}"))
}

pub fn upstream(url: &str) -> Result<SocketAddr, String> {
    url.strip_prefix("bolt://")
        .unwrap_or(url)
        .trim_end_matches('/')
        .parse()
        .map_err(e)
}

pub fn meta() -> Metadata {
    let user = std::env::var("DW_NEO4J_USER").unwrap_or_else(|_| "neo4j".into());
    let password = std::env::var("DW_NEO4J_PASSWORD").unwrap_or_else(|_| "dropwise".into());
    Metadata::from_iter(vec![
        ("user_agent", "dropwise-p4e-r2/0.1"),
        ("scheme", "basic"),
        ("principal", user.as_str()),
        ("credentials", password.as_str()),
    ])
}

pub async fn connect_direct(addr: &str) -> Result<DirectClient, String> {
    let addr = addr.strip_prefix("bolt://").unwrap_or(addr).trim_end_matches('/');
    let stream = Stream::connect(addr, None::<&str>).await.map_err(e)?;
    let stream = BufStream::new(stream).compat();
    let mut client = Client::new(stream, &[V4_4, V4_3, 0, 0]).await.map_err(e)?;
    let hello = client.hello(meta()).await.map_err(e)?;
    Success::try_from(hello).map_err(|m| format!("HELLO failed: {m:?}"))?;
    Ok(client)
}

pub async fn connect_pooled(addr: &str) -> Result<BoltPool, String> {
    let manager = Manager::new(addr.to_string(), None::<String>, [V4_4, V4_3, 0, 0], meta())
        .await
        .map_err(e)?;
    Pool::builder()
        .max_size(1)
        .test_on_check_out(true)
        .build(manager)
        .await
        .map_err(e)
}

pub fn first_int(values: Vec<Value>) -> Result<i64, String> {
    match values.into_iter().next() {
        Some(Value::Integer(value)) => Ok(value),
        Some(other) => Err(format!("expected integer, got {other:?}")),
        None => Err("query returned no row".into()),
    }
}

pub fn first_string(values: Vec<Value>) -> Result<String, String> {
    match values.into_iter().next() {
        Some(Value::String(value)) => Ok(value),
        Some(other) => Err(format!("expected string, got {other:?}")),
        None => Err("query returned no row".into()),
    }
}

pub async fn run_pull(
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

pub async fn write_on(client: &mut DirectClient, id: &str, value: i64) -> Result<i64, String> {
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

pub async fn read_on(client: &mut DirectClient, id: &str) -> Result<i64, String> {
    let params = Params::from_iter(vec![("id", Value::from(id))]);
    let rows = run_pull(
        client,
        "MATCH (k:DwBolt {id: $id}) RETURN k.v AS v",
        Some(params),
    )
    .await?;
    first_int(rows)
}

pub async fn tag_on(client: &mut DirectClient, tag: &str) -> Result<String, String> {
    let params = Params::from_iter(vec![("tag", Value::from(tag))]);
    let rows = run_pull(client, "RETURN $tag AS t", Some(params)).await?;
    first_string(rows)
}

/// The node's stored tag property, used to keep object identity distinct from
/// the successor's value even when the cancelled target changed `v`.
pub async fn node_tag_on(client: &mut DirectClient, id: &str) -> Result<String, String> {
    let params = Params::from_iter(vec![("id", Value::from(id))]);
    let rows = run_pull(
        client,
        "MATCH (k:DwBolt {id: $id}) RETURN k.tag AS t",
        Some(params),
    )
    .await?;
    first_string(rows)
}

pub fn normalize(prefix: &str, value: &str) -> String {
    value.strip_prefix(prefix).unwrap_or(value).to_string()
}

/// One pre-created business node. The role is a stable logical name; the id
/// and tag are fresh per scenario run.
#[derive(Debug, Clone)]
pub struct Node {
    pub role: &'static str,
    pub id: String,
    pub tag: String,
}

pub struct Env {
    pub pool: BoltPool,
    pub observer: DirectClient,
    /// The primary node's id, kept for the historical round-2 scenarios.
    pub id: String,
    /// The run tag prefix, kept for the historical round-2 scenarios.
    pub tag: String,
    pub proxy: Proxy,
    pub level: &'static str,
    pub run: usize,
    pub nodes: Vec<Node>,
}

impl Env {
    pub fn node(&self, role: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.role == role)
    }

    pub async fn write(&mut self, value: i64) -> Result<i64, String> {
        let id = self.id.clone();
        let mut conn = self.pool.get().await.map_err(e)?;
        write_on(&mut *conn, &id, value).await
    }

    pub async fn read(&mut self) -> Result<i64, String> {
        let id = self.id.clone();
        let mut conn = self.pool.get().await.map_err(e)?;
        read_on(&mut *conn, &id).await
    }

    pub async fn read_tag(&mut self, tag: &str) -> Result<String, String> {
        let mut conn = self.pool.get().await.map_err(e)?;
        tag_on(&mut *conn, tag).await
    }

    pub async fn write_node(&mut self, node: &Node, value: i64) -> Result<i64, String> {
        let id = node.id.clone();
        let mut conn = self.pool.get().await.map_err(e)?;
        write_on(&mut *conn, &id, value).await
    }

    pub async fn read_node(&mut self, node: &Node) -> Result<i64, String> {
        let id = node.id.clone();
        let mut conn = self.pool.get().await.map_err(e)?;
        read_on(&mut *conn, &id).await
    }

    pub async fn read_node_tag(&mut self, node: &Node) -> Result<String, String> {
        let id = node.id.clone();
        let mut conn = self.pool.get().await.map_err(e)?;
        node_tag_on(&mut *conn, &id).await
    }

    pub async fn observer_value(&mut self) -> Result<i64, String> {
        let id = self.id.clone();
        read_on(&mut self.observer, &id).await
    }

    pub async fn observer_value_node(&mut self, node: &Node) -> Result<i64, String> {
        read_on(&mut self.observer, &node.id).await
    }

    pub async fn observer_tag_node(&mut self, node: &Node) -> Result<String, String> {
        node_tag_on(&mut self.observer, &node.id).await
    }

    pub async fn checkout(&mut self) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(e)?;
        drop(conn);
        Ok(())
    }

    pub async fn cleanup(&mut self) -> Result<(), String> {
        for node in &self.nodes {
            let id = node.id.clone();
            let params = Params::from_iter(vec![("id", Value::from(id))]);
            run_pull(
                &mut self.observer,
                "MATCH (k:DwBolt {id: $id}) DETACH DELETE k",
                Some(params),
            )
            .await?;
        }
        Ok(())
    }

    pub fn snapshot(&self, phase: &str) {
        let state = format!("{:?}", self.pool.state());
        let conns = self.proxy.connections();
        let clients: Vec<String> = conns
            .iter()
            .map(|c| format!("{}:{}/{}", c.client, c.to_upstream, c.to_client))
            .collect();
        println!(
            "R2-SNAPSHOT variant={VARIANT} level={} run={} phase={phase} state={state} \
             proxy_connections={} proxy_client_bytes={clients:?}",
            self.level,
            self.run,
            conns.len()
        );
    }
}

/// Open the observer, create `roles` nodes with fresh ids/tags, and open the
/// pooled handle through the held-reply proxy. Each call is isolated: a new
/// proxy, observer, node ids and pool, then one warm-up write.
pub async fn setup_nodes(level: &'static str, roles: &[&'static str]) -> Result<Env, String> {
    let direct = url()?;
    let proxy = Proxy::start_with(
        upstream(&direct)?,
        ProxyOptions { reply_delay: REPLY_DELAY },
    )
    .await
    .map_err(e)?;
    let mut observer = connect_direct(&direct).await?;
    let n = NEXT.fetch_add(1, SeqCst);
    let run = RUN.fetch_add(1, SeqCst);
    println!("R2-RUN variant={VARIANT} level={level} run={run} n={n}");
    let base = format!("dw-p4e-bolt-r2-{}-{n}", std::process::id());
    let tag_prefix = format!("p4e-r2-tag-{}-{n}", std::process::id());

    let mut nodes = Vec::new();
    for role in roles {
        let id = if *role == "primary" { base.clone() } else { format!("{base}-{role}") };
        let tag = format!("{tag_prefix}-{role}");
        let params = Params::from_iter(vec![
            ("id", Value::from(id.as_str())),
            ("tag", Value::from(tag.as_str())),
        ]);
        run_pull(
            &mut observer,
            "CREATE (k:DwBolt {id: $id}) SET k.v = 0, k.tag = $tag",
            Some(params),
        )
        .await?;
        nodes.push(Node { role, id, tag });
    }
    let id = nodes
        .iter()
        .find(|node| node.role == "primary")
        .map(|node| node.id.clone())
        .ok_or_else(|| "setup_nodes needs a primary role".to_string())?;

    let pool = connect_pooled(&proxy.addr().to_string()).await?;
    let mut env = Env {
        pool,
        observer,
        id,
        tag: tag_prefix,
        proxy,
        level,
        run,
        nodes,
    };
    // Warm-up: consume connection setup/HELLO and make the first target
    // Pending its own exchange, exactly as in round 1.
    env.write(0).await?;
    Ok(env)
}

/// The historical round-2 shape: one business node.
pub async fn setup(level: &'static str) -> Result<Env, String> {
    setup_nodes(level, &["primary"]).await
}

pub fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

pub async fn bounded<T>(
    label: &str,
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(BOUND, future).await {
        Ok(result) => result,
        Err(_) => Err(format!("{label} timed out after {BOUND:?}")),
    }
}

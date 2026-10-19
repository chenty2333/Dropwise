//! Manual probe (not part of the ladder): cancel one pooled write while its
//! reply is held by `dropwise::net::Proxy`, then run a known-value read through
//! the same one-connection pool and report what it returns.

use std::net::SocketAddr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use neo4rs::{query, ConfigBuilder, Graph};

fn url() -> String {
    std::env::var("DW_NEO4J_URL").unwrap_or_else(|_| "bolt://127.0.0.1:7687".into())
}

fn upstream(url: &str) -> SocketAddr {
    url.trim_start_matches("bolt://").parse().expect("host:port")
}

fn through(proxy: SocketAddr) -> String {
    format!("bolt://{proxy}")
}

async fn connect(uri: &str, max: usize) -> Graph {
    Graph::connect(
        ConfigBuilder::new()
            .uri(uri)
            .user(std::env::var("DW_NEO4J_USER").unwrap_or_else(|_| "neo4j".into()))
            .password(std::env::var("DW_NEO4J_PASSWORD").unwrap_or_else(|_| "dropwise".into()))
            .max_connections(max)
            .fetch_size(10)
            .build()
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn value(graph: &Graph) -> Result<Option<i64>, String> {
    let mut rows = graph
        .execute(query("MATCH (n:Probe {id: 1}) RETURN n.v AS v"))
        .await
        .map_err(|e| e.to_string())?;
    match rows.next().await.map_err(|e| e.to_string())? {
        Some(row) => row.get::<i64>("v").map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn probe() {
    let direct = connect(&url(), 2).await;
    let _ = direct.run(query("MATCH (n:Probe) DETACH DELETE n")).await;
    direct
        .run(query("CREATE (:Probe {id: 1, v: 0})"))
        .await
        .unwrap();

    let proxy = Proxy::start_with(
        upstream(&url()),
        ProxyOptions { reply_delay: Duration::from_millis(50) },
    )
    .await
    .unwrap();
    let pooled = connect(&through(proxy.addr()), 1).await;
    println!("after connect {:?}", proxy.connections());
    pooled
        .run(query("MATCH (n:Probe {id: 1}) SET n.v = 0"))
        .await
        .unwrap();
    println!("after warmup {:?}", proxy.connections());

    let ms: u64 = std::env::var("DW_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);
    let started = std::time::Instant::now();
    let cancelled = tokio::time::timeout(
        Duration::from_millis(ms),
        pooled.run(query("MATCH (n:Probe {id: 1}) SET n.v = 1")),
    )
    .await;
    println!("target cancelled={} elapsed={:?}", cancelled.is_err(), started.elapsed());
    println!("connections={:?}", proxy.connections());
    tokio::time::sleep(Duration::from_millis(200)).await;
    println!("residue={:?}", proxy.residue().unwrap());
    println!("v before survivor={:?}", value(&direct).await);
    println!("connections before survivor={:?}", proxy.connections());

    let read = tokio::time::timeout(
        Duration::from_secs(3),
        pooled.execute(query("RETURN 'fresh' AS tag")),
    )
    .await;
    match read {
        Ok(Ok(mut stream)) => match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Ok(Some(row))) => println!("survivor row={:?}", row.get::<String>("tag")),
            Ok(Ok(None)) => println!("survivor row=<none>"),
            Ok(Err(err)) => println!("survivor next error={err}"),
            Err(_) => println!("survivor next timed out"),
        },
        Ok(Err(err)) => println!("survivor execute error={err}"),
        Err(_) => println!("survivor execute timed out"),
    }

    println!("connections after survivor={:?}", proxy.connections());
    println!("observed v={:?}", value(&direct).await);
    pooled.run(query("MATCH (n:Probe) DETACH DELETE n")).await.ok();
}

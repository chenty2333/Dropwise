//! Upstream baseline: tokio-postgres 0.7.18 + bb8 0.9.1 + bb8-postgres 0.9.0
//! behind a loopback byte proxy.  The proxy holds the first bytes of a selected
//! server reply and can either release them or keep the TCP connection open
//! forever.  The driver cancels the client future while it is frozen.
//!
//! This is the baseline, not a Dropwise result.  It exists so the online-guards
//! experiments can be compared with a mature client/pool on the same fault
//! shape instead of only with the repository's own previous versions.
//!
//! Modes:
//!   typed-begin  hold the BEGIN reply, cancel `client.transaction()`, release
//!   blackhole    hold the BEGIN reply forever, drop the pooled connection,
//!                then try two successive `pool.get()` calls
//!   commit       hold the COMMIT reply, cancel `tx.commit()`, check the row
//!                through an independent connection
//!   validate     open a transaction, drop the connection, and let bb8's
//!                `is_valid` (an empty simple query) check it out again
//!
//! Environment: DWHOST (default 127.0.0.1), DWPORT (required), DWUSER
//! (dropwise), DWPASS (dropwise), DWDB (dropwise).

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_postgres::NoTls;

struct Ctl {
    freeze_after: AtomicIsize,
    close_on_freeze: AtomicBool,
    frozen: watch::Sender<bool>,
    release: watch::Sender<bool>,
    held: Mutex<Vec<u8>>,
    first: Mutex<Vec<u8>>,
    connections: AtomicUsize,
}

struct Proxy {
    port: u16,
    ctl: Arc<Ctl>,
}

impl Proxy {
    async fn start(upstream: u16) -> Proxy {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (frozen, _) = watch::channel(false);
        let (release, _) = watch::channel(false);
        let ctl = Arc::new(Ctl {
            freeze_after: AtomicIsize::new(-1),
            close_on_freeze: AtomicBool::new(false),
            frozen,
            release,
            held: Mutex::new(Vec::new()),
            first: Mutex::new(Vec::new()),
            connections: AtomicUsize::new(0),
        });
        let proxy_ctl = ctl.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                proxy_ctl.connections.fetch_add(1, Ordering::SeqCst);
                let ctl = proxy_ctl.clone();
                tokio::spawn(async move {
                    let server = match TcpStream::connect(("127.0.0.1", upstream)).await {
                        Ok(server) => server,
                        Err(_) => return,
                    };
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();
                    let upstream_task = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
                        let _ = server_write.shutdown().await;
                    });
                    let mut total = 0usize;
                    let mut buffer = [0u8; 8192];
                    loop {
                        let n = match server_read.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let limit = ctl.freeze_after.load(Ordering::Acquire);
                        if limit < 0 {
                            if client_write.write_all(&buffer[..n]).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let limit = limit as usize;
                        let take = (limit - total).min(n);
                        if take > 0 {
                            if client_write.write_all(&buffer[..take]).await.is_err() {
                                break;
                            }
                            total += take;
                        }
                        if total >= limit {
                            ctl.first
                                .lock()
                                .unwrap()
                                .extend_from_slice(&buffer[..n.min(64)]);
                            let _ = ctl.frozen.send(true);
                            if ctl.close_on_freeze.load(Ordering::Acquire) {
                                let _ = client_write.shutdown().await;
                                break;
                            }
                            *ctl.held.lock().unwrap() = buffer[take..n].to_vec();
                            let mut release = ctl.release.subscribe();
                            let _ = release.wait_for(|released| *released).await;
                            let held = std::mem::take(&mut *ctl.held.lock().unwrap());
                            ctl.freeze_after.store(-1, Ordering::Release);
                            if client_write.write_all(&held).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                    upstream_task.abort();
                });
            }
        });
        Proxy { port, ctl }
    }

    fn arm(&self, bytes: isize, close: bool) {
        self.ctl.first.lock().unwrap().clear();
        self.ctl.held.lock().unwrap().clear();
        let _ = self.ctl.frozen.send(false);
        let _ = self.ctl.release.send(false);
        self.ctl.close_on_freeze.store(close, Ordering::Release);
        self.ctl.freeze_after.store(bytes, Ordering::Release);
    }

    fn release(&self) {
        let _ = self.ctl.release.send(true);
    }

    fn frozen(&self) -> watch::Receiver<bool> {
        self.ctl.frozen.subscribe()
    }
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn cfg(port: u16) -> String {
    format!(
        "host={} port={port} user={} password={} dbname={}",
        env("DWHOST", "127.0.0.1"),
        env("DWUSER", "dropwise"),
        env("DWPASS", "dropwise"),
        env("DWDB", "dropwise"),
    )
}

fn pg_port() -> u16 {
    std::env::var("DWPORT")
        .expect("set DWPORT to the PostgreSQL port")
        .parse()
        .expect("DWPORT must be a port")
}

async fn wait_frozen(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|frozen| *frozen).await;
}

async fn backend_state(port: u16, pid: i32) {
    let (client, connection) = match tokio_postgres::connect(&cfg(port), NoTls).await {
        Ok(pair) => pair,
        Err(error) => {
            println!("observer: connect failed: {error}");
            return;
        }
    };
    let task = tokio::spawn(connection);
    match client
        .query_one(
            "SELECT state, xact_start IS NOT NULL AS in_xact, query \
             FROM pg_stat_activity WHERE pid = $1",
            &[&pid],
        )
        .await
    {
        Ok(row) => {
            let state: Option<String> = row.get(0);
            let in_xact: Option<bool> = row.get(1);
            let query: Option<String> = row.get(2);
            println!("observer: pid={pid} state={state:?} in_xact={in_xact:?} query={query:?}");
        }
        Err(error) => println!("observer: query failed: {error}"),
    }
    drop(client);
    task.abort();
}

async fn typed_begin(port: u16) {
    let proxy = Proxy::start(port).await;
    let (mut client, connection) = tokio_postgres::connect(&cfg(proxy.port), NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    let pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    client.batch_execute("SELECT 1").await.unwrap();
    println!("typed-begin: proxy=127.0.0.1:{} pid={pid}", proxy.port);

    proxy.arm(1, false);
    {
        let mut begin = Box::pin(client.transaction());
        let frozen = proxy.frozen();
        tokio::select! {
            result = &mut begin => println!("typed-begin: BEGIN completed unexpectedly: {:?}", result.map(|_| ())),
            _ = wait_frozen(frozen) => println!("typed-begin: reply frozen; cancelling the BEGIN future"),
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("typed-begin: is_closed while held = {}", client.is_closed());
    backend_state(port, pid).await;
    proxy.release();
    tokio::time::sleep(Duration::from_millis(300)).await;
    match tokio::time::timeout(Duration::from_millis(700), client.simple_query("SELECT 1")).await {
        Ok(Ok(rows)) => println!("typed-begin: successor after release OK ({} messages)", rows.len()),
        Ok(Err(error)) => println!("typed-begin: successor after release failed: {error}"),
        Err(_) => println!("typed-begin: successor after release hung"),
    }
    backend_state(port, pid).await;
    drop(client);
    task.abort();
}

async fn blackhole(port: u16) {
    let proxy = Proxy::start(port).await;
    let manager = PostgresConnectionManager::new_from_stringlike(cfg(proxy.port), NoTls).unwrap();
    let pool = Pool::builder()
        .max_size(1)
        .connection_timeout(Duration::from_millis(700))
        .build(manager)
        .await
        .unwrap();
    let mut conn = pool.get().await.unwrap();
    let pid: i32 = conn
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    conn.batch_execute("SELECT 1").await.unwrap();
    println!("blackhole: proxy=127.0.0.1:{} pid={pid} max_size=1 timeout=700ms", proxy.port);

    proxy.arm(1, false);
    {
        let mut begin = Box::pin(conn.transaction());
        let frozen = proxy.frozen();
        tokio::select! {
            result = &mut begin => println!("blackhole: BEGIN completed unexpectedly: {:?}", result.map(|_| ())),
            _ = wait_frozen(frozen) => println!("blackhole: reply frozen permanently; cancelling BEGIN"),
        }
    }
    drop(conn);
    println!("blackhole: checked-out connection returned to the pool");
    tokio::time::sleep(Duration::from_millis(300)).await;
    backend_state(port, pid).await;

    for attempt in 1..=2 {
        let start = Instant::now();
        let got = pool.get().await;
        println!(
            "blackhole: successor get #{attempt} after {:?}: {}",
            start.elapsed(),
            match &got {
                Ok(_) => "ok".to_string(),
                Err(error) => format!("{error:?}"),
            }
        );
        drop(got);
        let state = pool.state();
        println!(
            "blackhole: state connections={} idle={}",
            state.connections, state.idle_connections
        );
    }
    backend_state(port, pid).await;
    println!("blackhole: proxy accepted {} connection(s)", proxy.ctl.connections.load(Ordering::SeqCst));
}

async fn commit(port: u16) {
    let proxy = Proxy::start(port).await;
    let (mut client, connection) = tokio_postgres::connect(&cfg(proxy.port), NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    let pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    client
        .batch_execute(
            "DROP TABLE IF EXISTS dw_commit_probe; \
             CREATE TABLE dw_commit_probe (k text PRIMARY KEY)",
        )
        .await
        .unwrap();
    println!("commit: proxy=127.0.0.1:{} pid={pid}", proxy.port);

    {
        let tx = client.transaction().await.unwrap();
        tx.batch_execute("INSERT INTO dw_commit_probe (k) VALUES ('committed-during-cancel')")
            .await
            .unwrap();
        proxy.arm(1, false);
        let mut commit = Box::pin(tx.commit());
        let frozen = proxy.frozen();
        tokio::select! {
            result = &mut commit => println!("commit: COMMIT completed unexpectedly: {result:?}"),
            _ = wait_frozen(frozen) => println!("commit: reply frozen; cancelling the COMMIT future"),
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("commit: is_closed while held = {}", client.is_closed());
    backend_state(port, pid).await;

    let (observer, observer_connection) = tokio_postgres::connect(&cfg(port), NoTls)
        .await
        .unwrap();
    let observer_task = tokio::spawn(observer_connection);
    let count: i64 = observer
        .query_one(
            "SELECT count(*) FROM dw_commit_probe WHERE k = 'committed-during-cancel'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    println!("commit: independent observer sees committed rows = {count}");
    drop(observer);
    observer_task.abort();

    proxy.release();
    tokio::time::sleep(Duration::from_millis(300)).await;
    match tokio::time::timeout(Duration::from_millis(700), client.simple_query("SELECT 1")).await {
        Ok(Ok(rows)) => println!("commit: successor after release OK ({} messages)", rows.len()),
        Ok(Err(error)) => println!("commit: successor after release failed: {error}"),
        Err(_) => println!("commit: successor after release hung"),
    }
    backend_state(port, pid).await;
    drop(client);
    task.abort();
}

async fn validate(port: u16) {
    let proxy = Proxy::start(port).await;
    let manager = PostgresConnectionManager::new_from_stringlike(cfg(proxy.port), NoTls).unwrap();
    let pool = Pool::builder()
        .max_size(1)
        .connection_timeout(Duration::from_millis(700))
        .build(manager)
        .await
        .unwrap();
    let conn = pool.get().await.unwrap();
    let pid: i32 = conn
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    conn.batch_execute("BEGIN").await.unwrap();
    println!("validate: opened a transaction on pid={pid}, returning the connection");
    drop(conn);
    backend_state(port, pid).await;

    let start = Instant::now();
    match pool.get().await {
        Ok(conn) => {
            println!(
                "validate: get succeeded after {:?} on an open transaction",
                start.elapsed()
            );
            let one: i32 = conn.query_one("SELECT 1", &[]).await.unwrap().get(0);
            println!("validate: query inside the inherited transaction returned {one}");
            backend_state(port, pid).await;
            conn.batch_execute("ROLLBACK").await.unwrap();
        }
        Err(error) => println!("validate: get failed after {:?}: {error:?}", start.elapsed()),
    }
    backend_state(port, pid).await;
}

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "typed-begin".into());
    let port = pg_port();
    match mode.as_str() {
        "typed-begin" => typed_begin(port).await,
        "blackhole" => blackhole(port).await,
        "commit" => commit(port).await,
        "validate" => validate(port).await,
        other => eprintln!("unknown mode {other:?}"),
    }
}

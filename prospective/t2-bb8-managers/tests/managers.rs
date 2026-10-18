//! Published bb8 managers against real servers (prospective/RESCAN.md, section 4).
//! Prints one RESCAN line per manager and cancellation source; no outcome asserts.
//!
//! The pool reaches its server through a loopback proxy that can delay one
//! server-to-client chunk; survivors write a value and read it back, and an
//! independent connection straight to the server confirms the write.
//!
//! Environment: DW_REDIS_PORT, DW_PG_PORT, DW_MEMCACHED_PORT (loopback servers),
//! BB8_VARIANT (label only).

#[path = "../../support/allowed.rs"]
mod allowed;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use allowed::{Calibration, Expect, Mode, Tally, Verdict, calibrate, op};
use dropwise::{Config, Ctx, Flavor, Race, Settle, explore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn e(err: impl std::fmt::Debug) -> String {
    format!("{err:?}")
}

fn port(var: &str) -> u16 {
    std::env::var(var).unwrap_or_else(|_| panic!("{var} not set")).parse().unwrap()
}

/// Forwards every accepted connection to `backend`. While armed, the next
/// server-to-client chunk is held for `delay` (once).
struct Proxy {
    port: u16,
    armed: Arc<AtomicBool>,
}

impl Proxy {
    async fn start(backend: u16, delay: Duration) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let armed = Arc::new(AtomicBool::new(false));
        let armed_loop = armed.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let armed = armed_loop.clone();
                tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect(("127.0.0.1", backend)).await else { return };
                    let (mut cr, mut cw) = client.into_split();
                    let (mut sr, mut sw) = server.into_split();
                    let up = tokio::io::copy(&mut cr, &mut sw);
                    let down = async {
                        let mut buf = [0; 8192];
                        loop {
                            let n = sr.read(&mut buf).await?;
                            if n == 0 {
                                return Ok::<_, std::io::Error>(());
                            }
                            if armed.swap(false, SeqCst) {
                                tokio::time::sleep(delay).await;
                            }
                            cw.write_all(&buf[..n]).await?;
                        }
                    };
                    let _ = tokio::join!(up, down);
                });
            }
        });
        Proxy { port, armed }
    }
}

/// Survivor view: one checkout that writes its own key, reads two keys preset
/// by an independent connection, then reads its own key back; and whether the
/// independent connection sees the survivor's write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    session: Vec<Result<String, String>>,
    observer_saw_write: bool,
}

trait Service {
    type Pool;
    fn name(&self) -> &'static str;
    fn backend(&self) -> u16;
    async fn pool(&self, port: u16, timeout: Duration) -> Result<Self::Pool, String>;
    /// `Pool::get()`, dropping the connection on success.
    async fn get(&self, pool: &Self::Pool) -> Result<(), String>;
    /// On one checkout: write `key` = "survivor", read `a`, read `b`, read `key`.
    async fn session(&self, pool: &Self::Pool, key: &str, a: &str, b: &str) -> Vec<Result<String, String>>;
    /// Straight to the server: set `a` = "value-a" and `b` = "value-b".
    async fn preset(&self, a: &str, b: &str);
    /// Read `key` on a fresh connection straight to the server.
    async fn observe(&self, key: &str) -> bool;
}

/// Run session steps in order and record each result. Like an application
/// that handles one failed call and keeps using the connection, later steps
/// still run after an error.
macro_rules! steps {
    ($($step:expr),+ $(,)?) => {{
        let mut out: Vec<Result<String, String>> = Vec::new();
        $(out.push($step);)+
        out
    }};
}

struct Redis;
impl Service for Redis {
    type Pool = bb8::Pool<bb8_redis::RedisConnectionManager>;
    fn name(&self) -> &'static str {
        "bb8-redis 0.26.0"
    }
    fn backend(&self) -> u16 {
        port("DW_REDIS_PORT")
    }
    async fn pool(&self, port: u16, timeout: Duration) -> Result<Self::Pool, String> {
        let manager = bb8_redis::RedisConnectionManager::new(format!("redis://127.0.0.1:{port}/")).map_err(e)?;
        bb8::Pool::builder().max_size(1).connection_timeout(timeout).build(manager).await.map_err(e)
    }
    async fn get(&self, pool: &Self::Pool) -> Result<(), String> {
        pool.get().await.map(drop).map_err(e)
    }
    async fn session(&self, pool: &Self::Pool, key: &str, a: &str, b: &str) -> Vec<Result<String, String>> {
        let mut conn = match pool.get().await {
            Ok(c) => c,
            Err(err) => return vec![Err(e(err))],
        };
        let c = &mut *conn;
        steps![
            redis::cmd("SET").arg(key).arg("survivor").query_async::<String>(c).await.map_err(e),
            redis::cmd("GET").arg(a).query_async::<String>(c).await.map_err(e),
            redis::cmd("GET").arg(b).query_async::<String>(c).await.map_err(e),
            redis::cmd("GET").arg(key).query_async::<String>(c).await.map_err(e),
        ]
    }
    async fn preset(&self, a: &str, b: &str) {
        let client = redis::Client::open(format!("redis://127.0.0.1:{}/", self.backend())).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let () = redis::cmd("SET").arg(a).arg("value-a").query_async(&mut conn).await.unwrap();
        let () = redis::cmd("SET").arg(b).arg("value-b").query_async(&mut conn).await.unwrap();
    }
    async fn observe(&self, key: &str) -> bool {
        let client = redis::Client::open(format!("redis://127.0.0.1:{}/", self.backend())).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let got: Option<String> = redis::cmd("GET").arg(key).query_async(&mut conn).await.unwrap();
        got.as_deref() == Some("survivor")
    }
}

struct Postgres;
fn pg_config(port: u16) -> String {
    format!("host=127.0.0.1 port={port} user=dropwise password=dropwise dbname=dropwise")
}
impl Service for Postgres {
    type Pool = bb8::Pool<bb8_postgres::PostgresConnectionManager<tokio_postgres::NoTls>>;
    fn name(&self) -> &'static str {
        "bb8-postgres 0.9.0"
    }
    fn backend(&self) -> u16 {
        port("DW_PG_PORT")
    }
    async fn pool(&self, port: u16, timeout: Duration) -> Result<Self::Pool, String> {
        let manager =
            bb8_postgres::PostgresConnectionManager::new_from_stringlike(pg_config(port), tokio_postgres::NoTls)
                .map_err(e)?;
        bb8::Pool::builder().max_size(1).connection_timeout(timeout).build(manager).await.map_err(e)
    }
    async fn get(&self, pool: &Self::Pool) -> Result<(), String> {
        pool.get().await.map(drop).map_err(e)
    }
    async fn session(&self, pool: &Self::Pool, key: &str, a: &str, b: &str) -> Vec<Result<String, String>> {
        let conn = match pool.get().await {
            Ok(c) => c,
            Err(err) => return vec![Err(e(err))],
        };
        let read = |k: &str| {
            let k = k.to_string();
            let conn = &conn;
            async move {
                conn.query_one("SELECT v FROM dw_bb8 WHERE k = $1", &[&k]).await.map(|r| r.get::<_, String>(0)).map_err(e)
            }
        };
        steps![
            conn.execute("INSERT INTO dw_bb8 (k, v) VALUES ($1, 'survivor')", &[&key])
                .await
                .map(|n| n.to_string())
                .map_err(e),
            read(a).await,
            read(b).await,
            read(key).await,
        ]
    }
    async fn preset(&self, a: &str, b: &str) {
        let (client, connection) = tokio_postgres::connect(&pg_config(self.backend()), tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        client
            .execute("INSERT INTO dw_bb8 (k, v) VALUES ($1, 'value-a'), ($2, 'value-b')", &[&a, &b])
            .await
            .unwrap();
    }
    async fn observe(&self, key: &str) -> bool {
        let (client, connection) = tokio_postgres::connect(&pg_config(self.backend()), tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        let row = client.query_one("SELECT count(*) FROM dw_bb8 WHERE k = $1", &[&key]).await.unwrap();
        row.get::<_, i64>(0) == 1
    }
}

struct Memcached;
impl Service for Memcached {
    // bb8-memcached 0.7.0 is built on bb8 0.8 (re-exported as bb8_memcached::bb8).
    type Pool = bb8_memcached::bb8::Pool<bb8_memcached::MemcacheConnectionManager>;
    fn name(&self) -> &'static str {
        "bb8-memcached 0.7.0"
    }
    fn backend(&self) -> u16 {
        port("DW_MEMCACHED_PORT")
    }
    async fn pool(&self, port: u16, timeout: Duration) -> Result<Self::Pool, String> {
        let manager = bb8_memcached::MemcacheConnectionManager::new(format!("tcp://127.0.0.1:{port}")).map_err(e)?;
        bb8_memcached::bb8::Pool::builder().max_size(1).connection_timeout(timeout).build(manager).await.map_err(e)
    }
    async fn get(&self, pool: &Self::Pool) -> Result<(), String> {
        pool.get().await.map(drop).map_err(e)
    }
    async fn session(&self, pool: &Self::Pool, key: &str, a: &str, b: &str) -> Vec<Result<String, String>> {
        let mut conn = match pool.get().await {
            Ok(c) => c,
            Err(err) => return vec![Err(e(err))],
        };
        let text = |v: Vec<u8>| String::from_utf8_lossy(&v).into_owned();
        steps![
            conn.set(&key, b"survivor", 0).await.map(|()| "OK".to_string()).map_err(e),
            conn.get(&a).await.map(text).map_err(e),
            conn.get(&b).await.map(text).map_err(e),
            conn.get(&key).await.map(text).map_err(e),
        ]
    }
    async fn preset(&self, a: &str, b: &str) {
        let pool = self.pool(self.backend(), Duration::from_secs(2)).await.unwrap();
        let mut conn = pool.get().await.unwrap();
        conn.set(&a, b"value-a", 0).await.unwrap();
        conn.set(&b, b"value-b", 0).await.unwrap();
    }
    async fn observe(&self, key: &str) -> bool {
        let pool = self.pool(self.backend(), Duration::from_secs(2)).await.unwrap();
        let mut conn = pool.get().await.unwrap();
        conn.get(&key).await.map(|v| v == b"survivor").unwrap_or(false)
    }
}

fn key() -> String {
    format!("dw-{}-{}", std::process::id(), NEXT.fetch_add(1, SeqCst))
}

/// Cancellation source: a Dropwise cut of `get()`, or bb8's own
/// `connection_timeout` firing while the proxy holds the validation reply.
async fn scenario<S: Service>(svc: &S, ctx: Option<&Ctx>, mode: Mode) -> Result<Obs, String> {
    let proxy = Proxy::start(svc.backend(), Duration::from_millis(500)).await;
    let timeout = if ctx.is_some() { Duration::from_secs(2) } else { Duration::from_millis(200) };
    let pool = svc.pool(proxy.port, timeout).await?;
    svc.get(&pool).await?; // warm: one idle, validated connection
    match ctx {
        Some(ctx) => {
            if let Some(r) = op(ctx, mode, svc.get(&pool)).await {
                r?;
            }
        }
        None if mode == Mode::Run => {
            proxy.armed.store(true, SeqCst);
            if svc.get(&pool).await.is_ok() {
                return Err("validation reply was not delayed past connection_timeout".into());
            }
            // The late reply lands before the next borrower arrives.
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        None => tokio::time::sleep(Duration::from_millis(700)).await,
    }
    let (own, a, b) = (key(), key(), key());
    svc.preset(&a, &b).await;
    let session = svc.session(&pool, &own, &a, &b).await;
    let observer_saw_write = svc.observe(&own).await;
    Ok(Obs { session, observer_saw_write })
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

fn variant() -> String {
    std::env::var("BB8_VARIANT").unwrap_or_else(|_| "release".into())
}

fn dropwise_cut<S: Service>(svc: &S) {
    let calibration = calibrate(&config(), |ctx, mode| async move { scenario(svc, Some(&ctx), mode).await });
    let tally = Tally::default();
    let report = explore(&config(), |ctx| {
        let (calibration, tally) = (&calibration, tally.clone());
        async move {
            let seen = scenario(svc, Some(&ctx), Mode::Run).await?;
            let checked = calibration.check(Expect::Atomic, &seen);
            tally.push(*checked.as_ref().unwrap_or(&Verdict::Neither));
            checked.map(drop)
        }
    });
    println!("{report}");
    println!("{}", tally.summary(&format!("M {} get cut [{}]", svc.name(), variant()), &report));
}

fn internal_timeout<S: Service>(svc: &S) {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let run = |mode| rt.block_on(scenario(svc, None, mode)).unwrap();
    let calibration = Calibration::new(vec![run(Mode::Skip), run(Mode::Skip)], Vec::new());
    let actual = run(Mode::Run);
    let verdict = calibration.check(Expect::Atomic, &actual);
    println!("RESCAN M {} connection_timeout [{}]: verdict={verdict:?}", svc.name(), variant());
}

fn prepare_postgres() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let (client, connection) = tokio_postgres::connect(&pg_config(port("DW_PG_PORT")), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        client.batch_execute("CREATE TABLE IF NOT EXISTS dw_bb8 (k TEXT PRIMARY KEY, v TEXT NOT NULL)").await.unwrap();
    });
}

#[test]
fn redis() {
    dropwise_cut(&Redis);
    internal_timeout(&Redis);
}

#[test]
fn postgres() {
    prepare_postgres();
    dropwise_cut(&Postgres);
    internal_timeout(&Postgres);
}

#[test]
fn memcached() {
    dropwise_cut(&Memcached);
    internal_timeout(&Memcached);
}

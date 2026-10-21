//! Small same-protocol contrast probe: `tokio-postgres` 0.7.18 direct `Client`
//! against the same PostgreSQL 18 server and `dropwise::net::Proxy`.
//!
//! This is not a cancellation ladder: it waits until the independent observer
//! sees the target applied, then drops the still-pending execute future while
//! the proxy holds the reply for 50 ms, three times. The `Client` sends each
//! request to a background connection task that owns the socket; the probe asks
//! whether that task drains the dropped request's `CommandComplete` /
//! `ReadyForQuery` before the next execute, or whether the next execute receives
//! the cancelled statement's rows_affected (3 instead of its own 1).
//!
//! Prints PGPROBE lines; no assertions.

use std::str::FromStr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};

const REPLY_DELAY: Duration = Duration::from_millis(50);
const APPLY_POLL: Duration = Duration::from_millis(2);
const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

const TARGET_SQL: &str = "INSERT INTO dw_r4_pgprobe (k, v) \
     VALUES ('t1', 1), ('t2', 1), ('t3', 1) ON CONFLICT (k) DO UPDATE SET v = excluded.v";
const NEXT_SQL: &str = "UPDATE dw_r4_pgprobe SET v = 7 WHERE k = 't1'";
const READ_T1_SQL: &str = "SELECT v FROM dw_r4_pgprobe WHERE k = 't1'";
const READ_ALL_SQL: &str = "SELECT k, v FROM dw_r4_pgprobe ORDER BY k";
const SEED_SQL: &str =
    "INSERT INTO dw_r4_pgprobe (k, v) VALUES ('b3', 3), ('t1', 0), ('t2', 0), ('t3', 0)";

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

async fn connect(config: Config) -> Result<Client, String> {
    let (client, connection) = config.connect(NoTls).await.map_err(e)?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("PGPROBE-CONNECTION-ERR {err}");
        }
    });
    Ok(client)
}

/// Wait until the target is visible on the independent direct connection. The
/// tested execute future is still pending because the proxy holds its reply for
/// `REPLY_DELAY`; the caller drops that future once this returns.
async fn wait_until_applied(observer: &Client) -> Result<(), String> {
    let start = std::time::Instant::now();
    while start.elapsed() < APPLY_TIMEOUT {
        let current = rows(observer).await?;
        if current.iter().any(|(k, v)| k == "t1" && *v == 1) {
            return Ok(());
        }
        tokio::time::sleep(APPLY_POLL).await;
    }
    Err("target did not become visible before the cut".into())
}

async fn rows(client: &Client) -> Result<Vec<(String, i32)>, String> {
    let rows = client.query(READ_ALL_SQL, &[]).await.map_err(e)?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get::<_, String>("k").map_err(e)?,
                row.try_get::<_, i32>("v").map_err(e)?,
            ))
        })
        .collect()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("PGPROBE-ERR {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let url = std::env::var("DW_PG_URL").map_err(|_| "DW_PG_URL is required".to_string())?;
    let server: std::net::SocketAddr = std::env::var("DW_PG_SERVER")
        .map_err(|_| "DW_PG_SERVER is required".to_string())?
        .parse()
        .map_err(|err| format!("bad DW_PG_SERVER: {err}"))?;

    let mut observer_config = Config::from_str(&url).map_err(|err| format!("bad DW_PG_URL: {err}"))?;
    observer_config.ssl_mode(SslMode::Disable);
    let observer = connect(observer_config).await?;
    observer
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS dw_r4_pgprobe (k text PRIMARY KEY, v int NOT NULL)",
        )
        .await
        .map_err(|err| format!("observer create table: {err}"))?;

    let proxy = Proxy::start_with(server, ProxyOptions { reply_delay: REPLY_DELAY })
        .await
        .map_err(e)?;
    // Config::host appends to hosts parsed from the URL, so the URL's direct
    // host would be tried first. Build a fresh Config for the proxy endpoint.
    let parsed = Config::from_str(&url).map_err(|err| format!("bad DW_PG_URL: {err}"))?;
    let mut tested_config = Config::new();
    tested_config
        .host("127.0.0.1")
        .port(proxy.addr().port())
        .ssl_mode(SslMode::Disable);
    if let Some(user) = parsed.get_user() {
        tested_config.user(user);
    }
    if let Some(password) = parsed.get_password() {
        tested_config.password(password);
    }
    if let Some(dbname) = parsed.get_dbname() {
        tested_config.dbname(dbname);
    }
    let tested = connect(tested_config).await?;
    tested.query_one("SELECT 1", &[]).await.map_err(e)?;

    for trial in 1..=3 {
        observer.execute("DELETE FROM dw_r4_pgprobe", &[]).await.map_err(e)?;
        observer.execute(SEED_SQL, &[]).await.map_err(e)?;

        let mut cancelled = false;
        tokio::select! {
            result = tested.execute(TARGET_SQL, &[]) => {
                let rows = result.map_err(e)?;
                println!("PGPROBE trial={trial} target_completed rows_affected={rows}");
            }
            applied = wait_until_applied(&observer) => {
                applied?;
                cancelled = true;
                println!(
                    "PGPROBE trial={trial} target_future_dropped_after_server_apply \
                     proxy_reply_delay={REPLY_DELAY:?}"
                );
            }
        }

        let before = rows(&observer).await?;
        let next_rows = tested.execute(NEXT_SQL, &[]).await.map_err(e)?;
        let own = tested
            .query_one(READ_T1_SQL, &[])
            .await
            .map_err(e)?
            .try_get::<_, i32>("v")
            .map_err(e)?;
        let visible = rows(&observer).await?;
        println!(
            "PGPROBE-RESULT trial={trial} cancelled={cancelled} before={before:?} \
             next_rows_affected={next_rows} own={own} visible={visible:?} \
             client_closed={} proxy_connections={}",
            tested.is_closed(),
            proxy.connections().len(),
        );
    }
    Ok(())
}

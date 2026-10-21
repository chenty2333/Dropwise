//! Shared environment for the orientdb-client 0.6.0 cancellation experiments.
//!
//! One OrientDB 3.0.23 container per runner; one unique database per run; one
//! unique record per scenario run. The tested `aio::SessionPool` (mobc) is
//! created with max_size 1 and its inner `OrientDB` client uses pool_size 1,
//! so both the session pool and the server-connection pool are bounded to one
//! connection. The independent observer is a second `OrientDB` client that
//! connects straight to the server, not through the reply-delay proxy.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use futures::{Stream, StreamExt};
use orientdb_client::aio::{OSession, OrientDB, SessionPool};
use orientdb_client::asynchronous::session::SessionPooled;
use orientdb_client::types::OResult;
use orientdb_client::ConnectionOptions;
use orientdb_client::OrientResult;

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub const REPLY_DELAY: Duration = Duration::from_millis(50);
pub const BOUND: Duration = Duration::from_secs(2);
pub const TARGET_V: i32 = 1;
pub const SURVIVOR_V: i32 = 7;

/// Concrete reply of one complete write round trip (script reply row).
pub type Reply = (i32, String);

pub fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

#[derive(Debug, Clone)]
pub struct ServerCfg {
    pub host: String,
    pub port: u16,
    pub db: String,
    pub user: String,
    pub pass: String,
}

impl ServerCfg {
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var(name).map_err(|err| format!("{name} is required: {err}"))
        };
        Ok(Self {
            host: var("DW_ORI_HOST")?,
            port: var("DW_ORI_PORT")?
                .parse()
                .map_err(|err| format!("DW_ORI_PORT is not a port: {err}"))?,
            db: var("DW_ORI_DB")?,
            user: var("DW_ORI_USER")?,
            pass: var("DW_ORI_PASS")?,
        })
    }

    fn upstream(&self) -> Result<SocketAddr, String> {
        format!("{}:{}", self.host, self.port).parse().map_err(e)
    }
}

fn options(host: &str, port: u16, pool_size: u32) -> ConnectionOptions {
    ConnectionOptions::builder()
        .host(host)
        .port(port)
        .pool_size(pool_size)
        .build()
}

pub async fn direct_client(cfg: &ServerCfg) -> Result<OrientDB, String> {
    OrientDB::connect(options(&cfg.host, cfg.port, 4))
        .await
        .map_err(e)
}

/// The tested client is built with server-pool size 1. The pool has no eager
/// connect: the first network operation of a scenario is the target checkout.
pub async fn pooled_client(addr: SocketAddr, pool_size: u32) -> Result<SessionPool, String> {
    let client = OrientDB::connect(options(&addr.ip().to_string(), addr.port(), pool_size))
        .await
        .map_err(e)?;
    let cfg = ServerCfg::from_env()?;
    client
        .sessions(&cfg.db, &cfg.user, &cfg.pass, None, Some(1))
        .await
        .map_err(e)
}

pub struct Env {
    pub cfg: ServerCfg,
    pub pool: SessionPool,
    pub observer: OSession,
    pub proxy: Proxy,
    /// `(label, record key)` created by the independent observer during setup.
    pub records: Vec<(&'static str, String)>,
}

impl Env {
    pub fn record(&self, label: &str) -> Result<&str, String> {
        self.records
            .iter()
            .find(|(name, _)| *name == label)
            .map(|(_, id)| id.as_str())
            .ok_or_else(|| format!("setup did not create record {label}"))
    }

    /// Survivor/target request through the tested pool: checkout plus one
    /// complete write query. The session guard is dropped inside this future,
    /// so a cancelled call returns it to the pool.
    /// Target/survivor request through the tested pool: checkout plus one
    /// complete side-effecting SQL script (`UPDATE ...; SELECT ...`). One
    /// request/response carries both the written value and the marker, so a
    /// stale reply can be told apart from the right one. The session guard is
    /// dropped inside this future, so a cancelled call returns it to the pool.
    pub async fn pooled_update(
        &self,
        record: &str,
        value: i32,
        marker: &str,
    ) -> Result<Reply, String> {
        let session = self.pool.get().await.map_err(e)?;
        update_session(&session, record, value, marker).await
    }

    /// Different-operation successor through the tested pool: checkout plus a
    /// read query. No side effect.
    pub async fn pooled_read(&self, record: &str) -> Result<Reply, String> {
        let session = self.pool.get().await.map_err(e)?;
        read_session(&session, record).await
    }

    pub async fn observer_read(&self, record: &str) -> Result<Reply, String> {
        let sql = format!("SELECT v, marker FROM DwOri WHERE k = '{record}'");
        first_reply(self.observer.query(sql).run().await.map_err(e)?).await
    }

    pub async fn pooled_stats(&self) -> (u64, u64, u64, u64) {
        (
            self.pool.size().await,
            self.pool.idle().await,
            self.pool.used().await,
            self.pool.max().await,
        )
    }

    pub async fn pooled_session_id(&self) -> Result<i32, String> {
        let session = self.pool.get().await.map_err(e)?;
        Ok(session.session_id)
    }

    pub async fn cleanup(&self) -> Result<(), String> {
        for (_, record) in &self.records {
            let sql = format!("DELETE FROM DwOri WHERE k = '{record}'");
            run_command(&self.observer, &sql).await?;
        }
        Ok(())
    }
}

/// Run one command and consume its stream completely.
pub async fn run_command(session: &OSession, sql: &str) -> Result<(), String> {
    let mut rows = Box::pin(session.command(sql).run().await.map_err(e)?);
    while let Some(row) = rows.next().await {
        row.map_err(e)?;
    }
    Ok(())
}

/// Read the first concrete reply row and drain the rest. Draining avoids the
/// client's detached `PagedResultSet` close request, which would open another
/// physical connection and make the reuse count noisy.
async fn first_reply(stream: impl Stream<Item = OrientResult<OResult>>) -> Result<Reply, String> {
    let mut rows = Box::pin(stream);
    let mut first: Option<Result<Reply, String>> = None;
    while let Some(row) = rows.next().await {
        let parsed = match row {
            Ok(row) => Ok((
                row.get_checked::<i32>("v").map_err(e)?,
                row.get_checked::<String>("marker").map_err(e)?,
            )),
            Err(err) => Err(e(err)),
        };
        if first.is_none() {
            first = Some(parsed);
        } else if parsed.is_err() {
            // Keep the first reply; a later page error is still a failure.
            return parsed;
        }
    }
    first.unwrap_or_else(|| Err("no reply row".into()))
}

/// Setup shared by every ladder level and calibration run.
///
/// No request is made through the tested pool here: schema and seed records go
/// through the independent direct observer, so the first Pending boundary of
/// the target is the session checkout itself.
pub async fn setup(records: &[&'static str]) -> Result<Env, String> {
    let cfg = ServerCfg::from_env()?;
    let direct = direct_client(&cfg).await?;
    let observer = direct
        .session(&cfg.db, &cfg.user, &cfg.pass)
        .await
        .map_err(e)?;

    run_command(&observer, "CREATE CLASS DwOri IF NOT EXISTS").await?;
    run_command(&observer, "CREATE PROPERTY DwOri.k IF NOT EXISTS STRING").await?;
    run_command(&observer, "CREATE PROPERTY DwOri.v IF NOT EXISTS INTEGER").await?;
    run_command(&observer, "CREATE PROPERTY DwOri.marker IF NOT EXISTS STRING").await?;

    let n = NEXT.fetch_add(1, SeqCst);
    let mut created = Vec::new();
    for label in records {
        let id = format!("r3-{}-{n}-{label}", std::process::id());
        run_command(
            &observer,
            &format!("INSERT INTO DwOri SET k = '{id}', v = 0, marker = 'seed'"),
        )
        .await?;
        created.push((*label, id));
    }

    let proxy = Proxy::start_with(
        cfg.upstream()?,
        ProxyOptions { reply_delay: REPLY_DELAY },
    )
    .await
    .map_err(e)?;
    let pool = pooled_client(proxy.addr(), 1).await?;
    Ok(Env { cfg, pool, observer, proxy, records: created })
}

/// One complete side-effecting round trip on an already-checked-out session.
pub async fn update_session(
    session: &OSession,
    record: &str,
    value: i32,
    marker: &str,
) -> Result<Reply, String> {
    let sql = format!(
        "UPDATE DwOri SET v = {value}, marker = '{marker}' WHERE k = '{record}'; \
         SELECT v, marker FROM DwOri WHERE k = '{record}'"
    );
    first_reply(session.script_sql(sql).run().await.map_err(e)?).await
}

/// One complete read round trip on an already-checked-out session.
pub async fn read_session(session: &OSession, record: &str) -> Result<Reply, String> {
    let sql = format!("SELECT v, marker FROM DwOri WHERE k = '{record}'");
    first_reply(session.query(sql).run().await.map_err(e)?).await
}

/// A session checked out during setup and held across the cut: the target and
/// the survivor use the same logical session guard, not two pool checkouts.
pub struct SessionEnv {
    pub env: Env,
    pub session: SessionPooled,
}

pub async fn session_setup(records: &[&'static str]) -> Result<SessionEnv, String> {
    let env = setup(records).await?;
    let session = env.pool.get().await.map_err(e)?;
    Ok(SessionEnv { env, session })
}

impl SessionEnv {
    pub fn record(&self, label: &str) -> Result<&str, String> {
        self.env.record(label)
    }

    pub async fn update(&self, record: &str, value: i32, marker: &str) -> Result<Reply, String> {
        update_session(&self.session, record, value, marker).await
    }

    pub async fn read(&self, record: &str) -> Result<Reply, String> {
        read_session(&self.session, record).await
    }

    pub async fn observer_read(&self, record: &str) -> Result<Reply, String> {
        self.env.observer_read(record).await
    }

    pub async fn stats(&self) -> (u64, u64, u64, u64) {
        self.env.pooled_stats().await
    }

    pub async fn cleanup(&self) -> Result<(), String> {
        self.env.cleanup().await
    }
}

pub async fn bounded<T>(
    label: &str,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(BOUND, future).await {
        Ok(result) => result,
        Err(_) => Err(format!("{label} timed out after {BOUND:?}")),
    }
}

pub fn upstream_of(cfg: &ServerCfg) -> Result<SocketAddr, String> {
    cfg.upstream()
}

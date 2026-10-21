//! Shared skytable 0.8.12 scenario plumbing for the round-3 exploration.
//!
//! Two reuse modes of the same client/version family:
//! * pooled: `skytable::pool::get_async(1, cfg)` (official bb8 0.9.1 pool);
//! * direct: one `ConnectionAsync` held outside the target future.
//!
//! Every scenario creates a fresh space/model on the server (through a direct
//! observer connection that does not pass through the proxy), seeds one row,
//! and then drives the tested connection through a `dropwise::net::Proxy` with
//! a 50 ms server->client reply delay. Skytable has no transactions and no
//! separate authentication round trip (`Config` authenticates during the
//! connection handshake), so the target is an ordinary BlueQL `update` on one
//! row. The survivor updates and reads the same row; the observer reads it over
//! its own direct connection.

#![allow(dead_code)]

use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
pub use dropwise::support::{bounded, unique};
use dropwise::{Config as DwConfig, Ctx, Flavor, Race, Settle};
use skytable::pool::ConnectionMgrTcp;
use skytable::response::Response;
use skytable::{Config, ConnectionAsync, Query};

pub const REPLY_DELAY: Duration = Duration::from_millis(50);
pub const BOUND: Duration = Duration::from_secs(2);
pub const SEED_V: u64 = 0;
pub const TARGET_V: u64 = 1;
pub const SURVIVOR_V: u64 = 7;
pub const OBSERVER_KEY: &str = "k";

pub fn config() -> DwConfig {
    DwConfig {
        max_runs: 64,
        max_cancellations: 1,
        races: vec![Race::Immediate, Race::AfterWake],
        repeat_at_site: false,
        gap_choices: Vec::new(),
        flavor: Flavor::MultiThread { workers: 1 },
        settle: Settle {
            tokio_time: Duration::ZERO,
            real_time: Duration::ZERO,
            watchdog: Duration::from_secs(10),
        },
        scenario_timeout: Duration::from_secs(20),
    }
}

pub fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Pooled,
    Direct,
}

impl Variant {
    pub fn from_arg(value: &str) -> Result<Self, String> {
        match value {
            "pooled" => Ok(Self::Pooled),
            "direct" => Ok(Self::Direct),
            other => Err(format!("unknown variant {other:?}; expected pooled|direct")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pooled => "pooled",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
}

impl Server {
    pub fn from_env() -> Result<Self, String> {
        let addr = env("DW_SKY_SERVER");
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| format!("DW_SKY_SERVER must be host:port, got {addr}"))?;
        Ok(Self {
            host: host.to_string(),
            port: port
                .parse()
                .map_err(|err| format!("bad DW_SKY_SERVER port: {err}"))?,
            user: env("DW_SKY_USER"),
            pass: env("DW_SKY_PASS"),
        })
    }

    pub fn config(&self) -> Config {
        Config::new(&self.host, self.port, &self.user, &self.pass)
    }
}

pub type SkyPool = bb8::Pool<ConnectionMgrTcp>;

pub struct Env {
    pub variant: Variant,
    pub server: Server,
    pub space: String,
    pub observer: ConnectionAsync,
    pub proxy: Proxy,
    pub pool: Option<SkyPool>,
    pub direct: Option<ConnectionAsync>,
    pub run: usize,
    pub keys: Vec<String>,
}

fn check_empty(result: Result<Response, skytable::error::Error>) -> Result<(), String> {
    match result {
        Ok(Response::Empty) => Ok(()),
        Ok(Response::Error(code)) => Err(format!("server error {code}")),
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    }
}

fn check_row(result: Result<Response, skytable::error::Error>) -> Result<u64, String> {
    match result {
        Ok(Response::Row(row)) => row.into_first_as::<u64>().map_err(|err| format!("{err:?}")),
        Ok(Response::Error(code)) => Err(format!("server error {code}")),
        Ok(Response::Empty) => Err("row not found (empty response)".into()),
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    }
}

pub fn update_query(space: &str, value: u64, key: &str) -> Query {
    let mut q = Query::new(&format!("update {space}.kv set v = ? where id = ?"));
    q.push_param(value);
    q.push_param(key);
    q
}

pub fn select_query(space: &str, key: &str) -> Query {
    let mut q = Query::new(&format!("select v from {space}.kv where id = ?"));
    q.push_param(key);
    q
}

pub fn create_space_query(space: &str) -> Query {
    Query::new(&format!("create space {space}"))
}

pub fn create_model_query(space: &str) -> Query {
    Query::new(&format!("create model {space}.kv(id: string, v: uint64)"))
}

pub fn drop_space_query(space: &str) -> Query {
    Query::new(&format!("drop space {space}"))
}

pub fn drop_model_query(space: &str) -> Query {
    Query::new(&format!("drop model {space}.kv"))
}

pub fn insert_query(space: &str, key: &str, value: u64) -> Query {
    let mut q = Query::new(&format!("insert into {space}.kv(?, ?)"));
    q.push_param(key);
    q.push_param(value);
    q
}

pub fn sysctl_query() -> Query {
    Query::new("sysctl report status")
}

impl Env {
    pub async fn setup(variant: Variant, run: usize) -> Result<Self, String> {
        Self::setup_keys(variant, run, &[OBSERVER_KEY]).await
    }

    pub async fn setup_keys(variant: Variant, run: usize, keys: &[&str]) -> Result<Self, String> {
        let server = Server::from_env()?;
        let mut observer = server.config().connect_async().await.map_err(e)?;
        let space = unique("dw_r3_sky").replace('-', "_");
        check_empty(observer.query(&create_space_query(&space)).await)
            .map_err(|err| format!("create space {space}: {err}"))?;
        check_empty(observer.query(&create_model_query(&space)).await)
            .map_err(|err| format!("create model {space}.kv: {err}"))?;
        for key in keys {
            check_empty(observer.query(&insert_query(&space, key, SEED_V)).await)
                .map_err(|err| format!("seed row {key}: {err}"))?;
        }

        let upstream = format!("{}:{}", server.host, server.port)
            .parse()
            .map_err(|err| format!("bad upstream address: {err}"))?;
        let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let tested = Config::new(
            "127.0.0.1",
            proxy.addr().port(),
            &server.user,
            &server.pass,
        );
        let (pool, direct) = match variant {
            Variant::Pooled => {
                let pool = skytable::pool::get_async(1, tested)
                    .await
                    .map_err(|err| format!("pool::get_async: {err:?}"))?;
                {
                    let mut conn = pool.get().await.map_err(e)?;
                    check_empty(conn.query(&sysctl_query()).await)
                        .map_err(|err| format!("pool warm-up: {err}"))?;
                }
                (Some(pool), None)
            }
            Variant::Direct => {
                let conn = tested
                    .connect_async()
                    .await
                    .map_err(|err| format!("connect_async: {err:?}"))?;
                (None, Some(conn))
            }
        };
        let env = Self {
            variant,
            server,
            space,
            observer,
            proxy,
            pool,
            direct,
            run,
            keys: keys.iter().map(|key| (*key).to_string()).collect(),
        };
        env.snapshot("setup");
        Ok(env)
    }

    pub fn snapshot(&self, label: &str) {
        let pool = self
            .pool
            .as_ref()
            .map(|pool| format!("{:?}", pool.state()))
            .unwrap_or_else(|| "n/a".into());
        println!(
            "SKY-SNAPSHOT variant={} run={} stage={label} proxy_connections={} pool={pool}",
            self.variant.name(),
            self.run,
            self.proxy.connections().len(),
        );
    }

    /// One BlueQL read on the observer's own direct connection.
    pub async fn observer_read(&mut self) -> Result<u64, String> {
        self.observer_read_key(OBSERVER_KEY).await
    }

    pub async fn observer_read_key(&mut self, key: &str) -> Result<u64, String> {
        check_row(self.observer.query(&select_query(&self.space, key)).await)
    }

    pub async fn observer_write(&mut self, value: u64) -> Result<(), String> {
        check_empty(self.observer.query(&update_query(&self.space, value, OBSERVER_KEY)).await)
    }

    /// One update on the tested handle/pool. For the pooled variant the
    /// checkout is inside this future, so it is inside the target on purpose.
    pub async fn update_key(&mut self, key: &str, value: u64) -> Result<(), String> {
        match self.variant {
            Variant::Pooled => {
                let mut conn = self.pool.as_ref().expect("pooled env").get().await.map_err(e)?;
                check_empty(conn.query(&update_query(&self.space, value, key)).await)
            }
            Variant::Direct => check_empty(
                self.direct
                    .as_mut()
                    .expect("direct env")
                    .query(&update_query(&self.space, value, key))
                    .await,
            ),
        }
    }

    pub async fn write(&mut self, value: u64) -> Result<(), String> {
        self.update_key(OBSERVER_KEY, value).await
    }

    /// One select on the tested handle/pool.
    pub async fn select_key(&mut self, key: &str) -> Result<u64, String> {
        match self.variant {
            Variant::Pooled => {
                let mut conn = self.pool.as_ref().expect("pooled env").get().await.map_err(e)?;
                check_row(conn.query(&select_query(&self.space, key)).await)
            }
            Variant::Direct => check_row(
                self.direct
                    .as_mut()
                    .expect("direct env")
                    .query(&select_query(&self.space, key))
                    .await,
            ),
        }
    }

    pub async fn read(&mut self) -> Result<u64, String> {
        self.select_key(OBSERVER_KEY).await
    }

    /// Same-argument retry for B2: the target's update value on the same row.
    pub async fn retry_same(&mut self) -> Result<(), String> {
        self.write(TARGET_V).await
    }

    /// Different operation for B3: a read of the same row.
    pub async fn different_op(&mut self) -> Result<u64, String> {
        self.read().await
    }

    pub async fn cleanup(&mut self) -> Result<(), String> {
        // Rows must be gone before the model, and the model before the space.
        for key in self.keys.clone() {
            let mut q = Query::new(&format!("delete from {}.kv where id = ?", self.space));
            q.push_param(key.as_str());
            match self.observer.query(&q).await {
                Ok(Response::Empty) | Ok(Response::Error(111)) => {}
                Ok(other) => return Err(format!("delete row: unexpected response {other:?}")),
                Err(err) => return Err(format!("delete row: {err:?}")),
            }
        }
        check_empty(self.observer.query(&drop_model_query(&self.space)).await)?;
        check_empty(self.observer.query(&drop_space_query(&self.space)).await)
    }
}

/// Mark the tested write as the cancellation target.
pub async fn cut(ctx: &Ctx, env: &mut Env) -> Result<bool, String> {
    let cancelled = match ctx.target(env.write(TARGET_V)).await {
        Some(result) => {
            result?;
            false
        }
        None => true,
    };
    println!(
        "SKY-CUT variant={} run={} cancelled={cancelled}",
        env.variant.name(),
        env.run
    );
    env.snapshot("after_target");
    Ok(cancelled)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Obs {
    /// Independent observer read after the cut and before the survivor.
    pub before: Result<u64, String>,
    /// Survivor's update of the same row to 7.
    pub write: Result<(), String>,
    /// Same-handle read-back of the same row.
    pub own: Result<u64, String>,
    /// Independent observer read after the survivor.
    pub visible: Result<u64, String>,
}

pub async fn observed_after_cut(env: &mut Env) -> Result<Obs, String> {
    let before = bounded(BOUND, "observer read before survivor", env.observer_read()).await;
    let write = bounded(BOUND, "survivor write", env.write(SURVIVOR_V)).await;
    env.snapshot("after_survivor_write");
    let own = bounded(BOUND, "survivor read", env.read()).await;
    let visible = bounded(BOUND, "observer read after survivor", env.observer_read()).await;
    let obs = Obs { before, write, own, visible };
    println!(
        "SKY-OBS variant={} run={} before={:?} write={:?} own={:?} visible={:?}",
        env.variant.name(),
        env.run,
        obs.before,
        obs.write,
        obs.own,
        obs.visible
    );
    let _ = bounded(BOUND, "cleanup", env.cleanup()).await;
    Ok(obs)
}

/// Manual calibration side: skip or complete the target, then run the same
/// survivor path. This is printed evidence independent of `explore_observed`'s
/// internal calibration.
pub async fn calibration(variant: Variant, run: usize, complete: bool) -> Result<Obs, String> {
    let mut env = Env::setup(variant, run).await?;
    if complete {
        env.write(TARGET_V).await?;
    }
    let obs = observed_after_cut(&mut env).await?;
    Ok(obs)
}

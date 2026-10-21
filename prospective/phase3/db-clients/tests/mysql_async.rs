//! mysql_async 0.37.1 cancellation-ablation ladder (phase 3, database targets).
//!
//! Two operations, five levels each. Every level uses the same scenario
//! skeleton and the same `Config`; only what runs after a cancelled target
//! changes:
//!
//! - `MY1` (pool, session state): `Pool::start_transaction`, the first round
//!   trip that establishes server-side transaction state.
//! - `MY2` (directly held `Conn`, ordinary request): a known-value `SELECT`.
//!
//! Pending comes from `dropwise::net::Proxy` holding every server reply for
//! 50 ms of real time. `B1` stops after the cancellation, `B2` retries the same
//! operation with the same arguments, `B3` runs a different operation of the
//! same kind and checks only `Ok`, `M` is `explore_observed(Expect::Atomic)`
//! with a survivor write confirmed by an independent connection, and
//! `M-residue` adds the bytes still unread in the client socket to the same
//! observation. Environment: DW_MYSQL_URL (loopback MariaDB 11.4).
//!
//! Prints one LADDER line per experiment and one LADDER-VIOLATION line per
//! violating plan. No outcome asserts.

#[path = "../../../support/ladder.rs"]
mod ladder;
#[path = "support/mysql_primitives.rs"]
mod primitives;

use std::net::SocketAddr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Settle};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Pool, TxOpts};
use primitives::{BOUND, REPLY_DELAY, e, known, one_conn_pool, through, upstream, url};

/// What the M survivor sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// A known-value query through the same pool or `Conn`.
    own: Result<i64, String>,
    /// Rows the survivor's write made visible to an independent connection.
    visible: Result<i64, String>,
    /// Bytes still unread in the client socket before the survivor starts;
    /// `None` for M, `Some` for M-residue.
    residue: Option<usize>,
}

struct Env {
    pool: Option<Pool>,
    conn: Option<Conn>,
    observer: Conn,
    table: String,
    proxy: Proxy,
    /// Direct URL, for the guarded contrast that replaces a broken `Conn`.
    direct_url: Option<String>,
}

impl Env {
    fn insert(&self, n: i32) -> String {
        primitives::insert_sql(&self.table, n)
    }

    /// The usage contrast for a directly held `Conn`: a cancelled operation
    /// leaves `Conn::is_disconnected()` true. The guarded caller checks it and
    /// replaces the connection instead of reusing it. The pool does the same
    /// check in its recycler, so this is a no-op there.
    async fn recover(&mut self) -> Result<bool, String> {
        if self.pool.is_some() {
            return Ok(false);
        }
        let url = self
            .direct_url
            .as_ref()
            .ok_or_else(|| "no direct URL for reconnect".to_string())?;
        primitives::recover_direct(&mut self.conn, url).await
    }

    /// Survivor: a known-value read and a side-effecting write through the
    /// scenario's shared object, counted by the independent connection.
    async fn survive(mut self, residue: Option<usize>) -> Result<Obs, String> {
        let (own, write) = if let Some(pool) = &self.pool {
            let mut conn = pool.get_conn().await.map_err(e)?;
            let own = primitives::select_value(&mut conn, 67890).await;
            let write = primitives::insert_row(&mut conn, &self.table, 1).await;
            (own, write)
        } else {
            let conn = self.conn.as_mut().expect("direct Conn scenario");
            let own = primitives::select_value(conn, 67890).await;
            let write = primitives::insert_row(conn, &self.table, 1).await;
            (own, write)
        };
        let visible = match write {
            Ok(()) => primitives::count_rows(&mut self.observer, &self.table).await,
            Err(err) => Err(format!("write: {err}")),
        };
        if let Some(pool) = self.pool.take() {
            let _ = tokio::time::timeout(BOUND, pool.disconnect()).await;
        }
        self.conn.take();
        let _ = tokio::time::timeout(BOUND, primitives::drop_table(&mut self.observer, &self.table))
            .await;
        Ok(Obs { own, visible, residue })
    }
}

/// Open the observer directly, create the table, and open the shared object
/// through the held-reply proxy. `direct` chooses pool (MY1) or `Conn` (MY2).
async fn setup(direct: bool, url: &str, pooled: &str, proxy: Proxy) -> Result<Env, String> {
    let mut observer = primitives::connect(url).await?;
    let table = primitives::table_name();
    primitives::create_table(&mut observer, &table).await?;
    let env = if direct {
        let mut conn = primitives::connect(pooled).await?;
        conn.query_drop("SELECT 1").await.map_err(e)?;
        Env { pool: None, conn: Some(conn), observer, table, proxy, direct_url: Some(url.to_string()) }
    } else {
        let pool = one_conn_pool(pooled)?;
        let mut conn = pool.get_conn().await.map_err(e)?;
        conn.query_drop("SELECT 1").await.map_err(e)?;
        drop(conn);
        Env { pool: Some(pool), conn: None, observer, table, proxy, direct_url: Some(url.to_string()) }
    };
    Ok(env)
}

fn scenario_env(direct: bool, url: &str, upstream: SocketAddr) -> impl std::future::Future<Output = Result<Env, String>> {
    let url = url.to_string();
    async move {
        let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let pooled = through(&url, proxy.addr())?;
        setup(direct, &url, &pooled, proxy).await
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

/// MY1's cancelled operation: BEGIN, then roll back when it completed.
async fn begin(pool: &Pool) -> Result<(), String> {
    let tx = pool.start_transaction(TxOpts::default()).await.map_err(e)?;
    tx.rollback().await.map_err(e)
}

/// B1: setup and the cancellation, nothing after it. A completed target is
/// dropped too; B1 judges only the cancellation.
fn b1(id: &str, direct: bool) {
    let report = explore(&config(), |ctx| async move {
        let url = url()?;
        let mut env = scenario_env(direct, &url, upstream(&url)?).await?;
        if direct {
            let conn = env.conn.as_mut().unwrap();
            let _ = ctx.target(known(conn)).await;
        } else {
            let _ = ctx.target(begin(env.pool.as_ref().unwrap())).await;
        }
        Ok(())
    });
    ladder::simple("B1", id, &report);
}

/// B2: after the cancellation, the same operation with the same arguments
/// through the same pool or `Conn`. Only `Ok` is checked.
fn b2(id: &str, direct: bool) {
    let report = explore(&config(), |ctx| async move {
        let url = url()?;
        let mut env = scenario_env(direct, &url, upstream(&url)?).await?;
        if direct {
            let conn = env.conn.as_mut().unwrap();
            match ctx.target(known(conn)).await {
                Some(result) => {
                    result?;
                }
                None => {
                    if std::env::var("DW_MYSQL_GUARD").as_deref() == Ok("1") {
                        env.recover().await?;
                    }
                    let conn = env.conn.as_mut().unwrap();
                    let retry = known(conn);
                    match tokio::time::timeout(BOUND, retry).await {
                        Ok(Ok(value)) => {
                            if std::env::var("DW_DIAG").is_ok() {
                                eprintln!("MY2 B2 retry returned {value}");
                            }
                        }
                        Ok(Err(err)) => return Err(format!("retry of the same query failed: {err}")),
                        Err(_) => return Err("retry of the same query timed out".into()),
                    }
                }
            }
        } else {
            let pool = env.pool.as_ref().unwrap();
            match ctx.target(begin(pool)).await {
                Some(result) => result?,
                None => match tokio::time::timeout(BOUND, begin(pool)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => return Err(format!("retry of BEGIN failed: {err}")),
                    Err(_) => return Err("retry of BEGIN timed out".into()),
                },
            }
        }
        Ok(())
    });
    ladder::simple("B2", id, &report);
}

/// B3: after the cancellation, a different operation of the same kind through
/// the same pool or `Conn`. Only `Ok` is checked.
fn b3(id: &str, direct: bool) {
    let report = explore(&config(), |ctx| async move {
        let url = url()?;
        let mut env = scenario_env(direct, &url, upstream(&url)?).await?;
        let cancelled = if direct {
            let conn = env.conn.as_mut().unwrap();
            match ctx.target(known(conn)).await {
                Some(result) => {
                    result?;
                    false
                }
                None => true,
            }
        } else {
            let pool = env.pool.as_ref().unwrap();
            match ctx.target(begin(pool)).await {
                Some(result) => {
                    result?;
                    false
                }
                None => true,
            }
        };
        if cancelled {
            if std::env::var("DW_MYSQL_GUARD").as_deref() == Ok("1") {
                env.recover().await?;
            }
            let other = async {
                if direct {
                    let conn = env.conn.as_mut().unwrap();
                    let rows: Vec<i64> = conn.query("SELECT 67890").await.map_err(e)?;
                    if std::env::var("DW_DIAG").is_ok() {
                        eprintln!("MY2 B3 different query returned {rows:?} (Ok is all that is judged)");
                    }
                    if rows.len() != 1 {
                        return Err(format!("different query returned {} rows", rows.len()));
                    }
                } else {
                    let mut conn = env.pool.as_ref().unwrap().get_conn().await.map_err(e)?;
                    conn.query_drop(env.insert(2)).await.map_err(e)?;
                }
                Ok::<(), String>(())
            };
            match tokio::time::timeout(BOUND, other).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(format!("different operation failed: {err}")),
                Err(_) => return Err("different operation timed out".into()),
            }
        }
        Ok(())
    });
    ladder::simple("B3", id, &report);
}

/// M or M-residue: the full template with the survivor and independent
/// observer. `residue` selects the M-residue variant.
fn m(id: &str, direct: bool, residue: bool) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let url = url()?;
        let mut env = scenario_env(direct, &url, upstream(&url)?).await?;
        if direct {
            let conn = env.conn.as_mut().unwrap();
            if let Some(result) = ctx.target(known(conn)).await {
                result?;
            }
        } else if let Some(result) = ctx.target(begin(env.pool.as_ref().unwrap())).await {
            result?;
        }
        let guarded = std::env::var("DW_MYSQL_GUARD").as_deref() == Ok("1");
        if guarded {
            env.recover().await?;
        }
        let residue = if residue {
            // Let a reply already on the wire reach the client's receive
            // buffer, then inspect it before the survivor reuses the object.
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

const MY1: (&str, bool) = ("MY1", false);
const MY2: (&str, bool) = ("MY2", true);

#[test]
fn my1_b1() {
    b1(MY1.0, MY1.1);
}

#[test]
fn my1_b2() {
    b2(MY1.0, MY1.1);
}

#[test]
fn my1_b3() {
    b3(MY1.0, MY1.1);
}

#[test]
fn my1_m() {
    m(MY1.0, MY1.1, false);
}

#[test]
fn my1_m_residue() {
    m(MY1.0, MY1.1, true);
}

#[test]
fn my2_b1() {
    b1(MY2.0, MY2.1);
}

#[test]
fn my2_b2() {
    b2(MY2.0, MY2.1);
}

#[test]
fn my2_b3() {
    b3(MY2.0, MY2.1);
}

#[test]
fn my2_m() {
    m(MY2.0, MY2.1, false);
}

#[test]
fn my2_m_residue() {
    m(MY2.0, MY2.1, true);
}

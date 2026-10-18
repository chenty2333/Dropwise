//! Cancellation-ablation ladder for the two sqlx-mysql scenarios (RESCAN.md,
//! section 6.7): B1 judges only the cancellation, B2 retries the same operation
//! with the same arguments, B3 runs a different operation of the same kind and
//! checks only `Ok`, and M is the existing
//! `explore_observed(Expect::Atomic)` survivor with its independent observer.
//!
//! Every level uses the same scenario skeleton and `Config` as `sqlx_mysql.rs`;
//! only what runs after a cancelled target changes. The reporting is shared in
//! `prospective/support/ladder.rs`. Environment: DW_MYSQL_URL (loopback
//! server), SCAN_VARIANT (label only).

#[path = "../../support/ladder.rs"]
mod ladder;

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Settle};
use sqlx::mysql::{MySqlConnection, MySqlPool, MySqlPoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

struct Env {
    pool: MySqlPool,
    observer: MySqlConnection,
    table: String,
}

async fn setup() -> Result<Env, String> {
    let url = std::env::var("DW_MYSQL_URL").map_err(e)?;
    let mut observer = MySqlConnection::connect(&url).await.map_err(e)?;
    let table = format!("dw_scan_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    observer
        .execute(AssertSqlSafe(format!("CREATE TABLE {table} (n INT NOT NULL) ENGINE=InnoDB")))
        .await
        .map_err(e)?;
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&url)
        .await
        .map_err(e)?;
    Ok(Env { pool, observer, table })
}

impl Env {
    fn insert(&self, n: i32) -> String {
        format!("INSERT INTO {} VALUES ({n})", self.table)
    }

    /// Survivor write through the pool, counted by the independent connection.
    /// Closing the pool ends any stale server transaction before the table is dropped.
    async fn survive(mut self) -> Result<i64, String> {
        self.pool.execute(AssertSqlSafe(self.insert(1))).await.map_err(e)?;
        let visible: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {}", self.table)))
            .fetch_one(&mut self.observer)
            .await
            .map_err(e)?;
        self.pool.close().await;
        self.observer.execute(AssertSqlSafe(format!("DROP TABLE {}", self.table))).await.map_err(e)?;
        Ok(visible)
    }
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(10),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

/// B3's different operation of the same kind: one autocommit insert through
/// the pool. Only `Ok` is judged; the value and the visibility are ignored.
async fn autocommit_insert(env: &Env) -> Result<(), String> {
    let insert = env.pool.execute(AssertSqlSafe(env.insert(1)));
    match tokio::time::timeout(Duration::from_secs(3), insert).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(e(err)),
        Err(_) => Err("autocommit insert timed out".into()),
    }
}

#[test]
fn s1_b1() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        // Drop a completed transaction too: B1 judges only the cancellation.
        let _ = ctx.target(env.pool.begin()).await;
        Ok(())
    });
    ladder::simple("B1", "S1", &report);
}

#[test]
fn s1_b2() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        match ctx.target(env.pool.begin()).await {
            Some(tx) => {
                tx.map_err(e)?.rollback().await.map_err(e)?;
            }
            None => {
                // The same operation with the same arguments, through the same pool.
                let retry = async {
                    let tx = env.pool.begin().await.map_err(e)?;
                    tx.rollback().await.map_err(e)?;
                    Ok::<(), String>(())
                };
                match tokio::time::timeout(Duration::from_secs(3), retry).await {
                    Ok(result) => result?,
                    Err(_) => return Err("retry of cancelled begin timed out".into()),
                }
            }
        }
        Ok(())
    });
    ladder::simple("B2", "S1", &report);
}

#[test]
fn s1_b3() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        let cancelled = match ctx.target(env.pool.begin()).await {
            Some(tx) => {
                tx.map_err(e)?.rollback().await.map_err(e)?;
                false
            }
            None => true,
        };
        if cancelled {
            autocommit_insert(&env).await?;
        }
        Ok(())
    });
    ladder::simple("B3", "S1", &report);
}

#[test]
fn s1_m() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup().await?;
        if let Some(tx) = ctx.target(env.pool.begin()).await {
            tx.map_err(e)?.rollback().await.map_err(e)?;
        }
        env.survive().await
    });
    ladder::observed("M", "S1", &report);
}

#[test]
fn s2_b1() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        let work = async {
            let mut tx = env.pool.begin().await?;
            tx.execute(AssertSqlSafe(env.insert(100))).await?;
            tx.commit().await
        };
        let _ = ctx.target(work).await;
        Ok(())
    });
    ladder::simple("B1", "S2", &report);
}

#[test]
fn s2_b2() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        let work = async {
            let mut tx = env.pool.begin().await?;
            tx.execute(AssertSqlSafe(env.insert(100))).await?;
            tx.commit().await
        };
        match ctx.target(work).await {
            Some(result) => {
                result.map_err(e)?;
            }
            None => {
                // The same whole transaction, same row, through the same pool.
                let retry = async {
                    let mut tx = env.pool.begin().await.map_err(e)?;
                    tx.execute(AssertSqlSafe(env.insert(100))).await.map_err(e)?;
                    tx.commit().await.map_err(e)?;
                    Ok::<(), String>(())
                };
                match tokio::time::timeout(Duration::from_secs(3), retry).await {
                    Ok(result) => result?,
                    Err(_) => return Err("retry of cancelled transaction timed out".into()),
                }
            }
        }
        Ok(())
    });
    ladder::simple("B2", "S2", &report);
}

#[test]
fn s2_b3() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        let work = async {
            let mut tx = env.pool.begin().await?;
            tx.execute(AssertSqlSafe(env.insert(100))).await?;
            tx.commit().await
        };
        let cancelled = match ctx.target(work).await {
            Some(result) => {
                result.map_err(e)?;
                false
            }
            None => true,
        };
        if cancelled {
            autocommit_insert(&env).await?;
        }
        Ok(())
    });
    ladder::simple("B3", "S2", &report);
}

#[test]
fn s2_m() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup().await?;
        let work = async {
            let mut tx = env.pool.begin().await?;
            tx.execute(AssertSqlSafe(env.insert(100))).await?;
            tx.commit().await
        };
        if let Some(r) = ctx.target(work).await {
            r.map_err(e)?;
        }
        env.survive().await
    });
    ladder::observed("M", "S2", &report);
}

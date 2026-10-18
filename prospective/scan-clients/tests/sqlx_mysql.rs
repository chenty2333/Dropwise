//! sqlx 0.9.0 MySQL driver, scanned like T3's R3.1/R3.5 (RESCAN.md, section 6).
//! Prints one RESCAN line per experiment; no outcome asserts.
//!
//! `MySqlTransactionManager::begin` raises `transaction_depth` only after the
//! BEGIN round trip, the shape that sqlx#4393 fixed for Postgres. Survivor: an
//! autocommit write through the same pool, counted by an independent connection.
//! Environment: DW_MYSQL_URL (loopback server), SCAN_VARIANT (label only).

#[path = "../../support/allowed.rs"]
mod allowed;

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use allowed::{Expect, Mode, Tally, Verdict, calibrate, op};
use dropwise::{Config, Ctx, Flavor, Race, Settle, explore};
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

fn rescan<S, Fut>(id: &str, mut scenario: S)
where
    S: FnMut(Ctx, Mode) -> Fut,
    Fut: Future<Output = Result<i64, String>>,
{
    let calibration = calibrate(&config(), &mut scenario);
    let tally = Tally::default();
    let report = explore(&config(), |ctx| {
        let fut = scenario(ctx, Mode::Run);
        let (calibration, tally) = (&calibration, tally.clone());
        async move {
            let seen = fut.await?;
            let checked = calibration.check(Expect::Atomic, &seen);
            tally.push(*checked.as_ref().unwrap_or(&Verdict::Neither));
            checked.map(drop)
        }
    });
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{report}");
    println!("{}", tally.summary(&format!("{id} [{variant}]"), &report));
}

#[test]
fn s1_pool_begin() {
    rescan("S1 sqlx-mysql Pool::begin", |ctx, mode| async move {
        let env = setup().await?;
        if let Some(tx) = op(&ctx, mode, env.pool.begin()).await {
            tx.map_err(e)?.rollback().await.map_err(e)?;
        }
        env.survive().await
    });
}

#[test]
fn s2_whole_transaction() {
    rescan("S2 sqlx-mysql begin+insert+commit", |ctx, mode| async move {
        let env = setup().await?;
        let work = async {
            let mut tx = env.pool.begin().await?;
            tx.execute(AssertSqlSafe(env.insert(100))).await?;
            tx.commit().await
        };
        if let Some(r) = op(&ctx, mode, work).await {
            r.map_err(e)?;
        }
        env.survive().await
    });
}

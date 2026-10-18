//! Post-hoc rescan of T3 (sqlx 0.9.0), see prospective/RESCAN.md. Separate from
//! the frozen study; does not assert on outcomes, prints one RESCAN line each.
//!
//! The scenario records two survivor observations of the same pool:
//! * `echo`: the frozen study's follow-up check (`SELECT $1` round trips);
//! * `visible`: an autocommit write through the pool, counted by an
//!   independent connection.
//! Each projection is judged by its own `explore_observed` run, so neither is
//! silently folded into the other's verdict.
//!
//! `SQLX_VARIANT` labels the build: `release` (crates.io 0.9.0) or `pr4394`
//! (0.9.0 with the upstream begin-cancellation fix), see run-rescan.sh.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{Config, Ctx, Expect, Flavor, Race, Report, Settle, Verdict, explore_observed};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, SqlSafeStr};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    echo: Vec<i64>,
    visible: i64,
}

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

struct Env {
    pool: PgPool,
    observer: PgConnection,
    table: String,
}

async fn setup() -> Result<Env, String> {
    let url = std::env::var("DATABASE_URL").map_err(e)?;
    let mut observer = PgConnection::connect(&url).await.map_err(e)?;
    let table = format!("dw_rescan_{}_{}", std::process::id(), NEXT.fetch_add(1, SeqCst));
    observer
        .execute(AssertSqlSafe(format!("CREATE TABLE {table} (n INT NOT NULL)")))
        .await
        .map_err(e)?;
    let pool = PgPoolOptions::new()
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

    /// Both survivor observations, then teardown. Closing the pool ends any
    /// stale server transaction before the table is dropped.
    async fn survive(mut self) -> Result<Obs, String> {
        let mut echo = Vec::new();
        for i in 0..8_i64 {
            let v: i64 = sqlx::query_scalar("SELECT $1::int8")
                .bind(1000 + i)
                .fetch_one(&self.pool)
                .await
                .map_err(e)?;
            echo.push(v);
        }
        self.pool.execute(AssertSqlSafe(self.insert(1))).await.map_err(e)?;
        let visible: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {}", self.table)))
            .fetch_one(&mut self.observer)
            .await
            .map_err(e)?;
        self.pool.close().await;
        self.observer
            .execute(AssertSqlSafe(format!("DROP TABLE {}", self.table)))
            .await
            .map_err(e)?;
        Ok(Obs { echo, visible })
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

fn variant() -> String {
    std::env::var("SQLX_VARIANT").unwrap_or_else(|_| "release".into())
}

/// Calibrate, explore, and print RESCAN lines for the new (`visible`) and the
/// frozen-study (`echo`) observation. Each projection is judged by its own
/// `explore_observed` run, so `visible` is no longer the only source of
/// verdicts; the two reports are independent runs of the same scenario.
fn rescan<S, Fut>(id: &str, mut scenario: S)
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<Obs, String>>,
{
    let visible = {
        let scenario = &mut scenario;
        explore_observed(&config(), Expect::Atomic, |ctx| {
            let fut = scenario(ctx);
            async move { fut.await.map(|obs| obs.visible) }
        })
    };
    let echo = {
        let scenario = &mut scenario;
        explore_observed(&config(), Expect::Atomic, |ctx| {
            let fut = scenario(ctx);
            async move { fut.await.map(|obs| obs.echo) }
        })
    };
    let v = variant();
    println!("{visible}");
    println!("{}", summary(&format!("{id} [{v}] new-observation"), &visible));
    println!("{echo}");
    println!("{}", summary(&format!("{id} [{v}] echo-observation"), &echo));
}

/// One line per experiment. The field names keep the old `allowed.rs` format;
/// the counts now come from `Report` (trial verdicts only, baseline excluded).
fn summary(id: &str, report: &Report) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} verdicts(all runs) pre={} post={} neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        report.baseline_errors.len(),
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    )
}

#[test]
fn r3_1_pool_begin() {
    rescan("R3.1", |ctx| async move {
        let env = setup().await?;
        if let Some(tx) = ctx.target(env.pool.begin()).await {
            tx.map_err(e)?.rollback().await.map_err(e)?;
        }
        env.survive().await
    });
}

#[test]
fn r3_2_pool_acquire() {
    rescan("R3.2", |ctx| async move {
        let env = setup().await?;
        if let Some(conn) = ctx.target(env.pool.acquire()).await {
            drop(conn.map_err(e)?);
        }
        env.survive().await
    });
}

#[test]
fn r3_3_prepare_on_pooled_connection() {
    rescan("R3.3", |ctx| async move {
        let env = setup().await?;
        let statement = format!("INSERT INTO {} VALUES ($1)", env.table);
        {
            let mut conn = env.pool.acquire().await.map_err(e)?;
            let prepare = conn.prepare(AssertSqlSafe(statement.clone()).into_sql_str());
            if let Some(r) = ctx.target(prepare).await {
                r.map_err(e)?;
            }
        }
        // Reuse the same SQL through the pool's statement cache, then remove
        // the row again so `visible` counts only the survivor write.
        sqlx::query(AssertSqlSafe(statement.as_str())).bind(7).execute(&env.pool).await.map_err(e)?;
        env.pool
            .execute(AssertSqlSafe(format!("DELETE FROM {} WHERE n = 7", env.table)))
            .await
            .map_err(e)?;
        env.survive().await
    });
}

#[test]
fn r3_4_frozen_fetch_target() {
    rescan("R3.4", |ctx| async move {
        let env = setup().await?;
        {
            let mut conn = env.pool.acquire().await.map_err(e)?;
            let fetch = sqlx::query_scalar::<_, i64>("SELECT 41::int8").fetch_one(&mut *conn);
            if let Some(r) = ctx.target(fetch).await {
                r.map_err(e)?;
            }
        }
        env.survive().await
    });
}

#[test]
fn r3_5_whole_transaction() {
    rescan("R3.5", |ctx| async move {
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
}

#[test]
fn r3_6_two_autocommit_writes() {
    rescan("R3.6", |ctx| async move {
        let env = setup().await?;
        let work = async {
            env.pool.execute(AssertSqlSafe(env.insert(100))).await?;
            env.pool.execute(AssertSqlSafe(env.insert(101))).await
        };
        if let Some(r) = ctx.target(work).await {
            r.map_err(e)?;
        }
        env.survive().await
    });
}

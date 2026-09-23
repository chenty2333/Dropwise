//! Pre-registered exploratory scenarios for T3 (sqlx 0.9.0), contract C3:
//! after a Postgres query future is dropped at a Pending boundary, later queries
//! through the same one-connection pool each return their own result.
//!
//! Each target is raced inside the program's own `tokio::select!`; the competing
//! branch is Dropwise's `Preempt`. Afterward, eight sequential, uniquely bound
//! queries verify response association. Tests only record full reports; they do
//! not assert whether a report is clean.

use std::future::Future;
use std::time::{Duration, Instant};

use dropwise::{explore, Config, Ctx, Flavor, Race, Report, Settle};
use futures_util::StreamExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

const FOLLOW_UPS: i64 = 8;
const STREAM_ROWS: i64 = 100_000;

#[derive(Clone, Copy)]
enum Scenario {
    SingleRow,
    LargeResult,
    ServerSleep,
}

/// Frozen configuration (PREREGISTRATION.md section 4), including the current-thread flavor.
fn config() -> Config {
    Config {
        races: vec![
            Race::Immediate,
            Race::Reschedule(1),
            Race::After(Duration::from_millis(1)),
        ],
        max_cancellations: 1,
        settle: Settle::default(),
        scenario_timeout: Duration::from_secs(10),
        flavor: Flavor::CurrentThread,
        ..Config::default()
    }
}

fn record(scenario: &str, report: &Report, elapsed: Duration) {
    let run = std::env::var("RUN").unwrap_or_else(|_| "dev".into());
    let dir = format!("{}/../runs/t3/{scenario}", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let full_report = format!("wall_clock_elapsed: {:.6}s\n{report}", elapsed.as_secs_f64());
    std::fs::write(format!("{dir}/{run}.txt"), &full_report).unwrap();
    println!(
        "{scenario}: {} plans, {} violations, {} unrealized, exhaustive={}, wall={:.6}s\n{report}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        elapsed.as_secs_f64(),
    );
}

async fn make_pool() -> Result<PgPool, String> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|e| format!("DATABASE_URL is not set: {e}"))?;
    PgPoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .connect(&url)
        .await
        .map_err(|e| format!("connecting one-connection pool: {e}"))
}

/// The query future competes in the caller's select, just as a real application
/// would race it against its own ready branch.
async fn race_query<F>(ctx: &Ctx, future: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
{
    let (operation, mut preempt) = ctx.race(future);
    tokio::select! {
        biased;
        result = operation => result?,
        _ = &mut preempt => {},
    }
    Ok(())
}

async fn verify_follow_ups(pool: &PgPool, base: i64) -> Result<(), String> {
    for i in 0..FOLLOW_UPS {
        let expected = base + i;
        let actual = sqlx::query_scalar::<_, i64>("SELECT $1::int8")
            .bind(expected)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("follow-up query {i} for {expected} returned an error: {e}"))?;
        if actual != expected {
            return Err(format!(
                "follow-up query {i} expected its own value {expected}, received {actual}"
            ));
        }
    }
    Ok(())
}

async fn warm_pool(pool: &PgPool) -> Result<(), String> {
    let warmed: i64 = sqlx::query_scalar("SELECT 0::int8")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("pool warm-up query: {e}"))?;
    if warmed != 0 {
        return Err(format!("pool warm-up returned {warmed}, expected 0"));
    }
    Ok(())
}

async fn run(ctx: Ctx, scenario: Scenario) -> Result<(), String> {
    let pool = make_pool().await?;
    // Establish and exercise the pool's sole connection before the cancellation target.
    warm_pool(&pool).await?;

    let base = match scenario {
        Scenario::SingleRow => {
            let value = 31_000_i64;
            race_query(&ctx, async {
                let actual = sqlx::query_scalar::<_, i64>("SELECT $1::int8")
                    .bind(value)
                    .fetch_one(&pool)
                    .await
                    .map_err(|e| format!("target single-row query: {e}"))?;
                if actual != value {
                    return Err(format!("target query expected {value}, received {actual}"));
                }
                Ok(())
            })
            .await?;
            31_100
        }
        Scenario::LargeResult => {
            race_query(&ctx, async {
                let rows = sqlx::query_scalar::<_, i64>(
                    "SELECT g::int8 FROM generate_series(1, 20000) AS g",
                )
                .fetch_all(&pool)
                .await
                .map_err(|e| format!("target large-result query: {e}"))?;
                if rows.len() != 20_000 || rows.first() != Some(&1) || rows.last() != Some(&20_000) {
                    return Err(format!(
                        "target large-result query returned {} rows (first {:?}, last {:?})",
                        rows.len(), rows.first(), rows.last()
                    ));
                }
                Ok(())
            })
            .await?;
            32_100
        }
        Scenario::ServerSleep => {
            let value = 33_000_i64;
            race_query(&ctx, async {
                let actual = sqlx::query_scalar::<_, i64>(
                    "SELECT $1::int8 FROM (SELECT pg_sleep(0.05)) AS delayed",
                )
                .bind(value)
                .fetch_one(&pool)
                .await
                .map_err(|e| format!("target pg_sleep query: {e}"))?;
                if actual != value {
                    return Err(format!("target pg_sleep query expected {value}, received {actual}"));
                }
                Ok(())
            })
            .await?;
            33_100
        }
    };

    verify_follow_ups(&pool, base).await
}

async fn run_partial_stream(ctx: Ctx) -> Result<(), String> {
    let pool = make_pool().await?;
    warm_pool(&pool).await?;

    let mut stream = Box::pin(
        sqlx::query_scalar::<_, i64>("SELECT g::int8 FROM generate_series(1, 100000) AS g")
            .fetch(&pool),
    );
    // Ensure the query stream has yielded rows and is only partially consumed
    // before its next() work is raced and can be dropped.
    for expected in 1..=3_i64 {
        match stream.as_mut().next().await {
            Some(Ok(actual)) if actual == expected => {}
            Some(Ok(actual)) => {
                return Err(format!(
                    "partial-stream prefix expected {expected}, received {actual}"
                ));
            }
            Some(Err(e)) => return Err(format!("partial-stream prefix query: {e}")),
            None => return Err("partial-stream query ended before its three-row prefix".into()),
        }
    }

    race_query(&ctx, async move {
        let mut last = 3_i64;
        while let Some(row) = stream.as_mut().next().await {
            let actual = row.map_err(|e| format!("partial-stream target query: {e}"))?;
            let expected = last + 1;
            if actual != expected {
                return Err(format!(
                    "partial-stream target expected row {expected}, received {actual}"
                ));
            }
            last = actual;
        }
        if last != STREAM_ROWS {
            return Err(format!(
                "partial-stream target ended at row {last}, expected {STREAM_ROWS}"
            ));
        }
        Ok(())
    })
    .await?;

    verify_follow_ups(&pool, 34_100).await
}

fn explore_and_record(name: &str, scenario: Scenario) {
    let started = Instant::now();
    let report = explore(&config(), |ctx| run(ctx, scenario));
    record(name, &report, started.elapsed());
}

#[test]
fn s3_1_single_row_query() {
    explore_and_record("s3_1", Scenario::SingleRow);
}

#[test]
fn s3_2_large_result() {
    explore_and_record("s3_2", Scenario::LargeResult);
}

#[test]
fn s3_3_server_sleep() {
    explore_and_record("s3_3", Scenario::ServerSleep);
}

#[test]
fn s3_4_partial_stream() {
    let started = Instant::now();
    let report = explore(&config(), run_partial_stream);
    record("s3_4", &report, started.elapsed());
}

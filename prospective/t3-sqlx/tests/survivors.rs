//! Small application-composition experiment, separate from the frozen T3 study.
//! Real SQLx/Postgres: does a cancelled borrower contaminate the next borrower?
//! Raw SQL BEGIN is an intentionally unsafe caller control, not a SQLx bug claim.

use std::error::Error;
use std::time::Duration;
#[path = "../../support/allowed.rs"]
mod allowed;
use allowed::{Expect, Mode, calibrate};
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    inherited: i32,
    committed: i32,
}

use dropwise::{Config, Ctx, CutOutcome, Flavor, Race, Report, Settle, explore};
use sqlx::Connection;
use sqlx::postgres::PgPoolOptions;

type TestError = Box<dyn Error + Send + Sync>;

async fn scenario(ctx: Ctx, guarded: bool, mode: Mode) -> Result<Observation, TestError> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    let mut first = pool.acquire().await?;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await?;
    sqlx::query("CREATE TEMP TABLE survivor_value (n INT NOT NULL)")
        .execute(&mut *first)
        .await?;
    sqlx::query("INSERT INTO survivor_value VALUES (0)")
        .execute(&mut *first)
        .await?;

    if mode == Mode::Skip {
        // `pre` calibration: the borrower never starts its operation.
    } else if guarded {
        let mut tx = first.begin().await?;
        sqlx::query("UPDATE survivor_value SET n = 1")
            .execute(&mut *tx)
            .await?;
        // A single application wait, after an actual uncommitted DB update.
        // The transaction guard belongs to the cancellable operation, not the
        // surviving caller. Both normal exit and cancellation drop the guard.
        ctx.target(async move {
            let _transaction = tx;
            tokio::task::yield_now().await;
        })
        .await;
    } else {
        sqlx::query("BEGIN").execute(&mut *first).await?;
        sqlx::query("UPDATE survivor_value SET n = 1")
            .execute(&mut *first)
            .await?;
        // Common caller mistake: cleanup only runs on the normal return path.
        if ctx.target(tokio::task::yield_now()).await.is_some() {
            sqlx::query("ROLLBACK").execute(&mut *first).await?;
        }
    }
    drop(first);

    // No sleep: reacquisition and a real query observe SQLx's queued cleanup.
    let mut next = pool.acquire().await?;
    let next_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *next)
        .await?;
    // The successor uses the recommended guard API. A poisoned pooled session
    // must not make this otherwise well-behaved borrower commit its predecessor.
    let mut successor_tx = next.begin().await?;
    let inherited: i32 = sqlx::query_scalar("SELECT n FROM survivor_value")
        .fetch_one(&mut *successor_tx)
        .await?;
    sqlx::query("UPDATE survivor_value SET n = n + 10")
        .execute(&mut *successor_tx)
        .await?;
    successor_tx.commit().await?;
    let committed: i32 = sqlx::query_scalar("SELECT n FROM survivor_value")
        .fetch_one(&mut *next)
        .await?;
    drop(next);
    pool.close().await;

    if next_pid != first_pid {
        return Err("same-backend reuse was not exercised".into());
    }
    println!("guarded={guarded} inherited={inherited} successor_commit={committed}");
    Ok(Observation {
        inherited,
        committed,
    })
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 16,
        scenario_timeout: Duration::from_secs(10),
        settle: Settle {
            tokio_time: Duration::ZERO,
            ..Settle::default()
        },
        ..Config::default()
    }
}

fn run(guarded: bool) -> Report {
    // Allowed set, not equality with the completed run: the abandoned borrower
    // may look as if it never started (pre) or as if it finished (post).
    let calibration = calibrate(&config(), |ctx, mode| async move {
        scenario(ctx, guarded, mode).await.map_err(|e| e.to_string())
    });
    let report = explore(&config(), |ctx| {
        let calibration = &calibration;
        async move {
            let observed = scenario(ctx, guarded, Mode::Run)
                .await
                .map_err(|e| e.to_string())?;
            calibration
                .check(Expect::Atomic, &observed)
                .map(drop)
                .map_err(|e| format!("successor differs: {e}"))
        }
    });
    println!("guarded={guarded}\n{report}");
    assert!(
        report.baseline_errors.is_empty() && report.baseline_settled,
        "{report}"
    );
    assert!(
        report.exhaustive && report.unsettled().next().is_none(),
        "{report}"
    );
    assert_eq!(report.trials.len(), 2, "one wait, two cancellation timings");
    assert!(
        report
            .trials
            .iter()
            .all(|t| t.outcomes == [CutOutcome::Cancelled]),
        "{report}"
    );
    report
}

#[test]
fn raw_begin_can_contaminate_the_next_borrower() {
    let report = run(false);
    assert_eq!(report.violations().count(), 2, "{report}");
    assert!(
        report.violations().all(|t| t
            .invariant
            .as_ref()
            .unwrap_err()
            .contains("successor differs")),
        "{report}"
    );
}

#[test]
fn transaction_guard_protects_the_next_borrower() {
    let report = run(true);
    assert!(report.is_clean(), "{report}");
}

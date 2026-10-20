//! Why a clean report is not automatically a safety conclusion.
//!
//! `cargo run --example limits`
//!
//! Three ways a run can fail to support "no violation within the covered
//! plans": the calibration cannot decide, no cancellation plan is generated
//! (the marked target never returns `Pending`), and the search budget truncates
//! the plan space. Each case is shown next to a corrected setup that produces a
//! decidable, clean-within-coverage report. The expected failures are asserted;
//! none of them panics the example.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};

use dropwise::{Config, Ctx, Expect, Race, Report, explore, explore_observed};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

static FLIP: AtomicU32 = AtomicU32::new(0);

/// The observation changes on every run, so the two `pre` samples (and the two
/// `post` samples) disagree. That is an unstable calibration, not a violation.
async fn unstable_calibration(ctx: Ctx) -> Result<u32, String> {
    let observation = FLIP.fetch_add(1, SeqCst);
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    Ok(observation % 2)
}

async fn stable_calibration(ctx: Ctx) -> Result<u32, String> {
    let completed = Arc::new(AtomicU32::new(0));
    let inner = completed.clone();
    ctx.target(async move {
        tokio::task::yield_now().await;
        inner.fetch_add(1, SeqCst);
    })
    .await;
    // pre: 0 (the target never started), post: 1 (it completed), and a
    // cancellation before the increment is also 0. `Expect::Atomic` allows
    // both, but the two calibrations are distinguishable.
    Ok(completed.load(SeqCst))
}

async fn no_pending(ctx: Ctx) -> Result<(), String> {
    // The target completes on its first poll: there is no boundary at which
    // to cancel it, so no plan can be generated.
    ctx.target(async {}).await;
    Ok(())
}

async fn one_pending(ctx: Ctx) -> Result<(), String> {
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    Ok(())
}

async fn three_pendings(ctx: Ctx) -> Result<(), String> {
    for _ in 0..3 {
        ctx.target(async {
            tokio::task::yield_now().await;
        })
        .await;
    }
    Ok(())
}

fn show_unknown() -> Report {
    let report = explore_observed(&config(), Expect::Atomic, unstable_calibration);
    println!("\n== unstable calibration: no verdict is safe ==");
    println!("{report}");
    assert!(report.unknown().count() > 0, "{report}");
    assert!(
        report.is_clean(),
        "an unstable calibration is not itself a violation: {report}"
    );
    assert!(
        !report.is_clean_within_coverage(),
        "unknown plans must not become a clean conclusion: {report}"
    );
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("undecided")),
        "the coverage gap names the undecided plans: {report}"
    );

    let stable = explore_observed(&config(), Expect::Atomic, stable_calibration);
    println!("\n== the same shape with a deterministic observation ==");
    println!("{stable}");
    assert!(stable.is_clean_within_coverage(), "{stable}");
    stable
}

fn show_missing_cancellation_point() -> Report {
    let report = explore(&config(), no_pending);
    println!("\n== target never returns Pending: nothing was cancelled ==");
    println!("{report}");
    assert!(report.trials.is_empty(), "{report}");
    assert!(
        report.is_clean(),
        "there is no violation to report: {report}"
    );
    assert!(
        !report.is_clean_within_coverage(),
        "a run that generated no plan is not evidence of safety: {report}"
    );
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("returned Pending")),
        "the report names the missing cancellation point: {report}"
    );

    let covered = explore(&config(), one_pending);
    println!("\n== the corrected target has one observed Pending boundary ==");
    println!("{covered}");
    assert!(covered.is_clean_within_coverage(), "{covered}");
    covered
}

fn show_truncated_search() -> Report {
    let truncated = explore(
        &Config {
            max_runs: 2,
            ..config()
        },
        three_pendings,
    );
    println!("\n== the run budget truncates the plan space ==");
    println!("{truncated}");
    assert!(truncated.truncated, "{truncated}");
    assert!(
        !truncated.is_clean_within_coverage(),
        "a truncated search cannot support a clean conclusion: {truncated}"
    );
    assert!(
        truncated
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("truncated")),
        "the report names the truncation: {truncated}"
    );

    let complete = explore(
        &Config {
            max_runs: 64,
            ..config()
        },
        three_pendings,
    );
    println!("\n== the same scenario with enough budget ==");
    println!("{complete}");
    assert!(complete.exhaustive, "{complete}");
    assert!(complete.is_clean_within_coverage(), "{complete}");
    complete
}

fn main() {
    show_unknown();
    show_missing_cancellation_point();
    show_truncated_search();
    println!(
        "\nall three limits demonstrated: `is_clean()` alone was true for the \
         unstable-calibration and missing-point runs, but neither is a clean \
         conclusion within coverage"
    );
}

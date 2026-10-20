//! Tests for the product-facing surface: plan replay, coverage classification,
//! diagnostics, and the small scenario helpers. Everything is in-process.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
use std::time::Duration;

use dropwise::support::{bounded, unique};
use dropwise::{
    Config, Ctx, Cut, CutOutcome, Expect, Gap, Plan, Race, ReplayError, Verdict, explore,
    explore_observed, replay, replay_observed,
};

fn immediate() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

fn cut(target: usize, after_pending: usize) -> Cut {
    Cut {
        target,
        after_pending,
        race: Race::Immediate,
    }
}

async fn fail_when_cancelled(ctx: Ctx) -> Result<(), String> {
    let out = ctx
        .target(async {
            tokio::task::yield_now().await;
        })
        .await;
    match out {
        Some(()) => Ok(()),
        None => Err("the target was cancelled".into()),
    }
}

#[test]
fn a_failing_plan_can_be_replayed_with_the_same_engine() {
    let report = explore(&immediate(), fail_when_cancelled);
    let failed = report
        .violations()
        .find(|trial| !trial.plan.is_empty())
        .expect("the scenario violates its invariant when cancelled")
        .clone();
    assert_eq!(failed.plan, [cut(0, 1)], "{report}");

    let replay = replay(&immediate(), &failed.replay_plan(), fail_when_cancelled)
        .expect("a plan produced by explore is replayable");
    assert_eq!(replay.trials.len(), 1, "{replay}");
    let trial = &replay.trials[0];
    assert_eq!(trial.plan, failed.plan, "{replay}");
    assert!(trial.is_realized(), "{replay}");
    assert!(trial.is_violation(), "{replay}");
    assert!(!replay.is_clean_within_coverage(), "{replay}");
    assert!(trial.sites[0].is_some(), "{replay}");
    assert!(
        replay.to_string().contains("violation observed"),
        "{replay}"
    );
}

#[test]
fn replay_reports_an_unreached_cut_as_a_coverage_gap() {
    // One Pending is observed; a plan for the second never reaches its cut.
    let report = replay(
        &immediate(),
        &Plan::new(vec![cut(0, 2)]),
        fail_when_cancelled,
    )
    .expect("the plan is structurally valid");
    let trial = &report.trials[0];
    assert_eq!(trial.outcomes, [CutOutcome::Unrealized], "{report}");
    assert!(!trial.is_realized(), "{report}");
    assert!(trial.sites[0].is_some(), "{report}");
    assert!(report.is_clean(), "no violation was observed: {report}");
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("not realized")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

async fn always_fails(ctx: Ctx) -> Result<(), String> {
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    Err("the scenario fails even without cancellation".into())
}

#[test]
fn replay_does_not_hide_a_baseline_failure() {
    let report = replay(&immediate(), &Plan::new(vec![cut(0, 1)]), always_fails)
        .expect("the plan itself is valid");
    assert!(
        report
            .baseline_errors
            .iter()
            .any(|error| error.contains("even without cancellation")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

async fn half_done(ctx: Ctx) -> Result<u32, String> {
    let count = Arc::new(AtomicU32::new(0));
    let inner = count.clone();
    ctx.target(async move {
        inner.fetch_add(1, SeqCst);
        tokio::task::yield_now().await; // Pending #1: count == 1
        inner.fetch_add(1, SeqCst);
        tokio::task::yield_now().await; // Pending #2: count == 2
    })
    .await;
    Ok(count.load(SeqCst))
}

#[test]
fn replay_observed_keeps_the_contract_and_the_calibration() {
    let report = explore_observed(&immediate(), Expect::Atomic, half_done);
    let failed = report
        .trials
        .iter()
        .find(|trial| trial.verdict == Some(Verdict::Neither))
        .expect("the first Pending leaves a half-done counter")
        .clone();

    let replay = replay_observed(
        &immediate(),
        Expect::Atomic,
        &failed.replay_plan(),
        half_done,
    )
    .expect("a plan produced by explore_observed is replayable");
    let trial = &replay.trials[0];
    assert_eq!(trial.verdict, Some(Verdict::Neither), "{replay}");
    assert!(trial.is_violation(), "{replay}");
    let comparison = trial.comparison.as_deref().expect("comparison is recorded");
    assert!(comparison.contains("observed 1"), "{comparison}");
    assert!(comparison.contains("pre=0"), "{comparison}");
    assert!(comparison.contains("post=2"), "{comparison}");
    assert!(
        replay.diagnostics().contains("comparison: observed 1"),
        "{}",
        replay.diagnostics()
    );
}

async fn two_steps(ctx: Ctx) -> Result<(), String> {
    for _ in 0..2 {
        ctx.target(async {
            tokio::task::yield_now().await;
        })
        .await;
    }
    Ok(())
}

#[test]
fn a_pair_plan_is_replayable() {
    let config = Config {
        max_cancellations: 2,
        ..immediate()
    };
    let report = explore(&config, two_steps);
    let pair = report
        .trials
        .iter()
        .find(|trial| trial.plan.len() == 2)
        .expect("a two-cut plan")
        .clone();

    let replay = replay(&config, &pair.replay_plan(), two_steps).expect("pair plan replay");
    assert_eq!(replay.trials.len(), 1, "{replay}");
    assert_eq!(replay.trials[0].plan.len(), 2, "{replay}");
    assert!(replay.trials[0].is_realized(), "{replay}");
}

async fn two_cuts_with_gap(ctx: Ctx) -> Result<bool, String> {
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    ctx.gap(Gap::Sleep(Duration::from_millis(10))).await;
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    Ok(true)
}

#[test]
fn a_manually_combined_two_cut_gap_plan_is_replayable() {
    // The search expands gap choices only for single-cut plans, so this
    // combination is not generated automatically. Replay still supports it:
    // validate_plan accepts it and Ctx::gap applies the assignment.
    let config = Config {
        max_cancellations: 2,
        ..immediate()
    };
    let plan = Plan {
        cuts: vec![cut(0, 1), cut(1, 1)],
        repeat: false,
        gaps: vec![Some(Gap::Now)],
    };
    let replay = replay_observed(&config, Expect::Atomic, &plan, two_cuts_with_gap)
        .expect("combined plan replay");
    assert_eq!(replay.trials.len(), 1, "{replay}");
    assert_eq!(replay.trials[0].plan.len(), 2, "{replay}");
    assert_eq!(replay.trials[0].gaps, [Some(Gap::Now)], "{replay}");
    assert!(replay.trials[0].is_realized(), "{replay}");
}

async fn gapped(ctx: Ctx) -> Result<bool, String> {
    ctx.target(async {
        tokio::task::yield_now().await;
    })
    .await;
    ctx.gap(Gap::Sleep(Duration::from_millis(10))).await;
    Ok(true)
}

#[test]
fn a_gap_substitution_is_kept_by_replay() {
    let config = Config {
        gap_choices: vec![Gap::Now],
        ..immediate()
    };
    let report = explore_observed(&config, Expect::Atomic, gapped);
    let selected = report
        .trials
        .iter()
        .find(|trial| trial.gaps == [Some(Gap::Now)])
        .expect("the substituted gap is explored")
        .clone();

    let replay = replay_observed(&config, Expect::Atomic, &selected.replay_plan(), gapped)
        .expect("gap plan replay");
    let trial = &replay.trials[0];
    assert_eq!(trial.gaps, [Some(Gap::Now)], "{replay}");
    assert_eq!(trial.verdict, selected.verdict, "{replay}");
}

async fn repeated(ctx: Ctx) -> Result<u8, String> {
    for _ in 0..3 {
        ctx.target(async {
            tokio::task::yield_now().await;
        })
        .await;
    }
    Ok(0)
}

#[test]
fn a_repeated_site_plan_is_replayable() {
    let config = Config {
        repeat_at_site: true,
        ..immediate()
    };
    let report = explore_observed(&config, Expect::Atomic, repeated);
    let repeated_trial = report
        .trials
        .iter()
        .find(|trial| trial.repeat && trial.cancelled > 1)
        .expect("a repeated-site plan")
        .clone();

    let replay = replay_observed(
        &config,
        Expect::Atomic,
        &repeated_trial.replay_plan(),
        repeated,
    )
    .expect("repeated-site plan replay");
    let trial = &replay.trials[0];
    assert!(trial.repeat, "{replay}");
    assert!(trial.cancelled > 1, "{replay}");
    assert_eq!(trial.verdict, repeated_trial.verdict, "{replay}");
}

async fn repeated_with_gap(ctx: Ctx) -> Result<u8, String> {
    for _ in 0..3 {
        ctx.target(async {
            tokio::task::yield_now().await;
        })
        .await;
        ctx.gap(Gap::Sleep(Duration::from_millis(10))).await;
    }
    Ok(0)
}

#[test]
fn a_repeated_site_plan_can_carry_a_gap() {
    let config = Config {
        repeat_at_site: true,
        gap_choices: vec![Gap::Now],
        ..immediate()
    };
    let report = explore_observed(&config, Expect::Atomic, repeated_with_gap);
    let selected = report
        .trials
        .iter()
        .find(|trial| trial.repeat && trial.gaps == [Some(Gap::Now)])
        .expect("a repeated-site plan with a gap substitution")
        .clone();

    let replay = replay_observed(
        &config,
        Expect::Atomic,
        &selected.replay_plan(),
        repeated_with_gap,
    )
    .expect("repeated-site gap replay");
    let trial = &replay.trials[0];
    assert!(trial.repeat, "{replay}");
    assert_eq!(trial.gaps, [Some(Gap::Now)], "{replay}");
    assert!(trial.cancelled > 1, "{replay}");
}

#[test]
fn unsupported_or_malformed_plans_are_rejected() {
    let config = immediate();
    let replay_error = |plan: Plan| {
        replay(&config, &plan, fail_when_cancelled).expect_err("the plan must be rejected")
    };

    assert_eq!(replay_error(Plan::default()), ReplayError::EmptyPlan);
    assert_eq!(
        replay_error(Plan::new(vec![cut(0, 0)])),
        ReplayError::CalibrationCut { target: 0 }
    );
    assert_eq!(
        replay_error(Plan::new(vec![cut(0, 1), cut(0, 2)])),
        ReplayError::DuplicateTarget { target: 0 }
    );
    assert_eq!(
        replay_error(Plan::new(vec![cut(1, 1), cut(0, 1)])),
        ReplayError::UnsortedTargets
    );
    assert_eq!(
        replay_error(Plan {
            repeat: true,
            cuts: vec![cut(0, 1), cut(1, 1)],
            gaps: Vec::new(),
        }),
        ReplayError::RepeatWithSeveralCuts
    );
    let two = Plan::new(vec![cut(0, 1), cut(1, 1)]);
    assert_eq!(
        replay_error(two),
        ReplayError::ExceedsMaxCancellations { cuts: 2, max: 1 }
    );
}

#[test]
fn replay_rejects_a_gap_assignment_the_scenario_no_longer_registers() {
    let plan = Plan {
        cuts: vec![cut(0, 1)],
        repeat: false,
        gaps: vec![Some(Gap::Now), Some(Gap::Yield(1))],
    };
    let error = replay_observed(&immediate(), Expect::Atomic, &plan, gapped)
        .expect_err("the second gap assignment cannot be applied");
    assert_eq!(
        error,
        ReplayError::UnappliedGap {
            assigned: 2,
            registered: 1,
        }
    );
}

#[test]
fn a_target_that_never_yields_pending_is_reported_as_a_coverage_gap() {
    let report = explore(&immediate(), |ctx: Ctx| async move {
        ctx.target(async {}).await;
        Ok(())
    });
    assert_eq!(report.baseline_targets, 1, "{report}");
    assert!(report.trials.is_empty(), "{report}");
    assert!(report.is_clean(), "{report}");
    assert!(!report.is_clean_within_coverage(), "{report}");
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("returned Pending")),
        "{report}"
    );
    assert!(
        report
            .to_string()
            .contains("no cancellation plan was generated"),
        "{report}"
    );
}

#[test]
fn a_truncated_search_is_reported_as_truncated() {
    let config = Config {
        max_runs: 2,
        ..immediate()
    };
    let report = explore(&config, |ctx: Ctx| async move {
        for _ in 0..3 {
            ctx.target(async {
                tokio::task::yield_now().await;
            })
            .await;
        }
        Ok(())
    });
    assert!(report.truncated, "{report}");
    assert!(!report.is_clean_within_coverage(), "{report}");
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("truncated")),
        "{report}"
    );
    assert!(report.to_string().contains("search truncated"), "{report}");
}

#[tokio::test(start_paused = true)]
async fn bounded_turns_a_hang_into_a_stable_error() {
    let value = bounded(Duration::from_secs(1), "fast step", async {
        Ok::<_, String>(7)
    })
    .await
    .expect("the fast step completes");
    assert_eq!(value, 7);

    let error = bounded(Duration::from_secs(1), "slow step", async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok::<_, String>(())
    })
    .await
    .expect_err("the slow step times out");
    assert_eq!(error, "slow step timed out after 1s");
}

#[test]
fn unique_identifiers_differ_within_one_process() {
    let first = unique("dropwise-test");
    let second = unique("dropwise-test");
    assert_ne!(first, second);
    assert!(first.starts_with("dropwise-test-"));
    assert!(second.starts_with("dropwise-test-"));
}

#[test]
fn baseline_only_budget_does_not_claim_targets_never_yielded() {
    for max_runs in [0, 1] {
        let config = Config {
            max_runs,
            ..immediate()
        };
        let report = explore(&config, two_steps);
        assert!(report.truncated, "{report}");
        assert!(!report.is_clean_within_coverage(), "{report}");
        let gaps = report.coverage_gaps().join("\n");
        assert!(gaps.contains("truncated"), "{gaps}");
        assert!(
            !gaps.contains("no marked target returned Pending"),
            "{gaps}"
        );
        assert!(!gaps.contains("no target was marked"), "{gaps}");
    }
}

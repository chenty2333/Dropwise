//! Incomplete experiments must not become definitive oracle results.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{
    Config, Ctx, Cut, CutOutcome, Expect, FieldContract, Gap, Observation, Plan, Race, Settle,
    Verdict, explore, explore_fields, explore_observed, replay_fields, replay_observed,
};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

fn plan(after_pending: usize) -> Plan {
    Plan::new(vec![Cut {
        target: 0,
        after_pending,
        race: Race::Immediate,
    }])
}

async fn prevent_settling(ctx: &Ctx) {
    // Hold paused time until the settle watchdog fires; release before runtime
    // shutdown so the test leaves no worker behind.
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (started, ready) = tokio::sync::oneshot::channel();
    tokio::task::spawn_blocking(move || {
        started.send(()).unwrap();
        let _ = wait.recv();
    });
    ready.await.unwrap();
    ctx.after_settle(move || {
        release.send(()).unwrap();
        Ok(())
    });
}

#[test]
fn a_stable_match_does_not_hide_an_unstable_calibration_side() {
    for expect in [Expect::Atomic, Expect::Completed, Expect::Undone] {
        let calls = AtomicUsize::new(0);
        let report = explore_observed(&config(), expect, |ctx: Ctx| {
            let call = calls.fetch_add(1, SeqCst);
            async move {
                ctx.target(tokio::task::yield_now()).await;
                // Baseline/trial/post agree; the two pre samples do not.
                Ok(if call == 2 { 1 } else { 0 })
            }
        });
        assert_eq!(report.trials[0].verdict, Some(Verdict::Unknown), "{report}");
        assert!(
            report.is_clean(),
            "uncertainty is not a violation: {report}"
        );
        assert!(!report.is_clean_within_coverage(), "{report}");
    }
}

#[test]
fn identical_calibrations_do_not_falsely_violate_undone() {
    let report = explore_observed(&config(), Expect::Undone, |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        Ok(0)
    });
    assert!(
        report.is_clean(),
        "the observation also equals pre: {report}"
    );
    assert_eq!(report.notes.len(), 1, "degeneracy must remain explicit");
}

#[test]
fn an_unreached_cut_is_not_judged_as_a_completed_effect() {
    let report = replay_observed(&config(), Expect::Undone, &plan(2), |ctx: Ctx| async move {
        Ok(ctx.target(tokio::task::yield_now()).await.is_some())
    })
    .unwrap();
    assert_eq!(report.trials[0].outcomes, [CutOutcome::Unrealized]);
    assert_eq!(report.trials[0].verdict, None, "{report}");
    assert!(report.is_clean(), "no cancellation happened: {report}");
    assert!(!report.is_clean_within_coverage());
}

#[test]
fn field_replay_does_not_judge_an_unreached_cut() {
    let report = replay_fields(
        &config(),
        FieldContract::new().effect("done", Expect::Undone),
        &plan(2),
        |ctx: Ctx| async move {
            Ok(Observation::new()
                .with("done", ctx.target(tokio::task::yield_now()).await.is_some()))
        },
    )
    .unwrap();
    assert_eq!(report.trials[0].outcomes, [CutOutcome::Unrealized]);
    assert!(report.trials[0].field_judgment.is_none(), "{report}");
    assert!(report.is_clean(), "{report}");
    assert!(!report.is_clean_within_coverage());
}

#[test]
fn search_does_not_judge_an_unreached_cut_in_either_observed_api() {
    for fields in [false, true] {
        let calls = AtomicUsize::new(0);
        let scenario = |ctx: Ctx| {
            let call = calls.fetch_add(1, SeqCst);
            async move {
                let done = ctx
                    .target(async {
                        // Only the baseline reaches a Pending boundary.
                        if call == 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .is_some();
                Ok(Observation::new().with("done", done))
            }
        };
        let report = if fields {
            explore_fields(
                &config(),
                FieldContract::new().effect("done", Expect::Undone),
                scenario,
            )
        } else {
            explore_observed(&config(), Expect::Undone, scenario)
        };
        assert_eq!(report.trials[0].outcomes, [CutOutcome::Unrealized]);
        assert!(report.trials[0].verdict.is_none(), "{report}");
        assert!(report.trials[0].field_judgment.is_none(), "{report}");
        assert_eq!(
            calls.load(SeqCst),
            2,
            "an unrealized trial needs no calibration"
        );
    }
}

#[test]
fn missing_calibration_targets_are_not_valid_samples() {
    for fields in [false, true] {
        let calls = AtomicUsize::new(0);
        let scenario = |ctx: Ctx| {
            let call = calls.fetch_add(1, SeqCst);
            async move {
                if call < 2 {
                    ctx.target(tokio::task::yield_now()).await;
                }
                Ok(Observation::new().with("state", 0))
            }
        };
        let report = if fields {
            replay_fields(
                &config(),
                FieldContract::new().state("state"),
                &plan(1),
                scenario,
            )
            .unwrap()
        } else {
            replay_observed(&config(), Expect::Atomic, &plan(1), scenario).unwrap()
        };
        assert!(!report.calibration_errors.is_empty(), "{report}");
        assert!(report.is_clean(), "{report}");
        assert!(!report.is_clean_within_coverage(), "{report}");
    }
}

#[test]
fn an_unsettled_calibration_is_not_a_valid_sample() {
    let config = Config {
        settle: Settle {
            watchdog: Duration::from_millis(50),
            ..Settle::default()
        },
        ..config()
    };
    for fields in [false, true] {
        let calls = AtomicUsize::new(0);
        let scenario = |ctx: Ctx| {
            let call = calls.fetch_add(1, SeqCst);
            async move {
                if call == 2 {
                    prevent_settling(&ctx).await;
                }
                ctx.target(tokio::task::yield_now()).await;
                Ok(Observation::new().with("state", 0))
            }
        };
        let report = if fields {
            replay_fields(
                &config,
                FieldContract::new().state("state"),
                &plan(1),
                scenario,
            )
            .unwrap()
        } else {
            replay_observed(&config, Expect::Atomic, &plan(1), scenario).unwrap()
        };
        assert!(
            report
                .calibration_errors
                .iter()
                .any(|e| e.contains("settle")),
            "{report}"
        );
        assert!(!report.is_clean_within_coverage(), "{report}");
    }
}

#[test]
fn an_unsettled_trial_is_not_given_a_calibrated_violation() {
    let config = Config {
        settle: Settle {
            watchdog: Duration::from_millis(50),
            ..Settle::default()
        },
        ..config()
    };
    for fields in [false, true] {
        let calls = AtomicUsize::new(0);
        let scenario = |ctx: Ctx| {
            let call = calls.fetch_add(1, SeqCst);
            async move {
                if call == 1 {
                    prevent_settling(&ctx).await;
                }
                Ok(Observation::new()
                    .with("done", ctx.target(tokio::task::yield_now()).await.is_some()))
            }
        };
        let report = if fields {
            replay_fields(
                &config,
                FieldContract::new().effect("done", Expect::Completed),
                &plan(1),
                scenario,
            )
            .unwrap()
        } else {
            replay_observed(&config, Expect::Completed, &plan(1), scenario).unwrap()
        };
        assert!(!report.trials[0].settled);
        assert!(report.trials[0].verdict.is_none(), "{report}");
        assert!(report.trials[0].field_judgment.is_none(), "{report}");
        assert!(report.is_clean(), "{report}");
        assert!(!report.is_clean_within_coverage());
        assert_eq!(calls.load(SeqCst), 2);
    }
}

#[test]
fn an_unapplied_gap_in_search_is_a_coverage_gap_in_every_api() {
    for api in 0..3 {
        let config = Config {
            gap_choices: vec![Gap::Yield(1)],
            ..config()
        };
        let scenario = |ctx: Ctx| async move {
            if ctx.target(tokio::task::yield_now()).await.is_some() {
                ctx.gap(Gap::Now).await;
            }
            Ok(Observation::new().with("state", 0))
        };
        let report = match api {
            0 => explore(&config, |ctx| async { scenario(ctx).await.map(|_| ()) }),
            1 => explore_observed(&config, Expect::Atomic, scenario),
            _ => explore_fields(&config, FieldContract::new().state("state"), scenario),
        };
        assert!(report.is_clean(), "{report}");
        assert!(!report.is_clean_within_coverage(), "{report}");
        assert!(
            report
                .coverage_gaps()
                .iter()
                .any(|gap| gap.contains("gap assignment")),
            "{report}"
        );
    }
}

#[test]
fn calibration_cannot_silently_skip_a_requested_gap() {
    let config = Config {
        gap_choices: vec![Gap::Yield(1)],
        ..config()
    };
    let report = explore_observed(&config, Expect::Atomic, |ctx: Ctx| async move {
        let entered = std::cell::Cell::new(false);
        ctx.target(async {
            entered.set(true);
            tokio::task::yield_now().await;
        })
        .await;
        if entered.get() {
            ctx.gap(Gap::Now).await;
        }
        Ok(0)
    });
    assert!(
        report
            .calibration_errors
            .iter()
            .any(|error| error.contains("gap")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

#[test]
fn excessive_mixed_calibration_is_reported_instead_of_allocated_or_overflowed() {
    for cuts in [13, 64] {
        let config = Config {
            max_cancellations: cuts,
            ..config()
        };
        let plan = Plan::new(
            (0..cuts)
                .map(|target| Cut {
                    target,
                    after_pending: 1,
                    race: Race::Immediate,
                })
                .collect(),
        );
        let calls = AtomicUsize::new(0);
        let report = replay_observed(&config, Expect::Atomic, &plan, |ctx: Ctx| {
            calls.fetch_add(1, SeqCst);
            async move {
                for _ in 0..cuts {
                    ctx.target(tokio::task::yield_now()).await;
                }
                Ok(0)
            }
        })
        .unwrap();
        assert_eq!(report.trials[0].verdict, Some(Verdict::Unknown), "{report}");
        assert!(!report.calibration_errors.is_empty(), "{report}");
        assert_eq!(
            calls.load(SeqCst),
            2,
            "do not enter the exponential calibration"
        );
    }
}

#[test]
fn bounded_gap_and_repeat_variants_preserve_the_search_prefix() {
    let config = Config {
        max_cancellations: 2,
        repeat_at_site: true,
        gap_choices: vec![Gap::Now, Gap::Yield(1)],
        ..config()
    };
    let scenario = |ctx: Ctx| async move {
        for _ in 0..2 {
            ctx.target(tokio::task::yield_now()).await;
            ctx.gap(Gap::Now).await;
        }
        Ok(())
    };
    let full = explore(&config, scenario);
    assert!(full.exhaustive);
    let expected: Vec<_> = full
        .trials
        .iter()
        .map(|trial| trial.replay_plan())
        .collect();
    for max_runs in 1..=expected.len() + 1 {
        let limited = explore(
            &Config {
                max_runs,
                ..config.clone()
            },
            scenario,
        );
        let actual: Vec<_> = limited
            .trials
            .iter()
            .map(|trial| trial.replay_plan())
            .collect();
        assert_eq!(actual, expected[..max_runs - 1], "budget {max_runs}");
        assert_eq!(limited.truncated, max_runs <= expected.len(), "{limited}");
    }
}

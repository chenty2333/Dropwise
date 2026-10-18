//! Tests for the observed API: allowed-set calibration, verdicts, the repeat
//! and gap dimensions, and the non-degenerate notes. Everything is in-process.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use dropwise::{
    assert_observed, explore, explore_observed, Config, Ctx, Expect, Gap, Race, Verdict,
};

fn immediate() -> Config {
    Config { races: vec![Race::Immediate], ..Config::default() }
}

/// A cancellation before the effect leaves the `pre` state, which `Atomic`
/// allows.
#[test]
fn atomic_accepts_the_pre_calibration() {
    let report = explore_observed(&immediate(), Expect::Atomic, move |ctx: Ctx| async move {
        let count = Arc::new(AtomicUsize::new(0));
        let inner = count.clone();
        ctx.target(async move {
            tokio::task::yield_now().await;
            inner.fetch_add(1, SeqCst);
        })
        .await;
        Ok(count.load(SeqCst))
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.trials.len(), 1, "{report}");
    assert_eq!(report.verdict_count(Verdict::Pre), 1, "{report}");
    assert!(report.trials.iter().all(|t| t.plan.iter().all(|c| c.after_pending > 0)));
}

/// A half-done state is neither calibration and is reported, with the observed
/// value and both baselines in the message.
#[test]
fn a_mixed_state_is_neither_calibration() {
    let report = explore_observed(&immediate(), Expect::Atomic, move |ctx: Ctx| async move {
        let count = Arc::new(AtomicUsize::new(0));
        let inner = count.clone();
        ctx.target(async move {
            inner.fetch_add(1, SeqCst);
            tokio::task::yield_now().await; // Pending #1: count = 1
            inner.fetch_add(1, SeqCst);
            tokio::task::yield_now().await; // Pending #2: count = 2
        })
        .await;
        Ok(count.load(SeqCst))
    });
    assert_eq!(report.trials.len(), 2, "{report}");
    assert_eq!(report.violations().count(), 1, "{report}");
    assert_eq!(report.verdict_count(Verdict::Neither), 1, "{report}");
    assert_eq!(report.verdict_count(Verdict::Post), 1, "{report}");
    let error = report.violations().next().unwrap().invariant.as_ref().unwrap_err();
    assert!(error.contains("Neither under Atomic"), "{error}");
    assert!(error.contains("observed 1"), "{error}");
    assert!(error.contains("pre=0"), "{error}");
    assert!(error.contains("post=2"), "{error}");
}

/// `Completed` rejects the not-started calibration that `Atomic` allows.
#[test]
fn completed_only_allows_the_post_calibration() {
    let report = explore_observed(&immediate(), Expect::Completed, move |ctx: Ctx| async move {
        let count = Arc::new(AtomicUsize::new(0));
        let inner = count.clone();
        ctx.target(async move {
            tokio::task::yield_now().await;
            inner.fetch_add(1, SeqCst);
        })
        .await;
        Ok(count.load(SeqCst))
    });
    assert_eq!(report.verdict_count(Verdict::Pre), 1, "{report}");
    assert_eq!(report.violations().count(), 1, "{report}");
    let error = report.violations().next().unwrap().invariant.as_ref().unwrap_err();
    assert!(error.contains("Pre under Completed"), "{error}");
}

/// `Undone` rejects a state that looks complete because the value was taken
/// out of the shared object but never reached the caller.
#[test]
fn undone_only_allows_the_pre_calibration() {
    let mut scenario = move |ctx: Ctx| async move {
        let queue = Arc::new(std::sync::Mutex::new(vec![7u32]));
        let inner = queue.clone();
        ctx.target(async move {
            let taken = inner.lock().unwrap().pop();
            tokio::task::yield_now().await;
            taken.ok_or_else(|| "empty queue".to_string())
        })
        .await;
        Ok(queue.lock().unwrap().len())
    };
    let atomic = explore_observed(&immediate(), Expect::Atomic, &mut scenario);
    assert!(atomic.is_clean(), "Atomic allows the completed-looking state:\n{atomic}");
    assert_eq!(atomic.verdict_count(Verdict::Post), 1, "{atomic}");

    let undone = explore_observed(&immediate(), Expect::Undone, &mut scenario);
    assert_eq!(undone.verdict_count(Verdict::Post), 1, "{undone}");
    assert_eq!(undone.violations().count(), 1, "{undone}");
    let error = undone.violations().next().unwrap().invariant.as_ref().unwrap_err();
    assert!(error.contains("Post under Undone"), "{error}");
}

/// The skip cut must drop the target before its first poll, so the `pre`
/// calibration sees `false` and is not degenerate.
#[test]
fn the_pre_calibration_does_not_poll_the_target() {
    let report = explore_observed(&immediate(), Expect::Atomic, move |ctx: Ctx| async move {
        let polled = Arc::new(AtomicBool::new(false));
        let inner = polled.clone();
        ctx.target(async move {
            inner.store(true, SeqCst);
            tokio::task::yield_now().await;
        })
        .await;
        Ok(polled.load(SeqCst))
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.verdict_count(Verdict::Post), 1, "{report}");
    assert!(report.notes.is_empty(), "pre was polled: {report}");
}

/// Equal `pre` and `post` are reported as a note, not as a failure, and are
/// classified as `Post`.
#[test]
fn equal_calibrations_get_a_note() {
    let report = explore_observed(&immediate(), Expect::Atomic, |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        Ok(42u32)
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.verdict_count(Verdict::Post), 1, "{report}");
    assert_eq!(report.notes.len(), 1, "{report}");
    assert!(report.notes[0].contains("pre and post observations are equal"), "{report}");
}

/// Two disagreeing calibration samples make the trial `Unknown`, never a
/// violation, and `assert_observed` still fails on it.
#[test]
fn unstable_calibration_is_unknown() {
    let flip = Arc::new(AtomicUsize::new(0));
    let scenario = move |ctx: Ctx| {
        let flip = flip.clone();
        async move {
            let n = flip.fetch_add(1, SeqCst);
            ctx.target(tokio::task::yield_now()).await;
            Ok(n % 2)
        }
    };
    let report = explore_observed(&immediate(), Expect::Atomic, scenario);
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.verdict_count(Verdict::Unknown), 1, "{report}");
    assert_eq!(report.unknown().count(), 1, "{report}");
    assert!(report.to_string().contains("cannot be decided"), "{report}");

    let flip = Arc::new(AtomicUsize::new(0));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_observed(Expect::Atomic, move |ctx: Ctx| {
            let flip = flip.clone();
            async move {
                let n = flip.fetch_add(1, SeqCst);
                ctx.target(tokio::task::yield_now()).await;
                Ok(n % 2)
            }
        });
    }));
    assert!(result.is_err(), "assert_observed must fail on Unknown");
}

/// A failing calibration run lands in `baseline_errors`; it must not panic.
#[test]
fn failing_calibration_is_a_baseline_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let report = explore_observed(&immediate(), Expect::Atomic, move |ctx: Ctx| {
        let calls = calls.clone();
        async move {
            if calls.fetch_add(1, SeqCst) >= 2 {
                return Err("calibration boom".to_string());
            }
            ctx.target(tokio::task::yield_now()).await;
            Ok(1u32)
        }
    });
    let errors = report.baseline_errors.join("\n");
    assert!(errors.contains("calibration pre"), "{report}");
    assert!(errors.contains("calibration boom"), "{report}");
}

/// A plan with several cuts calibrates "all skipped" as `pre` and "none
/// cancelled" as `post`.
#[test]
fn several_cuts_share_the_calibration() {
    let config = Config { max_cancellations: 2, ..immediate() };
    let report = explore_observed(&config, Expect::Atomic, move |ctx: Ctx| async move {
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let inner = count.clone();
            ctx.target(async move {
                tokio::task::yield_now().await;
                inner.fetch_add(1, SeqCst);
            })
            .await;
        }
        Ok(count.load(SeqCst))
    });
    // {first}, {second}, {first + second}
    assert_eq!(report.trials.len(), 3, "{report}");
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.verdict_count(Verdict::Pre), 3, "{report}");
    assert!(report.notes.is_empty(), "{report}");
}

/// Two cancelled operations may each look not started or completed. A pair
/// plan that leaves one of each is `Mixed`, allowed under `Atomic`, rather than
/// a violation.
#[test]
fn a_pair_plan_may_leave_one_operation_done_and_one_not() {
    let scenario = |ctx: Ctx| async move {
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let inner = first.clone();
        ctx.target(async move {
            inner.store(true, SeqCst); // committed before its only Pending
            tokio::task::yield_now().await;
        })
        .await;
        let inner = second.clone();
        ctx.target(async move {
            tokio::task::yield_now().await;
            inner.store(true, SeqCst); // committed after its only Pending
        })
        .await;
        Ok((first.load(SeqCst), second.load(SeqCst)))
    };
    let config = Config { max_cancellations: 2, ..immediate() };
    let report = explore_observed(&config, Expect::Atomic, scenario);
    assert!(report.is_clean(), "{report}");
    let pair = report.trials.iter().find(|t| t.plan.len() == 2).expect("pair plan");
    assert_eq!(pair.verdict, Some(Verdict::Mixed), "{report}");

    // `Completed` does not allow it.
    let report = explore_observed(&config, Expect::Completed, scenario);
    let pair = report.trials.iter().find(|t| t.plan.len() == 2).expect("pair plan");
    assert!(pair.is_violation(), "{report}");
}

/// `repeat_at_site` adds one plan per eligible single cut; the plan cancels
/// every later instance at the same call site and records how many.
#[test]
fn repeat_at_site_cancels_later_targets_at_the_same_site() {
    let scenario = |ctx: Ctx| async move {
        for _ in 0..3 {
            ctx.target(async {
                tokio::task::yield_now().await;
            })
            .await;
        }
        Ok(())
    };
    let off = explore(&immediate(), scenario);
    assert_eq!(off.trials.len(), 3, "{off}");

    let config = Config { repeat_at_site: true, ..immediate() };
    let report = explore(&config, scenario);
    // Three single cuts, plus a repeated plan for targets #0 and #1 (target #2
    // has no later target at the same site).
    assert_eq!(report.trials.len(), 5, "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.cancelled == 3).count(), 1, "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.cancelled == 2).count(), 1, "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.cancelled == 1).count(), 3, "{report}");
    for trial in report.trials.iter().filter(|t| t.cancelled > 1) {
        assert_eq!(trial.plan.len(), 1, "the repeated instances are not separate cuts: {report}");
    }
}

/// `gap_choices` adds one plan per gap site and choice; the substitution is
/// recorded on the trial and used by its calibration.
#[test]
fn gap_choices_are_explored_and_recorded() {
    let sleep = Duration::from_millis(20);
    let config = Config {
        gap_choices: vec![Gap::Now, Gap::Yield(1), Gap::Sleep(sleep)],
        ..immediate()
    };
    let report = explore_observed(&config, Expect::Atomic, |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        let start = tokio::time::Instant::now();
        ctx.gap(Gap::Sleep(Duration::from_millis(10))).await;
        Ok(start.elapsed() >= Duration::from_millis(10))
    });
    // The unmodified plan plus three substitutions of gap #0.
    assert_eq!(report.trials.len(), 4, "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.gaps.is_empty()).count(), 1, "{report}");
    assert_eq!(
        report
            .trials
            .iter()
            .filter(|t| t.gaps.as_slice() == [Some(Gap::Sleep(sleep))])
            .count(),
        1,
        "{report}"
    );
    assert!(report.exhaustive, "{report}");

    let plain = explore(&immediate(), |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        ctx.gap(Gap::Sleep(Duration::from_millis(10))).await;
        Ok(())
    });
    assert_eq!(plain.trials.len(), 1, "gap_choices is off by default: {plain}");
}

/// A gap choice equal to the site's default would repeat the unmodified plan,
/// so it is not explored again.
#[test]
fn a_gap_choice_equal_to_the_default_is_not_repeated() {
    let default = Gap::Sleep(Duration::from_millis(10));
    let config = Config { gap_choices: vec![Gap::Now, default], ..immediate() };
    let report = explore(&config, |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        ctx.gap(default).await;
        Ok(())
    });
    // The unmodified plan plus the `Now` substitution.
    assert_eq!(report.trials.len(), 2, "{report}");
    assert!(report.trials.iter().all(|t| t.gaps.iter().all(|g| *g != Some(default))), "{report}");
}

/// `assert_observed` accepts a clean scenario.
#[test]
fn assert_observed_accepts_a_clean_scenario() {
    assert_observed(Expect::Atomic, |ctx: Ctx| async move {
        ctx.target(tokio::task::yield_now()).await;
        Ok(0u32)
    });
}

/// Calibrating a `ctx.race` target skips it without polling the inner future:
/// the competitor is ready at once and the race branch never runs.
#[test]
fn the_pre_calibration_skips_race_targets_without_polling() {
    let report = explore_observed(&immediate(), Expect::Atomic, |ctx: Ctx| async move {
        let polled = Arc::new(AtomicBool::new(false));
        let inner = polled.clone();
        let (op, mut preempt) = ctx.race(async move {
            inner.store(true, SeqCst);
            tokio::task::yield_now().await;
            1u32
        });
        tokio::select! {
            _ = op => {}
            _ = &mut preempt => {}
        }
        Ok(polled.load(SeqCst))
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.verdict_count(Verdict::Post), 1, "{report}");
    assert!(report.notes.is_empty(), "the pre calibration polled the target: {report}");
}

/// A repeated plan calibrates "every instance skipped" as `pre` and "nothing
/// cancelled" as `post`, using the instance set the trial actually cancelled.
#[test]
fn repeat_at_site_is_calibrated() {
    let config = Config { repeat_at_site: true, ..immediate() };
    let report = explore_observed(&config, Expect::Atomic, |ctx: Ctx| async move {
        for _ in 0..3 {
            ctx.target(async {
                tokio::task::yield_now().await;
            })
            .await;
        }
        Ok(0u8)
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.trials.len(), 5, "{report}");
    assert!(report.trials.iter().all(|t| t.verdict.is_some()), "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.cancelled == 3).count(), 1, "{report}");
    assert_eq!(report.trials.iter().filter(|t| t.cancelled == 2).count(), 1, "{report}");
}

/// Calibration is computed once per (target set, gap assignment) and reused by
/// plans that differ only in `after_pending` or `race`.
#[test]
fn calibration_is_cached_across_plans() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let report = explore_observed(&immediate(), Expect::Atomic, move |ctx: Ctx| {
        let c = c.clone();
        async move {
            c.fetch_add(1, SeqCst);
            ctx.target(async {
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
            })
            .await;
            Ok(0u32)
        }
    });
    assert_eq!(report.trials.len(), 2, "{report}");
    // One baseline, two trials, two pre samples and two post samples: the
    // second trial reuses the first trial's calibration.
    assert_eq!(calls.load(SeqCst), 7, "{report}");
}

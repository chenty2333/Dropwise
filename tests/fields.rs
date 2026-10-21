//! Field-contract integration: the same scenarios judged by the single blob and
//! by named fields.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dropwise::fields::FieldStatus;
use dropwise::{
    Config, Ctx, Expect, FieldContract, Invariant, Observation, Outcome, Race, explore_fields,
    explore_observed, replay_fields,
};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

/// Two effects inside one target: `a` happens before the first suspension,
/// `b` after it.  A cut after the first `Pending` leaves `a` done and `b` not.
fn split_effects(
    ctx: Ctx,
) -> impl std::future::Future<Output = Result<Observation, String>> + Send + 'static {
    let a = Arc::new(AtomicBool::new(false));
    let b = Arc::new(AtomicBool::new(false));
    let (task_a, task_b) = (a.clone(), b.clone());
    async move {
        let target = ctx.target(async move {
            task_a.store(true, Ordering::SeqCst);
            tokio::task::yield_now().await;
            task_b.store(true, Ordering::SeqCst);
        });
        let _ = target.await;
        Ok(Observation::new()
            .with("a", a.load(Ordering::SeqCst))
            .with("b", b.load(Ordering::SeqCst)))
    }
}

/// A target that exposes work in progress to a state field: `dirty` is set
/// before the suspension and cleared after it.
fn leaky_state(
    ctx: Ctx,
) -> impl std::future::Future<Output = Result<Observation, String>> + Send + 'static {
    let dirty = Arc::new(AtomicBool::new(false));
    let task_dirty = dirty.clone();
    async move {
        let target = ctx.target(async move {
            task_dirty.store(true, Ordering::SeqCst);
            tokio::task::yield_now().await;
            task_dirty.store(false, Ordering::SeqCst);
        });
        let visible = target.await.is_some();
        Ok(Observation::new()
            .with("target.visible", visible)
            .with("client.clean", !dirty.load(Ordering::SeqCst)))
    }
}

/// The same execution is a violation under the single-blob `Expect::Atomic`
/// (the pair is neither pre nor post) but is allowed field by field: `a` kept
/// its post value while `b` kept its pre value, and the caller declared both
/// effects atomic.  No mixed calibration run is needed for this.
#[test]
fn per_field_atomic_accepts_what_the_blob_calls_neither() {
    let legacy = explore_observed(&config(), Expect::Atomic, split_effects);
    assert!(legacy.trials.iter().any(|t| t.is_violation()));
    assert!(!legacy.is_clean());

    let contract = FieldContract::new()
        .effect("a", Expect::Atomic)
        .effect("b", Expect::Atomic);
    let report = explore_fields(&config(), contract, split_effects);

    assert!(report.violations().next().is_none());
    assert!(report.calibration_errors.is_empty(), "{report}");
    assert!(report.is_clean_within_coverage(), "{report}");
    let mixed = report
        .trials
        .iter()
        .filter_map(|trial| trial.field_judgment.as_ref())
        .find(|judgment| {
            judgment
                .fields
                .iter()
                .any(|f| f.path == "a" && f.status == FieldStatus::Post)
                && judgment
                    .fields
                    .iter()
                    .any(|f| f.path == "b" && f.status == FieldStatus::Pre)
        });
    assert!(mixed.is_some(), "no per-field mixture was judged");
}

/// The single-blob run is clean because `target.visible` moves, but the
/// required `observer.rows` effect never moved: the field contract reports a
/// coverage gap instead of a pass.
#[test]
fn a_clean_blob_pass_can_hide_a_degenerate_required_effect() {
    let scenario = |ctx: Ctx| async move {
        let target = ctx.target(async {
            tokio::task::yield_now().await;
            true
        });
        let visible = target.await.unwrap_or(false);
        Ok(Observation::new()
            .with("target.visible", visible)
            .with("observer.rows", 0))
    };

    let legacy = explore_observed(&config(), Expect::Atomic, scenario);
    assert!(legacy.is_clean_within_coverage(), "{legacy}");

    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .effect("observer.rows", Expect::Atomic);
    let report = explore_fields(&config(), contract, scenario);

    assert!(report.violations().next().is_none(), "{report}");
    assert!(
        report
            .field_coverage_gaps()
            .iter()
            .any(|gap| gap.contains("observer.rows") && gap.contains("degenerate")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

/// A state field can betray a leak that an effect field justifies away: the
/// effect is in its allowed pre state while the handle is not clean.
#[test]
fn a_state_field_violation_is_reported() {
    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .state("client.clean");
    let report = explore_fields(&config(), contract, leaky_state);

    assert!(report.violations().next().is_some(), "{report}");
    assert!(!report.is_clean_within_coverage());
    let trial = report
        .trials
        .iter()
        .find(|trial| trial.is_violation())
        .expect("a violating trial");
    let judgment = trial.field_judgment.as_ref().unwrap();
    let clean = judgment
        .fields
        .iter()
        .find(|field| field.path == "client.clean")
        .unwrap();
    assert_eq!(clean.status, FieldStatus::Violated);
    assert!(judgment.coverage_complete());
}

/// Per-field expectations allow the mixture; a declared cross-field relation
/// says the two effects are one atomic effect and rejects it.
#[test]
fn a_cross_field_invariant_rejects_a_mixture_the_fields_allow() {
    let per_field = FieldContract::new()
        .effect("a", Expect::Atomic)
        .effect("b", Expect::Atomic);
    let report = explore_fields(&config(), per_field, split_effects);
    assert!(report.violations().next().is_none(), "{report}");
    assert!(report.is_clean_within_coverage(), "{report}");

    let correlated = FieldContract::new()
        .effect("a", Expect::Atomic)
        .effect("b", Expect::Atomic)
        .invariant(Invariant::all_or_none(["a", "b"]));
    let report = explore_fields(&config(), correlated, split_effects);
    assert!(report.violations().next().is_some(), "{report}");
    assert!(!report.is_clean_within_coverage(), "{report}");
    assert!(
        report.violations().any(
            |trial| trial.field_judgment.as_ref().is_some_and(|judgment| {
                judgment
                    .invariants
                    .iter()
                    .any(|invariant| invariant.result.is_violation())
            })
        ),
        "{report}"
    );
}

/// A declared effect that the scenario never produces is a contract/baseline
/// problem and an uncovered field, never a silent pass.
#[test]
fn a_declared_field_missing_from_the_baseline_is_a_coverage_gap() {
    let scenario = |ctx: Ctx| async move {
        let target = ctx.target(async {
            tokio::task::yield_now().await;
            true
        });
        let visible = target.await.unwrap_or(false);
        Ok(Observation::new().with("target.visible", visible))
    };
    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .effect("observer.rows", Expect::Atomic);
    let report = explore_fields(&config(), contract, scenario);

    assert!(report.violations().next().is_none(), "{report}");
    assert!(
        report
            .contract_errors
            .iter()
            .any(|error| error.contains("observer.rows") && error.contains("baseline")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

/// A strict contract turns a forgotten field into an explicit coverage gap.
#[test]
fn a_strict_contract_flags_an_undeclared_observed_field() {
    let scenario = |ctx: Ctx| async move {
        let target = ctx.target(async {
            tokio::task::yield_now().await;
            true
        });
        let visible = target.await.unwrap_or(false);
        Ok(Observation::new()
            .with("target.visible", visible)
            .with("extra.effect", 1))
    };
    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .strict();
    let report = explore_fields(&config(), contract, scenario);

    assert!(
        report
            .contract_errors
            .iter()
            .any(|error| error.contains("extra.effect") && error.contains("not declared")),
        "{report}"
    );
    assert!(!report.is_clean_within_coverage(), "{report}");
}

/// A malformed contract is reported, not run, and cannot be mistaken for a pass.
#[test]
fn an_invalid_contract_is_reported_without_running_the_scenario() {
    let report = explore_fields(&config(), FieldContract::new(), split_effects);
    assert!(report.trials.is_empty());
    assert!(!report.contract_errors.is_empty());
    assert!(!report.is_clean_within_coverage());
    assert!(report.to_string().contains("contract/coverage problem"));
}

/// A fully covered normal execution is clean under the field contract too.
#[test]
fn a_covered_normal_execution_is_clean() {
    let scenario = |ctx: Ctx| async move {
        let target = ctx.target(async {
            tokio::task::yield_now().await;
            true
        });
        let visible = target.await.unwrap_or(false);
        Ok(Observation::new()
            .with("target.visible", visible)
            .with("client.clean", true))
    };
    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .state("client.clean");
    let report = explore_fields(&config(), contract, scenario);
    assert!(report.violations().next().is_none(), "{report}");
    assert!(report.field_coverage_gaps().is_empty(), "{report}");
    assert!(report.is_clean_within_coverage(), "{report}");
}

/// Replaying a judged plan re-calibrates and re-judges the fields.
#[test]
fn field_judgment_can_be_replayed_from_a_trial_plan() {
    let contract = FieldContract::new()
        .effect("target.visible", Expect::Atomic)
        .state("client.clean");
    let report = explore_fields(&config(), contract.clone(), leaky_state);
    let trial = report.trials.first().expect("one plan");
    let plan = trial.replay_plan();
    let replay = replay_fields(&config(), contract, &plan, leaky_state).expect("valid plan");
    let replayed = replay.trials.first().expect("a replayed trial");
    assert!(replayed.is_violation(), "{replay}");
    let judgment = replayed.field_judgment.as_ref().unwrap();
    assert!(
        judgment
            .fields
            .iter()
            .any(|field| field.path == "client.clean" && field.status == FieldStatus::Violated)
    );
}

/// `exactly_one` is a usable cross-field policy: it rejects the mix but accepts
/// a run where exactly one of the two effects completed.
#[test]
fn exactly_one_accepts_a_single_completed_effect() {
    let contract = FieldContract::new()
        .effect("a", Expect::Atomic)
        .effect("b", Expect::Atomic)
        .invariant(Invariant::exactly_one(["a", "b"], Outcome::Post));
    let report = explore_fields(&config(), contract, split_effects);
    // The cut leaves a=post, b=pre: exactly one effect completed.
    assert!(report.violations().next().is_none(), "{report}");
    assert!(report.is_clean_within_coverage(), "{report}");
}

//! Shared reporting for the cancellation-ablation ladder (RESCAN.md, section 6.7).
//!
//! Every level uses the same scenario skeleton and `Config`; only what runs
//! after the cancelled operation changes. B1 stops after the cancellation and
//! is judged by [`dropwise::explore`] (panic, liveness, obligation leak). B2
//! retries the same operation with the same arguments, B3 runs a different
//! operation of the same kind and checks only `Ok`, and M is the existing
//! `explore_observed(Expect::Atomic)` survivor plus independent observer.
//!
//! `simple` and `observed` print one `LADDER` line per experiment, followed by
//! one `LADDER-VIOLATION` line per violating plan. The latter prints the plan
//! as `#target@pending:race` so M and B3 can be compared plan by plan.

use dropwise::{Report, Trial, Verdict};

/// The version label used by the scan scripts: `SCAN_VARIANT` for the client
/// scans, `BB8_VARIANT` for bb8, and `release` if neither is set.
pub fn variant() -> String {
    std::env::var("SCAN_VARIANT")
        .or_else(|_| std::env::var("BB8_VARIANT"))
        .unwrap_or_else(|_| "release".into())
}

/// One `LADDER` line for a level judged only by `dropwise::explore`.
pub fn simple(level: &str, id: &str, report: &Report) {
    let variant = variant();
    println!(
        "LADDER {level} {id} [{variant}]: plans={} violations={} unrealized={} unsettled={} \
         baseline_errors={} exhaustive={} baseline_targets={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.exhaustive,
        report.baseline_targets,
    );
    errors(level, id, &variant, report);
    violations(level, id, &variant, report);
}

/// One `LADDER` line for the full template (`explore_observed`).
pub fn observed(level: &str, id: &str, report: &Report) {
    let variant = variant();
    println!(
        "LADDER {level} {id} [{variant}]: plans={} violations={} unrealized={} unsettled={} \
         baseline_errors={} exhaustive={} baseline_targets={} pre={} post={} mixed={} \
         neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.exhaustive,
        report.baseline_targets,
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    );
    errors(level, id, &variant, report);
    violations(level, id, &variant, report);
}

fn errors(level: &str, id: &str, variant: &str, report: &Report) {
    for error in &report.baseline_errors {
        println!("LADDER-ERROR {level} {id} [{variant}]: {error}");
    }
}

fn violations(level: &str, id: &str, variant: &str, report: &Report) {
    for trial in report.violations() {
        let reason = match &trial.invariant {
            Err(error) => error.as_str(),
            Ok(()) => "obligation leak",
        };
        println!(
            "LADDER-VIOLATION {level} {id} [{variant}] plan={} cancelled={} verdict={:?} detail={reason}",
            plan(trial),
            trial.cancelled,
            trial.verdict,
        );
    }
}

/// The plan of a trial as `#target@pending:race`, joined with `+` for plans
/// with more than one cut.
pub fn plan(trial: &Trial) -> String {
    trial
        .plan
        .iter()
        .map(|cut| format!("#{}@{}:{:?}", cut.target, cut.after_pending, cut.race))
        .collect::<Vec<_>>()
        .join("+")
}

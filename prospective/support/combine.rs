//! Experiment-local generation of "one cancelled target + one successor" cases.
//!
//! This is research support, not Dropwise API and not a scenario framework. An
//! adapter registers
//!
//! * cancellation targets and successor operations (ordinary Rust types with a
//!   `Display` name),
//! * the business object and payload marker each one touches ([`Shape`]),
//! * how a successor is realized for a requested object/payload relation,
//! * a pre-execution applicability check with a reason, the [`Expect`]
//!   contract, and one scenario constructor.
//!
//! The shared layer does the Cartesian loop over target x successor x
//! object-relation x payload-relation, collapses dimensions that the shapes
//! cannot compare, applies the adapter's applicability check, caps the number
//! of executable cases, calls [`explore_observed`] once per case with that
//! case's own scenario/calibration, and keeps the case description next to the
//! existing [`Report`]. Replay takes the same case description plus a normal
//! [`Plan`] and calls [`replay_observed`].
//!
//! What this deliberately does not do: infer contracts, normalize runtime
//! values, judge prefix/subset observations, search recovery sequences, or let
//! an adapter hand over a ready-made list of final scenarios. The adapter
//! declares capabilities and parameter realization; the loop above is here.
#![allow(dead_code)]

use std::fmt::{self, Display};
use std::future::Future;
use std::pin::Pin;

use dropwise::{
    Config, Ctx, Expect, Plan, ReplayError, Report, explore_observed, replay_observed,
};

/// A future a scenario constructor can own without borrowing the adapter.
pub type CaseFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

/// One comparable business identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Domain of the value, e.g. `bolt/node` or `mysql/row-value`. Relations
    /// are only expanded when both sides carry the same kind.
    pub kind: &'static str,
    /// Logical value used for the relation. Adapters should use a stable name
    /// (an object role), not a per-run id.
    pub value: String,
}

impl Slot {
    pub fn new(kind: &'static str, value: impl Into<String>) -> Self {
        Self { kind, value: value.into() }
    }
}

/// What an operation touches. `None` means the dimension does not exist for
/// that operation; it is not an empty value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    pub object: Option<Slot>,
    pub payload: Option<Slot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRelation {
    Same,
    Different,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadRelation {
    Same,
    Different,
}

impl ObjectRelation {
    fn label(self) -> &'static str {
        match self {
            ObjectRelation::Same => "same",
            ObjectRelation::Different => "different",
        }
    }
}

impl PayloadRelation {
    fn label(self) -> &'static str {
        match self {
            PayloadRelation::Same => "same",
            PayloadRelation::Different => "different",
        }
    }
}

/// One generated combination: a target, the successor realized for this
/// combination's relations, and the relations themselves. A `None` relation
/// means the capability comparison did not apply, not that it was skipped
/// after execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case<T, S> {
    pub target: T,
    pub successor: S,
    pub object: Option<ObjectRelation>,
    pub payload: Option<PayloadRelation>,
}

impl<T: Display, S: Display> Case<T, S> {
    pub fn label(&self) -> String {
        let object = self.object.map(ObjectRelation::label).unwrap_or("n/a");
        let payload = self.payload.map(PayloadRelation::label).unwrap_or("n/a");
        format!(
            "target={} -> successor={} [object={object}, payload={payload}]",
            self.target, self.successor
        )
    }
}

/// A case the adapter refused before execution, or a relation the generator
/// could not realize/verify. It is never counted as run or passing.
#[derive(Debug, Clone)]
pub struct SkippedCase<T, S> {
    pub case: Case<T, S>,
    pub reason: String,
}

/// The stable, complete candidate list before execution.
#[derive(Debug, Clone)]
pub struct CasePlan<T, S> {
    /// Applicable cases that fit the limit, in generation order.
    pub cases: Vec<Case<T, S>>,
    /// Cases refused before execution, with the adapter/generator reason.
    pub skipped: Vec<SkippedCase<T, S>>,
    /// Applicable cases beyond the execution limit. Not run, not pass.
    pub unrun: Vec<Case<T, S>>,
    /// Why a relation dimension was not expanded, and other generation notes.
    pub notes: Vec<String>,
    pub limit: usize,
}

impl<T: Display, S: Display> CasePlan<T, S> {
    pub fn applicable(&self) -> usize {
        self.cases.len() + self.unrun.len()
    }

    pub fn is_complete(&self) -> bool {
        self.unrun.is_empty()
    }
}

/// The adapter side of the generator. Each client keeps its own operation,
/// result and observation types; only the loop and its report types are shared.
pub trait Adapter {
    type Target: Clone + Display;
    type Successor: Clone + Display;
    type Obs: PartialEq + fmt::Debug + Send + 'static;

    /// Registered cancellation targets, in stable generation order.
    fn targets(&self) -> Vec<Self::Target>;
    /// Registered successor operations, in stable generation order.
    fn successors(&self) -> Vec<Self::Successor>;

    /// What the target touches. Call it on a already-concrete target.
    fn target_shape(&self, target: &Self::Target) -> Shape;
    /// What the successor's base form touches. Fixed steps inside a successor
    /// are not dimensions; report the one parameter this operation wants
    /// compared, or `None`.
    fn successor_shape(&self, successor: &Self::Successor) -> Shape;

    /// Realize `successor` for the relations chosen by the shared layer.
    ///
    /// The shared layer only asks for relations whose kinds match. `Err` is a
    /// pre-execution capability refusal and is recorded with its reason.
    /// `realize` must actually change the parameter (e.g. the object role or
    /// payload value) when the relation says so; the shared layer verifies the
    /// realized shape and records a mismatch instead of running it.
    fn realize(
        &self,
        target: &Self::Target,
        successor: &Self::Successor,
        object: Option<ObjectRelation>,
        payload: Option<PayloadRelation>,
    ) -> Result<Self::Successor, String>;

    /// Adapter-owned contract check that is not expressible in [`Shape`].
    /// `Err` is recorded as "inapplicable" with the reason before execution.
    fn applicable(&self, case: &Case<Self::Target, Self::Successor>) -> Result<(), String> {
        let _ = case;
        Ok(())
    }

    /// The Dropwise contract for this case. It is declared, never inferred.
    fn expect(&self, case: &Case<Self::Target, Self::Successor>) -> Expect;

    /// Construct and run the whole scenario for one case.
    ///
    /// This is called by the engine for the uncancelled baseline, both
    /// calibration sides and every cancellation trial. It must therefore run
    /// the successor in all of those runs, not only after a cancellation, and
    /// must rebuild isolated state on every call. Return `Err` only for a
    /// setup/scenario failure; successor errors belong in the observation.
    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs>;
}

/// Generate the candidate cases without executing anything.
///
/// Order is target registration order, successor registration order, then
/// `Same`/`Different` for object and payload. The first `limit` applicable
/// cases are returned in [`CasePlan::cases`]; the rest are in
/// [`CasePlan::unrun`].
pub fn plan_cases<A: Adapter>(adapter: &A, limit: usize) -> CasePlan<A::Target, A::Successor> {
    let mut plan = CasePlan {
        cases: Vec::new(),
        skipped: Vec::new(),
        unrun: Vec::new(),
        notes: Vec::new(),
        limit,
    };
    let targets = adapter.targets();
    let successors = adapter.successors();
    if targets.is_empty() {
        plan.notes.push("no cancellation target registered".into());
    }
    if successors.is_empty() {
        plan.notes.push("no successor registered".into());
    }

    for target in &targets {
        let target_shape = adapter.target_shape(target);
        for successor in &successors {
            let base_shape = adapter.successor_shape(successor);
            let (objects, object_note) = object_options(&target_shape, &base_shape);
            let (payloads, payload_note) = payload_options(&target_shape, &base_shape);
            if let Some(note) = object_note {
                push_note(&mut plan.notes, note);
            }
            if let Some(note) = payload_note {
                push_note(&mut plan.notes, note);
            }

            for &object in &objects {
                for &payload in &payloads {
                    let requested = Case {
                        target: target.clone(),
                        successor: successor.clone(),
                        object,
                        payload,
                    };
                    let realized = match adapter.realize(target, successor, object, payload) {
                        Ok(realized) => realized,
                        Err(reason) => {
                            plan.skipped.push(SkippedCase {
                                case: requested,
                                reason: format!("capability: {reason}"),
                            });
                            continue;
                        }
                    };
                    let case = Case {
                        target: target.clone(),
                        successor: realized,
                        object,
                        payload,
                    };
                    let realized_shape = adapter.successor_shape(&case.successor);
                    if let Err(reason) =
                        check_relation(&target_shape, &realized_shape, object, payload)
                    {
                        plan.skipped.push(SkippedCase {
                            case,
                            reason: format!("generation: {reason}"),
                        });
                        continue;
                    }
                    match adapter.applicable(&case) {
                        Ok(()) if plan.cases.len() < limit => plan.cases.push(case),
                        Ok(()) => plan.unrun.push(case),
                        Err(reason) => plan.skipped.push(SkippedCase {
                            case,
                            reason: format!("inapplicable: {reason}"),
                        }),
                    }
                }
            }
        }
    }
    plan
}

fn object_options(
    target: &Shape,
    successor: &Shape,
) -> (Vec<Option<ObjectRelation>>, Option<String>) {
    match (&target.object, &successor.object) {
        (Some(t), Some(s)) if t.kind == s.kind => (
            vec![Some(ObjectRelation::Same), Some(ObjectRelation::Different)],
            None,
        ),
        (Some(t), Some(s)) => (
            vec![None],
            Some(format!(
                "object relation not expanded: target kind {} != successor kind {}",
                t.kind, s.kind
            )),
        ),
        (None, Some(s)) => (
            vec![None],
            Some(format!(
                "object relation not expanded: target has no business object; successor kind {}",
                s.kind
            )),
        ),
        (Some(t), None) => (
            vec![None],
            Some(format!(
                "object relation not expanded: successor has no business object; target kind {}",
                t.kind
            )),
        ),
        (None, None) => (vec![None], None),
    }
}

fn payload_options(
    target: &Shape,
    successor: &Shape,
) -> (Vec<Option<PayloadRelation>>, Option<String>) {
    match (&target.payload, &successor.payload) {
        (Some(t), Some(s)) if t.kind == s.kind => (
            vec![Some(PayloadRelation::Same), Some(PayloadRelation::Different)],
            None,
        ),
        (Some(t), Some(s)) => (
            vec![None],
            Some(format!(
                "payload relation not expanded: target kind {} != successor kind {}",
                t.kind, s.kind
            )),
        ),
        (None, Some(s)) => (
            vec![None],
            Some(format!(
                "payload relation not expanded: target has no payload; successor kind {}",
                s.kind
            )),
        ),
        (Some(t), None) => (
            vec![None],
            Some(format!(
                "payload relation not expanded: successor has no payload; target kind {}",
                t.kind
            )),
        ),
        (None, None) => (vec![None], None),
    }
}

fn push_note(notes: &mut Vec<String>, note: String) {
    if !notes.contains(&note) {
        notes.push(note);
    }
}

fn check_relation(
    target: &Shape,
    successor: &Shape,
    object: Option<ObjectRelation>,
    payload: Option<PayloadRelation>,
) -> Result<(), String> {
    if let Some(relation) = object {
        let (Some(t), Some(s)) = (&target.object, &successor.object) else {
            return Err("object relation requested but a realized shape has no object".into());
        };
        if t.kind != s.kind {
            return Err(format!(
                "object relation realized with different kinds: {} vs {}",
                t.kind, s.kind
            ));
        }
        match relation {
            ObjectRelation::Same if t.value != s.value => {
                return Err(format!(
                    "same-object requested but realized {} != {}",
                    t.value, s.value
                ));
            }
            ObjectRelation::Different if t.value == s.value => {
                return Err(format!(
                    "different-object requested but realized both {}",
                    t.value
                ));
            }
            _ => {}
        }
    }
    if let Some(relation) = payload {
        let (Some(t), Some(s)) = (&target.payload, &successor.payload) else {
            return Err("payload relation requested but a realized shape has no payload".into());
        };
        if t.kind != s.kind {
            return Err(format!(
                "payload relation realized with different kinds: {} vs {}",
                t.kind, s.kind
            ));
        }
        match relation {
            PayloadRelation::Same if t.value != s.value => {
                return Err(format!(
                    "same-payload requested but realized {} != {}",
                    t.value, s.value
                ));
            }
            PayloadRelation::Different if t.value == s.value => {
                return Err(format!(
                    "different-payload requested but realized both {}",
                    t.value
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// One case's existing Dropwise report, kept together with its description.
#[derive(Debug, Clone)]
pub struct CaseRun<T, S> {
    pub case: Case<T, S>,
    pub expect: Expect,
    pub report: Report,
}

impl<T: Display, S: Display> CaseRun<T, S> {
    pub fn label(&self) -> String {
        self.case.label()
    }

    /// The first violating plan, if any, ready for [`replay_case`].
    pub fn first_violation_plan(&self) -> Option<Plan> {
        self.report.violations().next().map(|trial| trial.replay_plan())
    }

    /// One line with the case description and the existing report's counts.
    pub fn summary(&self) -> String {
        format!(
            "CASE {} expect={:?} plans={} violations={} unrealized={} unsettled={} \
             baseline_errors={} calibration_errors={} exhaustive={} truncated={} \
             pre={} post={} mixed={} neither={} unknown={} gaps={}",
            self.label(),
            self.expect,
            self.report.trials.len(),
            self.report.violations().count(),
            self.report.unrealized().count(),
            self.report.unsettled().count(),
            self.report.baseline_errors.len(),
            self.report.calibration_errors.len(),
            self.report.exhaustive,
            self.report.truncated,
            self.report.verdict_count(dropwise::Verdict::Pre),
            self.report.verdict_count(dropwise::Verdict::Post),
            self.report.verdict_count(dropwise::Verdict::Mixed),
            self.report.verdict_count(dropwise::Verdict::Neither),
            self.report.verdict_count(dropwise::Verdict::Unknown),
            self.report.coverage_gaps().len(),
        )
    }

    /// Case description plus the engine's full diagnostics.
    pub fn diagnostics(&self) -> String {
        format!("CASE {}\n{}", self.label(), self.report.diagnostics())
    }
}

/// The plan plus the results of the cases that actually ran. `plan.unrun` and
/// `plan.skipped` are never in `results`.
#[derive(Debug, Clone)]
pub struct CaseRuns<T, S> {
    pub plan: CasePlan<T, S>,
    pub results: Vec<CaseRun<T, S>>,
}

impl<T: Display, S: Display> CaseRuns<T, S> {
    /// Print generation notes, exclusions, unrun cases and every report.
    /// Nothing is asserted: errors, gaps and violations are printed as results.
    pub fn print(&self) {
        println!(
            "CASE-GEN cases={} skipped={} unrun={} applicable={} limit={}",
            self.results.len(),
            self.plan.skipped.len(),
            self.plan.unrun.len(),
            self.plan.applicable(),
            self.plan.limit,
        );
        for note in &self.plan.notes {
            println!("CASE-NOTE {note}");
        }
        for skipped in &self.plan.skipped {
            println!("CASE-SKIP {} reason={}", skipped.case.label(), skipped.reason);
        }
        for case in &self.plan.unrun {
            println!(
                "CASE-UNRUN {} reason=limit {}",
                case.label(),
                self.plan.limit
            );
        }
        for run in &self.results {
            println!("{}", run.summary());
            for trial in run.report.violations() {
                println!(
                    "CASE-VIOLATION {} plan={} cancelled={} verdict={:?} detail={:?}",
                    run.label(),
                    trial.plan_label(),
                    trial.cancelled,
                    trial.verdict,
                    trial.invariant
                );
            }
            for error in &run.report.baseline_errors {
                println!("CASE-BASELINE-ERROR {} {error}", run.label());
            }
            for error in &run.report.calibration_errors {
                println!("CASE-CALIBRATION-ERROR {} {error}", run.label());
            }
            for gap in run.report.coverage_gaps() {
                println!("CASE-COVERAGE {} {gap}", run.label());
            }
        }
    }
}

/// Run one already-generated case through its own scenario and calibration.
pub fn run_case<A: Adapter>(
    adapter: &A,
    config: &Config,
    case: &Case<A::Target, A::Successor>,
) -> CaseRun<A::Target, A::Successor> {
    let expect = adapter.expect(case);
    let report = explore_observed(config, expect, |ctx| adapter.scenario(ctx, case.clone()));
    CaseRun { case: case.clone(), expect, report }
}

/// Generate up to `limit` applicable cases and run each one separately.
pub fn run_cases<A: Adapter>(
    adapter: &A,
    config: &Config,
    limit: usize,
) -> CaseRuns<A::Target, A::Successor> {
    let plan = plan_cases(adapter, limit);
    let results = plan
        .cases
        .iter()
        .map(|case| run_case(adapter, config, case))
        .collect();
    CaseRuns { plan: plan.clone(), results }
}

/// Re-attempt one plan for the same case. The case description is kept next to
/// the replay report; `replay_observed` validates the plan and re-calibrates.
pub fn replay_case<A: Adapter>(
    adapter: &A,
    config: &Config,
    case: &Case<A::Target, A::Successor>,
    plan: &Plan,
) -> Result<CaseRun<A::Target, A::Successor>, ReplayError> {
    let expect = adapter.expect(case);
    let report = replay_observed(config, expect, plan, |ctx| adapter.scenario(ctx, case.clone()))?;
    Ok(CaseRun { case: case.clone(), expect, report })
}

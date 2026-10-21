//! Experiment-local generation of "one cancelled target + one successor" cases.
//!
//! This is research support, not Dropwise API and not a scenario framework. An
//! adapter registers
//!
//! * cancellation targets and successor operations (ordinary Rust types with a
//!   `Display` name),
//! * their parameters through one typed [`ParamSlots`] traversal per operation,
//! * a pre-execution applicability check with a reason, the [`Expect`]
//!   contract, and one scenario constructor.
//!
//! The shared layer does the Cartesian loop over target x successor x
//! object-relation x payload-relation, collapses dimensions that the slot
//! domains cannot compare, applies the adapter's applicability check, caps the
//! number of executable cases, calls [`explore_observed`] once per case with
//! that case's own scenario/calibration, and keeps the case description next
//! to the existing [`Report`]. Replay takes the same case description plus a
//! normal [`Plan`] and calls [`replay_observed`].
//!
//! What this deliberately does not do: infer contracts, normalize runtime
//! values, judge prefix/subset observations, search recovery sequences, or let
//! an adapter hand over a ready-made list of final scenarios. The adapter
//! declares capabilities and parameter realization; the loop above is here.
#![allow(dead_code)]

use std::any::{Any, TypeId, type_name};
use std::fmt::{self, Display};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use dropwise::{Config, Ctx, Expect, Plan, ReplayError, Report, explore_observed, replay_observed};
use serde::Serialize;

#[path = "diagnose.rs"]
mod diagnose;

#[allow(unused_imports)]
pub use diagnose::{DiagnosticError, FieldDifference, IdentityBinding, IdentityMatch};

/// A future a scenario constructor can own without borrowing the adapter.
pub type CaseFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

/// Which business identity a parameter represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotRole {
    Object,
    Payload,
}

impl SlotRole {
    pub fn label(self) -> &'static str {
        match self {
            SlotRole::Object => "object",
            SlotRole::Payload => "payload",
        }
    }
}

/// One typed operation parameter. `domain` is a stable logical identity, not a
/// Rust type or a display string: equal primitive types do not establish a
/// shared domain. `alternatives` are adapter-declared representative values
/// used only when `Different` needs a value and the successor's seed is not
/// already unequal to the target. They are type-checked by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param<T> {
    pub domain: &'static str,
    pub value: T,
    pub alternatives: Vec<T>,
}

impl<T> Param<T> {
    pub fn new(domain: &'static str, value: T) -> Self {
        Self {
            domain,
            value,
            alternatives: Vec::new(),
        }
    }

    pub fn with_alternatives<I>(mut self, alternatives: I) -> Self
    where
        I: IntoIterator<Item = T>,
    {
        self.alternatives.extend(alternatives);
        self
    }
}

/// The single traversal an operation implements for shape extraction and
/// successor realization. It must report duplicate roles and domain collisions
/// instead of silently using the first slot.
pub trait ParamSlots {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String>;
}

/// Visitor used by both slot passes. The typed `Param<T>` is borrowed mutably so
/// the same declaration can also rewrite the successor's concrete value.
pub trait SlotVisitor {
    fn visit<T>(&mut self, role: SlotRole, param: &mut Param<T>) -> Result<(), String>
    where
        T: Clone + PartialEq + Display + 'static;
}

/// One extracted comparable business identity.
pub struct Slot {
    kind: &'static str,
    value: String,
    erased: Box<dyn ShapeValue>,
}

impl Slot {
    fn new<T>(kind: &'static str, value: &T) -> Self
    where
        T: Clone + PartialEq + Display + 'static,
    {
        Self {
            kind,
            value: value.to_string(),
            erased: Box::new(Held(value.clone())),
        }
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    fn type_id(&self) -> TypeId {
        self.erased.slot_type_id()
    }

    fn type_name(&self) -> &'static str {
        self.erased.slot_type_name()
    }

    fn value_ref<T: 'static>(&self) -> Option<&T> {
        self.erased.as_any().downcast_ref::<T>()
    }

    fn value_eq(&self, other: &Self) -> Result<bool, String> {
        if self.kind != other.kind {
            return Err(format!("domains differ: {} vs {}", self.kind, other.kind));
        }
        if self.type_id() != other.type_id() {
            return Err(format!(
                "domain '{}' is declared with incompatible Rust types: {} vs {}",
                self.kind,
                self.type_name(),
                other.type_name()
            ));
        }
        Ok(self.erased.eq_any(other.erased.as_any()))
    }
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("kind", &self.kind)
            .field("value", &self.value)
            .field("rust_type", &self.type_name())
            .finish()
    }
}

trait ShapeValue {
    fn as_any(&self) -> &dyn Any;
    fn slot_type_id(&self) -> TypeId;
    fn slot_type_name(&self) -> &'static str;
    fn clone_box(&self) -> Box<dyn ShapeValue>;
    fn eq_any(&self, other: &dyn Any) -> bool;
}

struct Held<T>(T);

impl<T> ShapeValue for Held<T>
where
    T: Clone + PartialEq + 'static,
{
    fn as_any(&self) -> &dyn Any {
        &self.0
    }

    fn slot_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn slot_type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn clone_box(&self) -> Box<dyn ShapeValue> {
        Box::new(Held(self.0.clone()))
    }

    fn eq_any(&self, other: &dyn Any) -> bool {
        other
            .downcast_ref::<T>()
            .is_some_and(|other| self.0 == *other)
    }
}

impl Clone for Slot {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind,
            value: self.value.clone(),
            erased: self.erased.clone_box(),
        }
    }
}

/// What an operation touches. `None` means the dimension does not exist for
/// that operation; it is not an empty value.
#[derive(Clone, Default)]
pub struct Shape {
    pub object: Option<Slot>,
    pub payload: Option<Slot>,
}

impl Shape {
    fn slot(&self, role: SlotRole) -> Option<&Slot> {
        match role {
            SlotRole::Object => self.object.as_ref(),
            SlotRole::Payload => self.payload.as_ref(),
        }
    }

    fn slot_mut(&mut self, role: SlotRole) -> &mut Option<Slot> {
        match role {
            SlotRole::Object => &mut self.object,
            SlotRole::Payload => &mut self.payload,
        }
    }
}

impl fmt::Debug for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shape")
            .field("object", &self.object)
            .field("payload", &self.payload)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ObjectRelation {
    Same,
    Different,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    type Target: Clone + Display + ParamSlots;
    type Successor: Clone + Display + ParamSlots;
    type Obs: PartialEq + fmt::Debug + Send + 'static + Serialize;

    /// Registered cancellation targets, in stable generation order.
    fn targets(&self) -> Vec<Self::Target>;
    /// Registered successor operations, in stable generation order.
    fn successors(&self) -> Vec<Self::Successor>;

    /// Adapter-owned contract check that is not expressible in the slot
    /// declaration. `Err` is recorded as "inapplicable" with the reason before
    /// execution.
    fn applicable(&self, case: &Case<Self::Target, Self::Successor>) -> Result<(), String> {
        let _ = case;
        Ok(())
    }

    /// The Dropwise contract for this case. It is declared, never inferred.
    fn expect(&self, case: &Case<Self::Target, Self::Successor>) -> Expect;

    /// Optional read-only identity annotation for diagnostics. It must keep the
    /// raw observed string and the target/successor labels separate; matching a
    /// target is not a violation and never replaces the equality verdict.
    fn identities(
        &self,
        _case: &Case<Self::Target, Self::Successor>,
        _observed: &Self::Obs,
    ) -> Vec<IdentityBinding> {
        Vec::new()
    }

    /// Construct and run the whole scenario for one case.
    ///
    /// This is called by the engine for the uncancelled baseline, both
    /// calibration sides and every cancellation trial. It must therefore run
    /// the successor in all of those runs, not only after a cancellation, and
    /// must rebuild isolated state on every call. Return `Err` only for a
    /// setup/scenario failure; successor errors belong in the observation.
    fn scenario(
        &self,
        ctx: Ctx,
        case: Case<Self::Target, Self::Successor>,
    ) -> CaseFuture<Self::Obs>;
}

struct ShapeVisitor {
    what: String,
    validate_alternatives: bool,
    shape: Shape,
}

impl ShapeVisitor {
    fn new(what: String, validate_alternatives: bool) -> Self {
        Self {
            what,
            validate_alternatives,
            shape: Shape::default(),
        }
    }
}

impl SlotVisitor for ShapeVisitor {
    fn visit<T>(&mut self, role: SlotRole, param: &mut Param<T>) -> Result<(), String>
    where
        T: Clone + PartialEq + Display + 'static,
    {
        if param.domain.is_empty() {
            return Err(format!(
                "{}: {} slot has an empty domain",
                self.what,
                role.label()
            ));
        }
        if self.shape.slot(role).is_some() {
            return Err(format!(
                "{}: ambiguous {} slot (more than one {} declaration)",
                self.what,
                role.label(),
                role.label()
            ));
        }
        for other_role in [SlotRole::Object, SlotRole::Payload] {
            if other_role == role {
                continue;
            }
            if let Some(other) = self.shape.slot(other_role)
                && other.kind() == param.domain
            {
                return Err(format!(
                    "{}: domain '{}' is declared by both {} and {}",
                    self.what,
                    param.domain,
                    other_role.label(),
                    role.label()
                ));
            }
        }
        if self.validate_alternatives {
            for (index, candidate) in param.alternatives.iter().enumerate() {
                if param.alternatives[..index].contains(candidate) {
                    return Err(format!(
                        "{}: duplicate representative for {} domain '{}'",
                        self.what,
                        role.label(),
                        param.domain
                    ));
                }
            }
        }
        *self.shape.slot_mut(role) = Some(Slot::new(param.domain, &param.value));
        Ok(())
    }
}

struct RealizeVisitor<'a> {
    target: &'a Shape,
    object: Option<ObjectRelation>,
    payload: Option<PayloadRelation>,
}

impl SlotVisitor for RealizeVisitor<'_> {
    fn visit<T>(&mut self, role: SlotRole, param: &mut Param<T>) -> Result<(), String>
    where
        T: Clone + PartialEq + Display + 'static,
    {
        let requested = match role {
            SlotRole::Object => self.object.map(|relation| match relation {
                ObjectRelation::Same => RequestedRelation::Same,
                ObjectRelation::Different => RequestedRelation::Different,
            }),
            SlotRole::Payload => self.payload.map(|relation| match relation {
                PayloadRelation::Same => RequestedRelation::Same,
                PayloadRelation::Different => RequestedRelation::Different,
            }),
        };
        let Some(requested) = requested else {
            return Ok(());
        };
        let target = self.target.slot(role).ok_or_else(|| {
            format!(
                "{} relation requested but the target has no declared {} slot",
                role.label(),
                role.label()
            )
        })?;
        if target.kind() != param.domain {
            return Err(format!(
                "{} relation requested across domains: target '{}' != successor '{}'",
                role.label(),
                target.kind(),
                param.domain
            ));
        }
        if target.type_id() != TypeId::of::<T>() {
            return Err(format!(
                "{} domain '{}' is declared with incompatible Rust types: {} vs {}",
                role.label(),
                param.domain,
                target.type_name(),
                type_name::<T>()
            ));
        }
        let target_value = target
            .value_ref::<T>()
            .expect("the slot type was checked immediately above");
        match requested {
            RequestedRelation::Same => {
                param.value = target_value.clone();
                Ok(())
            }
            RequestedRelation::Different => {
                if param.value != *target_value {
                    return Ok(());
                }
                for (index, candidate) in param.alternatives.iter().enumerate() {
                    if param.alternatives[..index].contains(candidate) {
                        return Err(format!(
                            "{} domain '{}' has duplicate declared representatives",
                            role.label(),
                            param.domain
                        ));
                    }
                }
                let candidate = param
                    .alternatives
                    .iter()
                    .find(|candidate| *candidate != target_value)
                    .ok_or_else(|| {
                        format!(
                            "{} domain '{}' has no declared representative different from '{}'",
                            role.label(),
                            param.domain,
                            target.value()
                        )
                    })?;
                param.value = candidate.clone();
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RequestedRelation {
    Same,
    Different,
}

fn shape_of<T>(operation: &T, what: &str, validate_alternatives: bool) -> Result<Shape, String>
where
    T: ParamSlots + Clone,
{
    let mut operation = operation.clone();
    let mut visitor = ShapeVisitor::new(what.to_string(), validate_alternatives);
    operation.visit_slots(&mut visitor)?;
    Ok(visitor.shape)
}

fn realize_successor<T>(
    target_shape: &Shape,
    successor: &T,
    object: Option<ObjectRelation>,
    payload: Option<PayloadRelation>,
) -> Result<T, String>
where
    T: ParamSlots + Clone,
{
    let mut successor = successor.clone();
    let mut visitor = RealizeVisitor {
        target: target_shape,
        object,
        payload,
    };
    successor.visit_slots(&mut visitor)?;
    Ok(successor)
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
        let target_shape = match shape_of(target, &format!("target {target}"), false) {
            Ok(shape) => shape,
            Err(reason) => {
                for successor in &successors {
                    reject_declaration(
                        &mut plan,
                        target,
                        successor,
                        format!("target {target}: {reason}"),
                    );
                }
                continue;
            }
        };
        for successor in &successors {
            let base_shape = match shape_of(successor, &format!("successor {successor}"), true) {
                Ok(shape) => shape,
                Err(reason) => {
                    reject_declaration(
                        &mut plan,
                        target,
                        successor,
                        format!("successor {successor}: {reason}"),
                    );
                    continue;
                }
            };
            let (objects, object_note) = match object_options(&target_shape, &base_shape) {
                Ok(options) => options,
                Err(reason) => {
                    reject_declaration(&mut plan, target, successor, reason);
                    continue;
                }
            };
            let (payloads, payload_note) = match payload_options(&target_shape, &base_shape) {
                Ok(options) => options,
                Err(reason) => {
                    reject_declaration(&mut plan, target, successor, reason);
                    continue;
                }
            };
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
                    let realized =
                        match realize_successor(&target_shape, successor, object, payload) {
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
                    let realized_shape = match shape_of(
                        &case.successor,
                        &format!("successor {}", case.successor),
                        true,
                    ) {
                        Ok(shape) => shape,
                        Err(reason) => {
                            plan.skipped.push(SkippedCase {
                                case,
                                reason: format!("generation: {reason}"),
                            });
                            continue;
                        }
                    };
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

// A malformed pair has no valid relation expansion, but must still be counted
// as excluded rather than disappearing into informational CASE-NOTE output.
fn reject_declaration<T: Clone, S: Clone>(
    plan: &mut CasePlan<T, S>,
    target: &T,
    successor: &S,
    reason: String,
) {
    plan.skipped.push(SkippedCase {
        case: Case {
            target: target.clone(),
            successor: successor.clone(),
            object: None,
            payload: None,
        },
        reason: format!("declaration: {reason}"),
    });
}

fn object_options(
    target: &Shape,
    successor: &Shape,
) -> Result<(Vec<Option<ObjectRelation>>, Option<String>), String> {
    match (&target.object, &successor.object) {
        (Some(t), Some(s)) if t.kind() == s.kind() => {
            if t.type_id() != s.type_id() {
                return Err(format!(
                    "object domain '{}' is declared with incompatible Rust types: {} vs {}",
                    t.kind(),
                    t.type_name(),
                    s.type_name()
                ));
            }
            Ok((
                vec![Some(ObjectRelation::Same), Some(ObjectRelation::Different)],
                None,
            ))
        }
        (Some(t), Some(s)) => Ok((
            vec![None],
            Some(format!(
                "object relation not expanded: target kind {} != successor kind {}",
                t.kind(),
                s.kind()
            )),
        )),
        (None, Some(s)) => Ok((
            vec![None],
            Some(format!(
                "object relation not expanded: target has no business object; successor kind {}",
                s.kind()
            )),
        )),
        (Some(t), None) => Ok((
            vec![None],
            Some(format!(
                "object relation not expanded: successor has no business object; target kind {}",
                t.kind()
            )),
        )),
        (None, None) => Ok((vec![None], None)),
    }
}

fn payload_options(
    target: &Shape,
    successor: &Shape,
) -> Result<(Vec<Option<PayloadRelation>>, Option<String>), String> {
    match (&target.payload, &successor.payload) {
        (Some(t), Some(s)) if t.kind() == s.kind() => {
            if t.type_id() != s.type_id() {
                return Err(format!(
                    "payload domain '{}' is declared with incompatible Rust types: {} vs {}",
                    t.kind(),
                    t.type_name(),
                    s.type_name()
                ));
            }
            Ok((
                vec![
                    Some(PayloadRelation::Same),
                    Some(PayloadRelation::Different),
                ],
                None,
            ))
        }
        (Some(t), Some(s)) => Ok((
            vec![None],
            Some(format!(
                "payload relation not expanded: target kind {} != successor kind {}",
                t.kind(),
                s.kind()
            )),
        )),
        (None, Some(s)) => Ok((
            vec![None],
            Some(format!(
                "payload relation not expanded: target has no payload; successor kind {}",
                s.kind()
            )),
        )),
        (Some(t), None) => Ok((
            vec![None],
            Some(format!(
                "payload relation not expanded: successor has no payload; target kind {}",
                t.kind()
            )),
        )),
        (None, None) => Ok((vec![None], None)),
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
        if t.kind() != s.kind() {
            return Err(format!(
                "object relation realized with different kinds: {} vs {}",
                t.kind(),
                s.kind()
            ));
        }
        let equal = t
            .value_eq(s)
            .map_err(|reason| format!("object relation check failed: {reason}"))?;
        match relation {
            ObjectRelation::Same if !equal => {
                return Err(format!(
                    "same-object requested but realized {} != {}",
                    t.value(),
                    s.value()
                ));
            }
            ObjectRelation::Different if equal => {
                return Err(format!(
                    "different-object requested but realized both {}",
                    t.value()
                ));
            }
            _ => {}
        }
    }
    if let Some(relation) = payload {
        let (Some(t), Some(s)) = (&target.payload, &successor.payload) else {
            return Err("payload relation requested but a realized shape has no payload".into());
        };
        if t.kind() != s.kind() {
            return Err(format!(
                "payload relation realized with different kinds: {} vs {}",
                t.kind(),
                s.kind()
            ));
        }
        let equal = t
            .value_eq(s)
            .map_err(|reason| format!("payload relation check failed: {reason}"))?;
        match relation {
            PayloadRelation::Same if !equal => {
                return Err(format!(
                    "same-payload requested but realized {} != {}",
                    t.value(),
                    s.value()
                ));
            }
            PayloadRelation::Different if equal => {
                return Err(format!(
                    "different-payload requested but realized both {}",
                    t.value()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// One engine comparison recorded without changing its result. `observed` is
/// the right-hand side of the `PartialEq` call; for the calibration checks the
/// engine calls `calibration == trial_observation`, so that side is the trial's
/// actual observation. Sample-stability comparisons use the same direction and
/// therefore label the second sample as observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationComparison {
    pub observed: String,
    pub reference: String,
    pub differences: Vec<FieldDifference>,
    /// Set when a tree could not be built or compared. An error is not equality.
    pub failure: Option<String>,
}

/// Read-only diagnostics collected while the engine compares observations.
/// Enabling them does not alter equality, verdict, Unknown or coverage.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticLog {
    pub comparisons: Vec<ObservationComparison>,
    pub identities: Vec<IdentityBinding>,
    pub errors: Vec<String>,
}

impl DiagnosticLog {
    fn push_comparison(&mut self, comparison: ObservationComparison) {
        if !self.comparisons.contains(&comparison) {
            self.comparisons.push(comparison);
        }
    }

    fn push_identity(&mut self, identity: IdentityBinding) {
        if !self.identities.contains(&identity) {
            self.identities.push(identity);
        }
    }

    fn push_error(&mut self, error: String) {
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
    }

    pub fn differences(&self) -> impl Iterator<Item = &FieldDifference> {
        self.comparisons
            .iter()
            .flat_map(|comparison| &comparison.differences)
    }
}

/// A wrapper whose `PartialEq` delegates to the original observation, so the
/// engine's verdict source is unchanged. It only records structured diffs when
/// that comparison is unequal. `Debug` also delegates, so the engine's existing
/// comparison text is unchanged.
struct DiagnosticObs<O> {
    value: O,
    tree: Result<diagnose::Value, String>,
    log: Arc<Mutex<DiagnosticLog>>,
}

impl<O> DiagnosticObs<O>
where
    O: Serialize,
{
    fn new(value: O, log: Arc<Mutex<DiagnosticLog>>, identities: Vec<IdentityBinding>) -> Self {
        let tree = diagnose::to_value(&value).map_err(|error| error.to_string());
        {
            let mut log = log.lock().unwrap();
            if let Err(error) = &tree {
                log.push_error(format!("observation serialization failed: {error}"));
            }
            for identity in identities {
                log.push_identity(identity);
            }
        }
        Self { value, tree, log }
    }
}

impl<O: fmt::Debug> fmt::Debug for DiagnosticObs<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

impl<O: PartialEq> PartialEq for DiagnosticObs<O> {
    fn eq(&self, other: &Self) -> bool {
        let equal = self.value == other.value;
        if equal {
            return true;
        }
        let comparison = match (&other.tree, &self.tree) {
            (Ok(observed), Ok(reference)) => {
                let differences = diagnose::diff_values(observed, reference);
                let failure = differences.is_empty().then(||
                    "original observations differ, but their serialized fields do not expose the difference".to_string());
                ObservationComparison {
                    observed: diagnose::render(observed),
                    reference: diagnose::render(reference),
                    differences,
                    failure,
                }
            }
            (Err(error), _) => ObservationComparison {
                observed: "<serialization failed>".to_string(),
                reference: match &self.tree {
                    Ok(tree) => diagnose::render(tree),
                    Err(other_error) => format!("<serialization failed: {other_error}>"),
                },
                differences: Vec::new(),
                failure: Some(format!("observed serialization failed: {error}")),
            },
            (_, Err(error)) => ObservationComparison {
                observed: other.tree.as_ref().map_or_else(
                    |error| format!("<serialization failed: {error}>"),
                    diagnose::render,
                ),
                reference: "<serialization failed>".to_string(),
                differences: Vec::new(),
                failure: Some(format!("reference serialization failed: {error}")),
            },
        };
        self.log.lock().unwrap().push_comparison(comparison);
        equal
    }
}

/// Whether to collect read-only observation diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Diagnostics {
    #[default]
    Off,
    On,
}

/// One case's existing Dropwise report, kept together with its description.
#[derive(Debug, Clone)]
pub struct CaseRun<T, S> {
    pub case: Case<T, S>,
    pub expect: Expect,
    pub report: Report,
    pub observation_diagnostics: DiagnosticLog,
}

impl<T: Display, S: Display> CaseRun<T, S> {
    pub fn label(&self) -> String {
        self.case.label()
    }

    /// The first violating plan, if any, ready for [`replay_case`].
    pub fn first_violation_plan(&self) -> Option<Plan> {
        self.report
            .violations()
            .next()
            .map(|trial| trial.replay_plan())
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

    /// Distinct field paths recorded by the structured diagnostics.
    pub fn diagnostic_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .observation_diagnostics
            .differences()
            .map(|difference| difference.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        paths
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
            println!(
                "CASE-SKIP {} reason={}",
                skipped.case.label(),
                skipped.reason
            );
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
            let mut printed_differences: Vec<&FieldDifference> = Vec::new();
            for comparison in &run.observation_diagnostics.comparisons {
                if let Some(failure) = &comparison.failure {
                    println!(
                        "CASE-DIAG-ERROR {} observed={:?} reference={:?} error={failure}",
                        run.label(),
                        comparison.observed,
                        comparison.reference,
                    );
                }
                for difference in &comparison.differences {
                    if printed_differences.contains(&difference) {
                        continue;
                    }
                    printed_differences.push(difference);
                    println!(
                        "CASE-DIAG {} path={} observed={:?} reference={:?}",
                        run.label(),
                        difference.path,
                        difference.observed,
                        difference.reference,
                    );
                }
            }
            for identity in &run.observation_diagnostics.identities {
                println!(
                    "CASE-ID {} field={} observed={:?} target={:?} successor={:?} match={}",
                    run.label(),
                    identity.field,
                    identity.observed,
                    identity.target,
                    identity.successor,
                    identity.matched.label(),
                );
            }
            for error in &run.observation_diagnostics.errors {
                println!("CASE-DIAG-ERROR {} {error}", run.label());
            }
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
    run_case_with(adapter, config, case, Diagnostics::Off)
}

/// Run one case, optionally recording structured comparison diagnostics. The
/// wrapper delegates equality and `Debug` to the original observation.
pub fn run_case_with<A: Adapter>(
    adapter: &A,
    config: &Config,
    case: &Case<A::Target, A::Successor>,
    diagnostics: Diagnostics,
) -> CaseRun<A::Target, A::Successor> {
    let expect = adapter.expect(case);
    let mut log = DiagnosticLog::default();
    let report = match diagnostics {
        Diagnostics::Off => {
            explore_observed(config, expect, |ctx| adapter.scenario(ctx, case.clone()))
        }
        Diagnostics::On => {
            let shared = Arc::new(Mutex::new(DiagnosticLog::default()));
            let scenario = |ctx: Ctx| {
                let future = adapter.scenario(ctx, case.clone());
                let shared = shared.clone();
                async move {
                    let observed = future.await?;
                    let identities = adapter.identities(case, &observed);
                    Ok(DiagnosticObs::new(observed, shared, identities))
                }
            };
            let report = explore_observed(config, expect, scenario);
            log = match Arc::try_unwrap(shared) {
                Ok(diagnostics) => diagnostics.into_inner().unwrap(),
                Err(shared) => shared.lock().unwrap().clone(),
            };
            report
        }
    };
    CaseRun {
        case: case.clone(),
        expect,
        report,
        observation_diagnostics: log,
    }
}

/// Generate up to `limit` applicable cases and run each one separately.
pub fn run_cases<A: Adapter>(
    adapter: &A,
    config: &Config,
    limit: usize,
) -> CaseRuns<A::Target, A::Successor> {
    run_cases_with(adapter, config, limit, Diagnostics::Off)
}

/// Like [`run_cases`], with the structured diagnostic switch. The returned
/// equality policy is unchanged. Extra diagnostic work may affect real-I/O
/// timing, so separate runs need not realize identical plans or verdict counts.
pub fn run_cases_with<A: Adapter>(
    adapter: &A,
    config: &Config,
    limit: usize,
    diagnostics: Diagnostics,
) -> CaseRuns<A::Target, A::Successor> {
    let plan = plan_cases(adapter, limit);
    let results = plan
        .cases
        .iter()
        .map(|case| run_case_with(adapter, config, case, diagnostics))
        .collect();
    CaseRuns {
        plan: plan.clone(),
        results,
    }
}

/// Re-attempt one plan for the same case. The case description is kept next to
/// the replay report; `replay_observed` validates the plan and re-calibrates.
pub fn replay_case<A: Adapter>(
    adapter: &A,
    config: &Config,
    case: &Case<A::Target, A::Successor>,
    plan: &Plan,
) -> Result<CaseRun<A::Target, A::Successor>, ReplayError> {
    replay_case_with(adapter, config, case, plan, Diagnostics::Off)
}

/// Like [`replay_case`], with the read-only diagnostic switch. Replay uses the
/// same observation policy and the same equality source as a normal run.
pub fn replay_case_with<A: Adapter>(
    adapter: &A,
    config: &Config,
    case: &Case<A::Target, A::Successor>,
    plan: &Plan,
    diagnostics: Diagnostics,
) -> Result<CaseRun<A::Target, A::Successor>, ReplayError> {
    let expect = adapter.expect(case);
    let mut log = DiagnosticLog::default();
    let report = match diagnostics {
        Diagnostics::Off => replay_observed(config, expect, plan, |ctx| {
            adapter.scenario(ctx, case.clone())
        })?,
        Diagnostics::On => {
            let shared = Arc::new(Mutex::new(DiagnosticLog::default()));
            let scenario = |ctx: Ctx| {
                let future = adapter.scenario(ctx, case.clone());
                let shared = shared.clone();
                async move {
                    let observed = future.await?;
                    let identities = adapter.identities(case, &observed);
                    Ok(DiagnosticObs::new(observed, shared, identities))
                }
            };
            let report = replay_observed(config, expect, plan, scenario)?;
            log = match Arc::try_unwrap(shared) {
                Ok(diagnostics) => diagnostics.into_inner().unwrap(),
                Err(shared) => shared.lock().unwrap().clone(),
            };
            report
        }
    };
    Ok(CaseRun {
        case: case.clone(),
        expect,
        report,
        observation_diagnostics: log,
    })
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn failed_observation_serialization_keeps_reference_side() {
        let log = Arc::new(Mutex::new(DiagnosticLog::default()));
        let reference = DiagnosticObs::new(1i32, log.clone(), vec![]);
        let observed = DiagnosticObs {
            value: 2i32,
            tree: Err("bad observation".into()),
            log: log.clone(),
        };
        assert_ne!(reference, observed);
        let entries = log.lock().unwrap();
        let entry = &entries.comparisons[0];
        assert_eq!(
            entry.reference,
            diagnose::render(&diagnose::to_value(&1i32).unwrap())
        );
        assert_eq!(entry.observed, "<serialization failed>");
        assert!(
            entry
                .failure
                .as_ref()
                .unwrap()
                .contains("observed serialization failed")
        );
    }

    #[test]
    fn omitted_serialized_fields_do_not_hide_diagnostic_failure() {
        #[derive(Debug, PartialEq, Serialize)]
        struct Observation {
            #[serde(skip)]
            important: i32,
        }
        let log = Arc::new(Mutex::new(DiagnosticLog::default()));
        let reference = DiagnosticObs::new(Observation { important: 1 }, log.clone(), vec![]);
        let observed = DiagnosticObs::new(Observation { important: 2 }, log.clone(), vec![]);
        assert_ne!(reference, observed);
        let entries = log.lock().unwrap();
        assert!(
            entries.comparisons[0]
                .failure
                .as_ref()
                .unwrap()
                .contains("do not expose")
        );
    }
}

//! Field-level observation contracts.
//!
//! [`explore_observed`](crate::explore_observed) compares one observation value
//! with a `pre` and a `post` calibration of the same scenario.  That is enough
//! to find a violation, but it cannot say *which* effect was observed, whether a
//! declared effect had any discriminating power at all, or whether two effects
//! are related.  This module replaces the single blob with named fields:
//!
//! * an **effect** field is compared with the pre and post calibration; both
//!   calibrations must exist and the two values must differ, otherwise the
//!   field is [`FieldStatus::Degenerate`] (no discriminating power) or
//!   [`FieldStatus::Uncovered`] (the scenario never produced it);
//! * a **state** field is an invariant/guard: the two calibrations must agree
//!   on its value and the trial must equal it, otherwise it is
//!   [`FieldStatus::Violated`];
//! * a per-field [`Expect`] decides whether `pre`, `post` or either outcome is
//!   allowed (the same `Expect` used by the single-blob API);
//! * [`Invariant`]s express relations *between* fields, such as "at most one of
//!   these effects may have happened", "all of these effects happen together",
//!   or "the successor effect implies the target effect".
//!
//! Coverage is part of the judgment, not an afterthought: a trial with an
//! uncovered, degenerate or unstable field is not a pass.  [`FieldJudgment`]
//! separates definite violations from coverage gaps, and the report keeps the
//! same distinction.
//!
//! The contract is declared by the scenario author.  Nothing here infers that a
//! field is an effect or a state from its name or value; a strict contract does
//! reject fields that the scenario observed without declaring them, so an
//! effect cannot be silently omitted from the contract.
//!
//! # Example
//!
//! ```
//! use dropwise::fields::{Expect, FieldContract, Observation};
//!
//! let mut pre = Observation::new();
//! pre.set("target.visible", false);
//! pre.set("successor.visible", true);
//! let mut post = Observation::new();
//! post.set("target.visible", true);
//! post.set("successor.visible", true);
//! let mut trial = Observation::new();
//! trial.set("target.visible", false);
//! trial.set("successor.visible", false);
//!
//! let contract = FieldContract::new()
//!     .effect("target.visible", Expect::Atomic)
//!     .state("successor.visible");
//! let judgment = contract.judge(&[pre], &[post], &trial);
//! assert!(judgment.has_violation());
//! assert!(judgment.coverage_complete());
//! ```
//!
//! [`Expect`] is re-exported from the engine because a field's contract has the
//! same three outcomes as a whole observation's contract.

pub use crate::Expect;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// One observed field value.  `Null` is a real value (a field observed to be
/// absent), distinct from a field that the scenario did not produce at all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Text(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            Value::UInt(value) => write!(f, "{value}"),
            Value::Text(value) => f.write_str(value),
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Value::UInt(value)
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Self {
        Value::UInt(u64::try_from(value).unwrap_or(u64::MAX))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Int(i64::from(value))
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Text(value.to_string())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::Text(value)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => value.into(),
            None => Value::Null,
        }
    }
}

/// A named observation: dotted paths to values.  Nested shapes are flattened by
/// the scenario when it builds the observation (`"target.rows"`); no implicit
/// flattening means a field name is always exactly what the contract declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    values: BTreeMap<String, Value>,
}

impl Observation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a field.  Paths are free-form dotted names; an empty path or an
    /// empty segment is rejected by [`FieldContract::validate`].
    pub fn set(&mut self, path: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.values.insert(path.into(), value.into());
        self
    }

    /// Builder form of [`Observation::set`].
    pub fn with(mut self, path: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set(path, value);
        self
    }

    pub fn get(&self, path: &str) -> Option<&Value> {
        self.values.get(path)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.values.contains_key(path)
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.values
            .iter()
            .map(|(path, value)| (path.as_str(), value))
    }
}

impl FromIterator<(String, Value)> for Observation {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self {
            values: iter.into_iter().collect(),
        }
    }
}

/// What a field means for the hand-off judgment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// An externally visible effect.  It must differ between the pre and the
    /// post calibration; otherwise the scenario has no way to tell whether it
    /// happened.
    Effect,
    /// An invariant or guard.  The two calibrations must agree, and the trial
    /// must equal that agreed value.
    State,
}

impl FieldKind {
    pub fn name(self) -> &'static str {
        match self {
            FieldKind::Effect => "effect",
            FieldKind::State => "state",
        }
    }
}

/// One declared field.  Construct through [`FieldContract`]; a plain effect
/// cannot be declared without an expectation, and a state cannot carry one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    path: String,
    kind: FieldKind,
    expect: Option<Expect>,
}

impl Field {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn kind(&self) -> FieldKind {
        self.kind
    }

    pub fn expectation(&self) -> Option<Expect> {
        self.expect
    }
}

/// A per-field observation contract, plus optional cross-field invariants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldContract {
    fields: Vec<Field>,
    invariants: Vec<Invariant>,
    strict: bool,
}

impl FieldContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare an effect field with the outcomes the caller allows.
    pub fn effect(mut self, path: impl Into<String>, expect: Expect) -> Self {
        self.fields.push(Field {
            path: path.into(),
            kind: FieldKind::Effect,
            expect: Some(expect),
        });
        self
    }

    /// Declare a state field: an invariant that must hold in the trial.
    pub fn state(mut self, path: impl Into<String>) -> Self {
        self.fields.push(Field {
            path: path.into(),
            kind: FieldKind::State,
            expect: None,
        });
        self
    }

    /// Add a relation between fields.  See [`Invariant`].
    pub fn invariant(mut self, invariant: Invariant) -> Self {
        self.invariants.push(invariant);
        self
    }

    /// Reject fields that the scenario observed but the contract did not
    /// declare.  Without this, forgetting an effect in the contract is
    /// invisible; with it, the report carries an explicit coverage gap.
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    pub fn invariants(&self) -> &[Invariant] {
        &self.invariants
    }

    pub fn is_strict(&self) -> bool {
        self.strict
    }

    /// Static contract problems: duplicate or malformed paths, an empty
    /// contract, and invariants that name undeclared or non-effect fields.
    /// Reported as coverage gaps rather than panics.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.fields.is_empty() {
            errors.push("the contract declares no fields".to_string());
        }
        let mut seen = BTreeSet::new();
        for field in &self.fields {
            if field.path.is_empty() || field.path.split('.').any(str::is_empty) {
                errors.push(format!(
                    "field path {:?} is empty or has an empty segment",
                    field.path
                ));
            }
            if !seen.insert(field.path.as_str()) {
                errors.push(format!("field {:?} is declared more than once", field.path));
            }
            match (field.kind, field.expect) {
                (FieldKind::Effect, None) => {
                    errors.push(format!("effect field {:?} has no expectation", field.path))
                }
                (FieldKind::State, Some(_)) => errors.push(format!(
                    "state field {:?} carries an effect expectation",
                    field.path
                )),
                _ => {}
            }
        }
        let effect_paths: BTreeSet<&str> = self
            .fields
            .iter()
            .filter(|field| field.kind == FieldKind::Effect)
            .map(|field| field.path.as_str())
            .collect();
        for invariant in &self.invariants {
            for path in invariant.referenced_fields() {
                if !seen.contains(path) {
                    errors.push(format!(
                        "invariant {} references undeclared field {path:?}",
                        invariant.describe()
                    ));
                } else if !effect_paths.contains(path) {
                    errors.push(format!(
                        "invariant {} references state field {path:?}; invariants compare effect outcomes",
                        invariant.describe()
                    ));
                }
            }
            if invariant.referenced_fields().is_empty() {
                errors.push("an invariant references no fields".to_string());
            }
            if invariant.has_duplicate_fields() {
                errors.push(format!(
                    "invariant {} references the same field twice",
                    invariant.describe()
                ));
            }
        }
        errors
    }

    /// Check that the uncancelled observation carries every declared field and,
    /// for a strict contract, that every observed field is declared.  Baseline
    /// coverage is checked once and reported on the report, so a forgotten
    /// field is visible before any trial is judged.
    pub fn check_baseline(&self, baseline: &Observation) -> Vec<String> {
        let mut errors = Vec::new();
        for field in &self.fields {
            if !baseline.contains(&field.path) {
                errors.push(format!(
                    "declared {} field {:?} is absent from the baseline observation",
                    field.kind.name(),
                    field.path
                ));
            }
        }
        if self.strict {
            let declared: BTreeSet<&str> = self
                .fields
                .iter()
                .map(|field| field.path.as_str())
                .collect();
            for path in baseline.paths() {
                if !declared.contains(path) {
                    errors.push(format!(
                        "observed field {path:?} is not declared (strict contract)"
                    ));
                }
            }
        }
        errors
    }

    /// Judge one trial against calibration samples of the same run.
    ///
    /// Every sample must contain every declared field.  More than one sample
    /// per side is supported because the engine takes two; disagreement within
    /// a side makes that field [`FieldStatus::Unknown`], never a violation.
    pub fn judge(
        &self,
        pre: &[Observation],
        post: &[Observation],
        actual: &Observation,
    ) -> FieldJudgment {
        let mut evaluations = Vec::with_capacity(self.fields.len());
        for field in &self.fields {
            evaluations.push(judge_field(field, pre, post, actual));
        }
        let mut undefined = Vec::new();
        if self.strict {
            let declared: BTreeSet<&str> = self
                .fields
                .iter()
                .map(|field| field.path.as_str())
                .collect();
            for observation in pre.iter().chain(post).chain(std::iter::once(actual)) {
                for path in observation.paths() {
                    if !declared.contains(path) {
                        undefined.push(path.to_string());
                    }
                }
            }
            undefined.sort();
            undefined.dedup();
        }
        let invariants = self
            .invariants
            .iter()
            .map(|invariant| invariant.evaluate(&evaluations))
            .collect();
        FieldJudgment {
            fields: evaluations,
            invariants,
            undeclared: undefined,
        }
    }
}

/// Which calibrated outcome an invariant talks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pre,
    Post,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Pre => "pre",
            Outcome::Post => "post",
        }
    }

    fn status(self) -> FieldStatus {
        match self {
            Outcome::Pre => FieldStatus::Pre,
            Outcome::Post => FieldStatus::Post,
        }
    }
}

/// A relation between effect fields, evaluated on the trial's per-field
/// statuses.  A relation that cannot be decided because a field is uncovered,
/// degenerate or unstable is a coverage gap, not a pass and not a violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invariant {
    /// At most one of `fields` has the given outcome.
    AtMostOne {
        fields: Vec<String>,
        outcome: Outcome,
    },
    /// At least one of `fields` has the given outcome.
    AtLeastOne {
        fields: Vec<String>,
        outcome: Outcome,
    },
    /// Exactly one of `fields` has the given outcome.
    ExactlyOne {
        fields: Vec<String>,
        outcome: Outcome,
    },
    /// Every field is `pre` or every field is `post`; no mixture is allowed.
    AllOrNone { fields: Vec<String> },
    /// If `when` has `when_outcome`, then `then` must have `then_outcome`.
    Implies {
        when: String,
        when_outcome: Outcome,
        then: String,
        then_outcome: Outcome,
    },
}

impl Invariant {
    pub fn at_most_one(
        fields: impl IntoIterator<Item = impl Into<String>>,
        outcome: Outcome,
    ) -> Self {
        Invariant::AtMostOne {
            fields: fields.into_iter().map(Into::into).collect(),
            outcome,
        }
    }

    pub fn at_least_one(
        fields: impl IntoIterator<Item = impl Into<String>>,
        outcome: Outcome,
    ) -> Self {
        Invariant::AtLeastOne {
            fields: fields.into_iter().map(Into::into).collect(),
            outcome,
        }
    }

    pub fn exactly_one(
        fields: impl IntoIterator<Item = impl Into<String>>,
        outcome: Outcome,
    ) -> Self {
        Invariant::ExactlyOne {
            fields: fields.into_iter().map(Into::into).collect(),
            outcome,
        }
    }

    pub fn all_or_none(fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Invariant::AllOrNone {
            fields: fields.into_iter().map(Into::into).collect(),
        }
    }

    pub fn implies(
        when: impl Into<String>,
        when_outcome: Outcome,
        then: impl Into<String>,
        then_outcome: Outcome,
    ) -> Self {
        Invariant::Implies {
            when: when.into(),
            when_outcome,
            then: then.into(),
            then_outcome,
        }
    }

    fn describe(&self) -> String {
        match self {
            Invariant::AtMostOne { fields, outcome } => format!(
                "at most one of [{}] is {}",
                fields.join(", "),
                outcome.name()
            ),
            Invariant::AtLeastOne { fields, outcome } => format!(
                "at least one of [{}] is {}",
                fields.join(", "),
                outcome.name()
            ),
            Invariant::ExactlyOne { fields, outcome } => format!(
                "exactly one of [{}] is {}",
                fields.join(", "),
                outcome.name()
            ),
            Invariant::AllOrNone { fields } => {
                format!("all of [{}] are pre or all are post", fields.join(", "))
            }
            Invariant::Implies {
                when,
                when_outcome,
                then,
                then_outcome,
            } => format!(
                "{when} = {} implies {then} = {}",
                when_outcome.name(),
                then_outcome.name()
            ),
        }
    }

    fn referenced_fields(&self) -> Vec<&str> {
        match self {
            Invariant::AtMostOne { fields, .. }
            | Invariant::AtLeastOne { fields, .. }
            | Invariant::ExactlyOne { fields, .. }
            | Invariant::AllOrNone { fields } => fields.iter().map(String::as_str).collect(),
            Invariant::Implies { when, then, .. } => vec![when.as_str(), then.as_str()],
        }
    }

    fn has_duplicate_fields(&self) -> bool {
        let fields = self.referenced_fields();
        let unique: BTreeSet<&str> = fields.iter().copied().collect();
        unique.len() != fields.len()
    }

    fn evaluate(&self, evaluations: &[FieldEvaluation]) -> InvariantEvaluation {
        let description = self.describe();
        let by_path: BTreeMap<&str, &FieldEvaluation> = evaluations
            .iter()
            .map(|evaluation| (evaluation.path.as_str(), evaluation))
            .collect();
        let result = match self {
            Invariant::AtMostOne { fields, outcome } => {
                let (known, unresolved) = count_outcome(&by_path, fields, *outcome);
                if known > 1 {
                    InvariantResult::Violated(format!("{known} fields are {}", outcome.name()))
                } else if known + unresolved > 1 {
                    InvariantResult::Undetermined(format!(
                        "{known} known and {unresolved} undecided fields"
                    ))
                } else {
                    InvariantResult::Holds
                }
            }
            Invariant::AtLeastOne { fields, outcome } => {
                let (known, unresolved) = count_outcome(&by_path, fields, *outcome);
                if known >= 1 {
                    InvariantResult::Holds
                } else if unresolved > 0 {
                    InvariantResult::Undetermined(format!("{unresolved} undecided fields"))
                } else {
                    InvariantResult::Violated(format!("no field is {}", outcome.name()))
                }
            }
            Invariant::ExactlyOne { fields, outcome } => {
                let (known, unresolved) = count_outcome(&by_path, fields, *outcome);
                if known > 1 {
                    InvariantResult::Violated(format!("{known} fields are {}", outcome.name()))
                } else if known == 1 && unresolved == 0 {
                    InvariantResult::Holds
                } else if known + unresolved == 0 {
                    InvariantResult::Violated(format!("no field is {}", outcome.name()))
                } else {
                    InvariantResult::Undetermined(format!(
                        "{known} known and {unresolved} undecided fields"
                    ))
                }
            }
            Invariant::AllOrNone { fields } => {
                let (pre, post, unresolved, neither) = outcomes_of(&by_path, fields);
                if pre > 0 && post > 0 {
                    InvariantResult::Violated(format!("{pre} pre and {post} post fields"))
                } else if neither > 0 {
                    InvariantResult::Violated(format!(
                        "{neither} field(s) are neither pre nor post"
                    ))
                } else if unresolved > 0 {
                    InvariantResult::Undetermined(format!("{unresolved} undecided fields"))
                } else if pre + post == 0 {
                    InvariantResult::Undetermined("no effect field was resolved".to_string())
                } else {
                    InvariantResult::Holds
                }
            }
            Invariant::Implies {
                when,
                when_outcome,
                then,
                then_outcome,
            } => {
                let when_status = by_path.get(when.as_str()).map(|e| e.status);
                let then_status = by_path.get(then.as_str()).map(|e| e.status);
                match when_status {
                    Some(status) if status == when_outcome.status() => match then_status {
                        Some(status) if status == then_outcome.status() => InvariantResult::Holds,
                        Some(FieldStatus::Pre | FieldStatus::Post) => {
                            InvariantResult::Violated(format!(
                                "{when} is {} but {then} is not {}",
                                when_outcome.name(),
                                then_outcome.name()
                            ))
                        }
                        Some(FieldStatus::Neither) => InvariantResult::Violated(format!(
                            "{when} is {} but {then} is neither pre nor post",
                            when_outcome.name()
                        )),
                        Some(_) => InvariantResult::Undetermined(format!(
                            "{then} is {}",
                            then_status.map_or("unknown", FieldStatus::name)
                        )),
                        None => InvariantResult::Undetermined(format!("{then} is not declared")),
                    },
                    Some(FieldStatus::Pre | FieldStatus::Post) => InvariantResult::Holds,
                    Some(FieldStatus::Neither) => {
                        InvariantResult::Undetermined(format!("{when} is neither pre nor post"))
                    }
                    Some(_) => InvariantResult::Undetermined(format!(
                        "{when} is {}",
                        when_status.map_or("unknown", FieldStatus::name)
                    )),
                    None => InvariantResult::Undetermined(format!("{when} is not declared")),
                }
            }
        };
        InvariantEvaluation {
            description,
            result,
        }
    }
}

fn count_outcome(
    by_path: &BTreeMap<&str, &FieldEvaluation>,
    fields: &[String],
    outcome: Outcome,
) -> (usize, usize) {
    let mut known = 0;
    let mut unresolved = 0;
    for path in fields {
        match by_path
            .get(path.as_str())
            .map(|evaluation| evaluation.status)
        {
            Some(FieldStatus::Pre) | Some(FieldStatus::Post) => {
                if by_path[path.as_str()].status == outcome.status() {
                    known += 1;
                }
            }
            Some(FieldStatus::Neither) => {}
            Some(_) | None => unresolved += 1,
        }
    }
    (known, unresolved)
}

fn outcomes_of(
    by_path: &BTreeMap<&str, &FieldEvaluation>,
    fields: &[String],
) -> (usize, usize, usize, usize) {
    let mut pre = 0;
    let mut post = 0;
    let mut unresolved = 0;
    let mut neither = 0;
    for path in fields {
        match by_path
            .get(path.as_str())
            .map(|evaluation| evaluation.status)
        {
            Some(FieldStatus::Pre) => pre += 1,
            Some(FieldStatus::Post) => post += 1,
            Some(FieldStatus::Neither) => neither += 1,
            Some(_) | None => unresolved += 1,
        }
    }
    (pre, post, unresolved, neither)
}

/// What the trial's value shows for one field, relative to its calibration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldStatus {
    /// The trial value equals the pre calibration.
    Pre,
    /// The trial value equals the post calibration.
    Post,
    /// A state field equals the value the two calibrations agreed on.
    Holds,
    /// The trial value is neither calibration (a definite violation for any
    /// expectation).
    Neither,
    /// A state field differs from the agreed calibration value.
    Violated,
    /// The field is unstable between calibration samples.
    Unknown,
    /// The pre and post calibrations are equal, so the field cannot tell
    /// whether the operation happened.
    Degenerate,
    /// A declared field is missing from a calibration sample or the trial.
    Uncovered,
}

impl FieldStatus {
    pub fn name(self) -> &'static str {
        match self {
            FieldStatus::Pre => "pre",
            FieldStatus::Post => "post",
            FieldStatus::Holds => "holds",
            FieldStatus::Neither => "neither",
            FieldStatus::Violated => "violated",
            FieldStatus::Unknown => "unknown",
            FieldStatus::Degenerate => "degenerate",
            FieldStatus::Uncovered => "uncovered",
        }
    }

    /// Whether this status leaves the field undecided (a coverage gap rather
    /// than a pass or a violation).
    pub fn is_gap(self) -> bool {
        matches!(
            self,
            FieldStatus::Unknown | FieldStatus::Degenerate | FieldStatus::Uncovered
        )
    }
}

/// The judgment for one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldEvaluation {
    pub path: String,
    pub kind: FieldKind,
    pub status: FieldStatus,
    /// The agreed pre-calibration value, when the pre samples were stable and
    /// contained the field.
    pub pre: Option<Value>,
    /// The agreed post-calibration value, when the post samples were stable and
    /// contained the field.
    pub post: Option<Value>,
    pub actual: Option<Value>,
    /// A definite violation of the field's own expectation or state contract.
    pub violation: Option<String>,
    /// Why the field cannot decide the trial (coverage gap).
    pub gap: Option<String>,
}

impl FieldEvaluation {
    pub fn is_violation(&self) -> bool {
        self.violation.is_some() || self.status == FieldStatus::Violated
    }

    pub fn is_gap(&self) -> bool {
        self.gap.is_some() || self.status.is_gap()
    }
}

/// Evaluation of one cross-field invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvariantEvaluation {
    pub description: String,
    pub result: InvariantResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvariantResult {
    Holds,
    Violated(String),
    Undetermined(String),
}

impl InvariantResult {
    pub fn is_violation(&self) -> bool {
        matches!(self, InvariantResult::Violated(_))
    }

    pub fn is_gap(&self) -> bool {
        matches!(self, InvariantResult::Undetermined(_))
    }
}

/// The complete judgment for one trial.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldJudgment {
    pub fields: Vec<FieldEvaluation>,
    pub invariants: Vec<InvariantEvaluation>,
    pub undeclared: Vec<String>,
}

impl FieldJudgment {
    /// Definite violations, with the field or invariant name attached.
    pub fn violations(&self) -> Vec<String> {
        let mut out = Vec::new();
        for field in &self.fields {
            if let Some(violation) = &field.violation {
                out.push(violation.clone());
            }
        }
        for invariant in &self.invariants {
            if let InvariantResult::Violated(detail) = &invariant.result {
                out.push(format!("{}: {detail}", invariant.description));
            }
        }
        out
    }

    /// Coverage gaps: fields that cannot decide the trial, invariants that
    /// cannot be evaluated, and (for a strict contract) observed fields that
    /// were not declared.
    pub fn gaps(&self) -> Vec<String> {
        let mut out = Vec::new();
        for field in &self.fields {
            if let Some(gap) = &field.gap {
                out.push(gap.clone());
            }
        }
        for invariant in &self.invariants {
            if let InvariantResult::Undetermined(detail) = &invariant.result {
                out.push(format!("{}: {detail}", invariant.description));
            }
        }
        for path in &self.undeclared {
            out.push(format!(
                "observed field {path:?} is not declared (strict contract)"
            ));
        }
        out
    }

    pub fn has_violation(&self) -> bool {
        self.fields.iter().any(FieldEvaluation::is_violation)
            || self.invariants.iter().any(|i| i.result.is_violation())
    }

    pub fn coverage_complete(&self) -> bool {
        self.gaps().is_empty()
    }

    /// One-line-per-field rendering for diagnostics.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for field in &self.fields {
            let _ = writeln!(
                out,
                "    {}: {} = {} (pre={}, post={}, actual={})",
                field.kind.name(),
                field.path,
                field.status.name(),
                field
                    .pre
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |v| format!("{v}")),
                field
                    .post
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |v| format!("{v}")),
                field
                    .actual
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |v| format!("{v}"))
            );
            if let Some(violation) = &field.violation {
                let _ = writeln!(out, "      violation: {violation}");
            }
            if let Some(gap) = &field.gap {
                let _ = writeln!(out, "      gap: {gap}");
            }
        }
        for invariant in &self.invariants {
            let outcome = match &invariant.result {
                InvariantResult::Holds => "holds".to_string(),
                InvariantResult::Violated(detail) => format!("violated: {detail}"),
                InvariantResult::Undetermined(detail) => format!("undetermined: {detail}"),
            };
            let _ = writeln!(out, "    invariant {}: {outcome}", invariant.description);
        }
        out
    }
}

fn judge_field(
    field: &Field,
    pre: &[Observation],
    post: &[Observation],
    actual: &Observation,
) -> FieldEvaluation {
    let mut evaluation = FieldEvaluation {
        path: field.path.clone(),
        kind: field.kind,
        status: FieldStatus::Uncovered,
        pre: None,
        post: None,
        actual: actual.get(&field.path).cloned(),
        violation: None,
        gap: None,
    };
    let pre_values = collect_values(pre, &field.path);
    let post_values = collect_values(post, &field.path);
    let (Some(pre_values), Some(post_values)) = (pre_values, post_values) else {
        evaluation.gap = Some(format!(
            "{} field {:?} is absent from a calibration sample",
            field.kind.name(),
            field.path
        ));
        return evaluation;
    };
    let Some(pre_value) = stable_value(&pre_values) else {
        evaluation.status = FieldStatus::Unknown;
        evaluation.gap = Some(format!(
            "{} field {:?} is unstable across pre calibration samples",
            field.kind.name(),
            field.path
        ));
        return evaluation;
    };
    let Some(post_value) = stable_value(&post_values) else {
        evaluation.status = FieldStatus::Unknown;
        evaluation.gap = Some(format!(
            "{} field {:?} is unstable across post calibration samples",
            field.kind.name(),
            field.path
        ));
        return evaluation;
    };
    evaluation.pre = Some(pre_value.clone());
    evaluation.post = Some(post_value.clone());
    let Some(actual_value) = actual.get(&field.path) else {
        evaluation.gap = Some(format!(
            "{} field {:?} is absent from the trial observation",
            field.kind.name(),
            field.path
        ));
        return evaluation;
    };

    match field.kind {
        FieldKind::Effect => {
            if pre_value == post_value {
                evaluation.status = FieldStatus::Degenerate;
                evaluation.gap = Some(format!(
                    "effect {:?} is degenerate: identical in pre and post ({}); this cut \
                     did not discriminate it",
                    field.path, pre_value
                ));
                return evaluation;
            }
            if *actual_value == pre_value {
                evaluation.status = FieldStatus::Pre;
                if field.expect == Some(Expect::Completed) {
                    evaluation.violation = Some(format!(
                        "effect {:?} was expected to complete but stayed at its pre value {}",
                        field.path, pre_value
                    ));
                }
            } else if *actual_value == post_value {
                evaluation.status = FieldStatus::Post;
                if field.expect == Some(Expect::Undone) {
                    evaluation.violation = Some(format!(
                        "effect {:?} was not allowed to complete but reached its post value {}",
                        field.path, post_value
                    ));
                }
            } else {
                evaluation.status = FieldStatus::Neither;
                evaluation.violation = Some(format!(
                    "effect {:?} observed {actual_value}, which is neither the pre value \
                     {pre_value} nor the post value {post_value}",
                    field.path
                ));
            }
        }
        FieldKind::State => {
            if pre_value != post_value {
                evaluation.status = FieldStatus::Unknown;
                evaluation.gap = Some(format!(
                    "state {:?} differs between pre ({pre_value}) and post ({post_value}) \
                     calibration; it is not an invariant of this scenario",
                    field.path
                ));
            } else if *actual_value == pre_value {
                evaluation.status = FieldStatus::Holds;
            } else {
                evaluation.status = FieldStatus::Violated;
                evaluation.violation = Some(format!(
                    "state {:?} must hold at {pre_value} but the trial observed {actual_value}",
                    field.path
                ));
            }
        }
    }
    evaluation
}

fn collect_values(samples: &[Observation], path: &str) -> Option<Vec<Value>> {
    let mut values = Vec::with_capacity(samples.len());
    for sample in samples {
        values.push(sample.get(path)?.clone());
    }
    Some(values)
}

fn stable_value(values: &[Value]) -> Option<Value> {
    let first = values.first()?;
    if values.iter().all(|value| value == first) {
        Some(first.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(path: &str, value: impl Into<Value>) -> Observation {
        Observation::new().with(path, value)
    }

    fn contract() -> FieldContract {
        FieldContract::new()
            .effect("target.visible", Expect::Atomic)
            .effect("successor.visible", Expect::Atomic)
            .state("client.clean")
    }

    #[test]
    fn effect_pre_post_and_state_hold() {
        let contract = contract();
        let pre = Observation::new()
            .with("target.visible", false)
            .with("successor.visible", false)
            .with("client.clean", true);
        let post = Observation::new()
            .with("target.visible", true)
            .with("successor.visible", true)
            .with("client.clean", true);
        let judgment = contract.judge(
            std::slice::from_ref(&pre),
            std::slice::from_ref(&post),
            &pre,
        );
        assert_eq!(judgment.fields[0].status, FieldStatus::Pre);
        assert_eq!(judgment.fields[1].status, FieldStatus::Pre);
        assert_eq!(judgment.fields[2].status, FieldStatus::Holds);
        assert!(judgment.coverage_complete());
        assert!(!judgment.has_violation());

        let judgment = contract.judge(
            &[pre],
            &[post],
            &Observation::new()
                .with("target.visible", true)
                .with("successor.visible", false)
                .with("client.clean", true),
        );
        assert_eq!(judgment.fields[0].status, FieldStatus::Post);
        assert_eq!(judgment.fields[1].status, FieldStatus::Pre);
        assert!(!judgment.has_violation());
        assert!(judgment.coverage_complete());
    }

    #[test]
    fn expectation_is_per_field() {
        let contract = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("b", Expect::Completed);
        let pre = Observation::new().with("a", false).with("b", false);
        let post = Observation::new().with("a", true).with("b", true);
        let actual = Observation::new().with("a", true).with("b", false);
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert!(!judgment.fields[0].is_violation());
        assert!(judgment.fields[1].is_violation());
        assert!(judgment.has_violation());
    }

    #[test]
    fn state_violation_is_reported() {
        let contract = FieldContract::new().state("client.clean");
        let pre = obs("client.clean", true);
        let post = obs("client.clean", true);
        let actual = obs("client.clean", false);
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert_eq!(judgment.fields[0].status, FieldStatus::Violated);
        assert!(judgment.has_violation());
        assert!(judgment.coverage_complete());
    }

    #[test]
    fn a_degenerate_effect_is_a_coverage_gap_not_a_pass() {
        let contract = FieldContract::new().effect("observer.rows", Expect::Atomic);
        let pre = obs("observer.rows", 0);
        let post = obs("observer.rows", 0);
        let actual = obs("observer.rows", 0);
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert_eq!(judgment.fields[0].status, FieldStatus::Degenerate);
        assert!(!judgment.has_violation());
        assert!(!judgment.coverage_complete());
    }

    #[test]
    fn a_missing_field_is_uncovered_and_not_a_violation() {
        let contract = FieldContract::new().effect("target.visible", Expect::Atomic);
        let pre = obs("other", false);
        let post = obs("other", true);
        let actual = obs("other", false);
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert_eq!(judgment.fields[0].status, FieldStatus::Uncovered);
        assert!(!judgment.has_violation());
        assert!(!judgment.coverage_complete());
    }

    #[test]
    fn unstable_calibration_is_unknown() {
        let contract = FieldContract::new().effect("a", Expect::Atomic);
        let pre1 = obs("a", false);
        let pre2 = obs("a", true);
        let post = obs("a", true);
        let judgment = contract.judge(&[pre1, pre2], std::slice::from_ref(&post), &post);
        assert_eq!(judgment.fields[0].status, FieldStatus::Unknown);
        assert!(!judgment.has_violation());
        assert!(!judgment.coverage_complete());
    }

    #[test]
    fn null_is_a_value_not_a_missing_field() {
        let contract = FieldContract::new().state("verdict");
        let pre = obs("verdict", Value::Null);
        let post = obs("verdict", Value::Null);
        let judgment = contract.judge(std::slice::from_ref(&pre), &[post], &pre);
        assert_eq!(judgment.fields[0].status, FieldStatus::Holds);
        assert!(judgment.coverage_complete());
    }

    #[test]
    fn neither_is_a_definite_violation() {
        let contract = FieldContract::new().effect("a", Expect::Atomic);
        let pre = obs("a", 0);
        let post = obs("a", 2);
        let judgment = contract.judge(&[pre], &[post], &obs("a", 1));
        assert_eq!(judgment.fields[0].status, FieldStatus::Neither);
        assert!(judgment.has_violation());
        assert!(judgment.coverage_complete());
    }

    #[test]
    fn per_field_mixture_is_expressible_without_mixed_calibration() {
        // Effect `a` completes before the cut, `b` does not.  The whole blob
        // is neither pre nor post, but each field's own expectation allows its
        // outcome.
        let contract = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("b", Expect::Atomic);
        let pre = Observation::new().with("a", false).with("b", false);
        let post = Observation::new().with("a", true).with("b", true);
        let actual = Observation::new().with("a", true).with("b", false);
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert!(!judgment.has_violation());
        assert!(judgment.coverage_complete());
        assert_eq!(judgment.fields[0].status, FieldStatus::Post);
        assert_eq!(judgment.fields[1].status, FieldStatus::Pre);
    }

    #[test]
    fn cross_field_invariants_decide_mixtures() {
        let contract = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("b", Expect::Atomic)
            .invariant(Invariant::all_or_none(["a", "b"]));
        let pre = Observation::new().with("a", false).with("b", false);
        let post = Observation::new().with("a", true).with("b", true);
        let split = Observation::new().with("a", true).with("b", false);
        let judgment = contract.judge(
            std::slice::from_ref(&pre),
            std::slice::from_ref(&post),
            &split,
        );
        assert!(judgment.has_violation());
        assert!(judgment.coverage_complete());
        assert_eq!(
            judgment.invariants[0].result,
            InvariantResult::Violated("1 pre and 1 post fields".into())
        );

        let one = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("b", Expect::Atomic)
            .invariant(Invariant::exactly_one(["a", "b"], Outcome::Post));
        let judgment = one.judge(
            std::slice::from_ref(&pre),
            std::slice::from_ref(&post),
            &split,
        );
        assert!(!judgment.has_violation());
        assert!(judgment.coverage_complete());
        let judgment = one.judge(std::slice::from_ref(&pre), &[post], &pre);
        assert!(judgment.has_violation());
    }

    #[test]
    fn implies_invariant() {
        let contract = FieldContract::new()
            .effect("successor", Expect::Atomic)
            .effect("target", Expect::Atomic)
            .invariant(Invariant::implies(
                "successor",
                Outcome::Post,
                "target",
                Outcome::Post,
            ));
        let pre = Observation::new()
            .with("successor", false)
            .with("target", false);
        let post = Observation::new()
            .with("successor", true)
            .with("target", true);
        // The successor happened without the target: a violation.
        let bad = Observation::new()
            .with("successor", true)
            .with("target", false);
        let judgment = contract.judge(
            std::slice::from_ref(&pre),
            std::slice::from_ref(&post),
            &bad,
        );
        assert!(judgment.has_violation());
        // The target happened alone: allowed, the antecedent is false.
        let ok = Observation::new()
            .with("successor", false)
            .with("target", true);
        let judgment = contract.judge(&[pre], &[post], &ok);
        assert!(!judgment.has_violation());
        assert!(judgment.coverage_complete());
    }

    #[test]
    fn an_undecided_invariant_is_a_gap_not_a_pass() {
        let contract = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("b", Expect::Atomic)
            .invariant(Invariant::exactly_one(["a", "b"], Outcome::Post));
        let pre = Observation::new().with("a", false).with("b", false);
        let post = Observation::new().with("a", true).with("b", false);
        let actual = Observation::new().with("a", false).with("b", false);
        // `b` is degenerate (pre == post), so "exactly one is post" cannot be decided.
        let judgment = contract.judge(&[pre], &[post], &actual);
        assert_eq!(judgment.fields[1].status, FieldStatus::Degenerate);
        assert!(!judgment.has_violation());
        assert!(!judgment.coverage_complete());
        assert!(matches!(
            judgment.invariants[0].result,
            InvariantResult::Undetermined(_)
        ));
    }

    #[test]
    fn strict_contract_flags_undeclared_observation_fields() {
        let contract = FieldContract::new().effect("a", Expect::Atomic).strict();
        let pre = Observation::new().with("a", false).with("extra", 1);
        let post = Observation::new().with("a", true).with("extra", 1);
        let actual = Observation::new().with("a", false).with("extra", 1);
        let judgment = contract.judge(std::slice::from_ref(&pre), &[post], &actual);
        assert!(judgment.undeclared.contains(&"extra".to_string()));
        assert!(!judgment.coverage_complete());
        assert!(
            contract
                .check_baseline(&pre)
                .iter()
                .any(|e| e.contains("extra"))
        );
    }

    #[test]
    fn contract_validation_rejects_structural_errors() {
        let empty = FieldContract::new();
        assert!(empty.validate().iter().any(|e| e.contains("no fields")));

        let duplicate = FieldContract::new()
            .effect("a", Expect::Atomic)
            .effect("a", Expect::Atomic);
        assert!(
            duplicate
                .validate()
                .iter()
                .any(|e| e.contains("more than once"))
        );

        let bad_path = FieldContract::new().effect("a..b", Expect::Atomic);
        assert!(
            bad_path
                .validate()
                .iter()
                .any(|e| e.contains("empty segment"))
        );

        let undeclared_invariant = FieldContract::new()
            .effect("a", Expect::Atomic)
            .invariant(Invariant::at_most_one(["a", "b"], Outcome::Post));
        assert!(
            undeclared_invariant
                .validate()
                .iter()
                .any(|e| e.contains("undeclared field"))
        );

        let state_invariant = FieldContract::new()
            .state("a")
            .invariant(Invariant::at_most_one(["a"], Outcome::Post));
        assert!(
            state_invariant
                .validate()
                .iter()
                .any(|e| e.contains("state field"))
        );
    }

    #[test]
    fn baseline_coverage_checks_declared_and_strict() {
        let contract = FieldContract::new().effect("a", Expect::Atomic).strict();
        let baseline = Observation::new().with("a", false);
        assert!(contract.check_baseline(&baseline).is_empty());
        let missing = Observation::new();
        assert_eq!(contract.check_baseline(&missing).len(), 1);
        let extra = Observation::new().with("a", false).with("b", true);
        assert!(
            contract
                .check_baseline(&extra)
                .iter()
                .any(|e| e.contains("not declared"))
        );
    }
}

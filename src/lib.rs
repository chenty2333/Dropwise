//! Dropwise: cancellation-correctness checking for async Rust.
//!
//! A scenario marks futures as cancellation targets with [`Ctx::target`] or,
//! inside the program's own `select!`, with [`Ctx::race`]. Dropwise re-runs the
//! scenario under every cancellation plan (which targets lose, after which
//! observed `Pending`, and when the competing branch becomes ready), lets
//! cancelled work settle, and then checks
//!
//! * the scenario's invariant and its [`Ctx::after_settle`] checks,
//! * the obligation ledger: every [`Obligation`] dropped unresolved, or still
//!   unresolved after settling, and
//! * for [`explore_observed`], the scenario's returned value against a
//!   calibration of the same scenario.
//!
//! Plans enumerate `Pending` boundaries of the marked futures for one input and
//! one (mostly reproducible) schedule; they are not source-level `.await`s and
//! not a proof over all executions.
//!
//! # Start here
//!
//! Add the crate as a development dependency (this checkout is not published),
//! write one target, then run `cargo test`:
//!
//! ```
//! use dropwise::{Ctx, assert_cancel_correct};
//!
//! assert_cancel_correct(|ctx: Ctx| async move {
//!     // The marked operation. In a real scenario this is your request; the
//!     // code after it is the caller/survivor on the same handle or pool.
//!     let completed = ctx
//!         .target(async {
//!             tokio::task::yield_now().await;
//!             "reply"
//!         })
//!         .await;
//!     if completed.is_none() {
//!         // The target future was dropped. Do explicit recovery here.
//!     }
//!     Ok(())
//! });
//! ```
//!
//! Runnable, in-process examples: `quickstart` (worker loop, buggy and fixed),
//! `direct_handle` (long-lived handle outside the target), `pooled_handoff`
//! (one-connection pool and its next borrower), and `limits` (unknown
//! calibration, missing cancellation point, truncated search). The user guide
//! with the full shortest path is `docs/using-dropwise.md`; the small scenario
//! helpers live in [`support`].
//!
//! # Observed outcomes
//!
//! [`explore_observed`] calibrates the same scenario by dropping the cut targets
//! before their first poll (`pre`) and by cancelling nothing (`post`), plus
//! every skipped/completed mixture for a plan with several cuts, then classifies
//! each trial as [`Verdict::Pre`], [`Verdict::Post`], [`Verdict::Mixed`],
//! [`Verdict::Neither`] or [`Verdict::Unknown`] under the caller's [`Expect`].
//! Calibration is on demand and cached. A calibration run that fails or leaks is
//! recorded in [`Report::calibration_errors`]; disagreeing samples are
//! [`Verdict::Unknown`]; neither is a violation. The optional
//! [`Config::repeat_at_site`] and [`Config::gap_choices`] dimensions cover
//! repeated cancellation at one call site and controllable waits before reuse;
//! [`Ctx::gap`] provides the wait itself.
//!
//! The caller declares the contract with [`Expect`]; it is not inferred. `pre`
//! means the operation never started, `post` means it completed, and `Unknown`
//! means the calibration could not decide. [`Report::coverage_gaps`] lists the
//! reasons a clean-looking run is not a conclusion; use
//! [`Report::is_clean_within_coverage`] (what [`assert_cancel_correct`] and
//! [`assert_observed`] check) instead of treating [`Report::is_clean`] as proof.
//!
//! # Field contracts
//!
//! [`explore_fields`] replaces the single blob with an [`Observation`] of named
//! fields and a declared [`FieldContract`].  Each effect field is judged
//! against its own pre/post calibration; a state field must agree across both
//! and hold in the trial.  Per-field [`Expect`] and cross-field [`Invariant`]s
//! let a legitimate per-operation mixture be expressed without inventing a
//! combined value.  This is not a drop-in replacement for the mixed verdict:
//! each effect needs its own field, and a fused aggregate may be `Neither` on a
//! legitimate one-done/one-skipped execution because mixed calibration is not
//! sampled here.  [Report::field_coverage_gaps] and [Report::contract_errors]
//! keep undecidable fields and contract problems visible, and
//! [`assert_fields`]/[`replay_fields`] mirror the observed entry points.
//!
//! # Reports and replay
//!
//! The [`Report`] `Display` output prints violations, baseline/calibration problems, and
//! coverage gaps concisely; [`Report::diagnostics`] prints every trial with its
//! plan and observation/calibration comparison. Each [`Trial`] records its
//! cancellation plan, call sites and outcomes. [`Trial::replay_plan`] returns a
//! [`Plan`] for [`replay`], [`replay_observed`] or [`replay_fields`], which
//! reattempt that exact plan with the same contract, calibration, settling and
//! coverage checks.
//! Malformed or unsupported plans are rejected with [`ReplayError`]. Replay is
//! an attempt, not a deterministic rerun: real I/O can move the target site or
//! the `Pending` boundary, and the report says `Unrealized` rather than silently
//! doing a different experiment.
//!
//! # Optional network feature
//!
//! With the optional `net` feature (Linux), `dropwise::net::Proxy` counts
//! forwarded bytes and its `residue` method reports unread client bytes without
//! consuming them. `start_with` can also hold replies for a real-time delay,
//! which makes the waiting operation return `Pending` without changing the
//! server. It is off by default; `residue` is Linux-only and returns
//! `Unsupported` elsewhere.
//!
//! The crate requires Rust 1.88 (verified by building and testing with that
//! toolchain).

mod explore;
pub mod fields;
pub mod models;
#[cfg(feature = "net")]
pub mod net;
mod obligation;
pub mod support;

pub use explore::{
    Config, Ctx, Cut, CutOutcome, Expect, Flavor, Gap, GapWait, Plan, Preempt, Race, Raced,
    ReplayError, Report, Settle, Target, Trial, Verdict, assert_cancel_correct, assert_fields,
    assert_observed, explore, explore_fields, explore_observed, replay, replay_fields,
    replay_observed,
};
pub use fields::{
    Field, FieldContract, FieldEvaluation, FieldJudgment, FieldKind, FieldStatus, Invariant,
    InvariantEvaluation, InvariantResult, Observation, Outcome, Value,
};
pub use obligation::{Leak, LeakKind, Obligation};

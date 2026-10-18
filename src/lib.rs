//! Dropwise: cancellation-correctness checking for async Rust.
//!
//! v0 is the dynamic half of the project: *systematic cancellation injection*.
//! A scenario marks futures as cancellation targets with [`Ctx::target`] or,
//! inside the program's own `select!`, with [`Ctx::race`]. Dropwise re-runs the
//! scenario under every cancellation plan (which targets lose, after which
//! observed `Pending`, and when the competing branch becomes ready), lets
//! cancelled work settle, and then checks
//!
//! * the scenario's invariant and its [`Ctx::after_settle`] checks, and
//! * the obligation ledger: every [`Obligation`] dropped unresolved, or still
//!   unresolved after settling.
//!
//! Plans enumerate `Pending` boundaries of the marked futures for one input and
//! one (mostly reproducible) schedule; they are not source-level `.await`s and
//! not a proof over all executions.
//!
//! [`explore_observed`] is for scenarios that return a value to judge. It
//! calibrates the same scenario by dropping the cut targets before their first
//! poll (`pre`) and by cancelling nothing (`post`), plus every skipped/completed
//! mixture for a plan with several cuts, then classifies each trial as
//! [`Verdict::Pre`], [`Verdict::Post`], [`Verdict::Mixed`], [`Verdict::Neither`]
//! or [`Verdict::Unknown`] under the caller's [`Expect`]. Calibration is on demand
//! and cached; disagreeing samples are `Unknown`, never a violation. The
//! optional [`Config::repeat_at_site`] and [`Config::gap_choices`] dimensions
//! cover repeated cancellation at one call site and controllable waits before
//! reuse; `Ctx::gap` provides the wait itself.
//!
//! With the optional `net` feature (Linux), `dropwise::net::Proxy` counts
//! forwarded bytes and its `residue` method reports unread client bytes without
//! consuming them. `Proxy::start_with` can also hold replies for a real-time
//! delay, which makes the waiting operation return `Pending` without changing
//! the server. It is off by default.
//!
//! `cargo run --example quickstart` walks through one buggy and one corrected
//! shape with the report printed; [`Report`] documents which signals mean what.

mod explore;
pub mod models;
#[cfg(feature = "net")]
pub mod net;
mod obligation;

pub use explore::{
    Config, Ctx, Cut, CutOutcome, Expect, Flavor, Gap, GapWait, Preempt, Race, Raced, Report,
    Settle, Target, Trial, Verdict, assert_cancel_correct, assert_observed, explore,
    explore_observed,
};
pub use obligation::{Leak, LeakKind, Obligation};

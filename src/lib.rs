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

mod explore;
pub mod models;
mod obligation;

pub use explore::{
    assert_cancel_correct, explore, Config, Ctx, Cut, CutOutcome, Flavor, Preempt, Race, Raced, Report,
    Settle, Target, Trial,
};
pub use obligation::{Leak, LeakKind, Obligation};

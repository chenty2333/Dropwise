//! Online hand-off guards: responsibility ledgers, quarantine and recovery for
//! resources that outlive cancelled operations.
//!
//! This crate is the online counterpart of the offline Dropwise engine.  The
//! offline engine enumerates cancellation points and compares an execution with
//! calibrated `pre`/`post` outcomes.  This crate is what a library, pool or
//! runtime can enforce *at the hand-off boundary itself*: a cancelled operation
//! leaves a duty behind, the resource is quarantined, and only an explicit
//! protocol-level recovery may make it reusable again.
//!
//! * [`guard`] — identity, epoch, open duties, exclusive recovery ownership.
//! * [`features`] — the observation vocabulary shared by the online policy and
//!   the offline rule learner.
//! * `recovery` — quarantine queue + worker with deadlines, retry, shutdown.
//! * `pool` — a real async pool whose return path cannot validate a suspect
//!   object and whose checkout path checks the ledger before running a probe.
//! * `policy` / `rules` — hand-written and offline-learned decision rules.
//! * `proto` — protocol adapters (framed request/reply, PostgreSQL sessions,
//!   multiplexed streams) that mint settlement and recovery witnesses.

pub mod features;
pub mod guard;
pub mod lab;
pub mod policy;
pub mod pool;
pub mod proto;
pub mod recovery;

pub use features::{Features, Value};
pub use guard::{
    Cause, Discharge, DischargeKind, Epoch, Guard, GuardStats, ObjId, Observation, Phase,
    Qualification, RecoveryError, RecoveryKind, RecoveryLimits, RecoveryPermit, RecoveryWitness,
    Refusal, SettleError, Suspect, Ticket,
};
pub use policy::{Plan, RecoveryPolicy, RuleTable};
pub use pool::{
    GuardedSession, Lease, Manager, Pool, PoolConfig, PoolSnapshot, ShutdownReport, ValidateOutcome,
};
pub use recovery::{EffectContract, PoolError, RecoveryAttempt, RecoveryContext};

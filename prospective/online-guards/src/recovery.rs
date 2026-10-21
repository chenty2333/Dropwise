//! Recovery vocabulary: what a protocol adapter is allowed to do after an
//! abandoned operation, and what it must prove before the object is reusable.

use crate::features::Features;
use crate::guard::{Cause, Epoch, ObjId, RecoveryLimits, RecoveryWitness};

/// What is known about the *effect* of the abandoned operation, as declared by
/// the API whose operation was abandoned.  This is not a property of the wire
/// protocol; it decides which recovery actions are permitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectContract {
    /// The operation may already have applied, and the caller is told the
    /// outcome is unknown.  Completing a request that was only partially
    /// written is permitted, because "applied" is inside the allowed outcome
    /// set.  It is still not a rollback and does not authorise a retry.
    AtomicEffectMayApply,
    /// The caller declared that a cancelled operation must not apply.  A
    /// partially written request cannot be completed; the adapter may still
    /// drain an already-written request whose effect may have happened, but it
    /// must not send any remaining bytes that would cause the effect.
    MustNotApply,
}

/// Facts for one recovery attempt.  The object and epoch are the ones being
/// recovered; a witness minted for a different epoch is rejected by the ledger.
#[derive(Clone, Debug)]
pub struct RecoveryContext {
    pub obj: ObjId,
    pub epoch: Epoch,
    pub attempt: u32,
    pub plan: &'static str,
    pub limits: RecoveryLimits,
    pub features: Features,
    /// Time left before the object's quarantine deadline.  Adapters use it to
    /// bound drains rather than inventing an independent timeout.
    pub remaining: std::time::Duration,
    /// Whether the abandoned operation was observed to leave a complete request
    /// on the wire, as far as the adapter tracked it.  Informational: the
    /// adapter must re-check its own state.
    pub write_state: &'static str,
    pub read_state: &'static str,
    /// Why the object was abandoned.  An I/O failure means the socket may be
    /// dead even when no byte was counted; `discard_local` must not certify it.
    pub cause: Cause,
    /// The effect contract recorded when the *abandoned operation* armed, not
    /// the connection's current builder setting.
    pub contract: EffectContract,
}

impl RecoveryContext {
    /// Mint the witness for a successful attempt after the adapter has checked
    /// the protocol state.  `evidence` is a short protocol fact, not a guess.
    pub fn witness(&self, plan: &'static str, evidence: &'static str) -> RecoveryWitness {
        RecoveryWitness::new(self.obj, self.epoch, plan, evidence)
    }
}

impl RecoveryContext {
    /// A plan name that includes the attempt, for adapters that want to keep
    /// first-attempt and retry evidence distinct in logs.
    pub fn attempt_plan(&self) -> &'static str {
        self.plan
    }
}

/// Result of one recovery attempt.
#[derive(Debug)]
pub enum RecoveryAttempt {
    /// The protocol observed a clean state; the object may be reused.
    Recovered(RecoveryWitness),
    /// No protocol path can make this object reusable.
    Failed(Cause),
    /// This attempt did not settle the question (deadline, partial frame).  The
    /// object stays quarantined and may be retried within its limits.
    Unresolved(Cause),
}

impl RecoveryAttempt {
    pub fn recovered(ctx: &RecoveryContext, evidence: &'static str) -> Self {
        RecoveryAttempt::Recovered(ctx.witness(ctx.plan, evidence))
    }
}

/// Why an acquisition failed.  Kept separate from protocol/business errors so
/// that availability cost is measurable.
#[derive(Debug)]
pub enum PoolError {
    Closed,
    Connect(std::io::Error),
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Closed => f.write_str("pool is closed"),
            PoolError::Connect(e) => write!(f, "connect failed: {e}"),
        }
    }
}

impl std::error::Error for PoolError {}

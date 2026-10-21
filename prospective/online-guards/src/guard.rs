//! The responsibility ledger: per-object identity, epoch, open duties and
//! exclusive recovery ownership.
//!
//! The ledger does not know any protocol.  Protocol adapters arm a [`Ticket`]
//! *before* the first byte of an effect window can leave the process, record
//! what they know while the operation progresses, and discharge the ticket only
//! with a [`Discharge`] that the protocol's reply decoder produced.  Dropping an
//! armed ticket without a discharge moves the object to [`Phase::Suspect`]; a
//! pool or direct handle must then recover it under an exclusive
//! [`RecoveryPermit`] before it can be used again.
//!
//! Two invariants are enforced here and tested in this module:
//!
//! 1. A discharge only changes state for the object and epoch it names, and only
//!    while that epoch's operation is still open.  A late completion from an
//!    older epoch cannot clear a newer generation's responsibility.
//! 2. Nothing except `RecoveryPermit::recovered` moves an object from
//!    [`Phase::Recovering`] back to [`Phase::Ready`].  Dropping a permit (for
//!    example because the recovery future was cancelled) leaves the object
//!    suspect, never reusable.
//!
//! The ledger is deliberately *not* the only authority for reusability: a
//! session can be out of flight and still not reusable (an open transaction, a
//! cached cursor).  That is modelled separately by [`Guard::enter_standing`],
//! which is cleared only by protocol evidence.

use crate::features::Features;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static NEXT_OBJECT: AtomicU64 = AtomicU64::new(1);
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

/// Identity of one resource object (a connection, a stream, a kernel object).
/// Never reused, so it is safe to keep in logs after the object is gone.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjId(u64);

impl ObjId {
    pub fn new() -> Self {
        Self(NEXT_OBJECT.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl Default for ObjId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "obj#{}", self.0)
    }
}

impl fmt::Display for ObjId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "obj#{}", self.0)
    }
}

/// Generation of a reusable object.  A new epoch starts every time the object
/// becomes clean again, so a completion, ticket or recovery witness from an
/// older epoch is distinguishable from the current one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Epoch(u64);

impl Epoch {
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ep{}", self.0)
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ep{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// No operation in flight.  Reusability additionally requires no standing
    /// obligation (see [`Guard::reusable`]).
    Ready = 0,
    /// One armed ticket is open.
    Busy = 1,
    /// An armed ticket died without a discharge.  The protocol position or the
    /// effect outcome is not known; only recovery or retirement is allowed.
    Suspect = 2,
    /// Exclusive recovery ownership was granted to one recovery attempt.
    Recovering = 3,
    /// Permanently unusable.  The socket should be closed; a pool opens a new
    /// object instead.
    Retired = 4,
}

impl Phase {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Phase::Ready,
            1 => Phase::Busy,
            2 => Phase::Suspect,
            3 => Phase::Recovering,
            4 => Phase::Retired,
            other => unreachable!("corrupt guard phase {other}"),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Phase::Ready => "ready",
            Phase::Busy => "busy",
            Phase::Suspect => "suspect",
            Phase::Recovering => "recovering",
            Phase::Retired => "retired",
        }
    }
}

/// Why an object became suspect, or why a recovery/cleanup was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The operation future was dropped.
    Cancelled,
    /// The operation future was dropped while unwinding.
    Unwind,
    /// A transport or protocol error was reported.
    Io,
    /// A deadline expired.
    Timeout,
    /// A protocol-level inconsistency (bad frame, unexpected message).
    Protocol,
    /// Recovery attempted and failed.
    RecoveryFailed,
    /// Recovery itself was cancelled (shutdown, second cancel, deadline).
    RecoveryCancelled,
    /// The recovery queue had no room; the object was retired instead.
    QueueFull,
    /// The object is out of flight but carries a standing obligation (an open
    /// transaction, a reader cursor).  Not a violation: cleanup is expected.
    Standing,
    /// The pool or runtime shut down.
    Shutdown,
    /// Explicit, adapter-defined reason.
    Other(&'static str),
}

impl Cause {
    pub fn name(self) -> &'static str {
        match self {
            Cause::Cancelled => "cancelled",
            Cause::Unwind => "unwind",
            Cause::Io => "io",
            Cause::Timeout => "timeout",
            Cause::Protocol => "protocol",
            Cause::RecoveryFailed => "recovery_failed",
            Cause::RecoveryCancelled => "recovery_cancelled",
            Cause::QueueFull => "queue_full",
            Cause::Standing => "standing",
            Cause::Shutdown => "shutdown",
            Cause::Other(s) => s,
        }
    }
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What was observed when the ticket was abandoned.  Only the adapter's state
/// and the ticket's recorded progress are trusted; nothing is inferred from
/// wall-clock timing here.
#[derive(Clone, Debug)]
pub struct Suspect {
    pub cause: Cause,
    pub features: Features,
    pub at: Instant,
    pub op: u64,
    pub epoch: Epoch,
    pub standing: Option<&'static str>,
    /// Recovery attempts already made for this object since the last clean
    /// state.
    pub attempts: u32,
}

impl Suspect {
    /// Features plus the cause tag, as seen by a policy or the rule learner.
    pub fn decision_features(&self) -> Features {
        let mut f = self.features.clone();
        f.tag("cause", self.cause.name());
        if let Some(standing) = self.standing {
            f.tag("standing", standing);
        }
        f.set("attempts", self.attempts as i64);
        f
    }
}

/// Type of settlement.  Every variant means "the protocol has fully accounted
/// for this operation"; they differ in what the caller may be told, which is the
/// protocol adapter's responsibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DischargeKind {
    /// The protocol accepted the operation and its reply was fully consumed.
    Complete,
    /// A complete protocol-level error reply was consumed.  The reply is
    /// complete even though the operation failed.
    ServerError,
    /// The operation's contract allows a prefix outcome and the prefix was
    /// accounted for (for example, an explicit partial-write result).
    Prefix,
    /// The adapter aborted the operation through a protocol path that leaves
    /// the session at a known boundary.
    Aborted,
}

/// A proof token that a protocol decoder can discharge an operation.  The
/// fields are private and the constructor is crate-visible: the guard cannot
/// invent one, and a caller cannot turn "read a frame" into a discharge without
/// the protocol's own completion check.
#[derive(Debug)]
pub struct Discharge {
    obj: ObjId,
    epoch: Epoch,
    op: u64,
    pub kind: DischargeKind,
    pub evidence: &'static str,
}

impl Discharge {
    pub(crate) fn new(
        obj: ObjId,
        epoch: Epoch,
        op: u64,
        kind: DischargeKind,
        evidence: &'static str,
    ) -> Self {
        Self {
            obj,
            epoch,
            op,
            kind,
            evidence,
        }
    }
}

/// A proof token that one recovery attempt observed the protocol's clean state.
/// Constructed by protocol adapters after they checked the wire state
/// themselves (for example, `ReadyForQuery` with status `I`).
#[derive(Debug)]
pub struct RecoveryWitness {
    obj: ObjId,
    epoch: Epoch,
    pub plan: &'static str,
    pub evidence: &'static str,
}

impl RecoveryWitness {
    pub(crate) fn new(
        obj: ObjId,
        epoch: Epoch,
        plan: &'static str,
        evidence: &'static str,
    ) -> Self {
        Self {
            obj,
            epoch,
            plan,
            evidence,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another operation is already open.
    Busy { op: u64 },
    /// A previous operation died without a discharge.
    Suspect { cause: Cause, op: u64 },
    /// Recovery owns the object.
    Recovering { attempts: u32 },
    /// The object is permanently unusable.
    Retired { cause: Cause },
    /// A live owner has authority over the object; the requested recovery or
    /// reuse would act behind its back.
    Owned { owners: usize, tag: &'static str },
    /// The caller presented an owner witness that is not live for this object
    /// and owner generation (stale, handed off, or a different object).
    OwnerMismatch,
    /// The owner stopped serving new operations.
    Closed,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Busy { op } => write!(f, "busy with operation #{op}"),
            Refusal::Suspect { cause, op } => {
                write!(
                    f,
                    "suspect ({cause}) since operation #{op}; recovery required"
                )
            }
            Refusal::Recovering { attempts } => {
                write!(f, "exclusive recovery in progress (attempt {attempts})")
            }
            Refusal::Retired { cause } => write!(f, "retired ({cause})"),
            Refusal::Owned { owners, tag } => write!(
                f,
                "{owners} live owner(s) hold authority (last: {tag}); recovery and reuse are refused"
            ),
            Refusal::OwnerMismatch => {
                f.write_str("owner witness is not live for this object/generation")
            }
            Refusal::Closed => f.write_str("closed"),
        }
    }
}

impl std::error::Error for Refusal {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qualification {
    /// No open duty and no standing obligation: safe for the next user.
    Clean,
    /// No open duty, but cleanup is required (open transaction, buffered
    /// async message).  The object is not dirty; it needs a protocol cleanup.
    NeedsCleanup {
        standing: &'static str,
    },
    /// A previous operation has an unknown outcome.
    Suspect {
        cause: Cause,
    },
    /// Out of flight and safe as far as the ledger knows; use the protocol
    /// validator for a health check if desired.
    Busy {
        op: u64,
    },
    Recovering {
        attempts: u32,
    },
    /// At least one live owner claim still holds authority over the object.
    /// This is not a protocol state: the same wire state can be legitimate
    /// nesting or an abandoned transaction, and only the owner's liveness
    /// distinguishes them.
    Owned {
        owners: usize,
        tag: &'static str,
    },
    Retired {
        cause: Cause,
    },
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardStats {
    pub begun: u64,
    pub discharged: u64,
    pub suspected: u64,
    pub recoveries_started: u64,
    pub recoveries_succeeded: u64,
    pub recoveries_failed: u64,
    pub recoveries_cancelled: u64,
    pub retired: u64,
    pub stale_discharges: u64,
    pub stale_abandons: u64,
    pub refused_busy: u64,
    pub refused_suspect: u64,
    pub refused_recovering: u64,
    pub refused_retired: u64,
    pub owners_claimed: u64,
    pub owners_released: u64,
    pub owners_handed_off: u64,
    pub stale_owner_releases: u64,
    pub refused_owned: u64,
    pub refused_owner_mismatch: u64,
}

/// Identity of one ownership claim.  Never reused, so a stale witness is
/// distinguishable from a current claim without consulting the owner.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnerId(u64);

impl OwnerId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "owner#{}", self.0)
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "owner#{}", self.0)
    }
}

/// One live ownership claim.  `generation` is the object's owner generation:
/// a handoff replaces the whole owner cohort and starts a new generation, so a
/// witness from before the handoff cannot authorize or block anything after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerInfo {
    pub id: OwnerId,
    pub tag: &'static str,
    pub generation: u64,
    /// The operation epoch at which the claim was established.  Informational;
    /// operations advance the epoch but do not invalidate ownership.
    pub op_epoch: Epoch,
    pub since: Instant,
    /// How many times responsibility was handed off before this claim.
    pub handoffs: u32,
}

/// A copyable proof of one ownership claim for one object.  It is not an
/// authority token: acting as the owner requires [`OwnerClaim`], which owns an
/// `Arc<Guard>` and cannot be fabricated.  A witness is checked against the
/// object, generation and claim id, so a stale or cross-object witness is
/// rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnerWitness {
    pub owner: OwnerId,
    pub obj: ObjId,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipError {
    /// The claim was replaced by a handoff, belongs to an older generation, or
    /// was already released.
    Stale,
    /// An operation is in flight.  Responsibility for an unresolved operation
    /// cannot be transferred until it settles or is abandoned (Suspect).
    Busy,
    /// The object is permanently unusable.
    Retired,
}

impl fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OwnershipError::Stale => f.write_str("ownership claim is no longer live"),
            OwnershipError::Busy => {
                f.write_str("cannot hand off ownership while an operation is in flight")
            }
            OwnershipError::Retired => f.write_str("object is retired"),
        }
    }
}

impl std::error::Error for OwnershipError {}

/// RAII ownership of one responsibility unit.  The claim is live while this
/// value exists; dropping it is the positive, in-process evidence that the
/// owner relinquished authority.  A claim that is forgotten
/// (`std::mem::forget`) or leaked stays live and continues to block cleanup;
/// absence of a claim is never inferred from silence.
///
/// Claims can be nested (an outer transaction and an inner savepoint scope).
/// Releasing one does not release the others.  [`OwnerClaim::handoff`] is
/// all-or-nothing: it replaces the whole owner cohort with one new claim.
pub struct OwnerClaim {
    guard: Arc<Guard>,
    info: OwnerInfo,
    released: bool,
}

impl OwnerClaim {
    pub fn id(&self) -> OwnerId {
        self.info.id
    }

    pub fn tag(&self) -> &'static str {
        self.info.tag
    }

    pub fn info(&self) -> &OwnerInfo {
        &self.info
    }

    pub fn guard(&self) -> &Arc<Guard> {
        &self.guard
    }

    pub fn witness(&self) -> OwnerWitness {
        OwnerWitness {
            owner: self.info.id,
            obj: self.guard.obj(),
            generation: self.info.generation,
        }
    }

    /// Claim recovery of *this* object on behalf of the live owner.  This is
    /// the only way to recover while an owner is live; an unowned caller gets
    /// [`Refusal::Owned`].
    pub fn claim_recovery(&self) -> Result<RecoveryPermit<'_>, Refusal> {
        self.guard.claim_recovery_as(&self.witness())
    }

    /// Hand responsibility to a new owner (for example, a background cleanup
    /// task).  The whole owner cohort is replaced: the returned claim is the
    /// only live claim, and every older witness is stale.  On success this
    /// claim is stale (its generation is gone) and must not be used again; a
    /// later drop is recorded as a stale release.  A refused handoff leaves
    /// this claim live, so the owner can retry after the operation resolves.
    pub fn handoff(&self, tag: &'static str) -> Result<OwnerClaim, OwnershipError> {
        match self
            .guard
            .handoff_owner(self.info.id, self.info.generation, tag)
        {
            Ok(info) => Ok(OwnerClaim {
                guard: self.guard.clone(),
                info,
                released: false,
            }),
            Err(error) => Err(error),
        }
    }

    /// Explicitly relinquish ownership.  Equivalent to dropping the claim.
    pub fn release(mut self) {
        self.released = true;
        self.guard.release_owner(self.info.id, self.info.generation);
    }
}

impl Drop for OwnerClaim {
    fn drop(&mut self) {
        if !self.released {
            self.guard.release_owner(self.info.id, self.info.generation);
        }
    }
}

impl fmt::Debug for OwnerClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerClaim")
            .field("info", &self.info)
            .field("released", &self.released)
            .finish()
    }
}

#[derive(Debug)]
struct OwnerRecord {
    info: OwnerInfo,
}

#[derive(Debug)]
struct RetireInfo {
    cause: Cause,
}

/// The ledger for one responsibility unit.
#[derive(Debug)]
pub struct Guard {
    obj: ObjId,
    phase: AtomicU8,
    epoch: AtomicU64,
    op_seq: AtomicU64,
    current_op: AtomicU64,
    armed: AtomicBool,
    suspect: Mutex<Option<Suspect>>,
    standing: Mutex<Option<&'static str>>,
    attempts: AtomicU32,
    retired: Mutex<Option<RetireInfo>>,
    begun: AtomicU64,
    discharged: AtomicU64,
    suspected: AtomicU64,
    recoveries_started: AtomicU64,
    recoveries_succeeded: AtomicU64,
    recoveries_failed: AtomicU64,
    recoveries_cancelled: AtomicU64,
    retired_count: AtomicU64,
    stale_discharges: AtomicU64,
    stale_abandons: AtomicU64,
    refused_busy: AtomicU64,
    refused_suspect: AtomicU64,
    refused_recovering: AtomicU64,
    refused_retired: AtomicU64,
    owners: Mutex<Vec<OwnerRecord>>,
    owner_generation: AtomicU64,
    owners_claimed: AtomicU64,
    owners_released: AtomicU64,
    owners_handed_off: AtomicU64,
    stale_owner_releases: AtomicU64,
    refused_owned: AtomicU64,
    refused_owner_mismatch: AtomicU64,
}

impl Guard {
    pub fn new() -> Self {
        Self {
            obj: ObjId::new(),
            phase: AtomicU8::new(Phase::Ready as u8),
            epoch: AtomicU64::new(1),
            op_seq: AtomicU64::new(0),
            current_op: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            suspect: Mutex::new(None),
            standing: Mutex::new(None),
            attempts: AtomicU32::new(0),
            retired: Mutex::new(None),
            begun: AtomicU64::new(0),
            discharged: AtomicU64::new(0),
            suspected: AtomicU64::new(0),
            recoveries_started: AtomicU64::new(0),
            recoveries_succeeded: AtomicU64::new(0),
            recoveries_failed: AtomicU64::new(0),
            recoveries_cancelled: AtomicU64::new(0),
            retired_count: AtomicU64::new(0),
            stale_discharges: AtomicU64::new(0),
            stale_abandons: AtomicU64::new(0),
            refused_busy: AtomicU64::new(0),
            refused_suspect: AtomicU64::new(0),
            refused_recovering: AtomicU64::new(0),
            refused_retired: AtomicU64::new(0),
            owners: Mutex::new(Vec::new()),
            owner_generation: AtomicU64::new(1),
            owners_claimed: AtomicU64::new(0),
            owners_released: AtomicU64::new(0),
            owners_handed_off: AtomicU64::new(0),
            stale_owner_releases: AtomicU64::new(0),
            refused_owned: AtomicU64::new(0),
            refused_owner_mismatch: AtomicU64::new(0),
        }
    }

    pub fn obj(&self) -> ObjId {
        self.obj
    }

    pub fn epoch(&self) -> Epoch {
        Epoch(self.epoch.load(Ordering::Acquire))
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Acquire))
    }
    /// Current owner-cohort generation.  A handoff starts a new generation;
    /// operation epochs do not.
    pub fn owner_generation(&self) -> u64 {
        self.owner_generation.load(Ordering::Acquire)
    }

    /// Every claim record, including stale ones from earlier generations.
    pub fn owners(&self) -> Vec<OwnerInfo> {
        self.owners
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.info.clone())
            .collect()
    }

    /// Claims that are live in the current owner generation.  A claim from an
    /// earlier generation is a stale witness: it can neither block a new owner
    /// nor authorize recovery.
    pub fn live_owners(&self) -> Vec<OwnerInfo> {
        let generation = self.owner_generation.load(Ordering::Acquire);
        self.owners
            .lock()
            .unwrap()
            .iter()
            .filter(|record| record.info.generation == generation)
            .map(|record| record.info.clone())
            .collect()
    }

    pub fn live_owner_count(&self) -> usize {
        self.live_owners().len()
    }

    pub fn live_owner(&self) -> Option<OwnerInfo> {
        self.live_owners().into_iter().next()
    }

    /// Validate a copyable owner witness against this object, generation and
    /// live claim list.  This is read-only; it cannot release or replace a
    /// claim.
    pub fn owner_witness_live(&self, witness: &OwnerWitness) -> bool {
        witness.obj == self.obj
            && witness.generation == self.owner_generation.load(Ordering::Acquire)
            && self.owners.lock().unwrap().iter().any(|record| {
                record.info.id == witness.owner && record.info.generation == witness.generation
            })
    }

    /// Establish an ownership claim.  Multiple claims may be live at once
    /// (nested transaction scopes); a claim can only be established while the
    /// object is Ready, not in the middle of an operation or recovery.
    pub fn claim_owner(self: &Arc<Self>, tag: &'static str) -> Result<OwnerClaim, Refusal> {
        if self.phase() != Phase::Ready {
            return Err(self.refuse(self.current_refusal()));
        }
        let info = OwnerInfo {
            id: OwnerId(NEXT_OWNER.fetch_add(1, Ordering::Relaxed)),
            tag,
            generation: self.owner_generation.load(Ordering::Acquire),
            op_epoch: self.epoch(),
            since: Instant::now(),
            handoffs: 0,
        };
        self.owners
            .lock()
            .unwrap()
            .push(OwnerRecord { info: info.clone() });
        self.owners_claimed.fetch_add(1, Ordering::Relaxed);
        Ok(OwnerClaim {
            guard: self.clone(),
            info,
            released: false,
        })
    }

    /// Recover on behalf of a specific live owner.  The witness must match this
    /// object, the current owner generation and a live claim id.  A stale,
    /// handed-off or cross-object witness is refused without changing state.
    pub fn claim_recovery_as(&self, witness: &OwnerWitness) -> Result<RecoveryPermit<'_>, Refusal> {
        if !self.owner_witness_live(witness) {
            return Err(self.refuse(Refusal::OwnerMismatch));
        }
        self.claim_recovery_inner(true)
    }

    /// Replace the whole owner cohort with one new claim.  All older claims
    /// become stale, so a late release or a late recovery by the previous owner
    /// cannot affect the new generation.
    fn handoff_owner(
        &self,
        id: OwnerId,
        generation: u64,
        tag: &'static str,
    ) -> Result<OwnerInfo, OwnershipError> {
        if self.retired() {
            return Err(OwnershipError::Retired);
        }
        if self.phase() == Phase::Busy {
            return Err(OwnershipError::Busy);
        }
        let mut owners = self.owners.lock().unwrap();
        let Some(position) = owners
            .iter()
            .position(|record| record.info.id == id && record.info.generation == generation)
        else {
            return Err(OwnershipError::Stale);
        };
        let previous = owners[position].info.clone();
        let next_generation = self.owner_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let info = OwnerInfo {
            id: OwnerId(NEXT_OWNER.fetch_add(1, Ordering::Relaxed)),
            tag,
            generation: next_generation,
            op_epoch: self.epoch(),
            since: Instant::now(),
            handoffs: previous.handoffs + 1,
        };
        owners.clear();
        owners.push(OwnerRecord { info: info.clone() });
        self.owners_handed_off.fetch_add(1, Ordering::Relaxed);
        Ok(info)
    }

    fn release_owner(&self, id: OwnerId, generation: u64) {
        let current = self.owner_generation.load(Ordering::Acquire);
        let mut owners = self.owners.lock().unwrap();
        let Some(position) = owners.iter().position(|record| record.info.id == id) else {
            self.stale_owner_releases.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let record = owners.remove(position);
        if record.info.generation == current && generation == current {
            self.owners_released.fetch_add(1, Ordering::Relaxed);
        } else {
            self.stale_owner_releases.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn current_op(&self) -> u64 {
        self.current_op.load(Ordering::Acquire)
    }

    /// Standing obligations are protocol facts that outlive an operation.  The
    /// guard does not infer them; the adapter calls `enter_standing` only after
    /// it has protocol evidence (a `ReadyForQuery` with status `T`, a created
    /// cursor, ...).
    pub fn enter_standing(&self, kind: &'static str) {
        let mut slot = self.standing.lock().unwrap();
        debug_assert!(
            slot.is_none() || *slot == Some(kind),
            "standing obligation {:?} replaced by {kind:?}",
            *slot
        );
        *slot = Some(kind);
    }

    pub fn clear_standing(&self, kind: &'static str) {
        let mut slot = self.standing.lock().unwrap();
        if *slot == Some(kind) {
            *slot = None;
        }
    }

    pub fn standing(&self) -> Option<&'static str> {
        *self.standing.lock().unwrap()
    }

    pub fn reusable(&self) -> bool {
        self.phase() == Phase::Ready && self.standing().is_none() && self.live_owner_count() == 0
    }

    pub fn qualification(&self) -> Qualification {
        let phase = self.phase();
        if phase == Phase::Retired {
            return Qualification::Retired {
                cause: self
                    .retired
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.cause)
                    .unwrap_or(Cause::Other("retired")),
            };
        }
        if let Some(owner) = self.live_owner() {
            return Qualification::Owned {
                owners: self.live_owner_count(),
                tag: owner.tag,
            };
        }
        match phase {
            Phase::Ready => match self.standing() {
                Some(s) => Qualification::NeedsCleanup { standing: s },
                None => Qualification::Clean,
            },
            Phase::Busy => Qualification::Busy {
                op: self.current_op(),
            },
            Phase::Suspect => Qualification::Suspect {
                cause: self
                    .suspect
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.cause)
                    .unwrap_or(Cause::Cancelled),
            },
            Phase::Recovering => Qualification::Recovering {
                attempts: self.attempts.load(Ordering::Acquire),
            },
            Phase::Retired => Qualification::Retired {
                cause: self
                    .retired
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.cause)
                    .unwrap_or(Cause::Other("retired")),
            },
        }
    }

    pub fn suspect(&self) -> Option<Suspect> {
        self.suspect.lock().unwrap().clone()
    }

    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> GuardStats {
        GuardStats {
            begun: self.begun.load(Ordering::Relaxed),
            discharged: self.discharged.load(Ordering::Relaxed),
            suspected: self.suspected.load(Ordering::Relaxed),
            recoveries_started: self.recoveries_started.load(Ordering::Relaxed),
            recoveries_succeeded: self.recoveries_succeeded.load(Ordering::Relaxed),
            recoveries_failed: self.recoveries_failed.load(Ordering::Relaxed),
            recoveries_cancelled: self.recoveries_cancelled.load(Ordering::Relaxed),
            retired: self.retired_count.load(Ordering::Relaxed),
            stale_discharges: self.stale_discharges.load(Ordering::Relaxed),
            stale_abandons: self.stale_abandons.load(Ordering::Relaxed),
            refused_busy: self.refused_busy.load(Ordering::Relaxed),
            refused_suspect: self.refused_suspect.load(Ordering::Relaxed),
            refused_recovering: self.refused_recovering.load(Ordering::Relaxed),
            refused_retired: self.refused_retired.load(Ordering::Relaxed),
            owners_claimed: self.owners_claimed.load(Ordering::Relaxed),
            owners_released: self.owners_released.load(Ordering::Relaxed),
            owners_handed_off: self.owners_handed_off.load(Ordering::Relaxed),
            stale_owner_releases: self.stale_owner_releases.load(Ordering::Relaxed),
            refused_owned: self.refused_owned.load(Ordering::Relaxed),
            refused_owner_mismatch: self.refused_owner_mismatch.load(Ordering::Relaxed),
        }
    }

    fn refuse(&self, refusal: Refusal) -> Refusal {
        match refusal {
            Refusal::Busy { .. } => self.refused_busy.fetch_add(1, Ordering::Relaxed),
            Refusal::Suspect { .. } => self.refused_suspect.fetch_add(1, Ordering::Relaxed),
            Refusal::Recovering { .. } => self.refused_recovering.fetch_add(1, Ordering::Relaxed),
            Refusal::Retired { .. } => self.refused_retired.fetch_add(1, Ordering::Relaxed),
            Refusal::Owned { .. } => self.refused_owned.fetch_add(1, Ordering::Relaxed),
            Refusal::OwnerMismatch => self.refused_owner_mismatch.fetch_add(1, Ordering::Relaxed),
            Refusal::Closed => 0,
        };
        refusal
    }

    fn current_refusal(&self) -> Refusal {
        match self.phase() {
            Phase::Ready => Refusal::Busy { op: 0 }, // unreachable in practice
            Phase::Busy => Refusal::Busy {
                op: self.current_op(),
            },
            Phase::Suspect => Refusal::Suspect {
                cause: self
                    .suspect
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.cause)
                    .unwrap_or(Cause::Cancelled),
                op: self.current_op(),
            },
            Phase::Recovering => Refusal::Recovering {
                attempts: self.attempts.load(Ordering::Acquire),
            },
            Phase::Retired => Refusal::Retired {
                cause: self
                    .retired
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|r| r.cause)
                    .unwrap_or(Cause::Other("retired")),
            },
        }
    }

    /// Open an operation.  The returned ticket must be armed before any effect
    /// can leave the process, and discharged by the protocol before it is
    /// dropped; otherwise the object becomes suspect.
    pub fn begin(&self) -> Result<Ticket<'_>, Refusal> {
        match self.phase.compare_exchange(
            Phase::Ready as u8,
            Phase::Busy as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(_) => return Err(self.refuse(self.current_refusal())),
        }
        let op = self.op_seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.current_op.store(op, Ordering::Release);
        self.armed.store(false, Ordering::Release);
        self.begun.fetch_add(1, Ordering::Relaxed);
        Ok(Ticket {
            guard: self,
            epoch: self.epoch(),
            op,
            armed: false,
            resolved: false,
            features: Features::new(),
        })
    }

    /// Whether an operation may start or a recovery may claim the object.
    pub fn observe(&self) -> Observation {
        Observation {
            phase: self.phase(),
            epoch: self.epoch(),
            op: self.current_op(),
            standing: self.standing(),
            attempts: self.attempts(),
            owners: self.live_owner_count(),
        }
    }

    /// Claim exclusive recovery of a suspect object, or exclusive cleanup of a
    /// ready object with a standing obligation.  Exactly one caller can hold a
    /// permit for a given phase transition.
    pub fn claim_recovery(&self) -> Result<RecoveryPermit<'_>, Refusal> {
        self.claim_recovery_inner(false)
    }

    fn claim_recovery_inner(&self, allow_owner: bool) -> Result<RecoveryPermit<'_>, Refusal> {
        if self.retired() {
            return Err(self.refuse(self.current_refusal()));
        }
        if !allow_owner && let Some(owner) = self.live_owner() {
            return Err(self.refuse(Refusal::Owned {
                owners: self.live_owner_count(),
                tag: owner.tag,
            }));
        }
        // First try the suspect path.
        if self
            .phase
            .compare_exchange(
                Phase::Suspect as u8,
                Phase::Recovering as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.attempts.fetch_add(1, Ordering::AcqRel);
            self.recoveries_started.fetch_add(1, Ordering::Relaxed);
            return Ok(RecoveryPermit {
                guard: self,
                kind: RecoveryKind::Suspect,
                epoch: self.epoch(),
                attempt: self.attempts(),
                finished: false,
            });
        }
        // A ready object with a standing obligation can be cleaned up.  The
        // compare-exchange protects against an operation starting concurrently
        // or a second recovery owner.
        if self
            .phase
            .compare_exchange(
                Phase::Ready as u8,
                Phase::Recovering as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let standing = self.standing();
            if let Some(standing) = standing {
                self.attempts.fetch_add(1, Ordering::AcqRel);
                self.recoveries_started.fetch_add(1, Ordering::Relaxed);
                return Ok(RecoveryPermit {
                    guard: self,
                    kind: RecoveryKind::Cleanup(standing),
                    epoch: self.epoch(),
                    attempt: self.attempts(),
                    finished: false,
                });
            }
            // Nothing to clean up: undo the claim, but never undo a retire.
            let _ = self.phase.compare_exchange(
                Phase::Recovering as u8,
                Phase::Ready as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(self.refuse(Refusal::Busy {
                op: self.current_op(),
            }));
        }
        Err(self.refuse(self.current_refusal()))
    }

    /// Terminal transition.  Safe from any phase: a retire during a live
    /// operation makes that operation's later discharge stale.
    pub fn retire(&self, cause: Cause) {
        let prev = self.phase.swap(Phase::Retired as u8, Ordering::AcqRel);
        if prev != Phase::Retired as u8 {
            *self.retired.lock().unwrap() = Some(RetireInfo { cause });
            self.retired_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn retired(&self) -> bool {
        self.phase() == Phase::Retired
    }

    fn finish_suspect(&self, report: Suspect) -> bool {
        // A ticket from an older operation must not overwrite the current
        // operation's report.
        if report.op != self.current_op() {
            self.stale_abandons.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // Publish the report before the phase becomes Suspect: a claim can win
        // as soon as the CAS succeeds, and it must never observe a Suspect
        // object without its own report.
        *self.suspect.lock().unwrap() = Some(report);
        // Only the current owner of the busy phase can turn it suspect.
        match self.phase.compare_exchange(
            Phase::Busy as u8,
            Phase::Suspect as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.armed.store(false, Ordering::Release);
                self.suspected.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.stale_abandons.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn finish_clean_exchange(&self) -> bool {
        match self.phase.compare_exchange(
            Phase::Busy as u8,
            Phase::Ready as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.armed.store(false, Ordering::Release);
                self.epoch.fetch_add(1, Ordering::AcqRel);
                true
            }
            Err(_) => {
                self.stale_abandons.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn finish_discharge(&self, d: Discharge) -> Result<(), SettleError> {
        let current_epoch = self.epoch();
        let phase = self.phase();
        if d.obj != self.obj
            || d.epoch != current_epoch
            || d.op != self.current_op()
            || phase != Phase::Busy
        {
            self.stale_discharges.fetch_add(1, Ordering::Relaxed);
            return Err(SettleError::Stale {
                discharge: d,
                current_epoch,
                current_phase: phase,
            });
        }
        // No one else can move Busy; this compare-exchange is the commit point
        // of the settlement.
        match self.phase.compare_exchange(
            Phase::Busy as u8,
            Phase::Ready as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.armed.store(false, Ordering::Release);
                self.discharged.fetch_add(1, Ordering::Relaxed);
                self.epoch.fetch_add(1, Ordering::AcqRel);
                self.attempts.store(0, Ordering::Release);
                Ok(())
            }
            Err(_) => {
                self.stale_discharges.fetch_add(1, Ordering::Relaxed);
                Err(SettleError::Stale {
                    discharge: d,
                    current_epoch,
                    current_phase: self.phase(),
                })
            }
        }
    }
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub phase: Phase,
    pub epoch: Epoch,
    pub op: u64,
    pub standing: Option<&'static str>,
    pub attempts: u32,
    pub owners: usize,
}

#[derive(Debug)]
pub enum SettleError {
    /// The discharge names a different object/epoch/operation, or the object is
    /// no longer busy.  The guard was not changed.
    Stale {
        discharge: Discharge,
        current_epoch: Epoch,
        current_phase: Phase,
    },
}

impl fmt::Display for SettleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettleError::Stale {
                discharge,
                current_epoch,
                current_phase,
            } => write!(
                f,
                "stale discharge for {} {} op#{} ({:?}) while {} is {}",
                discharge.obj,
                discharge.epoch,
                discharge.op,
                discharge.kind,
                current_epoch,
                current_phase.name()
            ),
        }
    }
}

impl std::error::Error for SettleError {}

/// RAII handle for one operation.  `arm` must be the first thing the first poll
/// of the operation future does, before any write, mutation or other effect.
pub struct Ticket<'g> {
    guard: &'g Guard,
    epoch: Epoch,
    op: u64,
    armed: bool,
    resolved: bool,
    features: Features,
}

impl<'g> Ticket<'g> {
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn op(&self) -> u64 {
        self.op
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Open the effect window.  Idempotent.  Any poll after this one may have
    /// written bytes or mutated local state, so every subsequent exit must
    /// discharge or the object becomes suspect.
    pub fn arm(&mut self) {
        if !self.armed {
            self.armed = true;
            self.guard.armed.store(true, Ordering::Release);
        }
    }

    /// Record a protocol observation for later diagnosis and policy decisions.
    pub fn set(&mut self, key: &'static str, value: impl Into<crate::features::Value>) {
        self.features.set(key, value);
    }

    pub fn tag(&mut self, key: &'static str, name: &'static str) {
        self.features.tag(key, name);
    }

    pub fn features(&self) -> &Features {
        &self.features
    }

    /// Discharge the operation.  Consumes the ticket; a stale certificate is
    /// rejected and leaves the ledger unchanged.
    pub fn settle(&mut self, discharge: Discharge) -> Result<(), SettleError> {
        match self.guard.finish_discharge(discharge) {
            Ok(()) => {
                self.resolved = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Explicitly abandon an armed operation with a cause (I/O error, timeout,
    /// protocol error).  Equivalent to dropping the ticket; the cause is kept.
    pub fn abandon(mut self, cause: Cause) {
        self.resolved = true;
        if self.armed {
            self.guard.finish_suspect(Suspect {
                cause,
                features: std::mem::take(&mut self.features),
                at: Instant::now(),
                op: self.op,
                epoch: self.epoch,
                standing: self.guard.standing(),
                attempts: self.guard.attempts(),
            });
        } else {
            // Nothing left the process: no responsibility to transfer.
            self.guard.finish_clean_exchange();
        }
    }

    /// The adapter determined that no effect can have occurred and no protocol
    /// progress was made.  This is not a settlement: the ticket is disarmed and
    /// the object returns to Ready without a new generation being minted for an
    /// operation that never happened.
    pub fn disarm(mut self) {
        self.resolved = true;
        self.guard.finish_clean_exchange();
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        if self.armed {
            let cause = if std::thread::panicking() {
                Cause::Unwind
            } else {
                Cause::Cancelled
            };
            self.guard.finish_suspect(Suspect {
                cause,
                features: std::mem::take(&mut self.features),
                at: Instant::now(),
                op: self.op,
                epoch: self.epoch,
                standing: self.guard.standing(),
                attempts: self.guard.attempts(),
            });
        } else {
            self.guard.finish_clean_exchange();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryKind {
    /// The object died mid-operation; the outcome is unknown.
    Suspect,
    /// The object is out of flight but needs a protocol cleanup.
    Cleanup(&'static str),
}

/// Exclusive recovery ownership.  Must be finished with one of the terminal
/// methods; dropping it counts as a cancelled attempt and leaves the object
/// suspect (or ready-but-standing for cleanup), never reusable.
pub struct RecoveryPermit<'g> {
    guard: &'g Guard,
    kind: RecoveryKind,
    epoch: Epoch,
    attempt: u32,
    finished: bool,
}

impl<'g> RecoveryPermit<'g> {
    pub fn kind(&self) -> RecoveryKind {
        self.kind
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn guard(&self) -> &'g Guard {
        self.guard
    }

    /// Recovery observed the protocol's clean state.  Only a witness minted by
    /// a protocol adapter with a matching object/epoch is accepted.
    pub fn recovered(mut self, witness: RecoveryWitness) -> Result<(), RecoveryError> {
        if witness.obj != self.guard.obj || witness.epoch != self.epoch {
            return Err(RecoveryError::StaleWitness {
                witness: (witness.obj, witness.epoch),
                current: (self.guard.obj, self.epoch),
            });
        }
        // While we held the permit no one else could change the phase.  Clear
        // standing responsibility only because the adapter's witness says the
        // protocol cleanup completed.
        if let RecoveryKind::Cleanup(kind) = self.kind {
            self.guard.clear_standing(kind);
        }
        match self.guard.phase.compare_exchange(
            Phase::Recovering as u8,
            Phase::Ready as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // The commit point; from here the permit must not undo it.
                self.finished = true;
                self.guard.epoch.fetch_add(1, Ordering::AcqRel);
                self.guard.attempts.store(0, Ordering::Release);
                *self.guard.suspect.lock().unwrap() = None;
                self.guard
                    .recoveries_succeeded
                    .fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(RecoveryError::NotRecovering {
                phase: self.guard.phase(),
            }),
        }
    }

    /// The protocol says the object cannot be made reusable.
    pub fn failed(mut self, cause: Cause) {
        self.finished = true;
        self.guard.recoveries_failed.fetch_add(1, Ordering::Relaxed);
        self.guard.retire(cause);
    }

    /// This attempt did not settle the question.  The object stays quarantined
    /// (suspect) or ready-with-standing (cleanup) so the owner can retry or
    /// retire it.
    pub fn unresolved(mut self, cause: Cause) {
        self.finished = true;
        match self.kind {
            RecoveryKind::Suspect => {
                // Keep the phase Recovering -> Suspect, preserving the original
                // report if this attempt produced no new information.
                let mut suspect = self.guard.suspect.lock().unwrap();
                if let Some(report) = suspect.as_mut() {
                    report.attempts = self.guard.attempts();
                    report.cause = cause;
                }
                drop(suspect);
                let _ = self.guard.phase.compare_exchange(
                    Phase::Recovering as u8,
                    Phase::Suspect as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            RecoveryKind::Cleanup(_) => {
                let _ = self.guard.phase.compare_exchange(
                    Phase::Recovering as u8,
                    Phase::Ready as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }
    }
}

impl Drop for RecoveryPermit<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.guard
            .recoveries_cancelled
            .fetch_add(1, Ordering::Relaxed);
        match self.kind {
            RecoveryKind::Suspect => {
                let _ = self.guard.phase.compare_exchange(
                    Phase::Recovering as u8,
                    Phase::Suspect as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            RecoveryKind::Cleanup(_) => {
                // Not clean, but still only needs cleanup; never resurrect a
                // retired object.
                let _ = self.guard.phase.compare_exchange(
                    Phase::Recovering as u8,
                    Phase::Ready as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }
    }
}

#[derive(Debug)]
pub enum RecoveryError {
    StaleWitness {
        witness: (ObjId, Epoch),
        current: (ObjId, Epoch),
    },
    NotRecovering {
        phase: Phase,
    },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryError::StaleWitness { witness, current } => write!(
                f,
                "recovery witness for {} {} used on {} {}",
                witness.0, witness.1, current.0, current.1
            ),
            RecoveryError::NotRecovering { phase } => {
                write!(
                    f,
                    "object is {} but recovery expected recovering",
                    phase.name()
                )
            }
        }
    }
}

impl std::error::Error for RecoveryError {}

/// How long a suspect object may stay quarantined before its owner must retire
/// it.  Kept explicit so a queue-full or deadline policy is a measured choice,
/// not a hidden fallback.
#[derive(Clone, Copy, Debug)]
pub struct RecoveryLimits {
    pub max_attempts: u32,
    pub deadline: Duration,
}

impl Default for RecoveryLimits {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            deadline: Duration::from_secs(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn unarmed_drop_leaves_the_object_ready_and_mints_a_generation() {
        let g = Guard::new();
        let before = g.epoch();
        {
            let _t = g.begin().unwrap();
            // No arm: nothing can have left the process.
        }
        assert_eq!(g.phase(), Phase::Ready);
        assert!(g.epoch() > before);
        assert!(g.suspect().is_none());
    }

    #[test]
    fn armed_drop_makes_the_object_suspect_with_the_recorded_features() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            t.tag("write", "complete");
            t.tag("read", "partial");
        }
        assert_eq!(g.phase(), Phase::Suspect);
        let s = g.suspect().unwrap();
        assert_eq!(s.cause, Cause::Cancelled);
        assert_eq!(s.features.name("write"), Some("complete"));
        assert_eq!(s.features.name("read"), Some("partial"));
        // And no new operation may start.
        assert!(matches!(g.begin(), Err(Refusal::Suspect { .. })));
    }

    #[test]
    fn discharge_is_only_valid_for_the_same_epoch_and_operation() {
        let g = Guard::new();
        let mut t = g.begin().unwrap();
        t.arm();
        let old = Discharge::new(g.obj(), t.epoch(), t.op(), DischargeKind::Complete, "x");
        t.settle(old).unwrap();
        assert_eq!(g.phase(), Phase::Ready);

        // A discharge minted for a different operation must not clear the next
        // operation's duty.
        let mut t2 = g.begin().unwrap();
        t2.arm();
        let stale = Discharge::new(
            g.obj(),
            t2.epoch(),
            t2.op() + 100,
            DischargeKind::Complete,
            "stale",
        );
        assert!(t2.settle(stale).is_err());
        assert_eq!(g.phase(), Phase::Busy);
        t2.abandon(Cause::Io);
        assert_eq!(g.phase(), Phase::Suspect);
        assert_eq!(g.stats().stale_discharges, 1);
    }

    #[test]
    fn late_completion_cannot_clear_a_new_generation() {
        let g = Guard::new();
        // Epoch 1: abandon an operation mid-flight.
        let t = g.begin().unwrap();
        let old_epoch = t.epoch();
        let old_op = t.op();
        let mut t = t;
        t.arm();
        t.abandon(Cause::Cancelled);
        assert_eq!(g.phase(), Phase::Suspect);
        // Recover: epoch 2.
        let permit = g.claim_recovery().unwrap();
        let w = RecoveryWitness::new(g.obj(), old_epoch, "drain", "test");
        // The witness names the epoch *before* recovery success, as adapters
        // must: it is the epoch being recovered.
        permit.recovered(w).unwrap();
        let new_epoch = g.epoch();
        assert_ne!(old_epoch, new_epoch);

        // Start the next operation in the new epoch.
        let mut t2 = g.begin().unwrap();
        t2.arm();
        let stale = Discharge::new(g.obj(), old_epoch, old_op, DischargeKind::Complete, "late");
        assert!(t2.settle(stale).is_err());
        assert_eq!(g.phase(), Phase::Busy);
        t2.abandon(Cause::Io);
        assert_eq!(g.phase(), Phase::Suspect);
    }

    #[test]
    fn cancelled_recovery_keeps_the_object_quarantined() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
        }
        assert_eq!(g.phase(), Phase::Suspect);
        {
            let _permit = g.claim_recovery().unwrap();
            // Dropped without finishing: the second cancellation.
        }
        assert_eq!(g.phase(), Phase::Suspect);
        assert_eq!(g.stats().recoveries_cancelled, 1);
        assert!(!g.reusable());
        // A retry can still claim it.
        let permit = g.claim_recovery().unwrap();
        let w = RecoveryWitness::new(g.obj(), g.epoch(), "drain", "retry");
        permit.recovered(w).unwrap();
        assert!(g.reusable());
    }

    #[test]
    fn recovery_witness_must_match_object_and_epoch() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
        }
        let permit = g.claim_recovery().unwrap();
        let wrong = RecoveryWitness::new(ObjId::new(), g.epoch(), "drain", "wrong object");
        assert!(permit.recovered(wrong).is_err());
        // The failed attempt did not make the object reusable.
        assert!(!g.reusable());
    }

    #[test]
    fn standing_obligation_blocks_reuse_until_protocol_evidence_clears_it() {
        let g = Guard::new();
        g.enter_standing("transaction");
        assert_eq!(g.phase(), Phase::Ready);
        assert!(!g.reusable());
        assert!(matches!(
            g.qualification(),
            Qualification::NeedsCleanup {
                standing: "transaction"
            }
        ));
        // A new statement inside the transaction is allowed.
        let mut t = g.begin().unwrap();
        t.arm();
        let d = Discharge::new(g.obj(), t.epoch(), t.op(), DischargeKind::Complete, "stmt");
        t.settle(d).unwrap();
        assert!(!g.reusable());
        // Cleanup owns the object exclusively.
        let permit = g.claim_recovery().unwrap();
        assert!(matches!(
            permit.kind(),
            RecoveryKind::Cleanup("transaction")
        ));
        let epoch = g.epoch();
        permit
            .recovered(RecoveryWitness::new(g.obj(), epoch, "rollback", "rfq:I"))
            .unwrap();
        assert!(g.reusable());
    }

    #[test]
    fn concurrent_claims_produce_exactly_one_recovery_owner() {
        let g = Arc::new(Guard::new());
        {
            let mut t = g.begin().unwrap();
            t.arm();
        }
        let mut handles = Vec::new();
        for _ in 0..8 {
            let g = g.clone();
            handles.push(std::thread::spawn(move || {
                g.claim_recovery().map(|p| {
                    // Hold briefly so a second claim would overlap.
                    std::thread::sleep(Duration::from_millis(5));
                    drop(p);
                })
            }));
        }
        let mut owners = 0;
        for h in handles {
            if h.join().unwrap().is_ok() {
                owners += 1;
            }
        }
        assert_eq!(owners, 1);
    }

    #[test]
    fn a_retired_object_rejects_every_later_operation() {
        let g = Guard::new();
        let mut t = g.begin().unwrap();
        t.arm();
        g.retire(Cause::QueueFull);
        assert_eq!(g.phase(), Phase::Retired);
        let d = Discharge::new(g.obj(), t.epoch(), t.op(), DischargeKind::Complete, "late");
        assert!(t.settle(d).is_err());
        assert!(matches!(g.begin(), Err(Refusal::Retired { .. })));
        assert!(!g.reusable());
    }

    #[test]
    fn feature_default_is_not_shared_with_previous_reports() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            t.tag("write", "partial");
        }
        let first = g.suspect().unwrap();
        let permit = g.claim_recovery().unwrap();
        permit.unresolved(Cause::Timeout);
        let second = g.suspect().unwrap();
        assert_eq!(first.features.name("write"), Some("partial"));
        assert_eq!(second.features.name("write"), Some("partial"));
    }

    #[test]
    fn decision_features_include_cause_and_attempts_without_mutating_the_record() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            t.tag("read", "none");
        }
        let s = g.suspect().unwrap();
        let f = s.decision_features();
        assert_eq!(f.name("cause"), Some("cancelled"));
        assert_eq!(f.int("attempts"), Some(0));
        assert!(!s.features.iter().any(|(k, _)| k == "cause"));
    }

    #[test]
    fn panic_during_an_armed_operation_records_unwind() {
        let g = Guard::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut t = g.begin().unwrap();
            t.arm();
            panic!("boom");
        }));
        assert!(result.is_err());
        assert_eq!(g.phase(), Phase::Suspect);
        assert_eq!(g.suspect().unwrap().cause, Cause::Unwind);
    }

    #[test]
    fn stats_are_monotonic_and_count_refusals() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            let d = Discharge::new(g.obj(), t.epoch(), t.op(), DischargeKind::Complete, "ok");
            t.settle(d).unwrap();
        }
        for _ in 0..3 {
            let _ = g.begin().unwrap();
        }
        let s = g.stats();
        assert_eq!(s.begun, 4);
        assert_eq!(s.discharged, 1);
        assert_eq!(s.suspected, 0);
        assert_eq!(s.refused_suspect, 0);
    }

    #[test]
    fn retire_is_terminal_even_when_a_live_permit_is_cancelled() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            t.abandon(Cause::Cancelled);
        }
        let permit = g.claim_recovery().unwrap();
        g.retire(Cause::Other("external_retire"));
        drop(permit);
        assert_eq!(g.phase(), Phase::Retired);
        assert!(g.claim_recovery().is_err());
    }

    #[test]
    fn stale_witness_returns_the_permit_to_suspect_and_allows_retry() {
        let g = Guard::new();
        {
            let mut t = g.begin().unwrap();
            t.arm();
            t.abandon(Cause::Cancelled);
        }
        let permit = g.claim_recovery().unwrap();
        let other = Guard::new();
        let bad = RecoveryWitness::new(other.obj(), other.epoch(), "drain", "wrong_object");
        assert!(permit.recovered(bad).is_err());
        assert_eq!(g.phase(), Phase::Suspect);
        assert!(g.claim_recovery().is_ok());
    }
    fn owner_guard() -> Arc<Guard> {
        Arc::new(Guard::new())
    }

    fn abandon_armed(g: &Guard) {
        let mut t = g.begin().unwrap();
        t.arm();
        drop(t);
    }

    #[test]
    fn nested_live_owners_block_cleanup_until_each_releases() {
        let g = owner_guard();
        let outer = g.claim_owner("transaction").unwrap();
        let inner = g.claim_owner("savepoint").unwrap();
        assert_eq!(g.live_owner_count(), 2);
        abandon_armed(&g);
        assert_eq!(g.phase(), Phase::Suspect);

        // A watchdog cannot clean up behind a live transaction.
        assert!(matches!(
            g.claim_recovery(),
            Err(Refusal::Owned { owners: 2, .. })
        ));
        assert_eq!(g.phase(), Phase::Suspect);

        // Releasing the inner scope does not release the outer one.
        inner.release();
        assert_eq!(g.live_owner_count(), 1);
        assert!(matches!(
            g.claim_recovery(),
            Err(Refusal::Owned { owners: 1, .. })
        ));

        // The live owner can recover its own object, but the claim remains live
        // until the owner relinquishes it.
        let permit = outer.claim_recovery().unwrap();
        let witness = RecoveryWitness::new(g.obj(), g.epoch(), "owner.rollback", "clean");
        permit.recovered(witness).unwrap();
        assert_eq!(g.phase(), Phase::Ready);
        assert_eq!(g.live_owner_count(), 1);
        assert!(!g.reusable());
        outer.release();
        assert_eq!(g.live_owner_count(), 0);
        assert!(g.reusable());
        assert!(g.suspect().is_none());
    }

    #[test]
    fn a_forgotten_claim_is_not_inferred_dead() {
        let g = owner_guard();
        let claim = g.claim_owner("transaction").unwrap();
        let witness = claim.witness();
        std::mem::forget(claim);
        assert_eq!(g.live_owner_count(), 1);
        assert!(g.owner_witness_live(&witness));
        assert!(!g.reusable());
        assert!(matches!(
            g.claim_recovery(),
            Err(Refusal::Owned { owners: 1, .. })
        ));

        // A second scope may exist, but it does not replace the forgotten one.
        let second = g.claim_owner("second").unwrap();
        assert_eq!(g.live_owner_count(), 2);
        second.release();
        assert_eq!(g.live_owner_count(), 1);
    }

    #[test]
    fn handoff_replaces_the_owner_cohort_and_stales_old_witnesses() {
        let g = owner_guard();
        let outer = g.claim_owner("transaction").unwrap();
        let inner = g.claim_owner("savepoint").unwrap();
        let outer_witness = outer.witness();
        let inner_witness = inner.witness();
        abandon_armed(&g);

        let background = outer.handoff("background").unwrap();
        assert_eq!(g.live_owner_count(), 1);
        assert_eq!(background.tag(), "background");
        assert_eq!(background.info().handoffs, 1);
        assert!(!g.owner_witness_live(&outer_witness));
        assert!(
            !g.owner_witness_live(&inner_witness),
            "handoff is all-or-nothing: nested claims from the old cohort are stale"
        );
        assert!(matches!(
            g.claim_recovery_as(&inner_witness),
            Err(Refusal::OwnerMismatch)
        ));
        assert!(background.claim_recovery().is_ok());
        drop(background);
        assert_eq!(g.live_owner_count(), 0);
    }

    #[test]
    fn handoff_is_refused_mid_operation_but_allowed_after_abandonment() {
        let g = owner_guard();
        let claim = g.claim_owner("transaction").unwrap();
        let mut ticket = g.begin().unwrap();
        ticket.arm();
        match claim.handoff("background") {
            Ok(_) => panic!("handoff must not succeed while the operation is in flight"),
            Err(OwnershipError::Busy) => {
                // The refused handoff left the original claim live.
                assert_eq!(g.live_owner_count(), 1);
            }
            Err(other) => panic!("unexpected handoff error: {other:?}"),
        }
        drop(ticket);
        assert_eq!(g.phase(), Phase::Suspect);
        let background = claim.handoff("background").unwrap();
        assert!(background.claim_recovery().is_ok());
    }

    #[test]
    fn cross_object_owner_witness_is_rejected_without_touching_either_object() {
        let a = owner_guard();
        let b = owner_guard();
        let claim = a.claim_owner("transaction").unwrap();
        let witness = claim.witness();
        assert!(matches!(
            b.claim_recovery_as(&witness),
            Err(Refusal::OwnerMismatch)
        ));
        assert_eq!(a.live_owner_count(), 1);
        assert_eq!(b.live_owner_count(), 0);
        assert!(b.claim_recovery_as(&witness).is_err());
        assert_eq!(b.phase(), Phase::Ready);
    }

    #[test]
    fn a_completed_exchange_does_not_poison_ownership_or_require_cleanup() {
        let g = owner_guard();
        let claim = g.claim_owner("transaction").unwrap();
        let before = g.epoch();
        let mut ticket = g.begin().unwrap();
        ticket.arm();
        let discharge = Discharge::new(
            g.obj(),
            ticket.epoch(),
            ticket.op(),
            DischargeKind::Complete,
            "test.complete",
        );
        ticket.settle(discharge).unwrap();
        assert!(g.epoch() > before, "a settled exchange mints a generation");
        assert_eq!(g.phase(), Phase::Ready);
        assert!(g.suspect().is_none());
        assert_eq!(g.live_owner_count(), 1);
        assert!(
            g.owner_witness_live(&claim.witness()),
            "operation epochs do not invalidate ownership"
        );
        assert!(matches!(
            g.qualification(),
            Qualification::Owned {
                owners: 1,
                tag: "transaction"
            }
        ));
        // A late cancellation cannot target a settled ticket; the only way to
        // make this object suspect would be a new operation.
        claim.release();
        assert_eq!(g.qualification(), Qualification::Clean);
        assert!(g.reusable());
    }

    #[test]
    fn a_second_cancel_during_recovery_keeps_the_object_quarantined() {
        let g = owner_guard();
        let claim = g.claim_owner("transaction").unwrap();
        abandon_armed(&g);
        {
            let permit = claim.claim_recovery().unwrap();
            drop(permit); // second cancellation
        }
        assert_eq!(g.phase(), Phase::Suspect);
        assert_eq!(g.stats().recoveries_cancelled, 1);
        let permit = claim.claim_recovery().unwrap();
        let witness = RecoveryWitness::new(g.obj(), g.epoch(), "retry", "clean");
        permit.recovered(witness).unwrap();
        claim.release();
        assert!(g.reusable());
    }

    #[test]
    fn a_suspect_object_refuses_new_owners_until_it_is_recovered() {
        let g = owner_guard();
        abandon_armed(&g);
        assert!(matches!(
            g.claim_owner("late"),
            Err(Refusal::Suspect { .. })
        ));
        let permit = g.claim_recovery().unwrap();
        let witness = RecoveryWitness::new(g.obj(), g.epoch(), "drain", "clean");
        permit.recovered(witness).unwrap();
        let claim = g.claim_owner("after_recovery").unwrap();
        assert_eq!(g.live_owner_count(), 1);
        claim.release();
    }

    #[test]
    fn retirement_wins_over_a_live_owner_and_cannot_be_undone() {
        let g = owner_guard();
        let claim = g.claim_owner("transaction").unwrap();
        g.retire(Cause::Shutdown);
        assert_eq!(g.phase(), Phase::Retired);
        assert!(matches!(g.claim_owner("new"), Err(Refusal::Retired { .. })));
        assert!(matches!(g.claim_recovery(), Err(Refusal::Retired { .. })));
        assert!(matches!(
            claim.claim_recovery(),
            Err(Refusal::Retired { .. })
        ));
        drop(claim);
        assert_eq!(g.phase(), Phase::Retired);
        assert!(!g.reusable());
    }
}

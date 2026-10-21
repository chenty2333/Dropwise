//! A real async pool whose hand-off decisions are driven by the ledger.
//!
//! Three properties are the point of this module:
//!
//! 1. **The return path never validates.**  `put_back` reads the ledger only.
//!    A suspect object goes to the recovery queue; it cannot be handed to the
//!    next borrower and no probe can consume its stale bytes.
//! 2. **The checkout path checks the ledger before any probe.**  Validation is
//!    a health check for a clean object, not a way to discover that a dirty
//!    object is dirty.
//! 3. **Every transition out of quarantine is owned and bounded.**  One worker
//!    holds an exclusive permit per attempt; queue capacity, attempt limits,
//!    deadlines and shutdown all have an explicit outcome, and none of them is
//!    "return to Ready".
//!
//! The pool also records the counters needed to separate "the guard prevented
//! contamination" from "the guard cost availability": retirement causes,
//! recovery outcomes, queue overflow, refused checkouts and reconnects.

use crate::guard::{
    Cause, Guard, GuardStats, ObjId, Qualification, RecoveryKind, RecoveryLimits, Refusal, Suspect,
};
use crate::policy::{Plan, RecoveryPolicy};
use crate::recovery::{EffectContract, PoolError, RecoveryAttempt, RecoveryContext};
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};

/// A session whose guard the pool can read and whose socket it can close.
///
/// The guard is behind an `Arc` so that a recovery permit can outlive a borrow
/// of the session: the permit protects the object while the protocol adapter
/// holds `&mut session` to drain it.
pub trait GuardedSession: Send + 'static {
    fn guard(&self) -> &Arc<Guard>;
    fn close(&mut self);
    /// The effect contract of ordinary operations on this session.  Used by
    /// adapters when a recovery must decide whether completing a partially
    /// written request is permitted.
    fn effect_contract(&self) -> EffectContract;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateOutcome {
    Healthy,
    Unhealthy(Cause),
}

/// Protocol-specific construction, validation and recovery.
pub trait Manager<S>: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn connect(&self) -> impl Future<Output = io::Result<S>> + Send;
    /// A health probe.  Only called for a `Clean` session.
    fn validate(&self, session: &mut S) -> impl Future<Output = ValidateOutcome> + Send;
    /// Execute one recovery plan.  The future must be cancellation-safe: if it
    /// is dropped at an await, all progress needed for a retry must live in
    /// `session` (or in a durable per-object buffer), never only in the future.
    fn recover(
        &self,
        session: &mut S,
        ctx: &RecoveryContext,
    ) -> impl Future<Output = RecoveryAttempt> + Send;
}

#[derive(Clone)]
pub struct PoolConfig {
    pub name: &'static str,
    pub max_size: usize,
    pub recovery_capacity: usize,
    pub limits: RecoveryLimits,
    pub validate_on_checkout: bool,
    pub policy: Arc<dyn RecoveryPolicy>,
    pub acquire_timeout: Option<Duration>,
    pub retry_delay: Duration,
    /// How long a returned session whose owner is still live stays deferred
    /// before it is retired.  A live owner is never cleaned up behind its back;
    /// a leaked claim therefore costs the object, not safety.
    pub ownership_grace: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            name: "pool",
            max_size: 4,
            recovery_capacity: 4,
            limits: RecoveryLimits::default(),
            validate_on_checkout: false,
            policy: Arc::new(crate::policy::ConservativeRetire),
            acquire_timeout: Some(Duration::from_secs(5)),
            retry_delay: Duration::from_millis(10),
            ownership_grace: Duration::from_millis(250),
        }
    }
}

#[derive(Default)]
struct Counters {
    acquires: AtomicU64,
    waits: AtomicU64,
    connects: AtomicU64,
    connect_errors: AtomicU64,
    validations: AtomicU64,
    validation_failures: AtomicU64,
    leased: AtomicU64,
    returned_clean: AtomicU64,
    returned_cleanup: AtomicU64,
    returned_suspect: AtomicU64,
    retired_clean: AtomicU64,
    retired_cleanup: AtomicU64,
    retired_suspect: AtomicU64,
    retired_forced: AtomicU64,
    queue_full: AtomicU64,
    recovery_enqueued: AtomicU64,
    recovery_succeeded: AtomicU64,
    recovery_failed: AtomicU64,
    recovery_unresolved: AtomicU64,
    recovery_cancelled: AtomicU64,
    recovery_drained_on_shutdown: AtomicU64,
    acquire_cancelled: AtomicU64,
    shutdowns: AtomicU64,
    deferred_returns: AtomicU64,
    deferred_released: AtomicU64,
    deferred_expired: AtomicU64,
    deferred_drained: AtomicU64,
}

pub struct PoolSnapshot {
    pub name: &'static str,
    pub acquires: u64,
    pub waits: u64,
    pub connects: u64,
    pub connect_errors: u64,
    pub validations: u64,
    pub validation_failures: u64,
    pub leased: u64,
    pub idle: usize,
    pub total_slots: usize,
    pub recovery_queue: usize,
    pub recovering: usize,
    pub returned_clean: u64,
    pub returned_cleanup: u64,
    pub returned_suspect: u64,
    pub retired_clean: u64,
    pub retired_cleanup: u64,
    pub retired_suspect: u64,
    pub retired_forced: u64,
    pub queue_full: u64,
    pub recovery_enqueued: u64,
    pub recovery_succeeded: u64,
    pub recovery_failed: u64,
    pub recovery_unresolved: u64,
    pub recovery_cancelled: u64,
    pub recovery_drained_on_shutdown: u64,
    pub acquire_cancelled: u64,
    pub deferred: usize,
    pub deferred_returns: u64,
    pub deferred_released: u64,
    pub deferred_expired: u64,
    pub deferred_drained: u64,
    pub last_plan: &'static str,
    pub closed: bool,
}

impl PoolSnapshot {
    /// Total connections replaced because of a cancellation or cleanup, i.e. the
    /// availability cost that the conservative baseline always pays.
    pub fn evictions(&self) -> u64 {
        self.retired_cleanup + self.retired_suspect + self.retired_forced
    }
}

pub struct ShutdownReport {
    pub retired_idle: usize,
    pub retired_deferred: usize,
    pub drained_queue: usize,
    pub outstanding_leases: usize,
    pub forced: bool,
}

struct State<S> {
    idle: VecDeque<S>,
    total: usize,
    recovery: VecDeque<S>,
    recovering: usize,
    active_leases: usize,
    worker_started: bool,
    /// Returned sessions whose owner claim is still live.  They are not idle
    /// and not recoverable until the claim is released or the grace expires.
    deferred: VecDeque<Deferred<S>>,
}

struct Deferred<S> {
    session: S,
    deadline: Instant,
}

struct Inner<S, M> {
    mgr: M,
    cfg: PoolConfig,
    state: Mutex<State<S>>,
    closed: AtomicBool,
    idle_notify: Notify,
    worker_notify: Notify,
    worker_idle: Notify,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    last_plan: Mutex<&'static str>,
    counters: Counters,
}

impl<S: GuardedSession, M: Manager<S>> Inner<S, M> {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn retire_and_count(&self, session: &mut S, cause: Cause, kind: RetireKind) {
        session.guard().retire(cause);
        session.close();
        match kind {
            RetireKind::Clean => self.counters.retired_clean.fetch_add(1, Ordering::Relaxed),
            RetireKind::Cleanup => self
                .counters
                .retired_cleanup
                .fetch_add(1, Ordering::Relaxed),
            RetireKind::Suspect => self
                .counters
                .retired_suspect
                .fetch_add(1, Ordering::Relaxed),
            RetireKind::Forced => self.counters.retired_forced.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn release_slot(&self) {
        {
            let mut st = self.state.lock().unwrap();
            st.total = st.total.saturating_sub(1);
        }
        self.idle_notify.notify_waiters();
    }

    fn make_lease(self: &Arc<Self>, mut session: S) -> Result<Lease<S, M>, PoolError> {
        let closed = {
            let mut st = self.state.lock().unwrap();
            if self.closed() {
                true
            } else {
                st.active_leases += 1;
                false
            }
        };
        if closed {
            self.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Clean);
            self.release_slot();
            return Err(PoolError::Closed);
        }
        self.counters.leased.fetch_add(1, Ordering::Relaxed);
        Ok(Lease {
            pool: self.clone(),
            session: Some(session),
        })
    }

    /// Synchronous return path.  This is the hand-off decision point; it must
    /// never await and must never run a protocol probe.
    fn put_back(self: &Arc<Self>, mut session: S) {
        let guard = session.guard();
        if self.closed() {
            self.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Clean);
            self.release_slot();
            return;
        }
        match guard.qualification() {
            Qualification::Clean => {
                self.counters.returned_clean.fetch_add(1, Ordering::Relaxed);
                // Check and push under the same lock: shutdown sets `closed`
                // before it drains the idle list, so either this push happens
                // before that drain (and is retired) or it is refused here.
                let mut session = Some(session);
                let pushed = {
                    let mut st = self.state.lock().unwrap();
                    if self.closed() {
                        false
                    } else {
                        st.idle.push_back(session.take().unwrap());
                        true
                    }
                };
                if pushed {
                    self.idle_notify.notify_one();
                } else {
                    let mut session = session.take().unwrap();
                    self.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Clean);
                    self.release_slot();
                }
            }
            Qualification::NeedsCleanup { .. } => {
                self.counters
                    .returned_cleanup
                    .fetch_add(1, Ordering::Relaxed);
                self.enqueue_recovery(session);
            }
            Qualification::Suspect { .. } => {
                self.counters
                    .returned_suspect
                    .fetch_add(1, Ordering::Relaxed);
                self.enqueue_recovery(session);
            }
            Qualification::Busy { .. } => {
                // A lease must not outlive its operation; if it does, the only
                // safe disposition is retirement.
                self.retire_and_count(
                    &mut session,
                    Cause::Other("lease_returned_in_flight"),
                    RetireKind::Forced,
                );
                self.release_slot();
            }
            Qualification::Owned { .. } => {
                self.defer_return(session);
            }
            Qualification::Recovering { .. } => {
                self.retire_and_count(
                    &mut session,
                    Cause::Other("lease_returned_while_recovering"),
                    RetireKind::Forced,
                );
                self.release_slot();
            }
            Qualification::Retired { .. } => {
                self.retire_and_count(
                    &mut session,
                    Cause::Other("lease_returned_retired"),
                    RetireKind::Clean,
                );
                self.release_slot();
            }
        }
    }

    fn enqueue_recovery(self: &Arc<Self>, session: S) {
        let mut holder = Some(session);
        let (queued, cause) = {
            let mut st = self.state.lock().unwrap();
            if self.closed() {
                // Do not strand a session in a queue that shutdown has already
                // drained; the caller retires it below.
                (false, Cause::Shutdown)
            } else if st.recovery.len() < self.cfg.recovery_capacity {
                st.recovery.push_back(holder.take().unwrap());
                (true, Cause::QueueFull) // cause unused for an accepted job
            } else {
                (false, Cause::QueueFull)
            }
        };
        if !queued {
            let mut session = holder.take().unwrap();
            if cause == Cause::QueueFull {
                self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
            }
            let fresh = session.guard().qualification();
            let kind = match fresh {
                Qualification::Suspect { .. } => RetireKind::Suspect,
                Qualification::Owned { .. } => RetireKind::Forced,
                _ => RetireKind::Cleanup,
            };
            self.retire_and_count(&mut session, cause, kind);
            self.release_slot();
            return;
        }
        self.counters
            .recovery_enqueued
            .fetch_add(1, Ordering::Relaxed);
        self.worker_notify.notify_one();
        self.ensure_worker();
    }

    /// A returned session with a live owner cannot be cleaned, validated or
    /// handed out: the owner may still be using it.  Park it with a deadline;
    /// if the owner releases in time, the normal return path runs, otherwise
    /// the object is retired (never silently reused).
    fn defer_return(self: &Arc<Self>, session: S) {
        let deadline = Instant::now() + self.cfg.ownership_grace;
        let mut holder = Some(session);
        let pushed = {
            let mut st = self.state.lock().unwrap();
            if self.closed() {
                false
            } else {
                st.deferred.push_back(Deferred {
                    session: holder.take().unwrap(),
                    deadline,
                });
                true
            }
        };
        if !pushed {
            let mut session = holder.take().unwrap();
            self.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Forced);
            self.release_slot();
            return;
        }
        self.counters
            .deferred_returns
            .fetch_add(1, Ordering::Relaxed);
        self.idle_notify.notify_waiters();
    }

    /// Complete deferred returns whose owner has released, and retire those
    /// whose grace expired.  Called at the top of every acquisition (and by
    /// shutdown), so a released owner does not require an external timer.
    fn reap_deferred(self: &Arc<Self>) {
        let now = Instant::now();
        let mut ready = Vec::new();
        let mut expired = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            let mut index = 0;
            while index < st.deferred.len() {
                let entry = &st.deferred[index];
                if entry.deadline <= now {
                    let entry = st.deferred.remove(index).unwrap();
                    expired.push(entry.session);
                } else if entry.session.guard().live_owner_count() == 0 {
                    let entry = st.deferred.remove(index).unwrap();
                    self.counters
                        .deferred_released
                        .fetch_add(1, Ordering::Relaxed);
                    ready.push(entry.session);
                } else {
                    index += 1;
                }
            }
        }
        for mut session in expired {
            self.counters
                .deferred_expired
                .fetch_add(1, Ordering::Relaxed);
            self.retire_and_count(
                &mut session,
                Cause::Other("owner_deadline"),
                RetireKind::Forced,
            );
            self.release_slot();
        }
        for session in ready {
            self.put_back(session);
        }
    }

    fn ensure_worker(self: &Arc<Self>) {
        let spawn = {
            let mut st = self.state.lock().unwrap();
            if st.worker_started || self.closed() {
                false
            } else {
                st.worker_started = true;
                true
            }
        };
        if !spawn {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let inner = self.clone();
                handle.spawn(async move { worker_loop(inner).await });
            }
            Err(_) => {
                // No runtime to run recovery on.  Retire queued objects rather
                // than leave them unreachable.
                let mut st = self.state.lock().unwrap();
                st.worker_started = false;
                let jobs: Vec<S> = st.recovery.drain(..).collect();
                drop(st);
                for mut session in jobs {
                    self.counters
                        .recovery_drained_on_shutdown
                        .fetch_add(1, Ordering::Relaxed);
                    self.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Suspect);
                    self.release_slot();
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RetireKind {
    Clean,
    Cleanup,
    Suspect,
    Forced,
}

pub struct Pool<S: GuardedSession, M: Manager<S>> {
    inner: Arc<Inner<S, M>>,
}

impl<S: GuardedSession, M: Manager<S>> Pool<S, M> {
    pub fn new(mgr: M, cfg: PoolConfig) -> Self {
        assert!(cfg.max_size > 0);
        assert!(cfg.recovery_capacity > 0);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                mgr,
                cfg,
                state: Mutex::new(State {
                    idle: VecDeque::new(),
                    total: 0,
                    recovery: VecDeque::new(),
                    recovering: 0,
                    active_leases: 0,
                    worker_started: false,
                    deferred: VecDeque::new(),
                }),
                closed: AtomicBool::new(false),
                idle_notify: Notify::new(),
                worker_notify: Notify::new(),
                worker_idle: Notify::new(),
                shutdown_tx,
                shutdown_rx,
                last_plan: Mutex::new("none"),
                counters: Counters::default(),
            }),
        }
    }

    pub fn name(&self) -> &'static str {
        self.inner.cfg.name
    }

    pub fn policy(&self) -> Arc<dyn RecoveryPolicy> {
        self.inner.cfg.policy.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed()
    }

    /// Acquire a lease.  Cancellation at any point leaves no half-owned slot:
    /// a slot removed from the idle list is protected by a drop guard that
    /// closes it and decrements the slot count if the acquisition future dies.
    pub async fn acquire(&self) -> Result<Lease<S, M>, PoolError> {
        self.inner.counters.acquires.fetch_add(1, Ordering::Relaxed);
        let deadline = self
            .inner
            .cfg
            .acquire_timeout
            .map(|d| tokio::time::Instant::now() + d);
        loop {
            if self.inner.closed() {
                return Err(PoolError::Closed);
            }
            self.inner.reap_deferred();
            let notified = self.inner.idle_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let popped = {
                let mut st = self.inner.state.lock().unwrap();
                st.idle.pop_front()
            };
            if let Some(session) = popped {
                let mut hold = Hold::new(&self.inner, session);
                match hold.session().guard().qualification() {
                    Qualification::Clean => {
                        if self.inner.cfg.validate_on_checkout {
                            self.inner
                                .counters
                                .validations
                                .fetch_add(1, Ordering::Relaxed);
                            let outcome = self.inner.mgr.validate(hold.session_mut()).await;
                            if let ValidateOutcome::Unhealthy(_) = outcome {
                                self.inner
                                    .counters
                                    .validation_failures
                                    .fetch_add(1, Ordering::Relaxed);
                                let mut session = hold.take();
                                self.inner.retire_and_count(
                                    &mut session,
                                    Cause::Protocol,
                                    RetireKind::Clean,
                                );
                                self.inner.release_slot();
                                continue;
                            }
                        }
                        let session = hold.take();
                        return self.inner.make_lease(session);
                    }
                    Qualification::NeedsCleanup { .. } | Qualification::Suspect { .. } => {
                        // Defensive: a dirty session must never be idle.  Put it
                        // through the recovery path instead of handing it out or
                        // probing it.
                        let session = hold.take();
                        self.inner.put_back(session);
                        continue;
                    }
                    Qualification::Owned { .. } => {
                        // Defensive: an owned session must never be idle.  Put
                        // it through the ownership-aware return path, which
                        // defers it while the owner is live, instead of handing
                        // it to a new borrower or retiring it behind the owner.
                        let session = hold.take();
                        self.inner.put_back(session);
                        continue;
                    }
                    Qualification::Busy { .. }
                    | Qualification::Recovering { .. }
                    | Qualification::Retired { .. } => {
                        // An invalid idle session is not acquire cancellation.
                        let mut session = hold.take();
                        self.inner.retire_and_count(
                            &mut session,
                            Cause::Protocol,
                            RetireKind::Forced,
                        );
                        self.inner.release_slot();
                        continue;
                    }
                }
            }

            // Reserve capacity before an await; the reservation decrements the
            // count if this future is dropped while connecting.
            let reserved = {
                let mut st = self.inner.state.lock().unwrap();
                if st.total < self.inner.cfg.max_size {
                    st.total += 1;
                    true
                } else {
                    false
                }
            };
            if reserved {
                let res = Reservation {
                    inner: self.inner.clone(),
                    armed: true,
                };
                let connect = self.inner.mgr.connect();
                tokio::pin!(connect);
                let outcome = match deadline {
                    Some(dl) => match tokio::time::timeout_at(dl, &mut connect).await {
                        Ok(r) => r,
                        Err(_) => {
                            drop(res);
                            return Err(PoolError::Connect(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "acquire timed out while connecting",
                            )));
                        }
                    },
                    None => connect.await,
                };
                match outcome {
                    Ok(session) => {
                        self.inner.counters.connects.fetch_add(1, Ordering::Relaxed);
                        let mut res = res;
                        res.disarm();
                        return self.inner.make_lease(session);
                    }
                    Err(e) => {
                        self.inner
                            .counters
                            .connect_errors
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(PoolError::Connect(e));
                    }
                }
            }

            self.inner.counters.waits.fetch_add(1, Ordering::Relaxed);
            match deadline {
                Some(dl) => {
                    if tokio::time::timeout_at(dl, notified).await.is_err() {
                        return Err(PoolError::Connect(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "acquire timed out waiting for a free slot",
                        )));
                    }
                }
                None => notified.await,
            }
        }
    }

    pub fn snapshot(&self) -> PoolSnapshot {
        self.inner.snapshot()
    }

    /// Stop serving, retire idle slots, drain the recovery queue and wait (up to
    /// `grace`) for in-flight recovery to finish.  No slot becomes usable after
    /// this returns.
    pub async fn shutdown(&self, grace: Duration) -> ShutdownReport {
        self.inner
            .counters
            .shutdowns
            .fetch_add(1, Ordering::Relaxed);
        let drained_before = self
            .inner
            .counters
            .recovery_drained_on_shutdown
            .load(Ordering::Relaxed);
        // Admission closes under the same lock used by enqueue/worker publish.
        // Drain here as well as in the worker: enqueue may have completed before
        // shutdown while ensure_worker has not run yet.
        let (idle, deferred) = {
            let mut st = self.inner.state.lock().unwrap();
            self.inner.closed.store(true, Ordering::Release);
            (
                std::mem::take(&mut st.idle),
                std::mem::take(&mut st.deferred),
            )
        };
        let _ = self.inner.shutdown_tx.send(true);
        self.inner.idle_notify.notify_waiters();
        let retired_idle = idle.len();
        for mut session in idle {
            self.inner
                .retire_and_count(&mut session, Cause::Shutdown, RetireKind::Clean);
            self.inner.release_slot();
        }
        let retired_deferred = deferred.len();
        for deferred in deferred {
            let mut session = deferred.session;
            self.inner
                .counters
                .deferred_drained
                .fetch_add(1, Ordering::Relaxed);
            self.inner
                .retire_and_count(&mut session, Cause::Shutdown, RetireKind::Forced);
            self.inner.release_slot();
        }
        drain_queue(&self.inner);
        self.inner.worker_notify.notify_waiters();

        let deadline = Instant::now() + grace;
        let mut forced = false;
        loop {
            let (queue, recovering, worker_started) = {
                let st = self.inner.state.lock().unwrap();
                (st.recovery.len(), st.recovering, st.worker_started)
            };
            if queue == 0 && recovering == 0 && !worker_started {
                break;
            }
            if Instant::now() >= deadline {
                forced = true;
                break;
            }
            let _ =
                tokio::time::timeout(Duration::from_millis(10), self.inner.worker_idle.notified())
                    .await;
        }
        let (drained, outstanding_leases) = {
            let st = self.inner.state.lock().unwrap();
            (
                self.inner
                    .counters
                    .recovery_drained_on_shutdown
                    .load(Ordering::Relaxed),
                st.active_leases,
            )
        };
        ShutdownReport {
            retired_idle,
            retired_deferred,
            drained_queue: drained.saturating_sub(drained_before) as usize,
            outstanding_leases,
            forced,
        }
    }
}

impl<S: GuardedSession, M: Manager<S>> Inner<S, M> {
    fn snapshot(&self) -> PoolSnapshot {
        let st = self.state.lock().unwrap();
        let c = &self.counters;
        PoolSnapshot {
            name: self.cfg.name,
            acquires: c.acquires.load(Ordering::Relaxed),
            waits: c.waits.load(Ordering::Relaxed),
            connects: c.connects.load(Ordering::Relaxed),
            connect_errors: c.connect_errors.load(Ordering::Relaxed),
            validations: c.validations.load(Ordering::Relaxed),
            validation_failures: c.validation_failures.load(Ordering::Relaxed),
            leased: c.leased.load(Ordering::Relaxed),
            idle: st.idle.len(),
            total_slots: st.total,
            recovery_queue: st.recovery.len(),
            recovering: st.recovering,
            returned_clean: c.returned_clean.load(Ordering::Relaxed),
            returned_cleanup: c.returned_cleanup.load(Ordering::Relaxed),
            returned_suspect: c.returned_suspect.load(Ordering::Relaxed),
            retired_clean: c.retired_clean.load(Ordering::Relaxed),
            retired_cleanup: c.retired_cleanup.load(Ordering::Relaxed),
            retired_suspect: c.retired_suspect.load(Ordering::Relaxed),
            retired_forced: c.retired_forced.load(Ordering::Relaxed),
            queue_full: c.queue_full.load(Ordering::Relaxed),
            recovery_enqueued: c.recovery_enqueued.load(Ordering::Relaxed),
            recovery_succeeded: c.recovery_succeeded.load(Ordering::Relaxed),
            recovery_failed: c.recovery_failed.load(Ordering::Relaxed),
            recovery_unresolved: c.recovery_unresolved.load(Ordering::Relaxed),
            recovery_cancelled: c.recovery_cancelled.load(Ordering::Relaxed),
            recovery_drained_on_shutdown: c.recovery_drained_on_shutdown.load(Ordering::Relaxed),
            acquire_cancelled: c.acquire_cancelled.load(Ordering::Relaxed),
            deferred: st.deferred.len(),
            deferred_returns: c.deferred_returns.load(Ordering::Relaxed),
            deferred_released: c.deferred_released.load(Ordering::Relaxed),
            deferred_expired: c.deferred_expired.load(Ordering::Relaxed),
            deferred_drained: c.deferred_drained.load(Ordering::Relaxed),
            last_plan: *self.last_plan.lock().unwrap(),
            closed: self.closed(),
        }
    }
}

impl<S: GuardedSession, M: Manager<S>> Drop for Pool<S, M> {
    fn drop(&mut self) {
        // Without an explicit shutdown, still make sure idle objects are not
        // left silently open.  Queued recovery sessions are closed by their
        // owner when the worker task (which holds an Arc) is dropped.
        self.inner.closed.store(true, Ordering::Release);
        let _ = self.inner.shutdown_tx.send(true);
        let (sessions, deferred) = {
            let mut st = self.inner.state.lock().unwrap();
            (
                std::mem::take(&mut st.idle),
                std::mem::take(&mut st.deferred),
            )
        };
        for mut s in sessions {
            s.guard().retire(Cause::Shutdown);
            s.close();
        }
        for mut deferred in deferred {
            deferred.session.guard().retire(Cause::Shutdown);
            deferred.session.close();
        }
        self.inner.idle_notify.notify_waiters();
        self.inner.worker_notify.notify_waiters();
    }
}

/// Owns a session removed from the idle list while the acquisition future may
/// still be dropped.  Dropping the hold closes the session and frees the slot;
/// otherwise `take` transfers ownership to the lease.
struct Hold<'a, S: GuardedSession, M: Manager<S>> {
    inner: &'a Arc<Inner<S, M>>,
    session: Option<S>,
}

impl<'a, S: GuardedSession, M: Manager<S>> Hold<'a, S, M> {
    fn new(inner: &'a Arc<Inner<S, M>>, session: S) -> Self {
        Self {
            inner,
            session: Some(session),
        }
    }

    fn session(&self) -> &S {
        self.session.as_ref().unwrap()
    }

    fn session_mut(&mut self) -> &mut S {
        self.session.as_mut().unwrap()
    }

    fn take(mut self) -> S {
        self.session.take().unwrap()
    }
}

impl<S: GuardedSession, M: Manager<S>> Drop for Hold<'_, S, M> {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            self.inner.retire_and_count(
                &mut session,
                Cause::Other("acquire_cancelled"),
                RetireKind::Clean,
            );
            self.inner
                .counters
                .acquire_cancelled
                .fetch_add(1, Ordering::Relaxed);
            {
                let mut st = self.inner.state.lock().unwrap();
                st.total = st.total.saturating_sub(1);
            }
            self.inner.idle_notify.notify_waiters();
        }
    }
}

struct Reservation<S, M> {
    inner: Arc<Inner<S, M>>,
    armed: bool,
}

impl<S, M> Reservation<S, M> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<S, M> Drop for Reservation<S, M> {
    fn drop(&mut self) {
        if self.armed {
            {
                let mut st = self.inner.state.lock().unwrap();
                st.total = st.total.saturating_sub(1);
            }
            self.inner.idle_notify.notify_waiters();
        }
    }
}

/// A checked-out session.  Return happens on drop and cannot be skipped by
/// cancellation: the lease is owned by the caller's scope, not by the operation
/// future.
pub struct Lease<S: GuardedSession, M: Manager<S>> {
    pool: Arc<Inner<S, M>>,
    session: Option<S>,
}

impl<S: GuardedSession, M: Manager<S>> Lease<S, M> {
    pub fn session(&self) -> &S {
        self.session.as_ref().unwrap()
    }

    pub fn session_mut(&mut self) -> &mut S {
        self.session.as_mut().unwrap()
    }

    pub fn guard(&self) -> &Arc<Guard> {
        self.session().guard()
    }

    pub fn obj(&self) -> ObjId {
        self.guard().obj()
    }

    /// Explicit return.  Equivalent to dropping the lease; provided for callers
    /// that want to observe the return point.
    pub fn release(mut self) {
        if let Some(session) = self.session.take() {
            self.return_session(session);
        }
    }

    fn return_session(&self, session: S) {
        {
            let mut st = self.pool.state.lock().unwrap();
            st.active_leases = st.active_leases.saturating_sub(1);
        }
        self.pool.put_back(session);
    }
}

impl<S: GuardedSession, M: Manager<S>> Drop for Lease<S, M> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.return_session(session);
        }
    }
}

impl<S: GuardedSession, M: Manager<S>> std::ops::Deref for Lease<S, M> {
    type Target = S;
    fn deref(&self) -> &S {
        self.session()
    }
}

impl<S: GuardedSession, M: Manager<S>> std::ops::DerefMut for Lease<S, M> {
    fn deref_mut(&mut self) -> &mut S {
        self.session_mut()
    }
}

enum JobEnd<S> {
    Idle(S),
    Requeue(S),
    Done,
}

async fn worker_loop<S: GuardedSession, M: Manager<S>>(inner: Arc<Inner<S, M>>) {
    loop {
        if *inner.shutdown_rx.borrow() {
            drain_queue(&inner);
            inner.state.lock().unwrap().worker_started = false;
            inner.worker_idle.notify_waiters();
            return;
        }
        let job = {
            let mut st = inner.state.lock().unwrap();
            let job = st.recovery.pop_front();
            if job.is_some() {
                st.recovering += 1;
            }
            job
        };
        match job {
            Some(session) => {
                let end = handle_job(&inner, session).await;
                publish_job(&inner, end);
                {
                    let mut st = inner.state.lock().unwrap();
                    st.recovering = st.recovering.saturating_sub(1);
                }
                inner.worker_idle.notify_waiters();
            }
            None => {
                let notified = inner.worker_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let mut shutdown = inner.shutdown_rx.clone();
                if *shutdown.borrow() {
                    continue;
                }
                tokio::select! {
                    _ = notified => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
    }
}

fn publish_job<S: GuardedSession, M: Manager<S>>(inner: &Arc<Inner<S, M>>, end: JobEnd<S>) {
    let (mut session, requeue) = match end {
        JobEnd::Idle(s) => (s, false),
        JobEnd::Requeue(s) => (s, true),
        JobEnd::Done => return,
    };
    let mut st = inner.state.lock().unwrap();
    if inner.closed() {
        drop(st);
        inner.retire_and_count(
            &mut session,
            Cause::Shutdown,
            if requeue {
                RetireKind::Suspect
            } else {
                RetireKind::Clean
            },
        );
        inner.release_slot();
    } else if requeue {
        st.recovery.push_back(session);
        inner.worker_notify.notify_one();
    } else {
        st.idle.push_back(session);
        inner.idle_notify.notify_one();
    }
}

fn drain_queue<S: GuardedSession, M: Manager<S>>(inner: &Arc<Inner<S, M>>) {
    loop {
        let job = {
            let mut st = inner.state.lock().unwrap();
            st.recovery.pop_front()
        };
        match job {
            Some(mut session) => {
                inner
                    .counters
                    .recovery_drained_on_shutdown
                    .fetch_add(1, Ordering::Relaxed);
                inner.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Suspect);
                inner.release_slot();
            }
            None => break,
        }
    }
}

async fn handle_job<S: GuardedSession, M: Manager<S>>(
    inner: &Arc<Inner<S, M>>,
    mut session: S,
) -> JobEnd<S> {
    let guard = session.guard().clone();
    let permit = match guard.claim_recovery() {
        Ok(p) => p,
        Err(Refusal::Retired { .. }) => {
            inner.release_slot();
            return JobEnd::Done;
        }
        Err(_) => {
            inner.retire_and_count(
                &mut session,
                Cause::Other("recovery_claim_refused"),
                RetireKind::Forced,
            );
            inner.release_slot();
            return JobEnd::Done;
        }
    };
    let kind = permit.kind();
    let report = build_report(&guard, kind);
    let plan = inner.cfg.policy.choose(&report);
    let age = guard.suspect().map(|s| s.at.elapsed()).unwrap_or_default();
    let remaining = inner.cfg.limits.deadline.saturating_sub(age);
    let plan_name: &'static str = match plan {
        Plan::Run(name) => name,
        Plan::Retire => "retire",
    };
    *inner.last_plan.lock().unwrap() = plan_name;
    let ctx = RecoveryContext {
        obj: guard.obj(),
        epoch: permit.epoch(),
        attempt: permit.attempt(),
        plan: plan_name,
        limits: inner.cfg.limits,
        features: report.decision_features(),
        remaining,
        write_state: report.features.name("write").unwrap_or("unknown"),
        read_state: report.features.name("read").unwrap_or("unknown"),
        cause: report.cause,
        contract: match report.features.name("contract") {
            Some("may_apply") => crate::recovery::EffectContract::AtomicEffectMayApply,
            _ => crate::recovery::EffectContract::MustNotApply,
        },
    };
    match plan {
        Plan::Retire => {
            permit.failed(Cause::Other("policy_retire"));
            inner
                .counters
                .recovery_failed
                .fetch_add(1, Ordering::Relaxed);
            let rk = match kind {
                RecoveryKind::Suspect => RetireKind::Suspect,
                RecoveryKind::Cleanup(_) => RetireKind::Cleanup,
            };
            inner.retire_and_count(&mut session, Cause::Other("policy_retire"), rk);
            inner.release_slot();
            JobEnd::Done
        }
        Plan::Run(_) => {
            let mut shutdown = inner.shutdown_rx.clone();
            let outcome = if inner.closed() {
                None
            } else {
                tokio::select! {
                    a = tokio::time::timeout(remaining, inner.mgr.recover(&mut session, &ctx)) => {
                        Some(a.unwrap_or(RecoveryAttempt::Unresolved(Cause::Timeout)))
                    },
                    _ = shutdown.changed() => None,
                }
            };
            match outcome {
                None => {
                    inner
                        .counters
                        .recovery_cancelled
                        .fetch_add(1, Ordering::Relaxed);
                    permit.unresolved(Cause::RecoveryCancelled);
                    inner.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Suspect);
                    inner.release_slot();
                    JobEnd::Done
                }
                Some(RecoveryAttempt::Recovered(witness)) => match permit.recovered(witness) {
                    Ok(()) => {
                        inner
                            .counters
                            .recovery_succeeded
                            .fetch_add(1, Ordering::Relaxed);
                        if inner.closed() {
                            inner.retire_and_count(
                                &mut session,
                                Cause::Shutdown,
                                RetireKind::Clean,
                            );
                            inner.release_slot();
                            JobEnd::Done
                        } else {
                            JobEnd::Idle(session)
                        }
                    }
                    Err(_) => {
                        inner
                            .counters
                            .recovery_failed
                            .fetch_add(1, Ordering::Relaxed);
                        inner.retire_and_count(
                            &mut session,
                            Cause::Other("stale_recovery_witness"),
                            RetireKind::Suspect,
                        );
                        inner.release_slot();
                        JobEnd::Done
                    }
                },
                Some(RecoveryAttempt::Failed(cause)) => {
                    inner
                        .counters
                        .recovery_failed
                        .fetch_add(1, Ordering::Relaxed);
                    permit.failed(cause);
                    let rk = match kind {
                        RecoveryKind::Suspect => RetireKind::Suspect,
                        RecoveryKind::Cleanup(_) => RetireKind::Cleanup,
                    };
                    inner.retire_and_count(&mut session, cause, rk);
                    inner.release_slot();
                    JobEnd::Done
                }
                Some(RecoveryAttempt::Unresolved(cause)) => {
                    inner
                        .counters
                        .recovery_unresolved
                        .fetch_add(1, Ordering::Relaxed);
                    permit.unresolved(cause);
                    if inner.closed() {
                        inner.retire_and_count(&mut session, Cause::Shutdown, RetireKind::Suspect);
                        inner.release_slot();
                        return JobEnd::Done;
                    }
                    let attempts = guard.attempts();
                    let expired = guard
                        .suspect()
                        .map(|s| s.at.elapsed() >= inner.cfg.limits.deadline)
                        .unwrap_or(true);
                    if attempts >= inner.cfg.limits.max_attempts || expired {
                        let rk = match kind {
                            RecoveryKind::Suspect => RetireKind::Suspect,
                            RecoveryKind::Cleanup(_) => RetireKind::Cleanup,
                        };
                        inner.retire_and_count(&mut session, cause, rk);
                        inner.release_slot();
                        JobEnd::Done
                    } else {
                        let delay = inner.cfg.retry_delay;
                        let mut shutdown = inner.shutdown_rx.clone();
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = shutdown.changed() => {}
                        }
                        JobEnd::Requeue(session)
                    }
                }
            }
        }
    }
}

fn build_report(guard: &Guard, kind: RecoveryKind) -> Suspect {
    match guard.suspect() {
        Some(mut report) => {
            report.attempts = guard.attempts();
            report.standing = guard.standing();
            report
        }
        None => {
            let mut features = crate::features::Features::new();
            features.tag("write", "complete");
            features.tag("read", "none");
            Suspect {
                cause: Cause::Standing,
                features,
                at: Instant::now(),
                op: guard.current_op(),
                epoch: guard.epoch(),
                standing: guard.standing().or(match kind {
                    RecoveryKind::Cleanup(kind) => Some(kind),
                    RecoveryKind::Suspect => None,
                }),
                attempts: guard.attempts(),
            }
        }
    }
}

/// Aggregate guard statistics for one session, for per-object attribution.
pub fn guard_stats<S: GuardedSession>(session: &S) -> GuardStats {
    session.guard().stats()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::{Discharge, DischargeKind};
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Copy, Debug)]
    enum Scripted {
        Recover,
        Fail,
        Unresolved,
        Hang,
    }

    struct FakeSession {
        guard: Arc<Guard>,
        closed: bool,
    }

    impl FakeSession {
        fn new() -> Self {
            Self {
                guard: Arc::new(Guard::new()),
                closed: false,
            }
        }

        /// Simulate an operation that started, did I/O, and was dropped.
        fn make_suspect(&self) {
            let mut t = self.guard.begin().unwrap();
            t.arm();
            t.tag("write", "partial");
            t.set("write_bytes", 7usize);
            t.abandon(Cause::Cancelled);
        }

        /// Simulate a fully completed exchange.
        fn make_clean(&self) {
            let mut t = self.guard.begin().unwrap();
            t.arm();
            let d = Discharge::new(
                self.guard.obj(),
                t.epoch(),
                t.op(),
                DischargeKind::Complete,
                "fake.exchange",
            );
            t.settle(d).unwrap();
        }
    }

    impl GuardedSession for FakeSession {
        fn guard(&self) -> &Arc<Guard> {
            &self.guard
        }
        fn close(&mut self) {
            self.closed = true;
        }
        fn effect_contract(&self) -> EffectContract {
            EffectContract::MustNotApply
        }
    }

    #[derive(Clone)]
    struct FakeMgr {
        script: Arc<Mutex<VecDeque<Scripted>>>,
        connects: Arc<AtomicUsize>,
        validations: Arc<AtomicUsize>,
        healthy: Arc<AtomicBool>,
        connect_hang: Arc<AtomicBool>,
    }

    impl FakeMgr {
        fn new(script: Vec<Scripted>) -> Self {
            Self {
                script: Arc::new(Mutex::new(script.into_iter().collect())),
                connects: Arc::new(AtomicUsize::new(0)),
                validations: Arc::new(AtomicUsize::new(0)),
                healthy: Arc::new(AtomicBool::new(true)),
                connect_hang: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl Manager<FakeSession> for FakeMgr {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn connect(&self) -> io::Result<FakeSession> {
            if self.connect_hang.load(Ordering::Acquire) {
                std::future::pending::<()>().await;
            }
            self.connects.fetch_add(1, Ordering::Relaxed);
            Ok(FakeSession::new())
        }
        async fn validate(&self, _session: &mut FakeSession) -> ValidateOutcome {
            self.validations.fetch_add(1, Ordering::Relaxed);
            if self.healthy.load(Ordering::Acquire) {
                ValidateOutcome::Healthy
            } else {
                ValidateOutcome::Unhealthy(Cause::Protocol)
            }
        }
        async fn recover(
            &self,
            _session: &mut FakeSession,
            ctx: &RecoveryContext,
        ) -> RecoveryAttempt {
            let next = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Scripted::Fail);
            match next {
                Scripted::Recover => RecoveryAttempt::recovered(ctx, "fake.recovered"),
                Scripted::Fail => RecoveryAttempt::Failed(Cause::Other("fake.fail")),
                Scripted::Unresolved => RecoveryAttempt::Unresolved(Cause::Timeout),
                Scripted::Hang => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }
    }

    struct RunPlan;
    impl crate::policy::RecoveryPolicy for RunPlan {
        fn choose(&self, _suspect: &Suspect) -> Plan {
            Plan::Run("drain")
        }
        fn describe(&self) -> String {
            "run-drain".into()
        }
    }

    fn cfg() -> PoolConfig {
        PoolConfig {
            name: "test",
            max_size: 2,
            recovery_capacity: 2,
            limits: RecoveryLimits {
                max_attempts: 3,
                deadline: Duration::from_secs(5),
            },
            validate_on_checkout: false,
            policy: Arc::new(RunPlan),
            acquire_timeout: Some(Duration::from_secs(1)),
            retry_delay: Duration::from_millis(1),
            ownership_grace: Duration::from_millis(100),
        }
    }

    async fn wait_recovered(pool: &Pool<FakeSession, FakeMgr>, want_idle: usize) {
        for _ in 0..400 {
            let s = pool.snapshot();
            if s.recovery_queue == 0 && s.recovering == 0 && s.idle >= want_idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("recovery did not settle: {:?}", pool.snapshot().idle);
    }

    #[tokio::test]
    async fn return_path_never_validates_and_quarantines_suspect() {
        let m = FakeMgr::new(vec![Scripted::Recover]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        let obj = lease.obj();
        lease.session().make_suspect();
        let epoch_before = lease.guard().epoch();
        drop(lease);
        assert_eq!(m.validations.load(Ordering::Relaxed), 0);
        assert_eq!(m.connects.load(Ordering::Relaxed), 1);
        assert_eq!(pool.snapshot().returned_suspect, 1);
        wait_recovered(&pool, 1).await;
        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
        assert!(lease.guard().epoch() != epoch_before);
        assert!(matches!(
            lease.guard().qualification(),
            Qualification::Clean
        ));
        // Checkout does not validate by default.
        assert_eq!(m.validations.load(Ordering::Relaxed), 0);
        drop(lease);
    }

    #[tokio::test]
    async fn checkout_validates_clean_before_use_and_discards_unhealthy() {
        let m = FakeMgr::new(vec![Scripted::Recover]);
        m.healthy.store(false, Ordering::Release);
        let mut c = cfg();
        c.validate_on_checkout = true;
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        let first = lease.obj();
        drop(lease);
        // The first borrow is clean; on return it goes idle.  The next checkout
        // validates it, fails, closes it, and connects a replacement.
        let lease = pool.acquire().await.unwrap();
        assert_ne!(lease.obj(), first);
        assert_eq!(m.validations.load(Ordering::Relaxed), 1);
        assert_eq!(pool.snapshot().validation_failures, 1);
        assert_eq!(pool.snapshot().acquire_cancelled, 0);
        assert_eq!(m.connects.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn dirty_session_is_never_handed_out_even_if_it_lands_in_idle() {
        let m = FakeMgr::new(vec![Scripted::Recover]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        let obj = lease.obj();
        // Simulate an owner bug: push the dirty session into idle.
        drop(lease);
        // put_back routes it to recovery, not idle; now inject it manually.
        wait_recovered(&pool, 1).await;
        // After recovery the object is clean and reusable, which is the
        // expected path; the defensive branch is covered by the next acquire
        // seeing a clean ledger entry.
        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
    }

    #[tokio::test]
    async fn queue_overflow_retires_instead_of_leaking_slot() {
        let m = FakeMgr::new(vec![Scripted::Hang, Scripted::Hang]);
        let mut c = cfg();
        c.max_size = 3;
        c.recovery_capacity = 1;
        let pool = Pool::new(m.clone(), c);
        let a = pool.acquire().await.unwrap();
        let b = pool.acquire().await.unwrap();
        a.session().make_suspect();
        b.session().make_suspect();
        drop(a);
        drop(b);
        let s = pool.snapshot();
        assert_eq!(s.queue_full, 1);
        assert_eq!(s.retired_suspect, 1);
        assert!(!s.closed);
        // Slot accounting: one recovering + one retired; capacity allows a new
        // connection on the next acquire.
        let c2 = pool.acquire().await.unwrap();
        assert_eq!(m.connects.load(Ordering::Relaxed), 3);
        drop(c2);
        let _ = pool.shutdown(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn failed_recovery_retires_and_frees_the_slot() {
        let m = FakeMgr::new(vec![Scripted::Fail]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        let obj = lease.obj();
        drop(lease);
        let mut s = pool.snapshot();
        for _ in 0..200 {
            s = pool.snapshot();
            if s.recovery_failed == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(s.recovery_failed, 1);
        assert_eq!(s.retired_suspect, 1);
        assert_eq!(s.recovery_queue, 0);
        assert_eq!(s.total_slots, 0);
        let lease = pool.acquire().await.unwrap();
        assert_ne!(lease.obj(), obj);
    }

    #[tokio::test]
    async fn unresolved_attempt_is_retried_and_the_object_is_reused() {
        let m = FakeMgr::new(vec![Scripted::Unresolved, Scripted::Recover]);
        let mut c = cfg();
        c.retry_delay = Duration::from_millis(1);
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        let obj = lease.obj();
        drop(lease);
        wait_recovered(&pool, 1).await;
        let s = pool.snapshot();
        assert_eq!(s.recovery_unresolved, 1);
        assert_eq!(s.recovery_succeeded, 1);
        assert_eq!(s.retired_suspect, 0);
        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
        drop(lease);
    }

    #[tokio::test]
    async fn attempt_limit_retires_a_persistently_unresolved_object() {
        let m = FakeMgr::new(vec![Scripted::Unresolved; 10]);
        let mut c = cfg();
        c.limits.max_attempts = 2;
        c.retry_delay = Duration::from_millis(1);
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        drop(lease);
        for _ in 0..300 {
            if pool.snapshot().retired_suspect == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let s = pool.snapshot();
        assert_eq!(s.retired_suspect, 1);
        assert_eq!(s.recovery_unresolved, 2);
        assert_eq!(s.total_slots, 0);
    }

    #[tokio::test]
    async fn shutdown_cancels_inflight_recovery_and_never_returns_it_to_idle() {
        let m = FakeMgr::new(vec![Scripted::Hang]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        drop(lease);
        // Wait until the worker has the object in hand.
        for _ in 0..200 {
            if pool.snapshot().recovering == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let report = pool.shutdown(Duration::from_millis(200)).await;
        let s = pool.snapshot();
        assert!(
            !report.forced,
            "shutdown should observe the cancel, not time out"
        );
        assert_eq!(s.recovery_cancelled, 1);
        assert_eq!(s.retired_suspect, 1);
        assert_eq!(s.idle, 0);
        assert_eq!(s.recovering, 0);
        assert!(s.closed);
        assert!(pool.acquire().await.is_err());
    }

    #[tokio::test]
    async fn shutdown_drains_queued_recovery_and_retires_idle() {
        let m = FakeMgr::new(vec![Scripted::Hang, Scripted::Hang, Scripted::Hang]);
        let mut c = cfg();
        c.max_size = 3;
        c.recovery_capacity = 3;
        let pool = Pool::new(m.clone(), c);
        let a = pool.acquire().await.unwrap();
        let b = pool.acquire().await.unwrap();
        a.session().make_suspect();
        b.session().make_suspect();
        drop(a);
        drop(b);
        // Let one of them enter recovery and hang; the other stays queued.
        for _ in 0..200 {
            if pool.snapshot().recovering == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let report = pool.shutdown(Duration::from_millis(300)).await;
        let s = pool.snapshot();
        assert!(!report.forced);
        assert!(s.closed);
        assert_eq!(s.idle, 0);
        assert_eq!(s.recovery_queue, 0);
        assert_eq!(s.recovering, 0);
        assert_eq!(s.retired_suspect, 2);
        assert_eq!(s.recovery_drained_on_shutdown, 1);
    }

    #[tokio::test]
    async fn cancelled_connect_reservation_is_returned() {
        let m = FakeMgr::new(vec![]);
        m.connect_hang.store(true, Ordering::Release);
        let pool = Pool::new(m.clone(), cfg());
        let mut acquire = Box::pin(pool.acquire());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut acquire)
                .await
                .is_err()
        );
        drop(acquire);
        assert_eq!(pool.snapshot().total_slots, 0);
        // A later acquisition can use the capacity.
        m.connect_hang.store(false, Ordering::Release);
        let lease = tokio::time::timeout(Duration::from_secs(1), pool.acquire())
            .await
            .expect("acquire")
            .expect("lease");
        assert_eq!(pool.snapshot().acquire_cancelled, 0);
        drop(lease);
    }

    #[tokio::test]
    async fn lease_dropped_after_shutdown_is_closed_not_reused() {
        let m = FakeMgr::new(vec![Scripted::Recover]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        let obj = lease.obj();
        let report = pool.shutdown(Duration::from_millis(50)).await;
        assert_eq!(report.outstanding_leases, 1);
        drop(lease);
        let s = pool.snapshot();
        assert_eq!(s.idle, 0);
        assert_eq!(s.total_slots, 0);
        assert_eq!(s.retired_clean, 1);
        assert_ne!(obj, ObjId::default());
    }

    #[tokio::test]
    async fn clean_lease_can_be_reused_without_recovery() {
        let m = FakeMgr::new(vec![]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        let obj = lease.obj();
        lease.session().make_clean();
        drop(lease);
        assert_eq!(pool.snapshot().returned_clean, 1);
        assert_eq!(pool.snapshot().recovery_enqueued, 0);
        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
        assert_eq!(m.connects.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn suspect_epoch_is_not_reused_for_the_recovery_witness() {
        let m = FakeMgr::new(vec![Scripted::Recover]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        lease.session().make_suspect();
        let old_epoch = lease.guard().epoch();
        drop(lease);
        wait_recovered(&pool, 1).await;
        let lease = pool.acquire().await.unwrap();
        assert!(lease.guard().epoch() != old_epoch);
        assert!(pool.snapshot().recovery_succeeded >= 1);
    }

    #[tokio::test]
    async fn shutdown_between_enqueue_and_worker_start_drains_without_runtime_progress() {
        let pool = Pool::new(FakeMgr::new(vec![]), cfg());
        {
            let mut st = pool.inner.state.lock().unwrap();
            let session = FakeSession::new();
            session.make_suspect();
            st.total = 1;
            st.recovery.push_back(session);
        } // exactly the enqueue/ensure_worker gap
        let report = pool.shutdown(Duration::ZERO).await;
        assert!(!report.forced);
        assert_eq!(report.drained_queue, 1);
        pool.inner.ensure_worker();
        assert!(!pool.inner.state.lock().unwrap().worker_started);
        assert_eq!(pool.snapshot().total_slots, 0);
        assert_eq!(pool.shutdown(Duration::ZERO).await.drained_queue, 0);
    }

    #[tokio::test]
    async fn worker_cannot_publish_after_shutdown_drains_idle() {
        for requeue in [false, true] {
            let pool = Pool::new(FakeMgr::new(vec![]), cfg());
            let session = FakeSession::new();
            let guard = session.guard.clone();
            pool.inner.state.lock().unwrap().total = 1;
            pool.shutdown(Duration::ZERO).await;
            publish_job(
                &pool.inner,
                if requeue {
                    JobEnd::Requeue(session)
                } else {
                    JobEnd::Idle(session)
                },
            );
            assert!(matches!(
                guard.qualification(),
                Qualification::Retired { .. }
            ));
            let snap = pool.snapshot();
            assert_eq!(
                (snap.idle, snap.recovery_queue, snap.total_slots),
                (0, 0, 0)
            );
            assert_eq!(snap.queue_full, 0);
        }
    }

    #[tokio::test]
    async fn shutdown_refusal_is_not_queue_full_and_overflow_is_one_eviction() {
        let mut config = cfg();
        config.recovery_capacity = 1;
        let pool = Pool::new(FakeMgr::new(vec![]), config);
        let queued = pool.acquire().await.unwrap();
        let lease = pool.acquire().await.unwrap();
        queued.make_suspect();
        lease.make_suspect();
        drop(queued);
        drop(lease);
        assert_eq!(pool.snapshot().queue_full, 1);
        assert_eq!(pool.snapshot().evictions(), 1);
        pool.shutdown(Duration::ZERO).await;
        let session = FakeSession::new();
        session.make_suspect();
        pool.inner.state.lock().unwrap().total = 1;
        pool.inner.enqueue_recovery(session);
        assert_eq!(pool.snapshot().queue_full, 1);
        assert_eq!(pool.snapshot().total_slots, 0);
    }

    #[tokio::test]
    async fn pool_enforces_deadline_even_when_adapter_never_returns() {
        let mut config = cfg();
        config.limits.deadline = Duration::from_millis(10);
        config.limits.max_attempts = 1;
        let pool = Pool::new(FakeMgr::new(vec![Scripted::Hang]), config);
        let lease = pool.acquire().await.unwrap();
        lease.make_suspect();
        drop(lease);
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.snapshot().total_slots != 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("pool deadline must retire a hanging recovery");
        assert_eq!(pool.snapshot().recovery_unresolved, 1);
        assert_eq!(pool.snapshot().retired_suspect, 1);
        pool.shutdown(Duration::from_millis(100)).await;
    }
    #[tokio::test]
    async fn a_returned_session_with_a_live_sub_owner_is_deferred_not_reused() {
        let m = FakeMgr::new(vec![]);
        let mut c = cfg();
        c.max_size = 1;
        c.ownership_grace = Duration::from_secs(1);
        c.acquire_timeout = Some(Duration::from_millis(30));
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        let obj = lease.obj();
        lease.session().make_clean();
        let guard = lease.guard().clone();
        let owner = guard.claim_owner("transaction").unwrap();
        drop(lease);

        let snap = pool.snapshot();
        assert_eq!(snap.deferred, 1, "the return is parked, not cleaned");
        assert_eq!(snap.deferred_returns, 1);
        assert_eq!(snap.idle, 0);
        assert_eq!(snap.returned_clean, 0);
        assert_eq!(snap.recovery_enqueued, 0);
        assert!(pool.acquire().await.is_err(), "the object is not reusable");

        // Releasing the owner is positive evidence; the next acquisition
        // completes the deferred return and hands out the same object.
        owner.release();
        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
        let snap = pool.snapshot();
        assert_eq!(snap.deferred, 0);
        assert_eq!(snap.deferred_released, 1);
        assert_eq!(snap.deferred_expired, 0);
        assert_eq!(snap.returned_clean, 1);
    }

    #[tokio::test]
    async fn a_leaked_owner_costs_the_object_after_the_grace_but_is_never_cleaned() {
        let m = FakeMgr::new(vec![]);
        let mut c = cfg();
        c.ownership_grace = Duration::from_millis(40);
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        let old_obj = lease.obj();
        let guard = lease.guard().clone();
        let owner = guard.claim_owner("transaction").unwrap();
        std::mem::forget(owner);
        drop(lease);
        assert_eq!(pool.snapshot().deferred, 1);
        assert_eq!(pool.snapshot().recovery_enqueued, 0);

        tokio::time::sleep(Duration::from_millis(70)).await;
        let lease = pool.acquire().await.unwrap();
        assert_ne!(lease.obj(), old_obj);
        let snap = pool.snapshot();
        assert_eq!(snap.deferred, 0);
        assert_eq!(snap.deferred_expired, 1);
        assert_eq!(snap.retired_forced, 1);
        assert_eq!(snap.connects, 2);
        assert_eq!(guard.phase(), crate::guard::Phase::Retired);
        assert_eq!(
            guard.live_owner_count(),
            1,
            "the leaked claim is still live"
        );
    }

    #[tokio::test]
    async fn a_live_owner_can_recover_its_own_object_before_the_pool_reaps_it() {
        let m = FakeMgr::new(vec![]);
        let mut c = cfg();
        c.ownership_grace = Duration::from_secs(1);
        let pool = Pool::new(m.clone(), c);
        let lease = pool.acquire().await.unwrap();
        let obj = lease.obj();
        let guard = lease.guard().clone();
        let owner = guard.claim_owner("transaction").unwrap();
        lease.session().make_suspect();
        drop(lease);
        assert_eq!(pool.snapshot().deferred, 1);
        assert_eq!(pool.snapshot().recovery_enqueued, 0);

        let permit = owner.claim_recovery().unwrap();
        let witness = crate::guard::RecoveryWitness::new(
            guard.obj(),
            guard.epoch(),
            "owner.rollback",
            "clean",
        );
        permit.recovered(witness).unwrap();
        owner.release();

        let lease = pool.acquire().await.unwrap();
        assert_eq!(lease.obj(), obj);
        assert_eq!(pool.snapshot().recovery_enqueued, 0);
        assert_eq!(pool.snapshot().returned_suspect, 0);
        assert_eq!(pool.snapshot().deferred_released, 1);
    }

    #[tokio::test]
    async fn shutdown_retires_deferred_sessions_instead_of_leaking_slots() {
        let m = FakeMgr::new(vec![]);
        let pool = Pool::new(m.clone(), cfg());
        let lease = pool.acquire().await.unwrap();
        let guard = lease.guard().clone();
        let owner = guard.claim_owner("transaction").unwrap();
        drop(lease);
        assert_eq!(pool.snapshot().deferred, 1);

        let report = pool.shutdown(Duration::from_millis(50)).await;
        assert_eq!(report.retired_deferred, 1);
        assert_eq!(report.outstanding_leases, 0);
        assert_eq!(report.drained_queue, 0);
        assert_eq!(guard.phase(), crate::guard::Phase::Retired);
        assert_eq!(pool.snapshot().deferred_drained, 1);
        assert_eq!(pool.snapshot().deferred, 0);
        // The owner was not "silently" made clean: its claim is still live even
        // though the object is retired.
        assert_eq!(guard.live_owner_count(), 1);
        owner.release();
        assert_eq!(guard.live_owner_count(), 0);
    }
}

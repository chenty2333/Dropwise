use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::Location;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use crate::obligation::{CurrentLedger, Leak, Ledger, set_current};

/// When the competing branch wins, measured from the moment the target returns
/// the planned `Pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Race {
    /// The competitor is ready at once.
    Immediate,
    /// The competitor becomes ready after yielding to the scheduler this many
    /// times (via `tokio::task::yield_now`). Tokio usually runs other ready tasks
    /// and polls its drivers first, but this is **not guaranteed**: the runtime
    /// may re-poll immediately.
    Reschedule(usize),
    /// The competitor becomes ready after this much Tokio time (virtual under
    /// `Flavor::CurrentThread`, real under `Flavor::MultiThread`).
    After(Duration),
    /// The competitor becomes ready once the target is woken, and the target is
    /// dropped without being polled again: the event it was waiting for has been
    /// delivered to it but not yet consumed (e.g. a `Notify` notification, a
    /// message handed to a waiting receiver). Needs no timing parameter.
    ///
    /// With [`Ctx::race`], such a run is only realizable if the program's
    /// `select!` may poll the competitor first; do not use it with a `biased;`
    /// `select!` that lists the target before the competitor.
    AfterWake,
}

impl fmt::Display for Race {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Race::Immediate => write!(f, "competitor ready immediately"),
            Race::Reschedule(n) => write!(f, "competitor ready after {n} reschedule(s)"),
            Race::After(d) => write!(f, "competitor ready after {d:?}"),
            Race::AfterWake => write!(f, "competitor ready once the target is woken"),
        }
    }
}

/// A pause a scenario asks for between an operation and the next use of the
/// shared object.
///
/// The scenario passes the pause it would use in production to [`Ctx::gap`];
/// [`Config::gap_choices`] can substitute other choices for a plan, so the
/// exploration covers both "the reply arrives before the reuse" and "it does
/// not". A gap is numbered by its `Ctx::gap` registration order within a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gap {
    /// Continue at once.
    Now,
    /// Yield to the Tokio scheduler this many times.
    Yield(usize),
    /// Wait for this much Tokio time (virtual under `Flavor::CurrentThread`).
    Sleep(Duration),
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gap::Now => write!(f, "now"),
            Gap::Yield(n) => write!(f, "after {n} yield(s)"),
            Gap::Sleep(d) => write!(f, "after {d:?}"),
        }
    }
}

/// What the calling context allows after an observed operation was cancelled.
///
/// This is the scenario author's explicit contract about the operation, not
/// something Dropwise infers from the observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Abandoning is allowed: the observation must equal the "operation never
    /// started" (`pre`) or "operation completed" (`post`) calibration.
    Atomic,
    /// The caller retries or carries on: the observation must equal `post`.
    Completed,
    /// The result must be handed to the caller (a consumed message, a dequeued
    /// job): only `pre` is allowed. Use this when taking the value and then
    /// being cancelled would lose it even though shared state looks complete.
    Undone,
}

/// How one cancellation trial's observation compared with its calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Equal to the "target dropped before its first poll" calibration.
    Pre,
    /// Equal to the "nothing cancelled" calibration. Also used when `pre` and
    /// `post` are equal, since the observation cannot distinguish them.
    Post,
    /// Equal to a calibration in which some of the cut targets were dropped
    /// before their first poll and the others completed (plans with several
    /// cuts only). Each cancelled operation is individually "not started" or
    /// "completed", which `Expect::Atomic` allows.
    Mixed,
    /// Equal to no calibration: a partial or contaminated state.
    Neither,
    /// Two samples of a calibration side disagreed, so the observation cannot
    /// be judged. Not a violation.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Single-threaded runtime with paused Tokio time. Timers and task
    /// scheduling are reproducible; real I/O, `std::time` and the blocking
    /// thread pool are not controlled.
    CurrentThread,
    /// Multi-threaded runtime with real time. Pending counts may vary between
    /// runs, so planned cancellations can go unrealized (see `Report::unrealized`).
    MultiThread { workers: usize },
}

/// What happens after the scenario returns and before results are collected.
///
/// Cancelled work may finish late (a blocking-pool read, a kernel operation, a
/// detached task). Settling gives it time to do so, so that late effects,
/// unowned results and `Ctx::after_settle` checks can be observed.
///
/// Under `Flavor::CurrentThread`, paused Tokio time does not auto-advance while
/// `spawn_blocking` work is running, so `tokio_time` also waits for blocking-pool
/// work (e.g. `tokio::fs`) that is in flight when settling starts. Work outside
/// Tokio (plain OS threads, the kernel, remote peers) needs `real_time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settle {
    /// Tokio time to let pass with the runtime still running.
    pub tokio_time: Duration,
    /// Wall-clock time to wait afterwards, for work outside Tokio's view
    /// (plain OS threads, the kernel, remote peers). Zero by default to keep
    /// tests fast. This is an observation window, not proof that the work is done.
    pub real_time: Duration,
    /// Wall-clock bound on waiting for `tokio_time` to pass. Blocking-pool work
    /// that never finishes keeps paused time from advancing; when the watchdog
    /// fires, the run is reported as not settled instead of hanging.
    /// Like [`Config::scenario_timeout`], it is enforced in-process.
    pub watchdog: Duration,
}

impl Default for Settle {
    fn default() -> Self {
        Self {
            tokio_time: Duration::from_millis(100),
            real_time: Duration::ZERO,
            watchdog: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Upper bound on scenario runs, baseline included.
    pub max_runs: usize,
    /// How many targets may be cancelled in the same run.
    pub max_cancellations: usize,
    /// Race modes tried at every cancellation point.
    pub races: Vec<Race>,
    /// Also explore, for every single cut, a plan that cancels the target and
    /// every later target created at the **same call site** at the same
    /// `Pending` boundary. Off by default. A plan counts the instances it
    /// actually cancelled in [`Trial::cancelled`].
    ///
    /// Use this for operations that leak a bounded amount per cancellation and
    /// only fail after repeated reuse (a flow-control window, a cached handle).
    /// The observation should reflect only the survivor's own result, not how
    /// many of the cancelled operations took effect; otherwise a legitimate
    /// mixture of partial effects is not equal to either calibration and is
    /// reported as a violation.
    pub repeat_at_site: bool,
    /// Extra gap choices tried for every single-cut plan (see [`Gap`]). Empty
    /// by default: the scenario's own default gap is used everywhere. Gap sites
    /// are numbered in `Ctx::gap` call order, and the assignment is recorded in
    /// [`Trial::gaps`] and part of the calibration cache key.
    pub gap_choices: Vec<Gap>,
    pub flavor: Flavor,
    pub settle: Settle,
    /// Wall-clock bound on one scenario run. A scenario that does not finish is
    /// reported as a liveness violation (and dropped) instead of hanging the
    /// exploration; stuck waiters are often exactly the defect being looked for.
    ///
    /// The bound is enforced in-process: it fires only when the scenario yields
    /// back to the runtime. A single `poll` that never returns (a busy loop, a
    /// blocking call on a runtime thread) is not interrupted, and tearing the
    /// runtime down still waits up to a second for blocking-pool work it cannot
    /// cancel. For batch runs, also bound each test process from outside.
    pub scenario_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_runs: 1000,
            max_cancellations: 1,
            races: vec![Race::Immediate, Race::Reschedule(1)],
            repeat_at_site: false,
            gap_choices: Vec::new(),
            flavor: Flavor::CurrentThread,
            settle: Settle::default(),
            scenario_timeout: Duration::from_secs(30),
        }
    }
}

/// One planned cancellation: target #`target` loses the race after its
/// `after_pending`-th `Pending`.
///
/// This is a *Pending boundary* of the marked future, not a source-level
/// `.await`: one `.await` may return `Pending` many times, and awaits that
/// complete immediately are never boundaries.
///
/// `after_pending == 0` means "dropped before the first poll". It is used only
/// by [`explore_observed`]'s calibration, never generated as a normal plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Index of the target in `ctx.target`/`ctx.race` call order within the run.
    pub target: usize,
    pub after_pending: usize,
    pub race: Race,
}

/// One run's cancellation plan, including the optional dimensions that are not
/// visible as separate [`Cut`]s.
#[derive(Debug, Clone, Default)]
struct Plan {
    cuts: Vec<Cut>,
    /// `cuts[0]` also applies to every later target created at the same call
    /// site, at that target's own `after_pending`-th `Pending`.
    repeat: bool,
    /// Per `Ctx::gap` registration, the selected `Config::gap_choices` index;
    /// `None` means the scenario's own default.
    gaps: Vec<Option<usize>>,
}

impl Plan {
    fn is_calibration_skip(&self) -> bool {
        !self.cuts.is_empty() && self.cuts.iter().all(|c| c.after_pending == 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    /// Lost the race; the competitor has not fired yet.
    Doomed,
    /// The competitor fired but the target was kept alive (only with `ctx.race`).
    Kept,
    Completed,
    /// Dropped after losing the race.
    Cancelled,
}

#[derive(Debug, Clone)]
struct TargetRecord {
    site: &'static Location<'static>,
    pendings: usize,
    status: Status,
    /// The planned cut for this target took effect.
    lost_race: bool,
}

type Check = Box<dyn FnOnce() -> Result<(), String> + Send>;

struct RunState {
    plan: Plan,
    /// Snapshot of `Config::gap_choices`, for `Ctx::gap`.
    gap_choices: Vec<Gap>,
    targets: Vec<TargetRecord>,
    checks: Vec<Check>,
    /// The scenario's default for every `Ctx::gap` call so far, in call order:
    /// the gap site index is the position.
    gap_defaults: Vec<Gap>,
}

/// Handle given to each scenario run. Cheap to clone and `Send`, so it can be
/// moved into spawned tasks.
#[derive(Clone)]
pub struct Ctx {
    state: Arc<Mutex<RunState>>,
}

impl Ctx {
    /// Mark `fut` as a cancellation target that Dropwise drops itself.
    /// Resolves to `None` if it was cancelled in this run, `Some(output)` otherwise.
    ///
    /// The code after `.await` stands in for the caller's post-cancellation
    /// control flow; it is the scenario author's job to make it faithful. Prefer
    /// [`Ctx::race`] when the real code has a `select!` to plug into.
    #[track_caller]
    pub fn target<F: Future>(&self, fut: F) -> Target<F> {
        let (core, cut) = TargetCore::register(self, Location::caller());
        if cut.is_some_and(|c| c.after_pending == 0) {
            // Calibration-only cut: dropped before its first poll.
            drop(fut);
            core.set(Status::Doomed);
            core.set(Status::Cancelled);
            return Target {
                core,
                cut,
                probe: None,
                phase: Phase::Skipped,
            };
        }
        Target {
            core,
            cut,
            probe: WakeProbe::for_cut(cut),
            phase: Phase::Running(Box::pin(fut)),
        }
    }

    /// Mark `fut` as a cancellation target inside the program's own `select!`.
    ///
    /// Returns the wrapped target and a [`Preempt`] future to add as a competing
    /// branch. When the plan says the target loses, it stops making progress and
    /// `Preempt` becomes ready; the program's `select!` then drops (or keeps) the
    /// target exactly as it would for a real competitor.
    ///
    /// ```ignore
    /// let (op, mut preempt) = ctx.race(read_frame(&mut conn));
    /// tokio::select! {
    ///     frame = op => handle(frame),
    ///     _ = &mut preempt => {} // stands for e.g. `interval.tick()`
    /// }
    /// ```
    #[track_caller]
    pub fn race<F: Future>(&self, fut: F) -> (Raced<F>, Preempt) {
        let (core, cut) = TargetCore::register(self, Location::caller());
        let shared = Arc::new(Mutex::new(RaceShared::default()));
        let skipped = cut.is_some_and(|c| c.after_pending == 0);
        if skipped {
            // Calibration-only cut: the target is dropped before its first
            // poll, and the competitor is ready at once.
            let mut sh = shared.lock().unwrap();
            sh.skipped = true;
            sh.doomed = true;
            sh.doomed_at = Some(tokio::time::Instant::now());
            drop(sh);
            core.set(Status::Doomed);
        }
        let probe = WakeProbe::for_cut(cut);
        let raced = Raced {
            core,
            cut,
            probe: probe.clone(),
            inner: Some(Box::pin(fut)),
            shared: shared.clone(),
        };
        let preempt = Preempt {
            shared,
            race: cut.map(|c| c.race),
            probe,
            delay: None,
            state: self.state.clone(),
            index: raced.core.index,
        };
        (raced, preempt)
    }

    /// The pause between an operation and the next use of the shared object.
    ///
    /// `default` is what the scenario would do in production. The exploration
    /// may substitute a choice from [`Config::gap_choices`]; calibration and
    /// plans without a substitution use `default`.
    ///
    /// Sites are numbered in call order, so a calibration that skips a target
    /// can change which later calls are reachable; keep gap calls on
    /// unconditional paths. `Ctx` is `Clone + Send`, so a spawned in-process
    /// server task can hold a clone and call this too, which puts its reply
    /// delay under the same gap assignment.
    pub fn gap(&self, default: Gap) -> GapWait {
        let mut st = self.state.lock().unwrap();
        let index = st.gap_defaults.len();
        st.gap_defaults.push(default);
        let chosen = st
            .plan
            .gaps
            .get(index)
            .copied()
            .flatten()
            .map(|choice| st.gap_choices[choice]);
        GapWait::new(chosen.unwrap_or(default))
    }

    /// Run `check` after the settle phase, when late effects of cancelled work
    /// have had a chance to happen. Its error counts as an invariant violation.
    pub fn after_settle(&self, check: impl FnOnce() -> Result<(), String> + Send + 'static) {
        self.state.lock().unwrap().checks.push(Box::new(check));
    }
}

/// Future returned by [`Ctx::gap`].
pub struct GapWait {
    gap: Gap,
    yielded: usize,
    /// The current `yield_now`, kept across polls: a fresh one every poll would
    /// always return `Pending` and never complete.
    yielding: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl GapWait {
    fn new(gap: Gap) -> Self {
        Self {
            gap,
            yielded: 0,
            yielding: None,
            sleep: None,
        }
    }
}

impl Future for GapWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        match this.gap {
            Gap::Now => Poll::Ready(()),
            Gap::Yield(n) => loop {
                if this.yielded >= n {
                    return Poll::Ready(());
                }
                let yield_now = this
                    .yielding
                    .get_or_insert_with(|| Box::pin(tokio::task::yield_now()));
                if yield_now.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                this.yielding = None;
                this.yielded += 1;
            },
            Gap::Sleep(d) => {
                let sleep = this
                    .sleep
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(d)));
                sleep.as_mut().poll(cx)
            }
        }
    }
}

/// Bookkeeping shared by both target kinds.
struct TargetCore {
    state: Arc<Mutex<RunState>>,
    index: usize,
    seen: usize,
}

impl TargetCore {
    fn register(ctx: &Ctx, site: &'static Location<'static>) -> (Self, Option<Cut>) {
        let mut st = ctx.state.lock().unwrap();
        let index = st.targets.len();
        st.targets.push(TargetRecord {
            site,
            pendings: 0,
            status: Status::Running,
            lost_race: false,
        });
        let cut = st
            .plan
            .cuts
            .iter()
            .find(|c| c.target == index)
            .copied()
            .or_else(|| {
                if !st.plan.repeat {
                    return None;
                }
                let start = st.plan.cuts.first().copied()?;
                if index <= start.target || st.targets.get(start.target)?.site != site {
                    return None;
                }
                Some(Cut {
                    target: index,
                    after_pending: start.after_pending,
                    race: start.race,
                })
            });
        (
            Self {
                state: ctx.state.clone(),
                index,
                seen: 0,
            },
            cut,
        )
    }

    fn set(&self, status: Status) {
        let mut st = self.state.lock().unwrap();
        let rec = &mut st.targets[self.index];
        rec.pendings = self.seen;
        rec.status = status;
        if status == Status::Doomed {
            rec.lost_race = true;
        }
    }

    fn status(&self) -> Status {
        self.state.lock().unwrap().targets[self.index].status
    }

    /// Count a `Pending`; true if this is the planned losing boundary.
    fn pending(&mut self, cut: Option<Cut>) -> bool {
        self.seen += 1;
        let lose = cut.is_some_and(|c| c.after_pending == self.seen);
        self.set(if lose {
            Status::Doomed
        } else {
            Status::Running
        });
        lose
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Waits until the competitor should fire.
enum Delay {
    Now,
    Reschedule {
        remaining: usize,
        current: Option<BoxFuture>,
    },
    Sleep(Pin<Box<tokio::time::Sleep>>),
    WaitWake(Arc<WakeProbe>),
}

impl Delay {
    /// Call when the target returns the planned `Pending`: `After` counts from here.
    fn new(race: Race, probe: Option<&Arc<WakeProbe>>) -> Self {
        Self::starting_at(race, tokio::time::Instant::now(), probe)
    }

    fn starting_at(
        race: Race,
        start: tokio::time::Instant,
        probe: Option<&Arc<WakeProbe>>,
    ) -> Self {
        match race {
            Race::Immediate | Race::Reschedule(0) => Delay::Now,
            Race::Reschedule(n) => Delay::Reschedule {
                remaining: n,
                current: None,
            },
            Race::After(d) => Delay::Sleep(Box::pin(tokio::time::sleep_until(start + d))),
            Race::AfterWake => {
                Delay::WaitWake(probe.expect("AfterWake targets have a probe").clone())
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match self {
            Delay::Now => Poll::Ready(()),
            Delay::WaitWake(probe) => {
                // Register first, so a wake between the check and returning is not lost.
                *probe.waker.lock().unwrap() = Some(cx.waker().clone());
                if probe.woken.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
            Delay::Sleep(sleep) => sleep.as_mut().poll(cx),
            Delay::Reschedule { remaining, current } => loop {
                if let Some(fut) = current {
                    if fut.as_mut().poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    *current = None;
                    *remaining -= 1;
                }
                if *remaining == 0 {
                    return Poll::Ready(());
                }
                *current = Some(Box::pin(tokio::task::yield_now()));
            },
        }
    }
}

/// Sits between a target's inner future and the task waker, to see when the
/// inner future is woken (only for `Race::AfterWake` plans).
#[derive(Default)]
struct WakeProbe {
    /// Woken since the inner future was last polled.
    woken: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl WakeProbe {
    fn for_cut(cut: Option<Cut>) -> Option<Arc<Self>> {
        matches!(
            cut,
            Some(Cut {
                race: Race::AfterWake,
                ..
            })
        )
        .then(Arc::default)
    }

    /// Poll `fut` through `probe` if there is one, else directly.
    fn poll<F: Future>(
        probe: &Option<Arc<Self>>,
        fut: Pin<&mut F>,
        cx: &mut Context<'_>,
    ) -> Poll<F::Output> {
        let Some(probe) = probe else {
            return fut.poll(cx);
        };
        probe.woken.store(false, Ordering::SeqCst);
        *probe.waker.lock().unwrap() = Some(cx.waker().clone());
        let waker = Waker::from(probe.clone());
        fut.poll(&mut Context::from_waker(&waker))
    }
}

impl Wake for WakeProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().unwrap().as_ref() {
            w.wake_by_ref();
        }
    }
}

enum Phase<F> {
    Running(Pin<Box<F>>),
    /// Lost the race; kept alive (unpolled) until the competitor fires.
    Doomed {
        _inner: Pin<Box<F>>,
        delay: Delay,
    },
    /// Calibration-only cut: dropped before its first poll.
    Skipped,
    Done,
}

/// Future returned by [`Ctx::target`].
pub struct Target<F> {
    core: TargetCore,
    cut: Option<Cut>,
    probe: Option<Arc<WakeProbe>>,
    phase: Phase<F>,
}

impl<F: Future> Future for Target<F> {
    type Output = Option<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `Target` is `Unpin`: the inner future is boxed.
        let this = self.get_mut();
        loop {
            match &mut this.phase {
                Phase::Running(inner) => match WakeProbe::poll(&this.probe, inner.as_mut(), cx) {
                    Poll::Ready(v) => {
                        this.phase = Phase::Done;
                        this.core.set(Status::Completed);
                        return Poll::Ready(Some(v));
                    }
                    Poll::Pending => {
                        if !this.core.pending(this.cut) {
                            return Poll::Pending;
                        }
                        let Phase::Running(inner) = std::mem::replace(&mut this.phase, Phase::Done)
                        else {
                            unreachable!()
                        };
                        let delay = Delay::new(
                            this.cut.expect("losing implies a cut").race,
                            this.probe.as_ref(),
                        );
                        this.phase = Phase::Doomed {
                            _inner: inner,
                            delay,
                        };
                    }
                },
                Phase::Doomed { delay, .. } => {
                    if delay.poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    // Drop the target before reporting, so its destructors run
                    // where a real `select!` would run them.
                    this.phase = Phase::Done;
                    this.core.set(Status::Cancelled);
                    return Poll::Ready(None);
                }
                Phase::Skipped => {
                    this.phase = Phase::Done;
                    return Poll::Ready(None);
                }
                Phase::Done => panic!("dropwise::Target polled after completion"),
            }
        }
    }
}

impl<F> Drop for Target<F> {
    fn drop(&mut self) {
        // Dropped by outer code while waiting for the competitor: still a cancellation.
        if matches!(self.phase, Phase::Doomed { .. }) {
            self.core.set(Status::Cancelled);
        }
    }
}

#[derive(Default)]
struct RaceShared {
    /// Calibration-only cut: the target is dropped before its first poll.
    skipped: bool,
    doomed: bool,
    /// When the target returned the planned `Pending`; `Race::After` counts from here.
    doomed_at: Option<tokio::time::Instant>,
    fired: bool,
    target_waker: Option<Waker>,
    competitor_waker: Option<Waker>,
}

/// Target future returned by [`Ctx::race`]; behaves like the wrapped future
/// except that it stalls between losing the race and the competitor firing.
pub struct Raced<F> {
    core: TargetCore,
    cut: Option<Cut>,
    probe: Option<Arc<WakeProbe>>,
    inner: Option<Pin<Box<F>>>,
    shared: Arc<Mutex<RaceShared>>,
}

impl<F: Future> Future for Raced<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        {
            let mut sh = this.shared.lock().unwrap();
            if sh.skipped {
                // Never poll the inner future: this is the "operation did not
                // start" calibration. The competitor is already ready.
                sh.target_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            if sh.doomed && !sh.fired {
                // Between losing and the competitor firing: no progress, like a
                // branch the real competitor is about to beat.
                sh.target_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        let inner = this
            .inner
            .as_mut()
            .expect("dropwise::Raced polled after completion");
        match WakeProbe::poll(&this.probe, inner.as_mut(), cx) {
            Poll::Ready(v) => {
                this.inner = None;
                this.core.set(Status::Completed);
                Poll::Ready(v)
            }
            Poll::Pending => {
                let fired = this.shared.lock().unwrap().fired;
                if fired {
                    // Kept alive by the program and polled again after losing once.
                    this.core.seen += 1;
                    this.core.set(Status::Kept);
                } else if this.core.pending(this.cut) {
                    let mut sh = this.shared.lock().unwrap();
                    sh.doomed = true;
                    sh.doomed_at = Some(tokio::time::Instant::now());
                    sh.target_waker = Some(cx.waker().clone());
                    if let Some(w) = sh.competitor_waker.take() {
                        w.wake();
                    }
                }
                Poll::Pending
            }
        }
    }
}

impl<F> Drop for Raced<F> {
    fn drop(&mut self) {
        if self.inner.is_some() && matches!(self.core.status(), Status::Doomed | Status::Kept) {
            self.core.set(Status::Cancelled);
        }
    }
}

/// Competing branch returned by [`Ctx::race`]. Ready exactly once, when the
/// plan says the target loses; pending forever otherwise, so it can be reused
/// across loop iterations with `&mut preempt`.
pub struct Preempt {
    shared: Arc<Mutex<RaceShared>>,
    race: Option<Race>,
    probe: Option<Arc<WakeProbe>>,
    /// Created once the target is doomed, so the delay is measured from the
    /// planned `Pending`, not from `Ctx::race`.
    delay: Option<Delay>,
    state: Arc<Mutex<RunState>>,
    index: usize,
}

impl Future for Preempt {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let skipped = this.shared.lock().unwrap().skipped;
        if skipped {
            // The calibration-only cut is already "lost": ready at once.
            this.shared.lock().unwrap().fired = true;
            this.mark_kept();
            return Poll::Ready(());
        }
        let doomed_at = {
            let mut sh = this.shared.lock().unwrap();
            if !sh.doomed || sh.fired {
                sh.competitor_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            sh.doomed_at.expect("doomed implies doomed_at")
        };
        let race = this.race.expect("doomed implies a cut");
        let delay = this
            .delay
            .get_or_insert_with(|| Delay::starting_at(race, doomed_at, this.probe.as_ref()));
        if delay.poll(cx).is_pending() {
            return Poll::Pending;
        }
        let target_waker = {
            let mut sh = this.shared.lock().unwrap();
            sh.fired = true;
            sh.target_waker.take()
        };
        this.mark_kept();
        // A program that keeps the target alive must be able to poll it again.
        if let Some(w) = target_waker {
            w.wake();
        }
        Poll::Ready(())
    }
}

impl Preempt {
    fn mark_kept(&self) {
        let mut st = self.state.lock().unwrap();
        let rec = &mut st.targets[self.index];
        if rec.status == Status::Doomed {
            rec.status = Status::Kept; // becomes Cancelled if the target is dropped
        }
    }
}

impl Drop for Preempt {
    /// A competitor that disappears can no longer win; release a stalled target
    /// instead of hanging the scenario. (A competitor that stays alive but is
    /// never polled still stalls it; that shows up as `CutOutcome::Stalled`.)
    fn drop(&mut self) {
        let mut sh = self.shared.lock().unwrap();
        if sh.doomed && !sh.fired {
            sh.fired = true;
            if let Some(w) = sh.target_waker.take() {
                w.wake();
            }
        }
    }
}

/// What a planned cut actually did in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutOutcome {
    /// The target lost the race and was dropped.
    Cancelled,
    /// The target lost the race but the program kept it alive (`ctx.race` only).
    Kept,
    /// The target lost the race but the competitor never fired before the run ended.
    Stalled,
    /// The planned boundary was never reached (target not created, or fewer
    /// `Pending`s than planned). Nothing was tested by this cut.
    Unrealized,
}

/// Result of one run with a non-empty cancellation plan.
#[derive(Debug, Clone)]
pub struct Trial {
    pub plan: Vec<Cut>,
    /// Call sites of the planned targets, parallel to `plan`; `None` if the
    /// target was not created in this run.
    pub sites: Vec<Option<&'static Location<'static>>>,
    /// Parallel to `plan`.
    pub outcomes: Vec<CutOutcome>,
    /// How many target instances this plan actually cancelled. With
    /// [`Config::repeat_at_site`] this can exceed `plan.len()`.
    pub cancelled: usize,
    /// Per `Ctx::gap` registration, the substituted gap; `None` means the
    /// scenario's own default was used. Empty when no `Ctx::gap` was called.
    pub gaps: Vec<Option<Gap>>,
    /// How the observed value compared with the calibration, when the run was
    /// started by [`explore_observed`]. `None` for [`explore`], or when the
    /// scenario produced no observation.
    pub verdict: Option<Verdict>,
    pub invariant: Result<(), String>,
    pub leaks: Vec<Leak>,
    /// False if the settle phase was cut short by the watchdog: late effects
    /// may be missing, so a clean result is inconclusive.
    pub settled: bool,
}

impl Trial {
    pub fn is_violation(&self) -> bool {
        self.invariant.is_err() || !self.leaks.is_empty()
    }

    pub fn is_realized(&self) -> bool {
        !self.outcomes.contains(&CutOutcome::Unrealized)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub trials: Vec<Trial>,
    /// Problems seen in the uncancelled run itself (not caused by cancellation).
    pub baseline_errors: Vec<String>,
    /// Number of targets reached in the uncancelled run.
    pub baseline_targets: usize,
    /// False if the uncancelled run's settle phase was cut short by the watchdog.
    pub baseline_settled: bool,
    /// True if every plan within the limits was run: `max_runs` did not cut the
    /// search short and at least one target was reached. Says nothing about
    /// source-level `.await`s, inputs or schedules that were not exercised.
    pub exhaustive: bool,
    /// Non-degenerate hints, e.g. a calibration whose `pre` and `post` are
    /// equal. Informational only; they do not affect [`Report::is_clean`].
    pub notes: Vec<String>,
}

impl Report {
    pub fn violations(&self) -> impl Iterator<Item = &Trial> {
        self.trials.iter().filter(|t| t.is_violation())
    }

    /// Trials whose settle phase was cut short by the watchdog.
    pub fn unsettled(&self) -> impl Iterator<Item = &Trial> {
        self.trials.iter().filter(|t| !t.settled)
    }

    /// Trials in which some planned cut never took effect.
    pub fn unrealized(&self) -> impl Iterator<Item = &Trial> {
        self.trials.iter().filter(|t| !t.is_realized())
    }

    /// Trials whose calibration could not judge the observation.
    pub fn unknown(&self) -> impl Iterator<Item = &Trial> {
        self.trials
            .iter()
            .filter(|t| t.verdict == Some(Verdict::Unknown))
    }

    /// Number of trials with this verdict; always 0 for a plain [`explore`]
    /// report, whose trials carry no verdict.
    pub fn verdict_count(&self, verdict: Verdict) -> usize {
        self.trials
            .iter()
            .filter(|t| t.verdict == Some(verdict))
            .count()
    }

    /// Nothing was *observed* to go wrong: the baseline run reported no problem
    /// and no trial violated its invariant or leaked.
    ///
    /// This is not a pass conclusion on its own. It says nothing about plans
    /// that were never realized ([`Trial::is_realized`]), observation windows
    /// cut short by the settle watchdog ([`Report::unsettled`]), plans the run
    /// budget did not reach (`exhaustive`), or schedules and inputs the scenario
    /// did not exercise. [`assert_cancel_correct`] also checks realization,
    /// settling and completeness within the configured limits, but cannot
    /// establish correctness for untested schedules or inputs.
    pub fn is_clean(&self) -> bool {
        self.baseline_errors.is_empty() && self.violations().next().is_none()
    }
}

fn describe(t: &Trial) -> String {
    let mut s = t
        .plan
        .iter()
        .zip(&t.sites)
        .zip(&t.outcomes)
        .map(|((c, site), outcome)| {
            let site = site.map_or("not created".to_string(), |s| s.to_string());
            format!(
                "target #{} ({site}) loses after pending #{}, {} -> {outcome:?}",
                c.target, c.after_pending, c.race
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    let gaps: Vec<_> = t
        .gaps
        .iter()
        .enumerate()
        .filter_map(|(i, gap)| gap.map(|g| format!("gap #{i} = {g}")))
        .collect();
    if !gaps.is_empty() {
        s.push_str(&format!(" [{}]", gaps.join(", ")));
    }
    if t.cancelled > t.plan.len() {
        s.push_str(&format!(" [{} instances cancelled]", t.cancelled));
    }
    s
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "dropwise: {} target(s), {} cancellation plan(s) explored{}",
            self.baseline_targets,
            self.trials.len(),
            if self.exhaustive {
                ""
            } else {
                " (NOT exhaustive)"
            }
        )?;
        if self.baseline_targets == 0 {
            writeln!(
                f,
                "  the uncancelled run reached no target: nothing was explored (mark ctx.target/ctx.race)"
            )?;
        }
        for e in &self.baseline_errors {
            writeln!(f, "  baseline (no cancellation): {e}")?;
        }
        for t in self.violations() {
            writeln!(f, "  {}:", describe(t))?;
            if let Err(e) = &t.invariant {
                writeln!(f, "    invariant violated: {e}")?;
            }
            for l in &t.leaks {
                writeln!(f, "    {l}")?;
            }
        }
        if !self.baseline_settled {
            writeln!(
                f,
                "  baseline: observation window ended before settling (watchdog)"
            )?;
        }
        let unsettled: Vec<_> = self.unsettled().collect();
        if !unsettled.is_empty() {
            writeln!(
                f,
                "  {} plan(s): observation window ended before settling (watchdog); results inconclusive:",
                unsettled.len()
            )?;
            for t in unsettled {
                writeln!(f, "    {}", describe(t))?;
            }
        }
        let unrealized: Vec<_> = self.unrealized().filter(|t| !t.is_violation()).collect();
        if !unrealized.is_empty() {
            writeln!(
                f,
                "  {} plan(s) not realized (nothing tested):",
                unrealized.len()
            )?;
            for t in unrealized {
                writeln!(f, "    {}", describe(t))?;
            }
        }
        let decided = self.trials.iter().filter(|t| t.verdict.is_some()).count();
        if decided > 0 {
            writeln!(
                f,
                "  verdicts ({decided} plan(s)): pre={} post={} mixed={} neither={} unknown={}",
                self.verdict_count(Verdict::Pre),
                self.verdict_count(Verdict::Post),
                self.verdict_count(Verdict::Mixed),
                self.verdict_count(Verdict::Neither),
                self.verdict_count(Verdict::Unknown),
            )?;
        }
        let unknown: Vec<_> = self.unknown().collect();
        if !unknown.is_empty() {
            writeln!(
                f,
                "  {} plan(s) cannot be decided (unstable calibration):",
                unknown.len()
            )?;
            for t in unknown {
                writeln!(f, "    {}", describe(t))?;
            }
        }
        for note in &self.notes {
            writeln!(f, "  note: {note}")?;
        }
        Ok(())
    }
}

fn build_runtime(flavor: Flavor, ledger: &Arc<Ledger>) -> tokio::runtime::Runtime {
    let mut builder = match flavor {
        Flavor::CurrentThread => {
            let mut b = tokio::runtime::Builder::new_current_thread();
            b.start_paused(true);
            b
        }
        Flavor::MultiThread { workers } => {
            let mut b = tokio::runtime::Builder::new_multi_thread();
            b.worker_threads(workers.max(1));
            b
        }
    };
    // Blocking-pool threads run cancelled work too, so they need the ledger as well.
    let ledger = ledger.clone();
    builder.on_thread_start(move || {
        set_current(Some(ledger.clone()));
    });
    builder.on_thread_stop(|| {
        set_current(None);
    });
    // enable_all: time plus I/O drivers (network, files, io-uring under tokio_unstable),
    // so scenarios can use real I/O; drivers the build does not include are skipped.
    builder.enable_all().build().expect("build tokio runtime")
}

/// Grace period for tearing down one run's runtime: a `Runtime` drop waits
/// forever for `spawn_blocking` work to return, and detached cancelled work that
/// never returns is exactly what a scenario leaves behind.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// The runtime of one run, shut down with [`SHUTDOWN_GRACE`] on every exit
/// path. Without the bound on the unwind path, a scenario panic hangs the whole
/// process instead of reaching the caller. `Runtime::shutdown_timeout` consumes
/// the runtime, which is why it is held in an `Option`.
struct RunRuntime {
    rt: Option<tokio::runtime::Runtime>,
}

impl RunRuntime {
    fn new(rt: tokio::runtime::Runtime) -> Self {
        Self { rt: Some(rt) }
    }

    fn get(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("shut down only when the run ends")
    }

    fn close(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(SHUTDOWN_GRACE);
        }
    }
}

impl Drop for RunRuntime {
    /// Reached only when the run unwinds before `close`.
    fn drop(&mut self) {
        self.close();
    }
}

struct RunResult {
    settled: bool,
    invariant: Result<(), String>,
    leaks: Vec<Leak>,
    targets: Vec<TargetRecord>,
    /// The default of every `Ctx::gap` registration in this run.
    gap_defaults: Vec<Gap>,
}

/// Real-time bound for one bounded `block_on`.
///
/// The watchdog thread waits on a condvar instead of sleeping for the whole
/// limit, so it is gone as soon as the run is over. Sleeping out the limit would
/// leave two threads (scenario plus settle) alive per run until their own
/// timeouts expire, which adds up to hundreds of threads in one search.
struct Watchdog {
    stop: Arc<(Mutex<bool>, Condvar)>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn start(limit: Duration, fired: tokio::sync::oneshot::Sender<()>) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let shared = stop.clone();
        let join = std::thread::spawn(move || {
            let guard = shared.0.lock().unwrap();
            let (stopped, _timed_out) = shared
                .1
                .wait_timeout_while(guard, limit, |stopped| !*stopped)
                .unwrap();
            if !*stopped {
                let _ = fired.send(());
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let (flag, cv) = &*self.stop;
        *flag.lock().unwrap() = true;
        cv.notify_one();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Drive `fut` on `rt`, giving up after `limit` of wall-clock time. The bound
/// is enforced from a plain thread because paused Tokio time may not advance.
fn block_on_bounded<F: Future>(
    rt: &tokio::runtime::Runtime,
    limit: Duration,
    fut: F,
) -> Option<F::Output> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let _watchdog = Watchdog::start(limit, tx);
    rt.block_on(async {
        tokio::select! {
            out = fut => Some(out),
            _ = rx => None,
        }
    })
}

/// Returns false if the watchdog cut the Tokio-time phase short.
fn settle(rt: &tokio::runtime::Runtime, settle: Settle) -> bool {
    // The sleep must be created inside the runtime, hence the async block.
    let settled = block_on_bounded(rt, settle.watchdog, async {
        tokio::time::sleep(settle.tokio_time).await
    })
    .is_some();
    if !settle.real_time.is_zero() {
        std::thread::sleep(settle.real_time);
        // Let tasks woken by the late work run.
        rt.block_on(async {
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
        });
    }
    settled
}

fn run_once<S, Fut>(config: &Config, scenario: &mut S, plan: Plan) -> RunResult
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let ledger = Arc::new(Ledger::default());
    let ctx = Ctx {
        state: Arc::new(Mutex::new(RunState {
            plan,
            gap_choices: config.gap_choices.clone(),
            targets: Vec::new(),
            checks: Vec::new(),
            gap_defaults: Vec::new(),
        })),
    };
    let mut rt = RunRuntime::new(build_runtime(config.flavor, &ledger));

    let _ledger = CurrentLedger::attach(ledger.clone());
    let mut invariant = block_on_bounded(rt.get(), config.scenario_timeout, scenario(ctx.clone()))
        .unwrap_or_else(|| {
            Err(format!(
                "scenario did not finish within {:?} (liveness)",
                config.scenario_timeout
            ))
        });
    let settled = settle(rt.get(), config.settle);
    let checks = std::mem::take(&mut ctx.state.lock().unwrap().checks);
    for check in checks {
        if let Err(e) = check() {
            invariant = match invariant {
                Ok(()) => Err(e),
                Err(prev) => Err(format!("{prev}; {e}")),
            };
        }
    }
    // Only what happened up to here counts; tasks torn down by runtime shutdown
    // are an accepted form of abandonment.
    let leaks = ledger.finish();
    drop(_ledger);
    rt.close();

    let (targets, gap_defaults) = {
        let st = ctx.state.lock().unwrap();
        (st.targets.clone(), st.gap_defaults.clone())
    };
    RunResult {
        settled,
        invariant,
        leaks,
        targets,
        gap_defaults,
    }
}

fn outcome(rec: Option<&TargetRecord>) -> CutOutcome {
    match rec {
        Some(r) if r.lost_race => match r.status {
            Status::Cancelled => CutOutcome::Cancelled,
            Status::Doomed => CutOutcome::Stalled,
            _ => CutOutcome::Kept,
        },
        _ => CutOutcome::Unrealized,
    }
}

fn trial_from(config: &Config, plan: &Plan, res: &RunResult) -> Trial {
    let sites = plan
        .cuts
        .iter()
        .map(|c| res.targets.get(c.target).map(|t| t.site))
        .collect();
    let outcomes = plan
        .cuts
        .iter()
        .map(|c| outcome(res.targets.get(c.target)))
        .collect();
    let cancelled = res
        .targets
        .iter()
        .filter(|r| r.lost_race && r.status == Status::Cancelled)
        .count();
    let gaps = plan
        .gaps
        .iter()
        .map(|choice| choice.map(|i| config.gap_choices[i]))
        .collect();
    Trial {
        plan: plan.cuts.clone(),
        sites,
        outcomes,
        cancelled,
        gaps,
        verdict: None,
        invariant: res.invariant.clone(),
        leaks: res.leaks.clone(),
        settled: res.settled,
    }
}

fn later_target_at_site(res: &RunResult, target: usize) -> bool {
    let Some(rec) = res.targets.get(target) else {
        return false;
    };
    res.targets
        .iter()
        .skip(target + 1)
        .any(|later| later.site == rec.site)
}

/// Queue the plans one run makes reachable. With `repeat_at_site` and
/// `gap_choices` off, this reproduces the breadth-first single/pair order.
fn extend_queue(
    config: &Config,
    plan: &Plan,
    res: &RunResult,
    slots: usize,
    queue: &mut VecDeque<Plan>,
    truncated: &mut bool,
) {
    if plan.cuts.len() >= config.max_cancellations
        || plan.repeat
        || plan.gaps.iter().any(Option::is_some)
        || plan.is_calibration_skip()
    {
        return;
    }
    // Only extend with later targets, so each combination is generated once.
    let start = plan.cuts.last().map_or(0, |c| c.target + 1);
    // Exactly `slots` plans can still be popped, and a pop frees one slot while
    // a push takes one: a plan queued behind that many others can never run, so
    // neither can its subtree. Skipping it keeps the queue proportional to the
    // run budget instead of to the combinatorial frontier, and leaves the
    // search non-exhaustive.
    'plans: for (target, rec) in res.targets.iter().enumerate().skip(start) {
        for after_pending in 1..=rec.pendings {
            for &race in &config.races {
                let cut = Cut {
                    target,
                    after_pending,
                    race,
                };
                let mut next = plan.clone();
                next.cuts.push(cut);
                let mut variants = vec![next.clone()];
                if next.cuts.len() == 1 {
                    if config.repeat_at_site && later_target_at_site(res, target) {
                        variants.push(Plan {
                            repeat: true,
                            ..next.clone()
                        });
                    }
                    if !config.gap_choices.is_empty() {
                        // One variant per gap site and substitution; the other
                        // sites keep their scenario default, and a choice equal
                        // to that site's default would only repeat the plan.
                        let templates = variants.clone();
                        for template in templates {
                            for (gap, default) in res.gap_defaults.iter().enumerate() {
                                for (choice, substitute) in config.gap_choices.iter().enumerate() {
                                    if substitute == default {
                                        continue;
                                    }
                                    let mut variant = template.clone();
                                    variant.gaps.resize(gap + 1, None);
                                    variant.gaps[gap] = Some(choice);
                                    variants.push(variant);
                                }
                            }
                        }
                    }
                }
                for variant in variants {
                    if queue.len() >= slots {
                        *truncated = true;
                        break 'plans;
                    }
                    queue.push_back(variant);
                }
            }
        }
    }
}

/// Run `scenario` under every cancellation plan, breadth-first: the baseline
/// (nothing cancelled), then every single cut, then pairs, ... up to
/// `config.max_cancellations`. Each run gets a fresh runtime.
///
/// Plans enumerate the observed `Pending` boundaries of the marked futures for
/// the scenario's fixed input; they are not a model of every schedule.
pub fn explore<S, Fut>(config: &Config, mut scenario: S) -> Report
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    search(config, |plan, _| {
        (
            run_once(config, &mut scenario, plan.clone()),
            Judgement::default(),
        )
    })
}

/// What a search step adds to its trial beyond the run itself.
#[derive(Default)]
struct Judgement {
    verdict: Option<Verdict>,
    /// Appended to the trial's invariant result.
    violation: Option<String>,
}

/// The breadth-first plan search shared by [`explore`] and [`explore_observed`].
/// `step` runs one plan; it may add calibration errors and notes to the report.
fn search(
    config: &Config,
    mut step: impl FnMut(&Plan, &mut Report) -> (RunResult, Judgement),
) -> Report {
    let mut report = Report::default();
    let mut queue: VecDeque<Plan> = VecDeque::from([Plan::default()]);
    let mut runs = 0;
    let mut truncated = false;

    while runs < config.max_runs {
        let Some(plan) = queue.pop_front() else { break };
        runs += 1;
        let (res, judgement) = step(&plan, &mut report);

        if plan.cuts.is_empty() {
            report.baseline_targets = res.targets.len();
            report.baseline_settled = res.settled;
            if let Err(e) = &res.invariant {
                report.baseline_errors.push(e.clone());
            }
            report
                .baseline_errors
                .extend(res.leaks.iter().map(|l| l.to_string()));
        } else {
            let mut trial = trial_from(config, &plan, &res);
            trial.verdict = judgement.verdict;
            if let Some(message) = judgement.violation {
                trial.invariant = match trial.invariant {
                    Ok(()) => Err(message),
                    Err(prev) => Err(format!("{prev}; {message}")),
                };
            }
            report.trials.push(trial);
        }

        let slots = config.max_runs - runs;
        extend_queue(config, &plan, &res, slots, &mut queue, &mut truncated);
    }

    report.exhaustive = queue.is_empty() && !truncated && report.baseline_targets > 0;
    report
}

fn run_observed<T, S, Fut>(
    config: &Config,
    scenario: &mut S,
    plan: Plan,
) -> (RunResult, Option<Result<T, String>>)
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let slot = Arc::new(Mutex::new(None));
    let mut wrapper = |ctx: Ctx| {
        let slot = slot.clone();
        let fut = scenario(ctx);
        async move {
            let out = fut.await;
            *slot.lock().unwrap() = Some(out);
            Ok(())
        }
    };
    let mut res = run_once(config, &mut wrapper, plan);
    let observed = slot.lock().unwrap().take();
    // The wrapper maps to `()`, so put a scenario error back where the report
    // shows it (baseline_errors for the baseline, the invariant for a trial).
    if let Some(Err(e)) = &observed {
        res.invariant = match std::mem::replace(&mut res.invariant, Ok(())) {
            Ok(()) => Err(e.clone()),
            Err(prev) => Err(format!("{prev}; {e}")),
        };
    }
    (res, observed)
}

/// The target set a plan calibrated: the explicit cuts, or for a repeated site
/// every instance that actually lost its race (falling back to the planned
/// target when none was created).
fn calibration_targets(plan: &Plan, res: &RunResult) -> Vec<usize> {
    let mut targets: Vec<usize> = if plan.repeat {
        let cut: Vec<_> = res
            .targets
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.lost_race)
            .map(|(index, _)| index)
            .collect();
        if cut.is_empty() {
            plan.cuts.iter().map(|c| c.target).collect()
        } else {
            cut
        }
    } else {
        plan.cuts.iter().map(|c| c.target).collect()
    };
    targets.sort_unstable();
    targets.dedup();
    targets
}

/// One calibration side (or a combination of skipped/completed targets): the
/// target indices, whether each is dropped before its first poll, and the gap
/// assignment. Trailing default gaps are dropped so equivalent assignments
/// share a cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CalibrationKey {
    targets: Vec<usize>,
    skipped: Vec<bool>,
    gaps: Vec<Option<usize>>,
}

impl CalibrationKey {
    /// `targets` must be sorted and deduplicated, `skipped` parallel to it.
    fn new(targets: &[usize], skipped: Vec<bool>, gaps: &[Option<usize>]) -> Self {
        let mut gaps = gaps.to_vec();
        while gaps.last() == Some(&None) {
            gaps.pop();
        }
        Self {
            targets: targets.to_vec(),
            skipped,
            gaps,
        }
    }

    fn side(&self) -> &'static str {
        if self.skipped.iter().all(|s| *s) {
            "pre"
        } else if self.skipped.iter().all(|s| !*s) {
            "post"
        } else {
            "mixed"
        }
    }

    fn plan(&self) -> Plan {
        let cuts = self
            .targets
            .iter()
            .zip(&self.skipped)
            .filter(|(_, skip)| **skip)
            .map(|(target, _)| Cut {
                target: *target,
                after_pending: 0,
                race: Race::Immediate,
            })
            .collect();
        Plan {
            cuts,
            repeat: false,
            gaps: self.gaps.clone(),
        }
    }

    fn label(&self) -> String {
        let targets = self
            .targets
            .iter()
            .zip(&self.skipped)
            .map(|(t, skip)| format!("#{t}{}", if *skip { " skipped" } else { "" }))
            .collect::<Vec<_>>()
            .join(", ");
        format!("calibration {} for target(s) [{targets}]", self.side())
    }
}

/// Two samples of one calibration side. `Stable` means both runs agreed.
enum Side<T> {
    Stable(T),
    Unstable,
    Absent,
}

impl<T: PartialEq> Side<T> {
    fn from_samples(mut samples: Vec<T>) -> Self {
        match samples.len() {
            0 => Side::Absent,
            _ if samples.windows(2).all(|w| w[0] == w[1]) => Side::Stable(samples.swap_remove(0)),
            _ => Side::Unstable,
        }
    }

    fn matches(&self, actual: &T) -> bool {
        matches!(self, Side::Stable(v) if v == actual)
    }

    fn unstable(&self) -> bool {
        matches!(self, Side::Unstable)
    }
}

impl<T: fmt::Debug> Side<T> {
    fn describe(&self) -> String {
        match self {
            Side::Stable(v) => format!("{v:?}"),
            Side::Unstable => "unstable (the two samples disagree)".into(),
            Side::Absent => "not calibrated".into(),
        }
    }
}

fn calibration_side<T, S, Fut>(
    config: &Config,
    scenario: &mut S,
    key: &CalibrationKey,
    errors: &mut Vec<String>,
) -> Side<T>
where
    T: PartialEq + fmt::Debug,
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut samples = Vec::new();
    for _ in 0..2 {
        let (res, observed) = run_observed(config, scenario, key.plan());
        if let Err(e) = &res.invariant {
            errors.push(format!("{} failed: {e}", key.label()));
            return Side::Absent;
        }
        if !res.leaks.is_empty() {
            let leaks = res
                .leaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            errors.push(format!("{} leaked: {leaks}", key.label()));
            return Side::Absent;
        }
        match observed {
            Some(Ok(value)) => samples.push(value),
            Some(Err(e)) => {
                errors.push(format!("{} failed: {e}", key.label()));
                return Side::Absent;
            }
            None => {
                errors.push(format!("{} produced no observation", key.label()));
                return Side::Absent;
            }
        }
    }
    Side::from_samples(samples)
}

fn ensure_calibrated<T, S, Fut>(
    config: &Config,
    scenario: &mut S,
    cache: &mut HashMap<CalibrationKey, Side<T>>,
    key: &CalibrationKey,
    errors: &mut Vec<String>,
) where
    T: PartialEq + fmt::Debug,
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    if cache.contains_key(key) {
        return;
    }
    let value = calibration_side(config, scenario, key, errors);
    cache.insert(key.clone(), value);
}

fn classify<T: PartialEq>(
    pre: &Side<T>,
    post: &Side<T>,
    mixed: &[&Side<T>],
    actual: &T,
) -> Verdict {
    if post.matches(actual) {
        Verdict::Post
    } else if pre.matches(actual) {
        Verdict::Pre
    } else if mixed.iter().any(|side| side.matches(actual)) {
        Verdict::Mixed
    } else if pre.unstable() || post.unstable() || mixed.iter().any(|side| side.unstable()) {
        Verdict::Unknown
    } else {
        Verdict::Neither
    }
}

fn allows(expect: Expect, verdict: Verdict) -> bool {
    match expect {
        Expect::Atomic => {
            matches!(
                verdict,
                Verdict::Pre | Verdict::Post | Verdict::Mixed | Verdict::Unknown
            )
        }
        Expect::Completed => matches!(verdict, Verdict::Post | Verdict::Unknown),
        Expect::Undone => matches!(verdict, Verdict::Pre | Verdict::Unknown),
    }
}

fn degeneracy_note<T: PartialEq + fmt::Debug>(
    targets: &[usize],
    pre: &Side<T>,
    post: &Side<T>,
) -> Option<String> {
    let (Side::Stable(a), Side::Stable(b)) = (pre, post) else {
        return None;
    };
    if a != b {
        return None;
    }
    Some(format!(
        "target(s) {}: pre and post observations are equal ({a:?}); this observation cannot tell whether the operation happened, only whether it contaminated shared state",
        targets
            .iter()
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The calibrations a trial is judged against: `pre` (every target skipped),
/// `post` (none skipped), and for a plan with several explicit cuts judged
/// under `Expect::Atomic`, every mixture in between. Repeated-site plans get
/// only the two extremes: their instance count makes mixtures unaffordable,
/// which is why `Config::repeat_at_site` asks for a survivor-only observation.
fn calibration_keys(
    targets: &[usize],
    plan: &Plan,
    expect: Expect,
) -> (CalibrationKey, CalibrationKey, Vec<CalibrationKey>) {
    let n = targets.len();
    let pre = CalibrationKey::new(targets, vec![true; n], &plan.gaps);
    let post = CalibrationKey::new(targets, vec![false; n], &plan.gaps);
    let mut mixed = Vec::new();
    if expect == Expect::Atomic && !plan.repeat && n >= 2 {
        for mask in 1..(1u64 << n) - 1 {
            let skipped = (0..n).map(|i| mask & (1 << i) != 0).collect();
            mixed.push(CalibrationKey::new(targets, skipped, &plan.gaps));
        }
    }
    (pre, post, mixed)
}

/// Like [`explore`], but the scenario returns an observation that is judged
/// against a calibration of the same run.
///
/// The scenario marks its operation with [`Ctx::target`] or [`Ctx::race`] as
/// usual; no `Mode` parameter is needed. For each plan, Dropwise calibrates by
/// re-running the same scenario:
///
/// * `pre`: every cut target is dropped before its first poll
///   (a [`Cut`] with `after_pending == 0`, never part of the normal plans);
/// * `post`: nothing is cancelled and every target completes;
/// * for a plan with several cuts under [`Expect::Atomic`], every mixture of
///   skipped and completed targets ([`Verdict::Mixed`]): each cancelled
///   operation may independently look not started or completed.
///
/// Each calibration is sampled twice; disagreement makes the verdict
/// [`Verdict::Unknown`] instead of a violation. Calibration failures (the
/// scenario returns `Err`, or leaks) are recorded in [`Report::baseline_errors`]
/// rather than panicking. Samples are cached by target set, skip/complete
/// assignment and gap assignment, so plans that differ only in `after_pending`
/// or `race` share them. Calibration runs are not counted against
/// `Config::max_runs` and do not appear as trials.
///
/// [`Expect`] is the scenario author's contract: `Atomic` allows `pre`, `post`
/// or a mixture, `Completed` allows only `post`, and `Undone` allows only `pre`
/// (for a value that must reach the caller). What to observe remains the
/// author's choice; nothing here infers whether an operation is consuming.
///
/// Targets are numbered in creation order. Dropping one target before its
/// first poll can change the control flow, and therefore the numbering of later
/// targets; calibrations are keyed by the numbers used in the plan.
///
/// A [`Ctx::race`] target skipped for `pre` never polls its future, and its
/// competitor is ready at once. If the program keeps the losing future and
/// polls it again (a `select!` loop over `&mut op`), the skipped target never
/// completes, so the calibration run ends at `Config::scenario_timeout` and is
/// reported as a calibration failure: `pre` cannot be produced for that shape.
pub fn explore_observed<T, S, Fut>(config: &Config, expect: Expect, mut scenario: S) -> Report
where
    T: PartialEq + fmt::Debug + Send + 'static,
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let mut cache: HashMap<CalibrationKey, Side<T>> = HashMap::new();
    search(config, |plan, report| {
        let (res, observed) = run_observed(config, &mut scenario, plan.clone());
        let mut judgement = Judgement::default();
        let Some(Ok(actual)) = &observed else {
            return (res, judgement);
        };
        let targets = calibration_targets(plan, &res);
        if plan.cuts.is_empty() || targets.is_empty() {
            return (res, judgement);
        }
        let (pre_key, post_key, mixed_keys) = calibration_keys(&targets, plan, expect);
        for key in [&pre_key, &post_key].into_iter().chain(&mixed_keys) {
            ensure_calibrated(
                config,
                &mut scenario,
                &mut cache,
                key,
                &mut report.baseline_errors,
            );
        }
        let pre = &cache[&pre_key];
        let post = &cache[&post_key];
        let mixed: Vec<_> = mixed_keys.iter().map(|key| &cache[key]).collect();
        let verdict = classify(pre, post, &mixed, actual);
        if !allows(expect, verdict) {
            let mixtures = if mixed.is_empty() {
                String::new()
            } else {
                format!(", and {} mixed calibration(s)", mixed.len())
            };
            judgement.violation = Some(format!(
                "{verdict:?} under {expect:?}: observed {actual:?}, pre={}, post={}{mixtures}",
                pre.describe(),
                post.describe()
            ));
        }
        judgement.verdict = Some(verdict);
        if let Some(note) = degeneracy_note(&targets, pre, post)
            && !report.notes.contains(&note)
        {
            report.notes.push(note);
        }
        (res, judgement)
    })
}

/// Like [`explore_observed`] with default config, panicking with the report on
/// any violation, unknown verdict, unrealized plan, unsettled run, calibration
/// failure, or incomplete search.
#[track_caller]
pub fn assert_observed<T, S, Fut>(expect: Expect, scenario: S)
where
    T: PartialEq + fmt::Debug + Send + 'static,
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let report = explore_observed(&Config::default(), expect, scenario);
    let ok = report.is_clean()
        && report.exhaustive
        && report.baseline_settled
        && report.unrealized().next().is_none()
        && report.unsettled().next().is_none()
        && report.unknown().next().is_none();
    assert!(ok, "{report}");
}

/// Like [`explore`] with default config, panicking with the report on any
/// violation, unrealized plan, unsettled run, or incomplete search.
#[track_caller]
pub fn assert_cancel_correct<S, Fut>(scenario: S)
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let report = explore(&Config::default(), scenario);
    let ok = report.is_clean()
        && report.exhaustive
        && report.baseline_settled
        && report.unrealized().next().is_none()
        && report.unsettled().next().is_none();
    assert!(ok, "{report}");
}

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::panic::Location;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use crate::obligation::{CurrentLedger, set_current, Leak, Ledger};

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
    pub flavor: Flavor,
    pub settle: Settle,
    /// Wall-clock bound on one scenario run. A scenario that does not finish is
    /// reported as a liveness violation (and dropped) instead of hanging the
    /// exploration; stuck waiters are often exactly the defect being looked for.
    ///
    /// The bound is enforced in-process: it fires only when the scenario yields
    /// back to the runtime. A single `poll` that never returns (a busy loop, a
    /// blocking call on a runtime thread) is not interrupted, and dropping the
    /// runtime afterwards may still wait for blocking-pool work. For batch runs,
    /// also bound each test process from outside.
    pub scenario_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_runs: 1000,
            max_cancellations: 1,
            races: vec![Race::Immediate, Race::Reschedule(1)],
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Index of the target in `ctx.target`/`ctx.race` call order within the run.
    pub target: usize,
    pub after_pending: usize,
    pub race: Race,
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
    plan: Vec<Cut>,
    targets: Vec<TargetRecord>,
    checks: Vec<Check>,
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
        Target { core, cut, probe: WakeProbe::for_cut(cut), phase: Phase::Running(Box::pin(fut)) }
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
        let probe = WakeProbe::for_cut(cut);
        let raced = Raced { core, cut, probe: probe.clone(), inner: Some(Box::pin(fut)), shared: shared.clone() };
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

    /// Run `check` after the settle phase, when late effects of cancelled work
    /// have had a chance to happen. Its error counts as an invariant violation.
    pub fn after_settle(&self, check: impl FnOnce() -> Result<(), String> + Send + 'static) {
        self.state.lock().unwrap().checks.push(Box::new(check));
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
        st.targets.push(TargetRecord { site, pendings: 0, status: Status::Running, lost_race: false });
        let cut = st.plan.iter().find(|c| c.target == index).copied();
        (Self { state: ctx.state.clone(), index, seen: 0 }, cut)
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
        self.set(if lose { Status::Doomed } else { Status::Running });
        lose
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Waits until the competitor should fire.
enum Delay {
    Now,
    Reschedule { remaining: usize, current: Option<BoxFuture> },
    Sleep(Pin<Box<tokio::time::Sleep>>),
    WaitWake(Arc<WakeProbe>),
}

impl Delay {
    /// Call when the target returns the planned `Pending`: `After` counts from here.
    fn new(race: Race, probe: Option<&Arc<WakeProbe>>) -> Self {
        Self::starting_at(race, tokio::time::Instant::now(), probe)
    }

    fn starting_at(race: Race, start: tokio::time::Instant, probe: Option<&Arc<WakeProbe>>) -> Self {
        match race {
            Race::Immediate | Race::Reschedule(0) => Delay::Now,
            Race::Reschedule(n) => Delay::Reschedule { remaining: n, current: None },
            Race::After(d) => Delay::Sleep(Box::pin(tokio::time::sleep_until(start + d))),
            Race::AfterWake => Delay::WaitWake(probe.expect("AfterWake targets have a probe").clone()),
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match self {
            Delay::Now => Poll::Ready(()),
            Delay::WaitWake(probe) => {
                // Register first, so a wake between the check and returning is not lost.
                *probe.waker.lock().unwrap() = Some(cx.waker().clone());
                if probe.woken.load(Ordering::SeqCst) { Poll::Ready(()) } else { Poll::Pending }
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
        matches!(cut, Some(Cut { race: Race::AfterWake, .. })).then(Arc::default)
    }

    /// Poll `fut` through `probe` if there is one, else directly.
    fn poll<F: Future>(probe: &Option<Arc<Self>>, fut: Pin<&mut F>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let Some(probe) = probe else { return fut.poll(cx) };
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
    Doomed { _inner: Pin<Box<F>>, delay: Delay },
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
                        let Phase::Running(inner) = std::mem::replace(&mut this.phase, Phase::Done) else {
                            unreachable!()
                        };
                        let delay = Delay::new(this.cut.expect("losing implies a cut").race, this.probe.as_ref());
                        this.phase = Phase::Doomed { _inner: inner, delay };
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
            if sh.doomed && !sh.fired {
                // Between losing and the competitor firing: no progress, like a
                // branch the real competitor is about to beat.
                sh.target_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        let inner = this.inner.as_mut().expect("dropwise::Raced polled after completion");
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
        let doomed_at = {
            let mut sh = this.shared.lock().unwrap();
            if !sh.doomed || sh.fired {
                sh.competitor_waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            sh.doomed_at.expect("doomed implies doomed_at")
        };
        let race = this.race.expect("doomed implies a cut");
        let delay = this.delay.get_or_insert_with(|| Delay::starting_at(race, doomed_at, this.probe.as_ref()));
        if delay.poll(cx).is_pending() {
            return Poll::Pending;
        }
        let target_waker = {
            let mut sh = this.shared.lock().unwrap();
            sh.fired = true;
            sh.target_waker.take()
        };
        {
            let mut st = this.state.lock().unwrap();
            let rec = &mut st.targets[this.index];
            if rec.status == Status::Doomed {
                rec.status = Status::Kept; // becomes Cancelled if the target is dropped
            }
        }
        // A program that keeps the target alive must be able to poll it again.
        if let Some(w) = target_waker {
            w.wake();
        }
        Poll::Ready(())
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

    pub fn is_clean(&self) -> bool {
        self.baseline_errors.is_empty() && self.violations().next().is_none()
    }
}

fn describe(t: &Trial) -> String {
    t.plan
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
        .join(" + ")
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "dropwise: {} target(s), {} cancellation plan(s) explored{}",
            self.baseline_targets,
            self.trials.len(),
            if self.exhaustive { "" } else { " (NOT exhaustive)" }
        )?;
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
            writeln!(f, "  baseline: observation window ended before settling (watchdog)")?;
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
            writeln!(f, "  {} plan(s) not realized (nothing tested):", unrealized.len())?;
            for t in unrealized {
                writeln!(f, "    {}", describe(t))?;
            }
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

struct RunResult {
    settled: bool,
    invariant: Result<(), String>,
    leaks: Vec<Leak>,
    targets: Vec<TargetRecord>,
}

/// Real-time bound for one bounded `block_on`.
///
/// The watchdog thread waits on a condvar instead of sleeping for the whole
/// limit, so it is gone as soon as the run is over. Sleeping out the limit would
/// leave two threads alive per run (scenario plus settle) for the duration of
/// `scenario_timeout`, which adds up to hundreds of threads in one search.
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
            let (stopped, _timed_out) =
                shared.1.wait_timeout_while(guard, limit, |stopped| !*stopped).unwrap();
            if !*stopped {
                let _ = fired.send(());
            }
        });
        Self { stop, join: Some(join) }
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
fn block_on_bounded<F: Future>(rt: &tokio::runtime::Runtime, limit: Duration, fut: F) -> Option<F::Output> {
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
    let settled =
        block_on_bounded(rt, settle.watchdog, async { tokio::time::sleep(settle.tokio_time).await })
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

fn run_once<S, Fut>(config: &Config, scenario: &mut S, plan: Vec<Cut>) -> RunResult
where
    S: FnMut(Ctx) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let ledger = Arc::new(Ledger::default());
    let ctx = Ctx {
        state: Arc::new(Mutex::new(RunState { plan, targets: Vec::new(), checks: Vec::new() })),
    };
    let rt = build_runtime(config.flavor, &ledger);

    let _ledger = CurrentLedger::attach(ledger.clone());
    let mut invariant = block_on_bounded(&rt, config.scenario_timeout, scenario(ctx.clone()))
        .unwrap_or_else(|| {
            Err(format!("scenario did not finish within {:?} (liveness)", config.scenario_timeout))
        });
    let settled = settle(&rt, config.settle);
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
    rt.shutdown_timeout(Duration::from_secs(1));

    let targets = ctx.state.lock().unwrap().targets.clone();
    RunResult { settled, invariant, leaks, targets }
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
    let mut report = Report::default();
    let mut queue: VecDeque<Vec<Cut>> = VecDeque::from([Vec::new()]);
    let mut runs = 0;

    while runs < config.max_runs {
        let Some(plan) = queue.pop_front() else { break };
        runs += 1;
        let res = run_once(config, &mut scenario, plan.clone());

        if plan.is_empty() {
            report.baseline_targets = res.targets.len();
            report.baseline_settled = res.settled;
            if let Err(e) = &res.invariant {
                report.baseline_errors.push(e.clone());
            }
            report.baseline_errors.extend(res.leaks.iter().map(|l| l.to_string()));
        } else {
            let sites = plan.iter().map(|c| res.targets.get(c.target).map(|t| t.site)).collect();
            let outcomes = plan.iter().map(|c| outcome(res.targets.get(c.target))).collect();
            report.trials.push(Trial {
                plan: plan.clone(),
                sites,
                outcomes,
                invariant: res.invariant,
                leaks: res.leaks,
                settled: res.settled,
            });
        }

        if plan.len() < config.max_cancellations {
            // Only extend with later targets, so each combination is generated once.
            let start = plan.last().map_or(0, |c| c.target + 1);
            for (target, rec) in res.targets.iter().enumerate().skip(start) {
                for after_pending in 1..=rec.pendings {
                    for &race in &config.races {
                        let mut next = plan.clone();
                        next.push(Cut { target, after_pending, race });
                        queue.push_back(next);
                    }
                }
            }
        }
    }

    report.exhaustive = queue.is_empty() && report.baseline_targets > 0;
    report
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

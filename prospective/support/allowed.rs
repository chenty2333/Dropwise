//! Allowed-set oracle for the T1-T3 rescan (experiment-local, not Dropwise API).
//!
//! A cancelled operation is judged against calibration runs of the same
//! scenario instead of a hand-written expected value:
//!
//! * `pre`: the operation is dropped before its first poll (`Mode::Skip`), so
//!   it never started;
//! * `post`: the operation runs to completion (`Mode::Run`, nothing cancelled).
//!
//! Each side is run twice. If the two runs disagree, the scenario is not
//! deterministic enough to judge and verdicts involving that side are
//! `Unknown`, never a violation.
//!
//! Choosing *what* to observe, and whether the calling context allows `Pre`,
//! is still the scenario author's contract; nothing here infers correctness.
//!
//! Remaining users after the migration to `explore_observed`:
//!
//! * `t2-bb8/tests/rescan.rs` R2.2: bb8's own `connection_timeout`, no cut;
//! * `t2-bb8-managers/tests/deadpool.rs` D2: deadpool's `recycle_timeout`, no cut;
//! * `t2-bb8-managers/tests/managers.rs` `internal_timeout`: bb8's
//!   `connection_timeout`, no cut;
//! * `t3-sqlx/tests/commit_reply.rs`: a manual `select!` on a reply gate, no cut.
//!
//! Every experiment that used `allowed.rs` and has a Dropwise cut now uses
//! `dropwise::explore_observed`.
#![allow(dead_code)]

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use dropwise::{Config, Ctx, Report, Target, explore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Drop the operation before its first poll: the `pre` calibration.
    Skip,
    /// Run the operation as a Dropwise target (completes in the baseline).
    Run,
}

/// What the calling context allows after the operation was cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// The caller abandons the operation: it must look not started or finished.
    Atomic,
    /// The caller retries or carries on: the end state must equal completion.
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Pre,
    Post,
    /// Neither calibration: a half-done or contaminated state.
    Neither,
    Unknown,
}

enum Side<T> {
    Stable(T),
    Unstable,
    Absent,
}

impl<T: PartialEq> Side<T> {
    fn from(mut runs: Vec<T>) -> Self {
        match runs.len() {
            0 => Side::Absent,
            _ if runs.windows(2).all(|w| w[0] == w[1]) => Side::Stable(runs.swap_remove(0)),
            _ => Side::Unstable,
        }
    }

    fn matches(&self, actual: &T) -> bool {
        matches!(self, Side::Stable(v) if v == actual)
    }
}

pub struct Calibration<T> {
    pre: Side<T>,
    post: Side<T>,
}

impl<T: PartialEq + Debug> Calibration<T> {
    /// `pre` may be empty when only `Expect::Completed` will be checked.
    pub fn new(pre: Vec<T>, post: Vec<T>) -> Self {
        Self { pre: Side::from(pre), post: Side::from(post) }
    }

    pub fn classify(&self, actual: &T) -> Verdict {
        if self.post.matches(actual) {
            Verdict::Post
        } else if self.pre.matches(actual) {
            Verdict::Pre
        } else if matches!(self.post, Side::Unstable) || matches!(self.pre, Side::Unstable) {
            Verdict::Unknown
        } else {
            Verdict::Neither
        }
    }

    /// `Ok` for an allowed or unknown verdict, `Err` describing a violation.
    pub fn check(&self, expect: Expect, actual: &T) -> Result<Verdict, String> {
        let verdict = self.classify(actual);
        let allowed = match expect {
            Expect::Atomic => matches!(verdict, Verdict::Pre | Verdict::Post | Verdict::Unknown),
            Expect::Completed => matches!(verdict, Verdict::Post | Verdict::Unknown),
        };
        if allowed {
            Ok(verdict)
        } else {
            Err(format!("{verdict:?} under {expect:?}: observed {actual:?}, {}", self.describe()))
        }
    }

    fn describe(&self) -> String {
        let side = |s: &Side<T>| match s {
            Side::Stable(v) => format!("{v:?}"),
            Side::Unstable => "unstable".into(),
            Side::Absent => "not calibrated".into(),
        };
        format!("pre={}, post={}", side(&self.pre), side(&self.post))
    }
}

/// Dropped before the first poll (`Skip`) or run as a Dropwise target (`Run`).
/// Resolves to `None` when skipped or cancelled.
pub enum Op<F: Future> {
    Skipped,
    Target(Pin<Box<Target<F>>>),
}

#[track_caller]
pub fn op<F: Future>(ctx: &Ctx, mode: Mode, fut: F) -> Op<F> {
    match mode {
        Mode::Skip => Op::Skipped,
        Mode::Run => Op::Target(Box::pin(ctx.target(fut))),
    }
}

impl<F: Future> Future for Op<F> {
    type Output = Option<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            Op::Skipped => Poll::Ready(None),
            Op::Target(t) => t.as_mut().poll(cx),
        }
    }
}

/// Calibrate `scenario` with two `Skip` and two `Run` baselines under `config`
/// (only its flavor, settle and timeout matter: nothing is cancelled).
pub fn calibrate<T, S, Fut>(config: &Config, scenario: S) -> Calibration<T>
where
    T: PartialEq + Debug + Send + 'static,
    S: FnMut(Ctx, Mode) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let (pre, post) = samples(config, scenario);
    Calibration::new(pre, post)
}

/// The raw `(pre, post)` calibration samples, for judging several projections
/// of one observation.
pub fn samples<T, S, Fut>(config: &Config, mut scenario: S) -> (Vec<T>, Vec<T>)
where
    T: PartialEq + Debug + Send + 'static,
    S: FnMut(Ctx, Mode) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let config = Config { max_runs: 1, ..config.clone() };
    let mut sample = |mode: Mode| {
        let mut out = Vec::new();
        for _ in 0..2 {
            let slot = Arc::new(Mutex::new(None));
            let report = explore(&config, |ctx| {
                let slot = slot.clone();
                let fut = scenario(ctx, mode);
                async move {
                    *slot.lock().unwrap() = Some(fut.await?);
                    Ok(())
                }
            });
            assert!(report.baseline_errors.is_empty(), "{mode:?} calibration failed:\n{report}");
            out.push(slot.lock().unwrap().take().expect("calibration produced no observation"));
        }
        out
    };
    let pre = sample(Mode::Skip);
    let post = sample(Mode::Run);
    (pre, post)
}

/// Verdicts recorded by every run of an exploration, baseline first.
#[derive(Clone, Default)]
pub struct Tally(Arc<Mutex<Vec<Verdict>>>);

impl Tally {
    pub fn push(&self, v: Verdict) {
        self.0.lock().unwrap().push(v);
    }

    /// One line per experiment, for the rescan log.
    pub fn summary(&self, id: &str, report: &Report) -> String {
        let all = self.0.lock().unwrap();
        // Runs that fail before reaching the verdict record nothing; then the
        // first entry is not necessarily the baseline, so only totals are given.
        let count = |v: Verdict| all.iter().filter(|x| **x == v).count();
        format!(
            "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} verdicts(all runs) pre={} post={} neither={} unknown={}",
            report.trials.len(),
            report.violations().count(),
            report.unrealized().count(),
            report.exhaustive,
            report.baseline_errors.len(),
            count(Verdict::Pre),
            count(Verdict::Post),
            count(Verdict::Neither),
            count(Verdict::Unknown),
        )
    }
}

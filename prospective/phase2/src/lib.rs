use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use dropwise::{Config, Ctx, CutOutcome, Flavor, Race, Report, Settle};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Baseline,
    Simple,
    DropwiseMatched,
    DropwiseFull,
}

impl Mode {
    pub fn from_env() -> Self {
        match std::env::var("PHASE2_MODE").as_deref() {
            Ok("baseline") => Self::Baseline,
            Ok("simple") => Self::Simple,
            Ok("dropwise-matched") => Self::DropwiseMatched,
            Ok("dropwise-full") => Self::DropwiseFull,
            other => panic!("PHASE2_MODE must be baseline, simple, dropwise-matched, or dropwise-full; got {other:?}"),
        }
    }

    pub fn is_dropwise(self) -> bool {
        matches!(self, Self::DropwiseMatched | Self::DropwiseFull)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Invocation {
    pub pending_count: usize,
    pub cancelled: bool,
    pub detail: String,
}

pub type Metrics = Arc<Mutex<Vec<Invocation>>>;

pub fn add_metric(metrics: &Metrics, invocation: Invocation) {
    metrics.lock().unwrap().push(invocation);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetEnd {
    Completed,
    Cancelled,
}

pub struct TargetRun<T> {
    pub end: TargetEnd,
    pub output: Option<T>,
    pub pending_count: usize,
}

pub struct PendingWatch<F> {
    inner: Pin<Box<F>>,
    pending_count: Arc<AtomicUsize>,
    event_txs: Vec<mpsc::UnboundedSender<usize>>,
    simple_cancel_tx: Option<mpsc::UnboundedSender<()>>,
}

impl<F> PendingWatch<F> {
    fn new(
        inner: F,
        pending_count: Arc<AtomicUsize>,
        event_txs: Vec<mpsc::UnboundedSender<usize>>,
        simple_cancel_tx: Option<mpsc::UnboundedSender<()>>,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            pending_count,
            event_txs,
            simple_cancel_tx,
        }
    }
}

pub fn observe_pending<F>(future: F, event_tx: mpsc::UnboundedSender<usize>) -> PendingWatch<F> {
    PendingWatch::new(future, Arc::new(AtomicUsize::new(0)), vec![event_tx], None)
}

impl<F: Future> Future for PendingWatch<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.inner.as_mut().poll(cx) {
            Poll::Ready(output) => Poll::Ready(output),
            Poll::Pending => {
                let n = this.pending_count.fetch_add(1, Ordering::SeqCst) + 1;
                for tx in &this.event_txs {
                    let _ = tx.send(n);
                }
                if n == 1 {
                    if let Some(tx) = &this.simple_cancel_tx {
                        let _ = tx.send(());
                    }
                }
                Poll::Pending
            }
        }
    }
}

async fn drive_until_target_finishes<D: Future<Output = ()>>(driver: D) {
    driver.await;
    std::future::pending::<()>().await;
}

pub async fn run_target<F, D>(
    mode: Mode,
    ctx: Option<Ctx>,
    target: F,
    driver: D,
    pending_events: Vec<mpsc::UnboundedSender<usize>>,
) -> TargetRun<F::Output>
where
    F: Future,
    D: Future<Output = ()>,
{
    let pending_count = Arc::new(AtomicUsize::new(0));
    let waiting_driver = drive_until_target_finishes(driver);

    let (end, output) = match mode {
        Mode::Baseline => {
            let watched = PendingWatch::new(target, pending_count.clone(), pending_events, None);
            tokio::select! {
                biased;
                output = watched => (TargetEnd::Completed, Some(output)),
                _ = waiting_driver => unreachable!("the driver is deliberately pending"),
            }
        }
        Mode::Simple => {
            let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel();
            let watched = PendingWatch::new(
                target,
                pending_count.clone(),
                pending_events,
                Some(cancel_tx),
            );
            tokio::select! {
                biased;
                _ = cancel_rx.recv() => (TargetEnd::Cancelled, None),
                output = watched => (TargetEnd::Completed, Some(output)),
                _ = waiting_driver => unreachable!("the driver is deliberately pending"),
            }
        }
        Mode::DropwiseMatched | Mode::DropwiseFull => {
            let ctx = ctx.expect("Dropwise mode requires a Ctx");
            let watched = PendingWatch::new(target, pending_count.clone(), pending_events, None);
            let (raced, mut preempt) = ctx.race(watched);
            tokio::select! {
                biased;
                _ = &mut preempt => (TargetEnd::Cancelled, None),
                output = raced => (TargetEnd::Completed, Some(output)),
                _ = waiting_driver => unreachable!("the driver is deliberately pending"),
            }
        }
    };

    TargetRun {
        end,
        output,
        pending_count: pending_count.load(Ordering::SeqCst),
    }
}

struct SharedState<T> {
    future: Option<Pin<Box<dyn Future<Output = T> + Send>>>,
    output: Option<T>,
}

/// A pollable handle whose inner future survives dropping a `select!` branch.
/// Used only for waiter futures that intentionally outlive the selected caller.
pub struct SharedCall<T> {
    state: Arc<Mutex<SharedState<T>>>,
}

impl<T> Clone for SharedCall<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T> SharedCall<T>
where
    T: Clone + Send + 'static,
{
    pub fn new<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self {
            state: Arc::new(Mutex::new(SharedState {
                future: Some(Box::pin(future)),
                output: None,
            })),
        }
    }
}

impl<T> Future for SharedCall<T>
where
    T: Clone + Send + 'static,
{
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.state.lock().unwrap();
        if let Some(output) = &state.output {
            return Poll::Ready(output.clone());
        }
        let result = state
            .future
            .as_mut()
            .expect("SharedCall future polled after completion without an output")
            .as_mut()
            .poll(cx);
        match result {
            Poll::Ready(output) => {
                state.future = None;
                state.output = Some(output.clone());
                Poll::Ready(output)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub fn config(mode: Mode) -> Config {
    let mut config = Config::default();
    config.max_cancellations = 1;
    config.flavor = Flavor::MultiThread { workers: 2 };
    config.settle = Settle::default();
    config.scenario_timeout = Duration::from_secs(10);
    match mode {
        Mode::DropwiseMatched => {
            config.races = vec![Race::Immediate];
            config.max_runs = 2;
        }
        Mode::DropwiseFull => {
            config.races = vec![
                Race::Immediate,
                Race::Reschedule(1),
                Race::After(Duration::from_millis(10)),
            ];
            config.max_runs = 128;
        }
        Mode::Baseline | Mode::Simple => unreachable!("plain modes do not use Dropwise config"),
    }
    config
}

fn record_path(scene: &str, mode: Mode) -> PathBuf {
    let run = std::env::var("RUN").expect("RUN=1,2,3 is required");
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runs")
        .join("reports")
        .join(scene)
        .join(format!("{}-{run}.txt", mode_name(mode)))
}

pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Baseline => "baseline",
        Mode::Simple => "simple",
        Mode::DropwiseMatched => "dropwise-matched",
        Mode::DropwiseFull => "dropwise-full",
    }
}

fn metric_text(metrics: &Metrics) -> String {
    let mut output = String::new();
    for (i, metric) in metrics.lock().unwrap().iter().enumerate() {
        output.push_str(&format!(
            "invocation[{i}]: pending={} cancelled={} detail={}\n",
            metric.pending_count, metric.cancelled, metric.detail
        ));
    }
    output
}

fn write_record(path: PathBuf, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("report parent"))
        .expect("create report directory");
    std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn valid_dropwise_report(mode: Mode, report: &Report) -> Result<(), String> {
    if !report.baseline_settled {
        return Err("baseline did not settle".into());
    }
    if !report.baseline_errors.is_empty() {
        return Err(format!("baseline errors: {:?}", report.baseline_errors));
    }
    if report.baseline_targets != 1 {
        return Err(format!(
            "expected one marked target, got {}",
            report.baseline_targets
        ));
    }
    if report.trials.is_empty() {
        return Err("Dropwise explored no cancellation plan".into());
    }
    if mode == Mode::DropwiseFull && !report.exhaustive {
        return Err("full Dropwise search was not exhaustive".into());
    }
    if mode == Mode::DropwiseMatched && report.trials.len() != 1 {
        return Err(format!(
            "matched budget expected exactly one plan, got {}",
            report.trials.len()
        ));
    }
    let mut realized_cancellations = 0;
    for trial in &report.trials {
        if trial.is_violation() {
            return Err("Dropwise reported a violation".into());
        }
        if trial.is_realized() && !trial.settled {
            return Err("Dropwise observation window was cut short".into());
        }
        if trial.is_realized() {
            if trial.outcomes.iter().any(|o| *o != CutOutcome::Cancelled) {
                return Err(format!(
                    "planned race did not cancel the target: {:?}",
                    trial.outcomes
                ));
            }
            realized_cancellations += trial.outcomes.len();
        }
    }
    if realized_cancellations == 0 {
        return Err("no cancellation plan was actually realized".into());
    }
    if mode == Mode::DropwiseMatched && report.trials.iter().any(|trial| !trial.is_realized()) {
        return Err("the one-plan budget-matched comparison was not realized".into());
    }
    Ok(())
}

pub fn execute<F, M>(scene: &'static str, mut scenario: M)
where
    F: Future<Output = Result<(), String>>,
    M: FnMut(Mode, Option<Ctx>, Metrics) -> F,
{
    let mode = Mode::from_env();
    let metrics: Metrics = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let result = if mode.is_dropwise() {
        let config = config(mode);
        let report = dropwise::explore(&config, |ctx| scenario(mode, Some(ctx), metrics.clone()));
        let validation = valid_dropwise_report(mode, &report);
        let contents = format!(
            "scene={scene}\nmode={}\nrun={}\nconfig={config:?}\nelapsed={:.6}s\nvalidation={validation:?}\n\nREPORT DISPLAY\n{report}\nREPORT DEBUG\n{report:#?}\nINVOCATION METRICS\n{}",
            mode_name(mode),
            std::env::var("RUN").unwrap_or_default(),
            started.elapsed().as_secs_f64(),
            metric_text(&metrics),
        );
        write_record(record_path(scene, mode), &contents);
        validation
    } else {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build plain comparison runtime");
        let run = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(10),
                scenario(mode, None, metrics.clone()),
            )
            .await
        });
        let result = match run {
            Ok(result) => result,
            Err(_) => Err("plain run exceeded 10s scenario timeout".into()),
        };
        let contents = format!(
            "scene={scene}\nmode={}\nrun={}\nelapsed={:.6}s\nresult={result:?}\nINVOCATION METRICS\n{}",
            mode_name(mode),
            std::env::var("RUN").unwrap_or_default(),
            started.elapsed().as_secs_f64(),
            metric_text(&metrics),
        );
        write_record(record_path(scene, mode), &contents);
        runtime.shutdown_timeout(Duration::from_secs(1));
        result
    };
    assert!(result.is_ok(), "{scene} {}: {result:?}", mode_name(mode));
}

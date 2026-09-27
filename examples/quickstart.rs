//! Start here: `cargo run --example quickstart`
//!
//! A worker drains jobs off a queue and writes them out, giving another branch
//! (a flush tick) a chance to win at every suspension point. Whether that is
//! cancellation-correct depends on where the in-flight job lives: Dropwise
//! reruns the loop with each observed `Pending` losing the race once and reports
//! the runs where a job went missing.
//!
//! Both scenarios below are the *same* loop; only the helper differs.

use std::time::Duration;

use dropwise::models::mpsc::UnboundedReceiver;
use dropwise::{explore, Config, Ctx, Race};
use tokio::sync::mpsc;

#[derive(Default)]
struct Sink {
    written: Vec<u32>,
}

impl Sink {
    /// Simulated I/O: one `Pending` boundary, so the write can be interrupted.
    async fn write(&mut self, job: u32) {
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.written.push(job);
    }
}

/// Three jobs, then the queue closes. Built inside the scenario because Dropwise
/// reruns it once per cancellation plan.
async fn setup() -> mpsc::UnboundedReceiver<u32> {
    let (tx, rx) = mpsc::unbounded_channel();
    for job in 0..3 {
        tx.send(job).unwrap();
    }
    drop(tx);
    rx
}

/// Take one job and write it. The job is `recv`ed by the library model, so it
/// arrives as an `Obligation`: responsibility for it transfers to this future,
/// and it lives nowhere else.
async fn drain(rx: &mut UnboundedReceiver<u32>, sink: &mut Sink) -> bool {
    match rx.recv().await {
        Some(job) => {
            sink.write(*job.get()).await;
            job.discharge();
            true
        }
        None => false,
    }
}

/// Same as `drain`, except the job is handed to the caller before the write, so
/// losing the race leaves it somewhere that still intends to write it.
async fn drain_keep_pending(
    rx: &mut UnboundedReceiver<u32>,
    pending: &mut Option<u32>,
    sink: &mut Sink,
) -> bool {
    if pending.is_none() && let Some(job) = rx.recv().await {
        *pending = Some(job.discharge());
    }
    match *pending {
        Some(job) => {
            sink.write(job).await;
            *pending = None;
            true
        }
        None => false,
    }
}

/// Run the loop under every single-cancellation plan and return the report.
fn explore_loop<S, F>(name: &str, scenario: S) -> dropwise::Report
where
    S: Fn(Ctx) -> F,
    F: std::future::Future<Output = Result<(), String>>,
{
    // One race mode keeps the plan count small; `Config::default()` would also
    // try `Race::Reschedule(1)`, which is a different competitor timing.
    let config = Config { races: vec![Race::Immediate], ..Config::default() };
    let report = explore(&config, scenario);
    println!(
        "\n== {name}: {} target(s), {} plan(s), {} violation(s), {} unrealized, exhaustive: {}",
        report.baseline_targets,
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
    );
    // `Display` lists only what needs attention: violations, baseline problems,
    // plans cut short by the settle watchdog, unrealized plans, a truncated
    // search. `is_clean` alone would answer "nothing wrong" for a run that
    // tested nothing.
    print!("{report}");
    report
}

fn main() {
    let buggy = explore_loop("job inside the cancellable future", |ctx: Ctx| async move {
        let mut rx = UnboundedReceiver::from(setup().await);
        let mut sink = Sink::default();
        loop {
            // `ctx.race` marks the future and hands back the competitor: the
            // program's own `select!` decides what losing looks like, so the
            // post-cancellation control flow here is the code being tested.
            let (job, mut flush) = ctx.race(drain(&mut rx, &mut sink));
            let more = tokio::select! {
                more = job => more,
                _ = &mut flush => continue, // stands for `interval.tick()`
            };
            if !more {
                break;
            }
        }
        if sink.written == [0, 1, 2] {
            Ok(())
        } else {
            Err(format!("wrote {:?}, expected [0, 1, 2]", sink.written))
        }
    });

    let fixed = explore_loop("job kept by the caller", |ctx: Ctx| async move {
        let mut rx = UnboundedReceiver::from(setup().await);
        let mut sink = Sink::default();
        let mut pending = None;
        loop {
            let (job, mut flush) = ctx.race(drain_keep_pending(&mut rx, &mut pending, &mut sink));
            let more = tokio::select! {
                more = job => more,
                _ = &mut flush => continue,
            };
            if !more {
                break;
            }
        }
        if sink.written == [0, 1, 2] {
            Ok(())
        } else {
            Err(format!("wrote {:?}, expected [0, 1, 2]", sink.written))
        }
    });

    // The defect is a dropped obligation (the job went with the cancelled
    // future) plus the invariant that noticed the missing write.
    assert!(buggy.violations().count() > 0, "expected the first shape to lose a job");
    // In CI the two checks below are one call:
    // `dropwise::assert_cancel_correct(|ctx| async move { ..the fixed loop.. })`,
    // which panics with this same report on any violation, unrealized plan,
    // unsettled run or truncated search.
    assert!(fixed.is_clean(), "the second shape must find no violation: {fixed}");
    assert!(fixed.exhaustive && fixed.unrealized().next().is_none(), "{fixed}");
    println!("\nboth checks passed: the first shape loses a job, the second never does");
}

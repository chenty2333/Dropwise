//! Retrospective mechanism comparison. See PLAN.md for the fixed denominator.
use dropwise::{Config, Ctx, CutOutcome, Race, Settle, explore};
use serde_json::json;
use std::{
    cell::Cell,
    future::{Future, poll_fn},
    path::Path,
    pin::pin,
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const SETTLE: Duration = Duration::from_millis(100);

// Independent B: no Dropwise code participates in selecting this cancellation.
async fn first_pending<F: Future>(future: F, armed: bool, cuts: &Cell<usize>) -> Option<F::Output> {
    let mut future = pin!(future);
    poll_fn(|cx| match future.as_mut().poll(cx) {
        Poll::Pending if armed => {
            cuts.set(cuts.get() + 1);
            Poll::Ready(None)
        }
        Poll::Pending => Poll::Pending,
        Poll::Ready(value) => Poll::Ready(Some(value)),
    })
    .await
}

fn notification() -> Arc<Notify> {
    let notify = Arc::new(Notify::new());
    let manager = notify.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        manager.notify_waiters();
    });
    notify
}

async fn notify_plain(fixed: bool, cancel: bool, cuts: &Cell<usize>) -> Result<(), String> {
    let notify = notification();
    let run = async {
        if fixed {
            let notified = notify.notified();
            tokio::pin!(notified);
            loop {
                if first_pending(notified.as_mut(), cancel && cuts.get() == 0, cuts)
                    .await
                    .is_some()
                {
                    break;
                }
            }
        } else {
            loop {
                if first_pending(notify.notified(), cancel && cuts.get() == 0, cuts)
                    .await
                    .is_some()
                {
                    break;
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(8), run)
        .await
        .map_err(|_| "client did not exit after notification".into())
}

async fn notify_dropwise(ctx: Ctx, fixed: bool) -> Result<(), String> {
    let notify = notification();
    let run = async {
        if fixed {
            let (notified, mut tick) = ctx.race(notify.notified());
            tokio::pin!(notified);
            loop {
                tokio::select! {
                    _ = &mut tick => {},
                    _ = &mut notified => break,
                }
            }
        } else {
            loop {
                let (notified, mut tick) = ctx.race(notify.notified());
                tokio::select! {
                    _ = &mut tick => {},
                    _ = notified => break,
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(8), run)
        .await
        .map_err(|_| "client did not exit after notification".into())
}

fn fds() -> Result<usize, String> {
    let mut entries = std::fs::read_dir("/proc/self/fd").map_err(|e| e.to_string())?;
    entries
        .try_fold(0, |n, entry| entry.map(|_| n + 1))
        .map_err(|e| e.to_string())
}
fn fd_check(before: usize) -> Result<(), String> {
    let after = fds()?;
    if after > before {
        Err(format!("fd count increased: {before} -> {after}"))
    } else {
        Ok(())
    }
}
async fn warmup(path: &Path) -> Result<usize, String> {
    drop(
        tokio::fs::File::open(path)
            .await
            .map_err(|e| e.to_string())?,
    );
    fds()
}
async fn open_dropwise(ctx: Ctx, path: &Path) -> Result<(), String> {
    let before = warmup(path).await?;
    let file = ctx.target(tokio::fs::File::open(path)).await;
    drop(file.transpose().map_err(|e| e.to_string())?);
    ctx.after_settle(move || fd_check(before));
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 5, "usage: binary N|U buggy|fixed MODE FILE");
    let (case, variant, mode, path) = (&args[1], &args[2], &args[3], Path::new(&args[4]));
    assert!(["N", "U"].contains(&case.as_str()));
    assert!(["buggy", "fixed"].contains(&variant.as_str()));
    assert!(["A", "B", "B-settle", "C-matched", "C-full", "C-no-settle"].contains(&mode.as_str()));
    let fixed = variant == "fixed";
    let start = Instant::now();
    let mut metrics;
    if mode.starts_with('C') {
        let config = Config {
            max_runs: if mode == "C-matched" { 2 } else { 32 },
            max_cancellations: 1,
            races: if mode == "C-matched" {
                vec![Race::Immediate]
            } else {
                vec![Race::Immediate, Race::Reschedule(1), Race::AfterWake]
            },
            scenario_timeout: Duration::from_secs(2),
            settle: Settle {
                tokio_time: if mode == "C-no-settle" {
                    Duration::ZERO
                } else {
                    SETTLE
                },
                real_time: Duration::ZERO,
                watchdog: Duration::from_secs(2),
            },
            ..Config::default()
        };
        let report = explore(&config, |ctx| async move {
            if case == "N" {
                notify_dropwise(ctx, fixed).await
            } else {
                open_dropwise(ctx, path).await
            }
        });
        metrics = json!({
            "baseline_errors": report.baseline_errors,
            "baseline_settled": report.baseline_settled,
            "baseline_targets": report.baseline_targets,
            "plans": report.trials.len(),
            "cancelled": report.trials.iter().flat_map(|t| &t.outcomes).filter(|&&o| o == CutOutcome::Cancelled).count(),
            "kept": report.trials.iter().flat_map(|t| &t.outcomes).filter(|&&o| o == CutOutcome::Kept).count(),
            "violations": report.violations().count(),
            "unrealized": report.unrealized().count(),
            "unsettled": report.unsettled().count(),
            "exhaustive": report.exhaustive,
            "errors": report.trials.iter().filter_map(|t| t.invariant.as_ref().err()).collect::<Vec<_>>()
        });
    } else {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let cuts = Cell::new(0);
        let result = runtime.block_on(async {
            let before = if case == "N" {
                notify_plain(fixed, mode != "A", &cuts).await?;
                None
            } else {
                let before = warmup(path).await?;
                let file = first_pending(tokio::fs::File::open(path), mode != "A", &cuts).await;
                drop(file.transpose().map_err(|e| e.to_string())?);
                Some(before)
            };
            if mode != "B" {
                tokio::time::sleep(SETTLE).await;
            }
            if let Some(before) = before {
                fd_check(before)?;
            }
            Ok::<_, String>(())
        });
        metrics = json!({"plans": usize::from(mode != "A"), "cancelled": if case == "N" && fixed { 0 } else { cuts.get() },
            "kept": if case == "N" && fixed { cuts.get() } else { 0 }, "cut_events": cuts.get(), "violations": usize::from(result.is_err()),
            "unrealized": usize::from(mode != "A" && cuts.get() == 0),
            "unsettled": null, "exhaustive": null, "baseline_errors": [],
            "errors": result.err().into_iter().collect::<Vec<_>>()});
    }
    metrics["case"] = json!(case);
    metrics["variant"] = json!(variant);
    metrics["mode"] = json!(mode);
    metrics["wall_seconds"] = json!(start.elapsed().as_secs_f64());
    println!("{metrics}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        rc::Rc,
        task::{Context, Poll},
    };

    struct PendingDrop(Rc<Cell<usize>>);
    impl Future for PendingDrop {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }
    impl Drop for PendingDrop {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    #[test]
    fn independent_cut_drops_pending_but_not_ready() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let cuts = Cell::new(0);
        let drops = Rc::new(Cell::new(0));
        assert_eq!(
            rt.block_on(first_pending(std::future::ready(7), true, &cuts)),
            Some(7)
        );
        assert_eq!(cuts.get(), 0);
        assert_eq!(
            rt.block_on(first_pending(PendingDrop(drops.clone()), true, &cuts)),
            None
        );
        assert_eq!((cuts.get(), drops.get()), (1, 1));
    }
    #[test]
    fn borrowed_pending_future_is_kept_by_caller() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let cuts = Cell::new(0);
        let drops = Rc::new(Cell::new(0));
        let mut future = Box::pin(PendingDrop(drops.clone()));
        assert_eq!(
            rt.block_on(first_pending(future.as_mut(), true, &cuts)),
            None
        );
        assert_eq!((cuts.get(), drops.get()), (1, 0));
        drop(future);
        assert_eq!(drops.get(), 1);
    }
}

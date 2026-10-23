//! Runtime teardown and competitor lifetime are not part of the tested program.

use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::task::Poll;
use std::time::Duration;

use dropwise::{Config, Ctx, CutOutcome, Obligation, Race, Settle, explore};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        scenario_timeout: Duration::from_millis(100),
        ..Config::default()
    }
}

#[test]
fn dropping_the_competitor_before_the_target_is_polled_does_not_stall_it() {
    let report = explore(&config(), |ctx: Ctx| async move {
        let (target, competitor) = ctx.race(tokio::task::yield_now());
        drop(competitor);
        target.await;
        Ok(())
    });
    assert!(
        report.is_clean(),
        "the harness must not strand the target: {report}"
    );
    assert_eq!(report.trials.len(), 1);
    assert_eq!(report.trials[0].outcomes, [CutOutcome::Unrealized]);
}

#[test]
fn dropping_a_competitor_after_the_cut_releases_the_target() {
    let report = explore(&config(), |ctx: Ctx| async move {
        let (target, competitor) = ctx.race(tokio::task::yield_now());
        tokio::pin!(target);
        poll_fn(|cx| {
            assert!(target.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(competitor);
        target.await;
        Ok(())
    });
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.trials[0].outcomes, [CutOutcome::Kept]);
}

#[test]
fn runtime_shutdown_does_not_turn_a_stalled_cut_into_cancellation() {
    let report = explore(&config(), |ctx: Ctx| async move {
        let (ready, started) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (target, _competitor) = ctx.race(tokio::task::yield_now());
            tokio::pin!(target);
            poll_fn(|cx| {
                assert!(target.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            ready.send(()).unwrap();
            // The target and competitor survive the observation window.
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        Ok(())
    });
    assert_eq!(report.trials[0].outcomes, [CutOutcome::Stalled], "{report}");
    assert_eq!(report.trials[0].cancelled, 0, "{report}");
    assert!(!report.is_clean_within_coverage(), "{report}");
    assert!(
        report
            .coverage_gaps()
            .iter()
            .any(|gap| gap.contains("stalled")),
        "{report}"
    );
}

#[test]
fn real_time_settling_keeps_the_current_thread_runtime_driving_work() {
    let config = Config {
        max_runs: 1,
        settle: Settle {
            tokio_time: Duration::ZERO,
            real_time: Duration::from_millis(150),
            ..Settle::default()
        },
        ..config()
    };
    let report = explore(&config, |ctx: Ctx| async move {
        let finished = Arc::new(AtomicBool::new(false));
        let task_finished = finished.clone();
        tokio::spawn(async move {
            // More than the old fixed sixteen yields; must execute during
            // the wall-clock window, not only after sleeping through it.
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
            task_finished.store(true, SeqCst);
        });
        ctx.after_settle(move || {
            if finished.load(SeqCst) {
                Ok(())
            } else {
                Err("settling starved the worker".into())
            }
        });
        Ok(())
    });
    assert!(report.baseline_errors.is_empty(), "{report}");
}

#[test]
fn a_caught_worker_panic_does_not_erase_an_unresolved_obligation() {
    let report = explore(
        &Config {
            max_runs: 1,
            ..config()
        },
        |_ctx: Ctx| async move {
            let task = tokio::spawn(async {
                let _owed = Obligation::new(1, "panicking worker");
                panic!("worker failed");
            });
            assert!(task.await.unwrap_err().is_panic());
            Ok(())
        },
    );
    assert!(
        report
            .baseline_errors
            .iter()
            .any(|e| e.contains("panicking worker")),
        "{report}"
    );
}

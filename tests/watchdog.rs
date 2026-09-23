//! A blocking task that never finishes inhibits paused-time auto-advance, so the
//! virtual settle sleep alone would never return. The real-time watchdog must end
//! the observation window and report it as incomplete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dropwise::{explore, Config, Ctx, Race, Settle};

#[test]
fn stuck_blocking_work_does_not_hang_settling() {
    let stop = Arc::new(AtomicBool::new(false));
    let config = Config {
        races: vec![Race::Immediate],
        settle: Settle { watchdog: Duration::from_millis(300), ..Settle::default() },
        ..Config::default()
    };
    let started = Instant::now();
    let s = stop.clone();
    let report = explore(&config, move |ctx: Ctx| {
        let s = s.clone();
        async move {
            ctx.target(async move {
                // Detached: keeps running after the target completes or is dropped.
                drop(tokio::task::spawn_blocking(move || {
                    while !s.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }));
                tokio::task::yield_now().await;
            })
            .await;
            Ok(())
        }
    });
    stop.store(true, Ordering::SeqCst);
    assert!(started.elapsed() < Duration::from_secs(20), "settling hung");
    assert!(!report.baseline_settled, "{report}");
    assert!(!report.trials.is_empty() && report.trials.iter().all(|t| !t.settled), "{report}");
    assert!(report.to_string().contains("observation window"), "{report}");
}

/// A scenario that never finishes (here: a waiter stranded by a cancelled
/// leader) is reported as a liveness violation instead of hanging exploration.
#[test]
fn stuck_scenario_is_a_liveness_violation() {
    let config = Config {
        races: vec![Race::Immediate],
        scenario_timeout: Duration::from_millis(300),
        ..Config::default()
    };
    let started = Instant::now();
    let report = explore(&config, |ctx: Ctx| async move {
        let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
        let tx = std::sync::Mutex::new(Some(tx));
        // Leader: does the work and must deliver the result to the waiter.
        ctx.target(async {
            tokio::task::yield_now().await;
            let tx = tx.lock().unwrap().take().unwrap();
            let _ = tx.send(7);
        })
        .await;
        // Keep the sender alive (as a shared cache entry would), so the waiter
        // is not woken by a closed channel either.
        let _keep = tx;
        rx.await.map(|_| ()).map_err(|e| e.to_string())
    });
    assert!(started.elapsed() < Duration::from_secs(20), "exploration hung");
    assert!(report.baseline_errors.is_empty(), "{report}");
    let v: Vec<_> = report.violations().collect();
    assert_eq!(v.len(), 1, "{report}");
    assert!(v[0].invariant.as_ref().unwrap_err().contains("liveness"), "{report}");
}

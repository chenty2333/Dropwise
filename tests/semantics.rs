//! Tests for the harness semantics: real `select!` integration, unrealized
//! plans, outstanding obligations, and late effects that need a settle phase.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dropwise::models::mpsc::Receiver;
use dropwise::{explore, Config, Ctx, CutOutcome, LeakKind, Obligation, Race, Settle};
use tokio::sync::mpsc;

#[derive(Default)]
struct Sink {
    delivered: Vec<u32>,
}

impl Sink {
    async fn write(&mut self, msg: u32) {
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.delivered.push(msg);
    }
}

async fn setup() -> Receiver<u32> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        for i in 0..3 {
            tx.send(i).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    Receiver::from(rx)
}

fn check(sink: &Sink) -> Result<(), String> {
    if sink.delivered == [0, 1, 2] {
        Ok(())
    } else {
        Err(format!("delivered {:?}, expected [0, 1, 2]", sink.delivered))
    }
}

async fn recv_and_forward(rx: &mut Receiver<u32>, sink: &mut Sink) -> bool {
    match rx.recv().await {
        Some(m) => {
            sink.write(*m.get()).await;
            m.discharge();
            true
        }
        None => false,
    }
}

/// The RFD 400 bug through the program's own `loop { select! }`: the losing
/// branch is dropped by `select!`, not by Dropwise.
#[test]
fn real_select_loop_finds_the_bug() {
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        let mut rx = setup().await;
        let mut sink = Sink::default();
        loop {
            let (op, mut tick) = ctx.race(recv_and_forward(&mut rx, &mut sink));
            tokio::select! {
                more = op => if !more { break },
                _ = &mut tick => {} // stands for a heartbeat branch
            }
        }
        check(&sink)
    });
    println!("{report}");
    assert!(report.exhaustive && report.unrealized().next().is_none(), "{report}");
    let v: Vec<_> = report.violations().collect();
    assert!(!v.is_empty(), "{report}");
    assert!(v.iter().all(|t| t.outcomes == [CutOutcome::Cancelled]));
    assert!(v.iter().any(|t| t.leaks.iter().any(|l| l.kind == LeakKind::DroppedUnresolved)));
}

/// Keeping the in-flight operation alive across iterations (the usual fix) is
/// reported as `Kept`, and the program is correct.
#[test]
fn kept_future_survives_losing_the_race() {
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        let mut rx = setup().await;
        let mut sink = Sink::default();
        {
            let (op, mut tick) = ctx.race(async {
                while recv_and_forward(&mut rx, &mut sink).await {}
            });
            tokio::pin!(op);
            loop {
                tokio::select! {
                    _ = &mut op => break,
                    _ = &mut tick => continue,
                }
            }
        }
        check(&sink)
    });
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert!(!report.trials.is_empty());
    assert!(report.trials.iter().all(|t| t.outcomes == [CutOutcome::Kept]), "{report}");
}

/// A plan whose boundary is not reached is reported, not silently passed.
#[test]
fn unrealized_plans_are_reported() {
    let runs = Arc::new(AtomicUsize::new(0));
    let config = Config { races: vec![Race::Immediate], ..Config::default() };
    let report = explore(&config, |ctx: Ctx| {
        // Nondeterministic on purpose: the baseline pends twice, later runs once.
        let pends = if runs.fetch_add(1, Ordering::SeqCst) == 0 { 2 } else { 1 };
        async move {
            ctx.target(async move {
                for _ in 0..pends {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            Ok(())
        }
    });
    assert!(report.is_clean());
    let unrealized: Vec<_> = report.unrealized().collect();
    assert_eq!(unrealized.len(), 1, "{report}");
    assert_eq!(unrealized[0].outcomes, [CutOutcome::Unrealized]);
    assert!(report.to_string().contains("not realized"));
}

#[test]
fn forgotten_and_forever_held_obligations_are_outstanding() {
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        std::mem::forget(Obligation::new(1u32, "forgotten"));
        tokio::spawn(async {
            let _held = Obligation::new(2u32, "held by a stuck task");
            std::future::pending::<()>().await;
        });
        ctx.target(tokio::task::yield_now()).await;
        Ok(())
    });
    let errs = report.baseline_errors.join("\n");
    assert!(errs.contains("`forgotten`") && errs.contains("`held by a stuck task`"), "{report}");
    assert!(errs.contains("still unresolved after settling"), "{report}");
}

/// Tokio #7979 shape: the cancelled operation completes later on another
/// thread, and its result (an owned resource) is dropped with no one to take it.
/// Paused Tokio time does not auto-advance while blocking-pool work runs, so the
/// default (virtual) settle already waits for it; no settling misses it.
#[test]
fn late_completion_is_seen_only_after_settling() {
    let scenario = |ctx: Ctx| async move {
        ctx.target(async {
            let fd = tokio::task::spawn_blocking(|| {
                std::thread::sleep(Duration::from_millis(30));
                Obligation::new(3, "opened fd")
            });
            fd.await.unwrap().discharge();
        })
        .await;
        Ok(())
    };
    let no_settle = Config {
        races: vec![Race::Immediate],
        settle: Settle { tokio_time: Duration::ZERO, real_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    };
    let r = explore(&no_settle, scenario);
    assert!(r.is_clean(), "without settling the late fd is not observed:\n{r}");

    let settled = Config { settle: Settle::default(), ..no_settle };
    let r = explore(&settled, scenario);
    let v: Vec<_> = r.violations().collect();
    assert_eq!(v.len(), 1, "{r}");
    assert_eq!(v[0].leaks[0].label, "opened fd");
}

/// omicron #10204 shape: the caller cleans up after cancellation while
/// cancelled work is still writing. Checked after settling. The writer is a
/// plain OS thread, outside Tokio's view, so only real-time settling waits for it.
#[test]
fn after_settle_checks_see_late_side_effects() {
    let scenario = |ctx: Ctx| async move {
        let cleaned = Arc::new(AtomicBool::new(false));
        let late_writes = Arc::new(Mutex::new(0));
        let (c, w) = (cleaned.clone(), late_writes.clone());
        ctx.target(async move {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                if c.load(Ordering::SeqCst) {
                    *w.lock().unwrap() += 1;
                }
                let _ = done_tx.send(());
            });
            let _ = done_rx.await;
        })
        .await;
        cleaned.store(true, Ordering::SeqCst); // e.g. remove the temp dir
        ctx.after_settle(move || match *late_writes.lock().unwrap() {
            0 => Ok(()),
            n => Err(format!("{n} write(s) into the directory after cleanup")),
        });
        Ok(())
    };
    let tokio_only = Config { races: vec![Race::Immediate], ..Config::default() };
    assert!(explore(&tokio_only, scenario).is_clean(), "virtual settling cannot see an OS thread");

    let config = Config {
        settle: Settle { real_time: Duration::from_millis(200), ..Settle::default() },
        ..tokio_only
    };
    let r = explore(&config, scenario);
    let v: Vec<_> = r.violations().collect();
    assert_eq!(v.len(), 1, "{r}");
    assert!(v[0].invariant.as_ref().unwrap_err().contains("after cleanup"));
}

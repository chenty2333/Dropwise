//! Labelled controls: snapshot projection, task counts and observation horizons.
#[path = "support/observation.rs"]
mod observation;
use dropwise::{Config, Race, Settle, explore};
use observation::{Verdict, compare};
use std::future::{Future, poll_fn};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering::SeqCst},
};
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn snapshot_and_successor_controls() {
    // Same cardinality, different ownership: a count projection hides a loss.
    let baseline = (vec![1, 2], vec![3]);
    let lost = (vec![1, 2], vec![4]);
    assert_eq!(
        compare([&baseline, &baseline], &lost, true),
        Verdict::Mismatch
    );
    assert_eq!(compare([&3, &3], &3, true), Verdict::Match); // known false negative
    assert_eq!(
        compare([&baseline, &baseline], &baseline, true),
        Verdict::Match
    );
    assert_eq!(compare([&0, &1], &0, true), Verdict::Unknown); // unstable baseline
    assert_eq!(compare([&0, &0], &0, false), Verdict::Unknown); // incomplete
}

#[test]
fn atomic_take_restore_positive_and_negative_controls() {
    struct Restore(Arc<Mutex<Option<u32>>>, Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            *self.0.lock().unwrap() = self.1.take();
        }
    }
    for guarded in [false, true] {
        let baselines = Arc::new(Mutex::new(Vec::new()));
        let mut config = Config {
            max_runs: 1,
            ..Config::default()
        };
        // Same input and contract: borrowing must leave the slot reusable.
        // Compare successor results, not a hard-coded application value.
        for calibration in [true, true, false] {
            if !calibration {
                config.max_runs = 16;
                config.races = vec![Race::Immediate, Race::AfterWake];
            }
            let baselines = baselines.clone();
            let report = explore(&config, move |ctx| {
                let baselines = baselines.clone();
                async move {
                    let slot = Arc::new(Mutex::new(Some(7)));
                    let shared = slot.clone();
                    ctx.target(async move {
                        let value = shared.lock().unwrap().take();
                        if guarded {
                            let _restore = Restore(shared, value);
                            tokio::task::yield_now().await;
                        } else {
                            tokio::task::yield_now().await;
                            *shared.lock().unwrap() = value;
                        }
                    })
                    .await;
                    let successor = slot.lock().unwrap().take();
                    let mut baselines = baselines.lock().unwrap();
                    if calibration {
                        baselines.push(successor);
                        return Ok(());
                    }
                    match compare([&baselines[0], &baselines[1]], &successor, true) {
                        Verdict::Match => Ok(()),
                        Verdict::Mismatch => Err("successor differs from baseline".into()),
                        Verdict::Unknown => panic!("control baseline unexpectedly unstable"),
                    }
                }
            });
            if !calibration {
                assert!(report.exhaustive && report.baseline_errors.is_empty());
                assert_eq!(report.violations().count(), if guarded { 0 } else { 2 });
            }
        }
    }
}

#[tokio::test]
async fn legal_partial_write_is_not_an_atomic_operation() {
    let (mut writer, mut reader) = tokio::io::duplex(1);
    let mut write = Box::pin(writer.write_all(b"ab"));
    assert!(
        poll_fn(|cx| Poll::Ready(write.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    drop(write);
    drop(writer);
    let mut observed = Vec::new();
    reader.read_to_end(&mut observed).await.unwrap();
    assert_eq!(observed, b"a");
    let raw_atomic_alarm = observed != b"" && observed != b"ab";
    assert!(
        raw_atomic_alarm,
        "pre/post-only oracle falsely rejects legal partial progress"
    );
    let atomic_verdict = |declared_atomic: bool| {
        if !declared_atomic {
            Verdict::Unknown
        } else if raw_atomic_alarm {
            Verdict::Mismatch
        } else {
            Verdict::Match
        }
    };
    assert_eq!(atomic_verdict(true), Verdict::Mismatch); // invalid contract => false positive
    assert_eq!(atomic_verdict(false), Verdict::Unknown);
    // Baseline equivalence is not the write_all cancellation contract either.
    assert_eq!(
        compare([&b"ab".to_vec(), &b"ab".to_vec()], &observed, true),
        Verdict::Mismatch
    );
    println!(
        "partial write: raw atomic and baseline comparison both alarm; contract-level verdict unknown without an atomicity requirement"
    );
}

#[test]
fn unannotated_child_is_invisible_to_the_obligation_ledger() {
    let counts = Arc::new(Mutex::new(Vec::new()));
    let output = counts.clone();
    let report = explore(
        &Config {
            races: vec![Race::Immediate, Race::AfterWake],
            settle: Settle {
                tokio_time: Duration::from_millis(10),
                ..Settle::default()
            },
            ..Config::default()
        },
        move |ctx| {
            let output = output.clone();
            async move {
                let handle = tokio::runtime::Handle::current();
                let before = handle.metrics().num_alive_tasks();
                let notify = Arc::new(tokio::sync::Notify::new());
                let child_notify = notify.clone();
                let child = tokio::spawn(async move {
                    child_notify.notified().await;
                });
                if ctx.target(tokio::task::yield_now()).await.is_some() {
                    notify.notify_one();
                }
                ctx.after_settle(move || {
                    let after = handle.metrics().num_alive_tasks();
                    output.lock().unwrap().push(after - before);
                    child.abort(); // only after observation; runtime also tears down
                    Ok(())
                });
                Ok(())
            }
        },
    );
    assert!(report.is_clean() && report.exhaustive, "{report}");
    let counts = counts.lock().unwrap();
    assert_eq!(*counts, [0, 1, 1]);
    println!("unannotated child: core ledger clean; task deltas {counts:?} catch two leaks");
}

#[tokio::test(start_paused = true)]
async fn task_counts_have_false_positives_and_identity_blind_spots() {
    let metrics = tokio::runtime::Handle::current().metrics();
    let base = metrics.num_alive_tasks();
    let delayed = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(30)).await;
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(metrics.num_alive_tasks(), base + 1); // false leak at T
    tokio::time::advance(Duration::from_millis(30)).await;
    delayed.await.unwrap();
    assert_eq!(metrics.num_alive_tasks(), base);
    let resident = tokio::spawn(std::future::pending::<()>());
    tokio::task::yield_now().await;
    let steady = metrics.num_alive_tasks();
    assert_eq!(
        compare([&steady, &steady], &metrics.num_alive_tasks(), true),
        Verdict::Match
    );
    resident.abort();
    resident.await.unwrap_err();
    let leaked_replacement = tokio::spawn(std::future::pending::<()>());
    tokio::task::yield_now().await;
    assert_eq!(metrics.num_alive_tasks(), steady); // false negative: identity changed
    leaked_replacement.abort();
    leaked_replacement.await.unwrap_err();
    println!(
        "task count: delayed cleanup false positive; authorized resident accepted; replacement leak missed"
    );
}

#[tokio::test(start_paused = true)]
async fn equal_t_and_2t_snapshots_are_not_a_fixed_point() {
    for (initial, final_value) in [(1, 0), (0, 1)] {
        let value = Arc::new(AtomicUsize::new(initial));
        let worker_value = value.clone();
        let (release, gate) = std::sync::mpsc::channel();
        let (done, completed) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            gate.recv().unwrap();
            worker_value.store(final_value, SeqCst);
            let _ = done.send(());
        });
        tokio::time::advance(Duration::from_millis(10)).await;
        let t = value.load(SeqCst);
        tokio::time::advance(Duration::from_millis(10)).await;
        let two_t = value.load(SeqCst);
        assert_eq!(t, two_t);
        assert_eq!(compare([&0, &0], &two_t, false), Verdict::Unknown);
        // A controlled external event remains possible despite virtual-time idle.
        release.send(()).unwrap();
        tokio::time::resume();
        let completion = tokio::time::timeout(Duration::from_secs(2), completed).await;
        worker.join().unwrap();
        completion.unwrap().unwrap();
        let after = value.load(SeqCst);
        assert_ne!(after, two_t);
        assert_eq!(
            compare([&0, &0], &after, true),
            if final_value == 0 {
                Verdict::Match
            } else {
                Verdict::Mismatch
            }
        );
        println!(
            "late effect {initial}->{final_value}: fixed T/2T={} but explicit completion={after}",
            two_t
        );
        tokio::time::pause();
    }
}

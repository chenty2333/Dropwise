//! Real bb8 owned connections transferred through Tokio oneshot. The connection
//! payload is an instrumented in-memory identity, not a database driver.
//! No changes to the frozen T2 scenarios or their recorded results.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use bb8::{ManageConnection, Pool};
use dropwise::{Config, CutOutcome, Race, explore};
use tokio::sync::oneshot;

#[derive(Clone)]
struct Manager {
    created: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
}

struct Resource {
    id: usize,
    live: Arc<AtomicUsize>,
}

impl Drop for Resource {
    fn drop(&mut self) {
        self.live.fetch_sub(1, SeqCst);
    }
}

impl ManageConnection for Manager {
    type Connection = Resource;
    type Error = String;

    async fn connect(&self) -> Result<Resource, String> {
        self.live.fetch_add(1, SeqCst);
        Ok(Resource {
            id: self.created.fetch_add(1, SeqCst),
            live: self.live.clone(),
        })
    }

    async fn is_valid(&self, _: &mut Resource) -> Result<(), String> {
        Ok(())
    }

    fn has_broken(&self, _: &mut Resource) -> bool {
        false
    }
}

#[test]
fn owned_connection_survives_cancelled_handoffs_and_reuse() {
    // These counters describe actually cancelled trials, not inferred coverage.
    let stages = Arc::new(std::array::from_fn::<_, 3, _>(|_| AtomicUsize::new(0)));
    let report = explore(
        &Config {
            races: vec![Race::Immediate, Race::AfterWake],
            max_runs: 32,
            scenario_timeout: Duration::from_secs(5),
            ..Config::default()
        },
        |ctx| {
            let stages = stages.clone();
            async move {
                let manager = Manager {
                    created: Arc::new(AtomicUsize::new(0)),
                    live: Arc::new(AtomicUsize::new(0)),
                };
                let pool = Pool::builder()
                    .max_size(1)
                    .connection_timeout(Duration::from_millis(100))
                    .build(manager.clone())
                    .await?;
                // Reuse the same pool and the same resource in all three rounds.
                // max_cancellations=1: this is NOT a multiple-cancellation claim.
                for round in 0..3 {
                    let resource = pool.get_owned().await.map_err(|e| format!("{e:?}"))?;
                    let (tx, rx) = oneshot::channel();
                    let sent = Arc::new(AtomicBool::new(false));
                    let producer_sent = sent.clone();
                    let producer = tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        // send failure returns ownership; send success transfers it
                        // into the channel, whose Drop must also release the lease.
                        match tx.send(resource) {
                            Ok(()) => producer_sent.store(true, SeqCst),
                            Err(resource) => drop(resource),
                        }
                    });
                    let received = Arc::new(AtomicBool::new(false));
                    let receiver_received = received.clone();
                    let result = ctx
                        .target(async move {
                            let resource = rx.await.map_err(|e| e.to_string())?;
                            receiver_received.store(true, SeqCst);
                            tokio::task::yield_now().await;
                            Ok::<_, String>(resource)
                        })
                        .await;
                    let cancelled = result.is_none();
                    if let Some(result) = result {
                        drop(result?);
                    }
                    producer.await.map_err(|e| e.to_string())?;
                    if cancelled {
                        let stage = if received.load(SeqCst) {
                            2
                        } else if sent.load(SeqCst) {
                            1
                        } else {
                            0
                        };
                        stages[stage].fetch_add(1, SeqCst);
                        println!("round={round} cancelled_stage={stage}");
                    }
                    // A real successor must obtain the sole resource; a count
                    // alone would not establish usable capacity or identity.
                    let successor = pool.get_owned().await.map_err(|e| format!("{e:?}"))?;
                    let state = pool.state();
                    if successor.id != 0
                        || manager.created.load(SeqCst) != 1
                        || manager.live.load(SeqCst) != 1
                        || state.connections != 1
                        || state.idle_connections != 0
                    {
                        return Err("identity or resource conservation failed".into());
                    }
                    drop(successor);
                    if pool.state().idle_connections != 1 {
                        return Err("successor did not return the resource".into());
                    }
                }
                Ok(())
            }
        },
    );
    println!("{report}");
    assert!(
        report.is_clean() && report.baseline_settled && report.exhaustive,
        "{report}"
    );
    assert!(report.unsettled().next().is_none(), "{report}");
    assert!(
        report
            .trials
            .iter()
            .all(|t| t.outcomes == [CutOutcome::Cancelled]),
        "{report}"
    );
    let observed = stages.each_ref().map(|n| n.load(SeqCst));
    println!("cancelled before-send / sent-unreceived / received: {observed:?}");
    assert!(
        observed.iter().all(|n| *n > 0),
        "missing handoff stage: {observed:?}"
    );
}

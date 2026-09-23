//! redis-rs#851 (reporter's program, bbaldino/redis_tokio_test@a996109): a worker
//! `select!`s between a control channel and `XREAD BLOCK 0` on a multiplexed connection.
//! When a control message wins, the XREAD future is dropped, but the command was already
//! sent: the server keeps blocking, and the next XREAD (with the new subscriber's key) is
//! queued behind it, so the new subscriber's messages never arrive. Cancelling the future
//! does not cancel the work on the server. The maintainer's advice: avoid BLOCK 0 (fixed
//! variant: a finite block, re-issued each iteration).
//!
//! Contract: a message added to a subscribed stream is received by the worker.
//! Needs Redis; see run.sh.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dropwise::{explore, Config, Ctx};
use redis::AsyncCommands;
use redis::streams::{StreamReadOptions, StreamReadReply};
use tokio::sync::mpsc;

static RUN: AtomicU32 = AtomicU32::new(0);

async fn scenario(ctx: Ctx, block_ms: usize) -> Result<(), String> {
    let url = std::env::var("REDIS_URL").map_err(|_| "REDIS_URL not set (use run.sh)".to_string())?;
    let run = RUN.fetch_add(1, Ordering::SeqCst);
    let client = redis::Client::open(url).map_err(|e| e.to_string())?;
    let mut worker_conn = client.get_multiplexed_tokio_connection().await.map_err(|e| e.to_string())?;
    let mut harness_conn = client.get_multiplexed_tokio_connection().await.map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<String>(1);
    let (got_tx, mut got_rx) = mpsc::channel::<String>(1);

    let worker = async move {
        let opts = StreamReadOptions::default().block(block_ms).count(1);
        let mut subscribers: HashMap<String, String> = HashMap::new();
        loop {
            let keys: Vec<String> = subscribers.keys().cloned().collect();
            let ids: Vec<String> = keys.iter().map(|k| subscribers[k].clone()).collect();
            let read = worker_conn.xread_options::<String, String, StreamReadReply>(&keys, &ids, &opts);
            let (read, mut preempt) = ctx.race(read);
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(key) => { subscribers.insert(key, "0".into()); }
                    None => return,
                },
                r = read, if !keys.is_empty() => {
                    if let Ok(reply) = r {
                        for k in reply.keys {
                            for id in k.ids {
                                subscribers.insert(k.key.clone(), id.id.clone());
                                let _ = got_tx.send(k.key.clone()).await;
                            }
                        }
                    }
                }
                _ = &mut preempt => {}
            }
        }
    };
    tokio::spawn(worker);

    let (a, b) = (format!("s{run}a"), format!("s{run}b"));
    tx.send(a).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await; // worker is now blocked on XREAD a
    tx.send(b.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _: String = harness_conn.xadd(&b, "*", &[("data", "hello2")]).await.map_err(|e| e.to_string())?;
    match got_rx.recv().await {
        Some(k) if k == b => Ok(()),
        other => Err(format!("worker received {other:?}")),
    }
}

fn config() -> Config {
    Config { scenario_timeout: Duration::from_secs(3), ..Config::default() }
}

#[test]
fn block_zero_strands_new_subscribers() {
    let report = explore(&config(), |c| scenario(c, 0));
    println!("BLOCK 0 (reporter)\n{report}");
    // The program's own select! drops the in-flight XREAD whenever a control message wins.
    assert!(!report.baseline_errors.is_empty(), "{report}");
    assert!(report.baseline_errors[0].contains("liveness"), "{report}");
}

#[test]
fn finite_block_is_clean() {
    let report = explore(&config(), |c| scenario(c, 100));
    println!("BLOCK 100 (maintainer's advice)\n{report}");
    // Real network I/O: the number of Pending results of an XREAD varies between runs, so
    // some planned boundaries may go unrealized. They are reported, not counted as passes.
    assert!(report.is_clean() && report.exhaustive, "{report}");
    println!("unrealized plans: {}", report.unrealized().count());
}

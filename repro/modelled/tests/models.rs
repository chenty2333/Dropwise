//! Reduced models of verified app-layer defects in large systems whose original code
//! cannot be exercised in isolation. Each model copies the shape of the code before and
//! after the real fix (links below); evidence level `modelled`, not `executed`.
//! In every buggy variant a state change is made before an await and undone after it,
//! so destroying the future at that await skips the undo.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dropwise::{explore, Config, Ctx, Obligation, Report};

fn cfg() -> Config {
    Config { scenario_timeout: Duration::from_secs(2), ..Config::default() }
}

fn assert_pair(buggy: Report, fixed: Report) {
    println!("buggy:\n{buggy}\nfixed:\n{fixed}");
    assert!(buggy.baseline_errors.is_empty(), "buggy baseline:\n{buggy}");
    assert!(buggy.violations().next().is_some(), "buggy variant not caught:\n{buggy}");
    assert!(fixed.is_clean() && fixed.exhaustive, "fixed variant not clean:\n{fixed}");
    assert!(fixed.unrealized().next().is_none(), "{fixed}");
}

async fn io() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

// ---------------------------------------------------------------------------------------
// databend#20020, fixed by databend#20021 (src/query/pipeline/src/sinks/async_sink.rs):
//   buggy:  self.called_on_finish = true; self.inner.on_finish().await?;
//   fixed:  let r = self.inner.on_finish().await; self.called_on_finish = true; r?;
// The wrapper's Drop runs a fallback finish only if !called_on_finish, so a cancelled
// on_finish must not be recorded as done. Contract: finish work runs to completion once.
mod databend_20020 {
    use super::*;

    pub struct Sinker {
        pub called_on_finish: bool,
        pub finish: Option<Obligation<()>>, // the finish work that must happen
        pub fixed: bool,
    }

    impl Sinker {
        async fn on_finish(&mut self) {
            io().await; // e.g. flushing a writer
            self.finish.take().map(|o| o.discharge());
        }

        pub async fn async_process(&mut self) {
            if self.called_on_finish {
                return;
            }
            if self.fixed {
                self.on_finish().await;
                self.called_on_finish = true;
            } else {
                self.called_on_finish = true;
                self.on_finish().await;
            }
        }
    }

    impl Drop for Sinker {
        fn drop(&mut self) {
            if !self.called_on_finish {
                // Synchronous fallback finish.
                self.finish.take().map(|o| o.discharge());
            }
        }
    }

    pub async fn run(ctx: Ctx, fixed: bool) -> Result<(), String> {
        let mut s = Sinker { called_on_finish: false, finish: Some(Obligation::new((), "sink finish")), fixed };
        ctx.target(s.async_process()).await;
        Ok(())
    }
}

#[test]
fn databend_20020() {
    assert_pair(
        explore(&cfg(), |c| databend_20020::run(c, false)),
        explore(&cfg(), |c| databend_20020::run(c, true)),
    );
}

// ---------------------------------------------------------------------------------------
// materialize#38577 (persist Pending::block_until_ready): the state was replaced by a
// `Blocking` placeholder before awaiting the write handle; dropping the future left the
// placeholder and every later into_result panicked ("block_until_ready cancelled?").
// Fixed: the handle stays in place while it is awaited. Contract: into_result works after
// a cancelled block_until_ready.
mod materialize_38577 {
    use super::*;

    pub enum Pending {
        Writing(Arc<tokio::sync::Notify>), // stands for the in-flight write handle
        Blocking,
        FinishedOk,
    }

    impl Pending {
        pub async fn block_until_ready(&mut self, fixed: bool) {
            if fixed {
                if let Pending::Writing(h) = self {
                    let h = h.clone();
                    tokio::select! { _ = h.notified() => {}, _ = io() => {} }
                }
                *self = Pending::FinishedOk;
            } else {
                let Pending::Writing(h) = std::mem::replace(self, Pending::Blocking) else { return };
                tokio::select! { _ = h.notified() => {}, _ = io() => {} }
                *self = Pending::FinishedOk;
            }
        }

        pub fn into_result(self) -> Result<(), String> {
            match self {
                Pending::FinishedOk | Pending::Writing(_) => Ok(()),
                Pending::Blocking => Err("panic: block_until_ready cancelled?".into()),
            }
        }
    }

    pub async fn run(ctx: Ctx, fixed: bool) -> Result<(), String> {
        let mut p = Pending::Writing(Arc::new(tokio::sync::Notify::new()));
        ctx.target(p.block_until_ready(fixed)).await;
        p.into_result()
    }
}

#[test]
fn materialize_38577() {
    assert_pair(
        explore(&cfg(), |c| materialize_38577::run(c, false)),
        explore(&cfg(), |c| materialize_38577::run(c, true)),
    );
}

// ---------------------------------------------------------------------------------------
// qdrant#9665 (ShardReplicaSet::queue_proxify_local): `local.take()` then await
// QueueProxyShard::new(..); cancellation drops the shard and leaves `local == None`.
// Fix proposed in qdrant#9666: do the async validation before take() and restore the shard
// with a guard. Contract: after a cancelled proxify, the replica set still has its shard.
mod qdrant_9665 {
    use super::*;

    pub struct ReplicaSet {
        pub local: Option<String>, // the local shard
    }

    impl ReplicaSet {
        pub async fn queue_proxify_local(&mut self, fixed: bool) {
            if fixed {
                io().await; // async WAL/version checks first
                let shard = self.local.take().unwrap();
                self.local = Some(format!("queue-proxy({shard})")); // synchronous from here
            } else {
                let shard = self.local.take().unwrap();
                io().await; // QueueProxyShard::new(..).await
                self.local = Some(format!("queue-proxy({shard})"));
            }
        }
    }

    pub async fn run(ctx: Ctx, fixed: bool) -> Result<(), String> {
        let mut rs = ReplicaSet { local: Some("shard-0".into()) };
        ctx.target(rs.queue_proxify_local(fixed)).await;
        match rs.local {
            Some(_) => Ok(()),
            None => Err("local shard slot is None after cancellation".into()),
        }
    }
}

#[test]
fn qdrant_9665() {
    assert_pair(
        explore(&cfg(), |c| qdrant_9665::run(c, false)),
        explore(&cfg(), |c| qdrant_9665::run(c, true)),
    );
}

// ---------------------------------------------------------------------------------------
// risingwave#3909, fixed by risingwave#3911 (src/common/src/cache.rs,
// lookup_with_request_dedup): on a miss the first caller fetches while later callers
// wait for it; if the first caller's future is dropped, the waiters are stranded.
// Fixed: the fetch runs in a spawned task. Contract: a concurrent second lookup completes.
mod risingwave_3909 {
    use super::*;
    use tokio::sync::oneshot;

    #[derive(Default)]
    pub struct Cache {
        values: Mutex<HashMap<u32, u32>>,
        pending: Mutex<HashMap<u32, Vec<oneshot::Sender<u32>>>>,
    }

    impl Cache {
        fn insert(&self, key: u32, v: u32) {
            self.values.lock().unwrap().insert(key, v);
            for tx in self.pending.lock().unwrap().remove(&key).unwrap_or_default() {
                let _ = tx.send(v);
            }
        }

        pub async fn lookup(self: &Arc<Self>, key: u32, fixed: bool) -> Option<u32> {
            if let Some(v) = self.values.lock().unwrap().get(&key) {
                return Some(*v);
            }
            let rx = {
                let mut pending = self.pending.lock().unwrap();
                match pending.get_mut(&key) {
                    Some(waiters) => {
                        let (tx, rx) = oneshot::channel();
                        waiters.push(tx);
                        Some(rx)
                    }
                    None => {
                        pending.insert(key, Vec::new());
                        None
                    }
                }
            };
            if let Some(rx) = rx {
                return rx.await.ok(); // follower
            }
            // leader
            let fetch = async move { io().await; key * 10 };
            if fixed {
                let this = self.clone();
                tokio::spawn(async move {
                    let v = fetch.await;
                    this.insert(key, v);
                    v
                })
                .await
                .ok()
            } else {
                let v = fetch.await;
                self.insert(key, v);
                Some(v)
            }
        }
    }

    pub async fn run(ctx: Ctx, fixed: bool) -> Result<(), String> {
        let cache = Arc::new(Cache::default());
        let c2 = cache.clone();
        let (op, mut preempt) = ctx.race(cache.lookup(1, fixed));
        let follower = tokio::spawn(async move { c2.lookup(1, fixed).await });
        tokio::select! { biased; _ = op => {}, _ = &mut preempt => {} }
        match follower.await {
            Ok(Some(10)) => Ok(()),
            other => Err(format!("follower got {other:?}")),
        }
    }
}

#[test]
fn risingwave_3909() {
    assert_pair(
        explore(&cfg(), |c| risingwave_3909::run(c, false)),
        explore(&cfg(), |c| risingwave_3909::run(c, true)),
    );
}

// ---------------------------------------------------------------------------------------
// neon#12345 (proxy/src/batch.rs, BatchQueue::call): the caller that becomes leader pops
// the queued jobs and awaits the batch processor; if the leader's call is cancelled
// (e.g. by an outer timeout), the popped jobs never get responses. Fixed shape modelled
// here: once a caller is leader, the batch runs to completion regardless of the caller
// (the real fix makes `call` cancellation-aware so it can only be cancelled while
// waiting). Contract: every queued job gets a response.
mod neon_12345 {
    use super::*;
    use tokio::sync::oneshot;

    #[derive(Default)]
    pub struct BatchQueue {
        queue: Mutex<Vec<(u32, oneshot::Sender<u32>)>>,
        leader: tokio::sync::Mutex<()>,
    }

    impl BatchQueue {
        pub async fn call(self: &Arc<Self>, req: u32, fixed: bool) -> Option<u32> {
            let (tx, mut rx) = oneshot::channel();
            self.queue.lock().unwrap().push((req, tx));
            loop {
                let guard = tokio::select! {
                    biased;
                    r = &mut rx => return r.ok(),
                    g = self.leader.lock() => g,
                };
                let jobs = std::mem::take(&mut *self.queue.lock().unwrap()); // pop jobs
                let batch = async move {
                    io().await; // the batch processor
                    for (req, tx) in jobs {
                        let _ = tx.send(req + 1);
                    }
                };
                if fixed {
                    let _ = tokio::spawn(batch).await;
                } else {
                    batch.await;
                }
                drop(guard);
            }
        }
    }

    pub async fn run(ctx: Ctx, fixed: bool) -> Result<(), String> {
        let q = Arc::new(BatchQueue::default());
        // L0 is leader and is processing a batch, holding the leader lock.
        let q0 = q.clone();
        let l0 = tokio::spawn(async move { q0.call(0, fixed).await });
        tokio::task::yield_now().await;
        // T (the target) queues its job and waits for the lock first, then F does.
        let tq = q.clone();
        let (op, mut preempt) = ctx.race(async move { tq.call(1, fixed).await });
        let fq = q.clone();
        let follower = tokio::spawn(async move { fq.call(7, fixed).await });
        // When L0 finishes, T becomes leader and pops [T, F]; if T is then dropped
        // mid-batch, F's response sender is dropped with it.
        tokio::select! { biased; _ = op => {}, _ = &mut preempt => {} }
        let _ = l0.await;
        match follower.await {
            Ok(Some(8)) => Ok(()),
            other => Err(format!("queued job got {other:?}")),
        }
    }
}

#[test]
fn neon_12345() {
    assert_pair(
        explore(&cfg(), |c| neon_12345::run(c, false)),
        explore(&cfg(), |c| neon_12345::run(c, true)),
    );
}

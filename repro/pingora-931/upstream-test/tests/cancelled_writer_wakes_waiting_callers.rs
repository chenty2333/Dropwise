use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pingora_memory_cache::{Lookup, RTCache};
use tokio::sync::Notify;

#[derive(Clone)]
struct LookupState {
    entered: Arc<Notify>,
    calls: Arc<AtomicUsize>,
}

struct CoordinatedLookup;

#[async_trait]
impl Lookup<u32, u32, LookupState> for CoordinatedLookup {
    async fn lookup(
        _key: &u32,
        extra: Option<&LookupState>,
    ) -> Result<(u32, Option<Duration>), Box<dyn std::error::Error + Send + Sync>> {
        let state = extra.expect("test lookup requires coordination state");
        let call = state.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            state.entered.notify_one();
            std::future::pending::<()>().await;
            unreachable!("the first lookup is cancelled while pending");
        }
        Ok((42, None))
    }
}

#[tokio::test]
async fn cancelled_writer_wakes_a_follower_already_waiting_on_its_lock() {
    // Keep the issue's configuration: no age or timeout escape hatch.
    let cache: Arc<RTCache<u32, u32, CoordinatedLookup, LookupState>> =
        Arc::new(RTCache::new(16, None, None));
    let state = LookupState {
        entered: Arc::new(Notify::new()),
        calls: Arc::new(AtomicUsize::new(0)),
    };

    let writer_cache = cache.clone();
    let writer_state = state.clone();
    let writer = tokio::spawn(async move { writer_cache.get(&1, None, Some(&writer_state)).await });
    tokio::time::timeout(Duration::from_secs(1), state.entered.notified())
        .await
        .expect("writer did not enter lookup");

    // Poll the follower once through Tokio's scheduler and require it to be
    // pending before cancelling the writer. This makes the test cover an
    // already-registered semaphore waiter, not just a caller arriving later.
    let follower_cache = cache.clone();
    let follower_state = state.clone();
    let follower = follower_cache.get(&1, None, Some(&follower_state));
    tokio::pin!(follower);
    tokio::select! {
        biased;
        result = &mut follower => panic!("follower returned before writer cancellation: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }

    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());

    let (value, _) = tokio::time::timeout(Duration::from_secs(1), &mut follower)
        .await
        .expect("cancelled writer did not wake its already-waiting follower");
    assert_eq!(value.unwrap(), 42);
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
}

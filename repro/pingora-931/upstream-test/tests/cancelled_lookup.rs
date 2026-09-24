use std::time::Duration;

use async_trait::async_trait;
use pingora_memory_cache::{Lookup, RTCache};

struct SlowLookup;

#[async_trait]
impl Lookup<u32, u32, ()> for SlowLookup {
    async fn lookup(
        _key: &u32,
        _extra: Option<&()>,
    ) -> Result<(u32, Option<Duration>), Box<dyn std::error::Error + Send + Sync>> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok((42, None))
    }
}

#[tokio::test]
async fn cancelled_lookup_does_not_block_later_callers() {
    // No lock_age and no lock_timeout.
    let cache: RTCache<u32, u32, SlowLookup, ()> = RTCache::new(16, None, None);

    // The first caller gives up while its lookup is still in flight (e.g. a request timeout).
    let first = tokio::time::timeout(Duration::from_millis(10), cache.get(&1, None, None)).await;
    assert!(first.is_err(), "the first get() should still be looking up");

    // A later caller for the same key must not wait forever on the abandoned lock.
    let (value, _) = tokio::time::timeout(Duration::from_secs(1), cache.get(&1, None, None))
        .await
        .expect("second get() hung: the lock of the cancelled first get() was never released");
    assert_eq!(value.unwrap(), 42);
}

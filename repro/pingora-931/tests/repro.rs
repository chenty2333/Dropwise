//! cloudflare/pingora#931: `RTCache::get` installs a per-key coalescing lock on a miss
//! and releases it only after `Lookup::lookup().await` returns. If the writer's `get`
//! future is dropped while the lookup is pending, the lock stays with zero permits and,
//! with no lock_age / lock_timeout, every later caller for that key waits forever.
//!
//! Contract: a later `get` for the same key completes. A hang is reported by Dropwise's
//! scenario watchdog as a liveness violation. The first `get` runs in the program's own
//! `select!` via `ctx.race`, so the real select! drops it.

use std::error::Error as StdError;
use std::time::Duration;

use dropwise::{explore, Config, Ctx, CutOutcome};

macro_rules! scenario {
    ($name:ident, $krate:ident) => {
        mod $name {
            use super::*;
            use $krate::{Lookup, RTCache};

            pub struct SlowLookup;

            #[async_trait::async_trait]
            impl Lookup<u32, u32, ()> for SlowLookup {
                async fn lookup(
                    _key: &u32,
                    _extra: Option<&()>,
                ) -> Result<(u32, Option<Duration>), Box<dyn StdError + Send + Sync>> {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok((42, None))
                }
            }

            pub async fn run(ctx: Ctx) -> Result<(), String> {
                // No lock_age, no lock_timeout: the configuration the issue describes.
                let cache: RTCache<u32, u32, SlowLookup, ()> = RTCache::new(16, None, None);
                {
                    let (op, mut preempt) = ctx.race(cache.get(&1, None, None));
                    tokio::select! {
                        biased;
                        _ = op => {}
                        _ = &mut preempt => {} // e.g. a request timeout or a lost select! branch
                    }
                }
                let (value, _status) = cache.get(&1, None, None).await;
                match value {
                    Ok(42) => Ok(()),
                    other => Err(format!("second get returned {:?}", other.map_err(|e| e.to_string()))),
                }
            }
        }
    };
}

scenario!(buggy, pmc_buggy);
scenario!(fixed, pmc_fixed);

fn config() -> Config {
    Config { scenario_timeout: Duration::from_secs(2), ..Config::default() }
}

#[test]
fn released_version_strands_later_callers() {
    let report = explore(&config(), buggy::run);
    println!("buggy (0.9.0)\n{report}");
    assert!(report.baseline_errors.is_empty() && report.exhaustive, "{report}");
    let v: Vec<_> = report.violations().collect();
    assert!(!v.is_empty(), "expected a violation:\n{report}");
    assert!(v.iter().all(|t| t.outcomes == [CutOutcome::Cancelled]));
    assert!(v.iter().any(|t| t.invariant.as_ref().unwrap_err().contains("liveness")), "{report}");
}

#[test]
fn proposed_fix_is_clean() {
    let report = explore(&config(), fixed::run);
    println!("fixed (#948)\n{report}");
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert!(report.unrealized().next().is_none(), "{report}");
}

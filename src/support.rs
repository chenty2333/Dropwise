//! Small helpers for writing cancellation scenarios.
//!
//! These are conveniences, not part of the exploration engine and not a
//! scenario framework. A scenario can use them or its own equivalents; nothing
//! in [`explore`](crate::explore) or
//! [`explore_observed`](crate::explore_observed) requires them.
//!
//! Both helpers were extracted from repeated work in the existing scenarios:
//! per-step timeout handling around a survivor or retry (so a hang becomes an
//! observation instead of a whole-run liveness violation), and per-run unique
//! identifiers (so calibration runs do not share server state).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Await `future` for at most `limit`, turning a timeout into a scenario
/// `Err` with a stable message.
///
/// Use this around a survivor, retry or independent-observer step whose failure
/// belongs in the observation. Without it, a step that never finishes ends the
/// whole run as a liveness violation at [`Config::scenario_timeout`], which
/// loses the other observation fields.
///
/// The timeout uses Tokio time, so under
/// [`Flavor::CurrentThread`](crate::Flavor::CurrentThread), the paused clock
/// can auto-advance when the runtime is idle, including during the scenario.
/// This is not a wall-clock deadline; prefer a real-time flavor when the
/// bounded step waits on real I/O.
///
/// [`Config::scenario_timeout`]: crate::Config::scenario_timeout
pub async fn bounded<F, T>(limit: Duration, label: &str, future: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match tokio::time::timeout(limit, future).await {
        Ok(result) => result,
        Err(_) => Err(format!("{label} timed out after {limit:?}")),
    }
}

/// A process-unique identifier with a readable prefix.
///
/// The value is `"{prefix}-{process id}-{counter}"`, where the counter is
/// process-local and increases by one per call. Rebuild it inside the scenario
/// (not once outside the closure) so every calibration and trial gets fresh
/// server-side state.
///
/// Do not put a raw unique id into the observed value: the two samples of a
/// calibration side would then always disagree and every plan would be
/// [`Verdict::Unknown`](crate::Verdict::Unknown). Normalize ids out of the
/// observation, or observe a value that does not contain them.
pub fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

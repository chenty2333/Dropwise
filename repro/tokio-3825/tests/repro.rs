//! tokio#3825: the reporter's shutdown loop recreates `notify.notified()` in every
//! `select!` iteration. A `notify_waiters()` that lands on the `Notified` future of an
//! iteration whose other branch (the interval tick) wins is dropped with that future, so
//! the client never leaves the loop. Maintainer's fix: create the `Notified` once, pin it
//! outside the loop, and poll it by reference.
//!
//! Contract (the reporter's intent, confirmed by the maintainer): after notify_waiters(),
//! the client loop exits. A hang is reported by the scenario watchdog as liveness.
//! The tick branch is Dropwise's `Preempt`.

use std::sync::Arc;
use std::time::Duration;

use dropwise::{explore, Config, Ctx, CutOutcome, Race};
use tokio::sync::Notify;

fn manager(notify: Arc<Notify>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(4)).await; // as in the report
        notify.notify_waiters();
    });
}

/// The reporter's loop.
async fn buggy(ctx: Ctx) -> Result<(), String> {
    let notify = Arc::new(Notify::new());
    manager(notify.clone());
    loop {
        let (notified, mut tick) = ctx.race(notify.notified());
        tokio::select! {
            biased;
            _ = notified => return Ok(()),
            _ = &mut tick => {} // "client tick"
        }
    }
}

/// The maintainer's fix.
async fn fixed(ctx: Ctx) -> Result<(), String> {
    let notify = Arc::new(Notify::new());
    manager(notify.clone());
    let (notified, mut tick) = ctx.race(notify.notified());
    tokio::pin!(notified);
    loop {
        tokio::select! {
            biased;
            _ = &mut notified => return Ok(()),
            _ = &mut tick => {}
        }
    }
}

fn config(races: Vec<Race>) -> Config {
    Config { races, scenario_timeout: Duration::from_secs(2), ..Config::default() }
}

#[test]
fn competitor_winning_before_the_notification_is_harmless() {
    // The tick wins before notify_waiters(): nothing is lost, the next iteration's
    // Notified is registered in time.
    let races = vec![Race::Immediate, Race::Reschedule(1), Race::After(Duration::from_secs(1))];
    let report = explore(&config(races), buggy);
    println!("{report}");
    assert!(report.is_clean() && report.exhaustive, "{report}");
}

#[test]
fn competitor_winning_after_the_notification_loses_it() {
    // The tick wins in the same poll that the notification arrived in (report: tick and
    // notify both at t = 4 s): the notified Notified is dropped and the loop never exits.
    let report = explore(&config(vec![Race::After(Duration::from_secs(5))]), buggy);
    println!("{report}");
    assert!(report.baseline_errors.is_empty(), "{report}");
    let v: Vec<_> = report.violations().collect();
    assert!(!v.is_empty(), "expected a hang:\n{report}");
    assert!(v.iter().all(|t| t.outcomes == [CutOutcome::Cancelled]));
    assert!(v[0].invariant.as_ref().unwrap_err().contains("liveness"), "{report}");
}

#[test]
fn maintainers_fix_is_clean() {
    let races = vec![Race::Immediate, Race::Reschedule(1), Race::After(Duration::from_secs(5))];
    let report = explore(&config(races), fixed);
    println!("{report}");
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert!(report.trials.iter().all(|t| t.outcomes == [CutOutcome::Kept]), "{report}");
}

// `Race::AfterWake`: no timing parameter, and the reporter's `select!` as written
// (unbiased, tick branch first). The tick wins in the same poll in which the
// notification was delivered to the `Notified` future ("when ticker and notify happen
// simultaneously"); with random branch order this is a realizable execution.

async fn buggy_as_reported(ctx: Ctx) -> Result<(), String> {
    let notify = Arc::new(Notify::new());
    manager(notify.clone());
    loop {
        let (notified, mut tick) = ctx.race(notify.notified());
        tokio::select! {
            _ = &mut tick => {} // "client tick"
            _ = notified => return Ok(()),
        }
    }
}

async fn fixed_as_reported(ctx: Ctx) -> Result<(), String> {
    let notify = Arc::new(Notify::new());
    manager(notify.clone());
    let (notified, mut tick) = ctx.race(notify.notified());
    tokio::pin!(notified);
    loop {
        tokio::select! {
            _ = &mut tick => {}
            _ = &mut notified => return Ok(()),
        }
    }
}

#[test]
fn after_wake_finds_the_lost_notification_without_timing() {
    let report = explore(&config(vec![Race::AfterWake]), buggy_as_reported);
    println!("{report}");
    assert!(report.baseline_errors.is_empty() && report.exhaustive, "{report}");
    let v: Vec<_> = report.violations().collect();
    assert_eq!(v.len(), 1, "{report}");
    assert_eq!(v[0].outcomes, [CutOutcome::Cancelled]);
    assert!(v[0].invariant.as_ref().unwrap_err().contains("liveness"), "{report}");
}

#[test]
fn after_wake_maintainers_fix_is_clean() {
    let report = explore(&config(vec![Race::AfterWake]), fixed_as_reported);
    println!("{report}");
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert!(report.trials.iter().all(|t| t.outcomes == [CutOutcome::Kept]), "{report}");
}

#[test]
fn immediate_cut_misses_it() {
    // For contrast: dropping the Notified before the notification arrives is harmless.
    let report = explore(&config(vec![Race::Immediate]), buggy_as_reported);
    assert!(report.is_clean() && report.exhaustive, "{report}");
}

use std::time::Duration;

use dropwise::models::mpsc::UnboundedReceiver;
use dropwise::{explore, Config, Ctx, Flavor, Obligation, Race};
use tokio::sync::mpsc;
use tokio::time::Instant;

fn immediate_only() -> Config {
    Config { races: vec![Race::Immediate], ..Config::default() }
}

async fn hold_across_await(label: &'static str) {
    let o = Obligation::new((), label);
    tokio::task::yield_now().await;
    o.discharge();
}

#[test]
fn multiple_targets_and_combinations() {
    let config = Config { max_cancellations: 2, ..immediate_only() };
    let report = explore(&config, |ctx: Ctx| async move {
        ctx.target(hold_across_await("first")).await;
        ctx.target(hold_across_await("second")).await;
        Ok(())
    });
    assert!(report.exhaustive, "{report}");
    assert_eq!(report.baseline_targets, 2);
    // {first}, {second}, {first + second}
    assert_eq!(report.trials.len(), 3, "{report}");
    assert_eq!(report.violations().count(), 3);
    let pair = report.trials.iter().find(|t| t.plan.len() == 2).unwrap();
    assert_eq!(pair.leaks.len(), 2);
}

#[test]
fn every_race_mode_is_tried() {
    let config = Config {
        races: vec![Race::Immediate, Race::Reschedule(1), Race::After(Duration::from_millis(5))],
        ..Config::default()
    };
    let report = explore(&config, |ctx: Ctx| async move {
        ctx.target(hold_across_await("x")).await;
        Ok(())
    });
    assert_eq!(report.trials.len(), 3, "{report}");
}

#[test]
fn after_race_lets_the_system_advance() {
    let d = Duration::from_millis(10);
    let config = Config { races: vec![Race::After(d)], ..Config::default() };
    let report = explore(&config, |ctx: Ctx| async move {
        let start = Instant::now();
        let out = ctx.target(tokio::time::sleep(Duration::from_secs(3600))).await;
        match out {
            None if start.elapsed() < d => Err(format!("dropped after {:?}", start.elapsed())),
            _ => Ok(()),
        }
    });
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert_eq!(report.trials.len(), 1);
}

/// With `Race::Reschedule(1)`, a producer runs while the target is suspended, so the
/// post-cancellation code sees a different queue state than with `Immediate`.
#[test]
fn yield_race_exposes_interleavings_immediate_misses() {
    let scenario = |ctx: Ctx| async move {
        let (tx, rx) = mpsc::unbounded_channel::<u32>();
        tokio::spawn(async move { tx.send(1).unwrap() });
        let mut rx = UnboundedReceiver::from(rx);
        if let Some(Some(m)) = ctx.target(rx.recv()).await {
            m.discharge();
        }
        // Assumption baked into the caller: if we were cancelled, nothing arrived yet.
        match rx.try_recv() {
            Ok(m) => {
                m.discharge();
                Err("message arrived while we thought the stream was idle".into())
            }
            Err(_) => Ok(()),
        }
    };
    assert!(explore(&immediate_only(), scenario).is_clean());
    let config = Config { races: vec![Race::Reschedule(1)], ..Config::default() };
    assert_eq!(explore(&config, scenario).violations().count(), 1);
}

#[test]
fn leaks_on_worker_threads_are_recorded() {
    let config = Config { flavor: Flavor::MultiThread { workers: 2 }, ..immediate_only() };
    let report = explore(&config, |ctx: Ctx| async move {
        tokio::spawn(async move {
            ctx.target(hold_across_await("on worker")).await;
        })
        .await
        .map_err(|e| e.to_string())
    });
    assert!(report.exhaustive, "{report}");
    let v: Vec<_> = report.violations().collect();
    assert_eq!(v.len(), 1, "{report}");
    assert_eq!(v[0].leaks[0].label, "on worker");
}

#[test]
fn obligations_outside_a_run_are_untracked() {
    let o = Obligation::new(1, "free");
    drop(o); // must not panic or record anywhere
}

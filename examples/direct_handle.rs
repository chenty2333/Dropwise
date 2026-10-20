//! Direct handle: a long-lived object stays outside the target future; only one
//! operation is cancelled, and the same handle serves the next operation.
//!
//! `cargo run --example direct_handle`
//!
//! This is the direct-handle shape from the research scenarios, reduced to an
//! in-process teaching model. It is **not** a real client, connection pool or
//! protocol. The point is where the target future's borrow ends:
//!
//! * `session` is created by the scenario, not by the target future;
//! * the target future borrows it for one `get`;
//! * `ctx.target` drops only that future, so the cancelled `get` cannot take
//!   the handle with it;
//! * the survivor is the next `get`/`put` on the same handle;
//! * the independent observer reads the server-side request log and the value
//!   the survivor actually wrote, not the return value of the cancelled call.

#[path = "support/model.rs"]
mod model;

use dropwise::{Config, Ctx, Expect, Race, Report, Verdict, explore_observed};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

/// `drain_on_cancel` is the only difference between the two runs: the buggy
/// caller assumes the handle is at a message boundary; the fixed caller
/// explicitly discards the reply of the operation it abandoned.
fn run(drain_on_cancel: bool) -> Report {
    explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| {
        let server = model::Server::new();
        async move {
            // The long-lived handle lives in the scenario, outside the target
            // future. Its lifetime does not end when the target loses.
            let mut session = model::Session::new(server.clone());

            let cancelled = ctx.target(session.get("a")).await.is_none();
            if cancelled && drain_on_cancel {
                // Explicit recovery, not an implicit reset: one reply belongs
                // to the abandoned operation.
                session.discard_stale_reply();
            }

            // Survivor: the next user of the same handle performs an effect.
            let successor = session.get("b").await;
            session.put("derived", successor.clone()).await;

            // Independent observer: the server-side log and the actual effect,
            // not the handle's own idea of what happened.
            Ok(model::observe(&server, successor))
        }
    })
}

fn main() {
    let buggy = run(false);
    println!("\n== direct handle, cancelled get leaves its reply queued ==");
    println!("{buggy}");
    assert!(
        buggy.violations().count() > 0,
        "the stale reply must be visible to the survivor"
    );
    assert_eq!(
        buggy.verdict_count(Verdict::Neither),
        1,
        "the survivor's effect matches neither calibration: {buggy}"
    );
    assert!(
        !buggy.is_clean_within_coverage(),
        "a violation is not a clean-within-coverage result: {buggy}"
    );

    let fixed = run(true);
    println!("\n== direct handle, the caller discards the abandoned reply ==");
    println!("{fixed}");
    assert!(
        fixed.is_clean_within_coverage(),
        "draining the stale reply leaves a decidable, clean run: {fixed}"
    );
    assert_eq!(
        fixed.verdict_count(Verdict::Post),
        1,
        "the fixed run matches the completed calibration: {fixed}"
    );

    println!(
        "\nboth expectations held: one cancelled operation, same handle, \
         the buggy successor wrote the stale value; the fixed one did not"
    );
}

//! Pooled hand-off: the cancelled operation's connection is returned to a
//! one-connection pool and immediately checked out by the next user.
//!
//! `cargo run --example pooled_handoff`
//!
//! This is the pooled shape from the research scenarios, reduced to an
//! in-process teaching model. It is **not** a real pool or driver. The target
//! future owns a lease, not the connection directly; dropping the target drops
//! the lease, and `Lease::drop` returns the connection to the pool. Whether that
//! return path resets the protocol position decides what the next borrower sees.
//!
//! * target: `pool.get("a")` is cancelled after it sent its request and while
//!   waiting for the reply;
//! * survivor: `pool.get("b")` checks out the same one connection, writes the
//!   value it received to `derived`;
//! * independent observer: the server-side request log and the actual
//!   `derived` value, read without going through the pool.

#[path = "support/model.rs"]
mod model;

use dropwise::{Config, Ctx, Expect, Race, Report, Verdict, explore_observed};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate],
        ..Config::default()
    }
}

/// `clean_on_return` is the pool's return path: the buggy pool returns the
/// connection as-is, the fixed pool drains the abandoned reply before making it
/// available to the next borrower.
fn run(clean_on_return: bool) -> Report {
    explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| {
        let server = model::Server::new();
        async move {
            let pool = model::Pool::new(server.clone(), clean_on_return);

            // The future owns a lease; the connection is outside it. Only the
            // operation is cancelled, and the lease's Drop returns the
            // connection to the pool.
            ctx.target(pool.get("a")).await;

            // Survivor: a different user checks out the same connection and
            // performs an operation with an effect.
            let successor = pool.get("b").await;
            pool.put("derived", successor.clone()).await;

            Ok(model::observe(&server, successor))
        }
    })
}

fn main() {
    let buggy = run(false);
    println!("\n== pooled hand-off, the pool returns a desynchronised connection ==");
    println!("{buggy}");
    assert!(
        buggy.violations().count() > 0,
        "the next borrower must see the stale reply"
    );
    assert_eq!(
        buggy.verdict_count(Verdict::Neither),
        1,
        "the next borrower's effect matches neither calibration: {buggy}"
    );
    assert!(
        !buggy.is_clean_within_coverage(),
        "a violation is not a clean-within-coverage result: {buggy}"
    );

    let fixed = run(true);
    println!("\n== pooled hand-off, the return path drains the abandoned reply ==");
    println!("{fixed}");
    assert!(
        fixed.is_clean_within_coverage(),
        "a clean return path leaves a decidable, clean run: {fixed}"
    );
    assert_eq!(
        fixed.verdict_count(Verdict::Post),
        1,
        "the fixed run matches the completed calibration: {fixed}"
    );

    println!(
        "\nboth expectations held: the buggy pool handed the connection on with an \
         unread reply; the fixed pool did not"
    );
}

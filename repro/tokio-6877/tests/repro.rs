//! tokio#6877: `write_all` / `read_exact` in a `select!` loop over `tokio::io::simplex`.
//! A partially written PATTERN is abandoned when another branch wins, and the next
//! iteration starts a new `write_all` from the beginning, so the reader sees torn data.
//!
//! buggy: the reporter's loop (a fresh `write_all` each iteration).
//! fixed: keep both in-flight operations (read and write) alive across iterations.
//! `biased;` replaces the reporter's random branch order so runs are reproducible;
//! the competing branch is Dropwise's `Preempt`, standing in for "the read won".

use dropwise::{explore, Config, Ctx, CutOutcome};
use tokio::io::{simplex, AsyncReadExt, AsyncWriteExt};

const PATTERN: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xA];
const ROUNDS: usize = 3;
// Smaller than PATTERN so that write_all actually suspends mid-pattern.
const CAPACITY: usize = 5;

async fn buggy(ctx: Ctx) -> Result<(), String> {
    let (mut rx, mut tx) = simplex(CAPACITY);
    let mut buf = vec![0; PATTERN.len()];
    let (mut read, mut written) = (0, 0);
    while read < ROUNDS {
        let (op, mut preempt) = ctx.race(tx.write_all(PATTERN));
        tokio::select! {
            biased;
            r = rx.read_exact(&mut buf) => {
                r.map_err(|e| e.to_string())?;
                if buf != PATTERN {
                    return Err(format!("round {read}: read {buf:?}"));
                }
                read += 1;
            }
            r = op, if written < ROUNDS => { r.map_err(|e| e.to_string())?; written += 1; }
            _ = &mut preempt => {}
        }
    }
    Ok(())
}

async fn fixed(ctx: Ctx) -> Result<(), String> {
    // Both in-flight operations survive losing a select! round: the maintainer's point
    // was that write_all *and* read_exact are not cancel safe. (A first version kept
    // only the writer alive; Dropwise showed the torn reads that remain.)
    let (mut rx, mut tx) = simplex(CAPACITY);
    let reader = async move {
        let mut buf = vec![0; PATTERN.len()];
        for round in 0..ROUNDS {
            rx.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
            if buf != PATTERN {
                return Err(format!("round {round}: read {buf:?}"));
            }
        }
        Ok::<_, String>(())
    };
    let (writer, mut preempt) = ctx.race(async move {
        for _ in 0..ROUNDS {
            tx.write_all(PATTERN).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    tokio::pin!(reader, writer);
    let mut writing = true;
    loop {
        tokio::select! {
            biased;
            r = &mut reader => return r,
            r = &mut writer, if writing => { r.map_err(|e| e.to_string())?; writing = false; }
            _ = &mut preempt => {}
        }
    }
}

#[test]
fn buggy_variant_is_caught() {
    let report = explore(&Config::default(), buggy);
    println!("{report}");
    // The program's own select! already cancels write_all every time the read wins,
    // so the defect shows without any injected cancellation (baseline).
    assert!(!report.baseline_errors.is_empty(), "{report}");
    assert!(report.violations().all(|t| t.outcomes == [CutOutcome::Cancelled]));
}

#[test]
fn fixed_variant_is_clean() {
    let config = Config { scenario_timeout: std::time::Duration::from_secs(3), ..Config::default() };
    let report = explore(&config, fixed);
    println!("{report}");
    assert!(report.is_clean() && report.exhaustive, "{report}");
    assert!(report.unrealized().next().is_none(), "{report}");
}

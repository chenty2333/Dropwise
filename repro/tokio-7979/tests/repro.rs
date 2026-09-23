//! tokio#7979: a cancelled io-uring `File::open` completes in the kernel after its
//! future was dropped; the CQE result is discarded, so the new fd is never closed.
//!
//! Contract: after the scenario and the settle phase, the process holds no more file
//! descriptors than before it. The late completion is only observable after settling.
//! Needs Linux with io_uring and `--cfg tokio_unstable` (see .cargo/config.toml).
//!
//! EXPECT=buggy (tokio 1.51.0) expects a violation; EXPECT=fixed (1.52.0) expects none.

use dropwise::{explore, Config, Ctx};

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map(|d| d.count()).unwrap_or(0)
}

async fn scenario(ctx: Ctx, path: std::path::PathBuf) -> Result<(), String> {
    // Warm-up: the runtime creates its io_uring instance (one fd) on first use.
    drop(tokio::fs::File::open(&path).await.map_err(|e| e.to_string())?);
    let before = open_fds();
    let file = ctx.target(tokio::fs::File::open(path)).await;
    drop(file);
    ctx.after_settle(move || {
        let after = open_fds();
        if after > before {
            Err(format!("{} file descriptor(s) leaked ({before} -> {after})", after - before))
        } else {
            Ok(())
        }
    });
    Ok(())
}

#[test]
fn cancelled_uring_open_does_not_leak_fds() {
    let expect = std::env::var("EXPECT").unwrap_or_else(|_| "buggy".into());
    let path = std::env::temp_dir().join("dropwise-repro-7979.txt");
    std::fs::write(&path, b"x").unwrap();
    let report = explore(&Config::default(), |ctx| scenario(ctx, path.clone()));
    println!("EXPECT={expect}\n{report}");
    assert!(report.baseline_errors.is_empty(), "{report}");
    assert!(!report.trials.is_empty(), "the open never suspended (io_uring not used?)\n{report}");
    assert!(report.unrealized().next().is_none() && report.unsettled().next().is_none(), "{report}");
    match expect.as_str() {
        "buggy" => assert!(report.violations().next().is_some(), "expected a violation:\n{report}"),
        _ => assert!(report.is_clean(), "expected no violation:\n{report}"),
    }
}

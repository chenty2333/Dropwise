//! Modeled on the serial-console proxy bug described in Oxide RFD 400:
//! a wrapper takes a message off a stream, then awaits more work on it.
//! The underlying `recv` is cancel-safe; the wrapper is not.

use std::time::Duration;

use dropwise::models::mpsc::Receiver;
use dropwise::{explore, Config, Ctx, Obligation};
use tokio::sync::mpsc;

#[derive(Default)]
struct Sink {
    delivered: Vec<u32>,
}

impl Sink {
    async fn write(&mut self, msg: u32) {
        tokio::time::sleep(Duration::from_millis(1)).await; // simulated I/O
        self.delivered.push(msg);
    }
}

/// Buggy: the message lives only inside this future between `recv` and `write`.
/// `Receiver` is the library model: each received message carries an obligation.
async fn recv_and_forward(rx: &mut Receiver<u32>, sink: &mut Sink) -> bool {
    match rx.recv().await {
        Some(m) => {
            sink.write(*m.get()).await;
            m.discharge();
            true
        }
        None => false,
    }
}

/// Fixed: the in-flight message is kept by the caller, outside the cancellable future.
async fn forward_fixed(
    rx: &mut mpsc::Receiver<u32>,
    pending: &mut Option<u32>,
    sink: &mut Sink,
) -> bool {
    if pending.is_none() {
        *pending = rx.recv().await;
    }
    match *pending {
        Some(m) => {
            sink.write(m).await;
            *pending = None;
            true
        }
        None => false,
    }
}

async fn setup() -> mpsc::Receiver<u32> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        for i in 0..3 {
            tx.send(i).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    rx
}

fn check(sink: &Sink) -> Result<(), String> {
    if sink.delivered == [0, 1, 2] {
        Ok(())
    } else {
        Err(format!("delivered {:?}, expected [0, 1, 2]", sink.delivered))
    }
}

#[test]
fn buggy_wrapper_loses_messages() {
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        let mut rx = Receiver::from(setup().await);
        let mut sink = Sink::default();
        // One select! round loses to another branch...
        ctx.target(recv_and_forward(&mut rx, &mut sink)).await;
        // ...and the loop keeps using the same stream afterwards.
        while recv_and_forward(&mut rx, &mut sink).await {}
        check(&sink)
    });
    println!("{report}");
    assert!(report.exhaustive);
    assert!(report.violations().any(|t| !t.leaks.is_empty() && t.invariant.is_err()));
}

#[test]
fn fixed_wrapper_is_cancel_correct() {
    dropwise::assert_cancel_correct(|ctx: Ctx| async move {
        let mut rx = setup().await;
        let mut sink = Sink::default();
        let mut pending = None;
        ctx.target(forward_fixed(&mut rx, &mut pending, &mut sink)).await;
        while forward_fixed(&mut rx, &mut pending, &mut sink).await {}
        check(&sink)
    });
}

#[test]
fn abandon_is_not_a_leak() {
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        ctx.target(async {
            let m = Obligation::new(7u32, "shutdown message");
            m.abandon("connection closing");
            tokio::task::yield_now().await;
        })
        .await;
        Ok(())
    });
    assert!(report.is_clean(), "{report}");
    // Cancelled while `m` was held: that *is* a leak, since abandon never ran.
    let report = explore(&Config::default(), |ctx: Ctx| async move {
        ctx.target(async {
            let m = Obligation::new(7u32, "held message");
            tokio::task::yield_now().await;
            m.discharge();
        })
        .await;
        Ok(())
    });
    assert!(report.baseline_errors.is_empty(), "{report}");
    // One violation per race mode at the single suspension point.
    assert_eq!(report.violations().count(), 2, "{report}");
}

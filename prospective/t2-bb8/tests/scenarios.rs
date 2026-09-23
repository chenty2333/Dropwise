//! Pre-registered scenarios S2.1-S2.5 for T2 (bb8 0.9.1), contract C2 (with amendment A1):
//! cancelling `Pool::get()` at any Pending boundary never loses a connection or capacity.
//! After the cancelled call(s) and a settle wait, (a) `max_size` connections can be checked
//! out at the same time within `connection_timeout`, and (b) with none checked out,
//! `pool.state().connections` equals the number of live connection objects.
//!
//! Every scenario also runs against the positive-control mutant (`bb8_mutant`), recorded
//! under runs/t2-control. Written before the first run; tests do not assert on outcomes.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{explore, Config, Ctx, Race, Report};

fn config(max_cancellations: usize) -> Config {
    Config {
        races: vec![Race::Immediate, Race::Reschedule(1), Race::After(Duration::from_millis(1))],
        max_cancellations,
        scenario_timeout: Duration::from_secs(10),
        ..Config::default()
    }
}

fn record(dir: &str, scenario: &str, report: &Report) {
    let run = std::env::var("RUN").unwrap_or_else(|_| "dev".into());
    let dir = format!("{}/../runs/{dir}/{scenario}", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/{run}.txt"), report.to_string()).unwrap();
    println!("{dir}: {} plans, {} violations, {} unrealized, exhaustive={}\n{report}",
        report.trials.len(), report.violations().count(), report.unrealized().count(), report.exhaustive);
}

#[derive(Clone, Copy)]
pub struct P {
    max: u32,
    test_on_check_out: bool,
    /// Idle connections created before the target call.
    warm: usize,
    /// Keep one connection checked out during the target call (pool at max_size).
    hold: bool,
    /// is_valid fails for the first connection ever created.
    invalid_first: bool,
    /// Number of target get() calls.
    targets: usize,
}

macro_rules! t2 {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use $krate::{ManageConnection, Pool};

            pub struct Manager {
                next: AtomicU32,
                live: Arc<AtomicI64>,
                invalid_first: bool,
            }

            pub struct Conn {
                id: u32,
                live: Arc<AtomicI64>,
            }

            impl Drop for Conn {
                fn drop(&mut self) {
                    self.live.fetch_sub(1, SeqCst);
                }
            }

            impl ManageConnection for Manager {
                type Connection = Conn;
                type Error = String;

                fn connect(&self) -> impl Future<Output = Result<Conn, String>> + Send {
                    let id = self.next.fetch_add(1, SeqCst);
                    let live = self.live.clone();
                    async move {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        live.fetch_add(1, SeqCst);
                        Ok(Conn { id, live })
                    }
                }

                fn is_valid(&self, conn: &mut Conn) -> impl Future<Output = Result<(), String>> + Send {
                    let bad = self.invalid_first && conn.id == 0;
                    async move {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        if bad { Err("invalid".into()) } else { Ok(()) }
                    }
                }

                fn has_broken(&self, _conn: &mut Conn) -> bool {
                    false
                }
            }

            async fn check(pool: &Pool<Manager>, live: &AtomicI64, max: u32) -> Result<(), String> {
                tokio::time::sleep(Duration::from_millis(200)).await; // let background work settle
                let mut held = Vec::new();
                for i in 0..max {
                    match pool.get().await {
                        Ok(c) => held.push(c),
                        Err(e) => return Err(format!("C2(a) capacity lost: get {} of {max} failed: {e:?}", i + 1)),
                    }
                }
                drop(held);
                tokio::time::sleep(Duration::from_millis(50)).await;
                let (counted, alive) = (pool.state().connections as i64, live.load(SeqCst));
                if counted != alive {
                    return Err(format!("C2(b) pool counts {counted} connections, {alive} exist"));
                }
                Ok(())
            }

            pub async fn run(ctx: Ctx, p: P) -> Result<(), String> {
                let live = Arc::new(AtomicI64::new(0));
                let manager = Manager { next: AtomicU32::new(0), live: live.clone(), invalid_first: p.invalid_first };
                let pool = Pool::builder()
                    .max_size(p.max)
                    .min_idle(None)
                    .test_on_check_out(p.test_on_check_out)
                    .connection_timeout(Duration::from_millis(500))
                    .build(manager)
                    .await
                    .map_err(|e| format!("build: {e:?}"))?;
                {
                    let mut warm = Vec::new();
                    for _ in 0..p.warm {
                        warm.push(pool.get().await.map_err(|e| format!("warm-up: {e:?}"))?);
                    }
                } // returned to the pool as idle connections
                let held = if p.hold { Some(pool.get().await.map_err(|e| format!("hold: {e:?}"))?) } else { None };
                for _ in 0..p.targets {
                    let (op, mut preempt) = ctx.race(pool.get());
                    tokio::select! {
                        biased;
                        r = op => drop(r), // Ok (connection returned) or a timeout error
                        _ = &mut preempt => {}
                    }
                }
                drop(held);
                check(&pool, &live, p.max).await
            }
        }
    };
}

t2!(real, bb8);
t2!(mutant, bb8_mutant);

fn both(scenario: &str, p: P, max_cancellations: usize) {
    record("t2", scenario, &explore(&config(max_cancellations), |c| real::run(c, p)));
    record("t2-control", scenario, &explore(&config(max_cancellations), |c| mutant::run(c, p)));
}

const BASE: P = P { max: 2, test_on_check_out: true, warm: 0, hold: false, invalid_first: false, targets: 1 };

#[test]
fn s2_1a_idle_available_test_on_check_out() {
    both("s2_1a", P { warm: 1, ..BASE }, 1);
}

#[test]
fn s2_1b_idle_available_no_test() {
    both("s2_1b", P { warm: 1, test_on_check_out: false, ..BASE }, 1);
}

#[test]
fn s2_2_no_idle_below_max() {
    both("s2_2", BASE, 1);
}

#[test]
fn s2_3_at_max_size_waiting() {
    both("s2_3", P { max: 1, hold: true, ..BASE }, 1);
}

#[test]
fn s2_4_invalid_connection_replaced() {
    both("s2_4", P { warm: 1, invalid_first: true, ..BASE }, 1);
}

#[test]
fn s2_5_two_cancellations() {
    both("s2_5", P { targets: 2, ..BASE }, 2);
}

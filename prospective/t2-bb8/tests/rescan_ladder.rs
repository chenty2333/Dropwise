//! Cancellation-ablation ladder for T2 (bb8 0.9.1), see prospective/RESCAN.md,
//! section 6.7. Same in-process request/reply model, `Manager`, `Conn`, `pool()`
//! and `Config` as `rescan.rs`: R2.1 marks `Pool::get()` as the target, R2.3
//! marks `get()` plus one `ECHO borrower` call. Only what runs after the
//! cancelled operation changes:
//!
//! * B1 stops after the target: only Dropwise's own checks judge it (panic,
//!   liveness, obligation leak).
//! * B2 retries the same operation with the same arguments through the same
//!   pool, under a 3 s timeout; an `Err` or a hang is a violation.
//! * B3 runs a different operation of the same kind (`ECHO ladder`) through the
//!   same pool and checks only that the call returns `Ok`: it never looks at the
//!   reply value and never consults the server log.
//! * M is the R2.1/R2.3 observation of `rescan.rs`: the next borrower's own
//!   reply, plus the server's answer to it, judged by `explore_observed`.
//!
//! `ladder::simple` and `ladder::observed` print one `LADDER` line per
//! experiment and one `LADDER-VIOLATION` line per violating plan, so M and B3
//! can be compared plan by plan.

#[path = "../../support/ladder.rs"]
mod ladder;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bb8::{ManageConnection, Pool};
use dropwise::{explore, explore_observed, Config, Ctx, Expect as ObservedExpect, Race};
use tokio::sync::mpsc;

/// Bound on one B2 retry or B3 probe: returning no result within this Tokio time
/// is a violation (the cancelled operation left the pool unable to serve the
/// next caller).
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Conn {
    requests: mpsc::UnboundedSender<String>,
    replies: mpsc::UnboundedReceiver<String>,
    in_flight: bool,
}

impl Conn {
    async fn call(&mut self, request: &str) -> Result<String, String> {
        self.in_flight = true;
        self.requests.send(request.into()).map_err(|_| "server gone")?;
        let reply = self.replies.recv().await.ok_or("server gone")?;
        self.in_flight = false;
        Ok(reply)
    }
}

/// Requests the server answered, as (request, reply), for every connection.
type Log = Arc<Mutex<Vec<(String, String)>>>;

struct Manager {
    log: Log,
    /// `has_broken` reports a connection with a request in flight. The ladder
    /// runs the plain manager (`false`), like R2.1/R2.3; the cautious control is
    /// R2.1c/R2.3c and is not a ladder level.
    careful: bool,
}

impl ManageConnection for Manager {
    type Connection = Conn;
    type Error = String;

    async fn connect(&self) -> Result<Conn, String> {
        let (requests, mut inbox) = mpsc::unbounded_channel::<String>();
        let (outbox, replies) = mpsc::unbounded_channel();
        let log = self.log.clone();
        tokio::spawn(async move {
            while let Some(request) = inbox.recv().await {
                let reply = match request.strip_prefix("ECHO ") {
                    Some(payload) => payload.to_string(),
                    None => "PONG".to_string(),
                };
                // Every reply takes a little time: the caller sees Pending.
                tokio::time::sleep(Duration::from_millis(1)).await;
                log.lock().unwrap().push((request, reply.clone()));
                if outbox.send(reply).is_err() {
                    return;
                }
            }
        });
        Ok(Conn { requests, replies, in_flight: false })
    }

    async fn is_valid(&self, conn: &mut Conn) -> Result<(), String> {
        match conn.call("PING").await?.as_str() {
            "PONG" => Ok(()),
            other => Err(format!("unexpected validation reply {other:?}")),
        }
    }

    fn has_broken(&self, conn: &mut Conn) -> bool {
        self.careful && conn.in_flight
    }
}

/// What a survivor sees: its own ECHO reply, and whether the server answered
/// that ECHO with the survivor's payload (the independent observer).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    reply: Result<String, String>,
    server_answered: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// R2.1: the target is `Pool::get()` itself.
    Get,
    /// R2.3: the target borrows a connection and makes one request.
    Request,
}

/// The pool of every level: max_size 1, `test_on_check_out(true)`, a 500 ms
/// connection timeout, and one warm-up `get`.
async fn pool() -> Result<(Pool<Manager>, Log), String> {
    let log = Log::default();
    let manager = Manager { log: log.clone(), careful: false };
    let pool = Pool::builder()
        .max_size(1)
        .min_idle(None)
        .test_on_check_out(true)
        .connection_timeout(Duration::from_millis(500))
        .build(manager)
        .await
        .map_err(|e| format!("build: {e:?}"))?;
    drop(pool.get().await.map_err(|e| format!("warm-up: {e:?}"))?);
    Ok((pool, log))
}

/// The `rescan.rs` target for one shape, marked with `ctx.target`: resolves to
/// `true` if it was cancelled in this run, `false` if it completed. A completed
/// target is ignored, and the pooled connection goes back to the pool.
async fn target(ctx: &Ctx, pool: &Pool<Manager>, shape: Shape) -> Result<bool, String> {
    match shape {
        Shape::Get => match ctx.target(pool.get()).await {
            Some(conn) => {
                drop(conn.map_err(|e| format!("target get: {e:?}"))?);
                Ok(false)
            }
            None => Ok(true),
        },
        Shape::Request => {
            let request = async {
                let mut conn = pool.get().await.map_err(|e| format!("target get: {e:?}"))?;
                conn.call("ECHO borrower").await
            };
            match ctx.target(request).await {
                Some(reply) => {
                    reply?;
                    Ok(false)
                }
                None => Ok(true),
            }
        }
    }
}

/// The next borrower of `rescan.rs`: its own ECHO reply, plus the independent
/// observer's view of the server log.
async fn survivor(pool: &Pool<Manager>, log: &Log) -> Obs {
    let reply = match pool.get().await {
        Ok(mut conn) => conn.call("ECHO survivor").await,
        Err(e) => Err(format!("get: {e:?}")),
    };
    let server_answered = log.lock().unwrap().iter().any(|(q, a)| q == "ECHO survivor" && a == "survivor");
    Obs { reply, server_answered }
}

fn config() -> Config {
    Config {
        races: vec![Race::Immediate, Race::AfterWake],
        scenario_timeout: Duration::from_secs(10),
        ..Config::default()
    }
}

/// One B2 retry or B3 probe under [`PROBE_TIMEOUT`]: an error or a timeout is a
/// violation.
async fn bounded(what: &str, op: impl Future<Output = Result<(), String>>) -> Result<(), String> {
    match tokio::time::timeout(PROBE_TIMEOUT, op).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("{what}: {e}")),
        Err(_) => Err(format!("{what}: no result within {PROBE_TIMEOUT:?} (hang)")),
    }
}

/// B1: pool, target, end. Nothing after the target observes the connection.
fn b1(id: &str, shape: Shape) {
    let report = explore(&config(), |ctx| async move {
        let (pool, _log) = pool().await?;
        target(&ctx, &pool, shape).await?;
        Ok(())
    });
    ladder::simple("B1", id, &report);
}

/// B2: retry the same operation with the same parameters through the same pool.
fn b2(id: &str, shape: Shape) {
    let report = explore(&config(), |ctx| async move {
        let (pool, _log) = pool().await?;
        if !target(&ctx, &pool, shape).await? {
            return Ok(());
        }
        let retry = async {
            match shape {
                Shape::Get => drop(pool.get().await.map_err(|e| format!("get: {e:?}"))?),
                Shape::Request => {
                    let mut conn = pool.get().await.map_err(|e| format!("get: {e:?}"))?;
                    conn.call("ECHO borrower").await?; // only `Ok` is checked
                }
            }
            Ok::<(), String>(())
        };
        bounded("retry", retry).await
    });
    ladder::simple("B2", id, &report);
}

/// B3: after a cancellation, run a different operation of the same kind through
/// the same pool. Only `Ok` is checked; the reply value is not inspected and the
/// server log is not consulted.
fn b3(id: &str, shape: Shape) {
    let report = explore(&config(), |ctx| async move {
        let (pool, _log) = pool().await?;
        if !target(&ctx, &pool, shape).await? {
            return Ok(());
        }
        let probe = async {
            let mut conn = pool.get().await.map_err(|e| format!("get: {e:?}"))?;
            conn.call("ECHO ladder").await?; // different payload, same kind of call
            Ok::<(), String>(())
        };
        bounded("probe", probe).await
    });
    ladder::simple("B3", id, &report);
}

/// M: the R2.1/R2.3 scenario of `rescan.rs`, unchanged: after the target, the
/// next borrower sends its own request and the server log is the observer.
fn m(id: &str, shape: Shape) {
    let report = explore_observed(&config(), ObservedExpect::Atomic, |ctx| async move {
        let (pool, log) = pool().await?;
        target(&ctx, &pool, shape).await?;
        Ok(survivor(&pool, &log).await)
    });
    ladder::observed("M", id, &report);
}

#[test]
fn r2_1_b1() {
    b1("R2.1", Shape::Get);
}

#[test]
fn r2_1_b2() {
    b2("R2.1", Shape::Get);
}

#[test]
fn r2_1_b3() {
    b3("R2.1", Shape::Get);
}

#[test]
fn r2_1_m() {
    m("R2.1", Shape::Get);
}

#[test]
fn r2_3_b1() {
    b1("R2.3", Shape::Request);
}

#[test]
fn r2_3_b2() {
    b2("R2.3", Shape::Request);
}

#[test]
fn r2_3_b3() {
    b3("R2.3", Shape::Request);
}

#[test]
fn r2_3_m() {
    m("R2.3", Shape::Request);
}

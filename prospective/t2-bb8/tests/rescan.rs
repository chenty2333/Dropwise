//! Post-hoc rescan of T2 (bb8 0.9.1), see prospective/RESCAN.md. Separate from
//! the frozen study; prints one RESCAN line per experiment.
//!
//! The frozen scenarios used a manager whose `is_valid` only slept, so a
//! connection carried no protocol state. Here a connection is an ordered
//! request/response channel to an in-process server task, the shape of most
//! line or frame protocols: a reply that nobody read is read by the next call.

#[path = "../../support/allowed.rs"]
mod allowed;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use allowed::{Calibration, Expect, Mode, Tally, Verdict, calibrate, op};
use bb8::{ManageConnection, Pool};
use dropwise::{Config, Ctx, Race, explore};
use tokio::sync::mpsc;

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
    /// `has_broken` reports a connection with a request in flight.
    careful: bool,
    /// Delay of the server's first PING reply after warm-up (R2.2).
    slow_ping: Option<Duration>,
}

impl ManageConnection for Manager {
    type Connection = Conn;
    type Error = String;

    async fn connect(&self) -> Result<Conn, String> {
        let (requests, mut inbox) = mpsc::unbounded_channel::<String>();
        let (outbox, replies) = mpsc::unbounded_channel();
        let log = self.log.clone();
        let mut slow = self.slow_ping;
        tokio::spawn(async move {
            let mut pings = 0;
            while let Some(request) = inbox.recv().await {
                let reply = match request.strip_prefix("ECHO ") {
                    Some(payload) => payload.to_string(),
                    None => "PONG".to_string(),
                };
                if reply == "PONG" {
                    pings += 1;
                }
                // Every reply takes a little time: the caller sees Pending.
                let delay = match slow {
                    Some(d) if pings == 2 => {
                        slow = None;
                        d
                    }
                    _ => Duration::from_millis(1),
                };
                tokio::time::sleep(delay).await;
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

async fn pool(careful: bool, slow_ping: Option<Duration>, timeout: Duration) -> Result<(Pool<Manager>, Log), String> {
    let log = Log::default();
    let manager = Manager { log: log.clone(), careful, slow_ping };
    let pool = Pool::builder()
        .max_size(1)
        .min_idle(None)
        .test_on_check_out(true)
        .connection_timeout(timeout)
        .build(manager)
        .await
        .map_err(|e| format!("build: {e:?}"))?;
    drop(pool.get().await.map_err(|e| format!("warm-up: {e:?}"))?);
    Ok((pool, log))
}

async fn survivor(pool: &Pool<Manager>, log: &Log) -> Obs {
    let reply = match pool.get().await {
        Ok(mut conn) => conn.call("ECHO survivor").await,
        Err(e) => Err(format!("get: {e:?}")),
    };
    let server_answered = log.lock().unwrap().iter().any(|(q, a)| q == "ECHO survivor" && a == "survivor");
    Obs { reply, server_answered }
}

async fn scenario(ctx: Ctx, mode: Mode, shape: Shape, careful: bool) -> Result<Obs, String> {
    let (pool, log) = pool(careful, None, Duration::from_millis(500)).await?;
    match shape {
        Shape::Get => {
            if let Some(conn) = op(&ctx, mode, pool.get()).await {
                drop(conn.map_err(|e| format!("target get: {e:?}"))?);
            }
        }
        Shape::Request => {
            let request = async {
                let mut conn = pool.get().await.map_err(|e| format!("target get: {e:?}"))?;
                conn.call("ECHO borrower").await
            };
            if let Some(reply) = op(&ctx, mode, request).await {
                reply?;
            }
        }
    }
    Ok(survivor(&pool, &log).await)
}

fn config() -> Config {
    Config {
        races: vec![Race::Immediate, Race::AfterWake],
        scenario_timeout: Duration::from_secs(10),
        ..Config::default()
    }
}

fn rescan(id: &str, shape: Shape, careful: bool) {
    let calibration = calibrate(&config(), |ctx, mode| scenario(ctx, mode, shape, careful));
    let tally = Tally::default();
    let report = explore(&config(), |ctx| {
        let (calibration, tally) = (&calibration, tally.clone());
        async move {
            let obs = scenario(ctx, Mode::Run, shape, careful).await?;
            let checked = calibration.check(Expect::Atomic, &obs);
            tally.push(*checked.as_ref().unwrap_or(&Verdict::Neither));
            checked.map(drop)
        }
    });
    println!("{report}");
    println!("{}", tally.summary(id, &report));
}

#[test]
fn r2_1_get_cancelled_during_is_valid() {
    rescan("R2.1 get", Shape::Get, false);
}

#[test]
fn r2_1c_careful_manager() {
    rescan("R2.1c get, has_broken sees in-flight request", Shape::Get, true);
}

#[test]
fn r2_3_borrower_cancelled_mid_request() {
    rescan("R2.3 request", Shape::Request, false);
}

#[test]
fn r2_3c_careful_manager() {
    rescan("R2.3c request, has_broken sees in-flight request", Shape::Request, true);
}

/// R2.2: no Dropwise cut. bb8's own `connection_timeout` (100 ms) fires while
/// `is_valid` waits for a PONG the server sends after 150 ms. The only
/// calibration is `pre` (the timed-out `get()` never happens); completing that
/// `get()` is impossible by construction.
#[test]
fn r2_2_internal_connection_timeout() {
    for careful in [false, true] {
        let run = |with_timed_out_get: bool| {
            tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap().block_on(
                async move {
                    let slow = with_timed_out_get.then_some(Duration::from_millis(150));
                    let (pool, log) = pool(careful, slow, Duration::from_millis(100)).await.unwrap();
                    if with_timed_out_get {
                        let first = pool.get().await;
                        assert!(first.is_err(), "the slow validation should time out");
                    }
                    survivor(&pool, &log).await
                },
            )
        };
        // `pre`: the timed-out get() never happens, and so neither does the
        // slow reply that caused it.
        let calibration = Calibration::new(vec![run(false), run(false)], Vec::new());
        let actual = run(true);
        let verdict = calibration.check(Expect::Atomic, &actual);
        let label = if careful { "R2.2c timeout, careful" } else { "R2.2 timeout" };
        println!("RESCAN {label}: verdict={verdict:?}");
    }
}

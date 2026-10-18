//! deadpool 0.13.1 compared with bb8 (prospective/RESCAN.md, section 5). Prints
//! one RESCAN line per experiment; no outcome asserts.
//!
//! deadpool keeps an object in an `UnreadyObject` guard while `recycle` runs and
//! discards it if that guard is dropped, so a checkout cancelled mid-recycle
//! cannot return a half-validated connection. An object dropped by its user,
//! however, always goes back to the pool; only the next `recycle` can reject it.
//!
//! Part A uses the in-process request/response model of the bb8 rescan, with a
//! strict recycle (reply must be PONG) and a lax one (any reply passes). Part B
//! runs deadpool-memcached 0.5.0 against memcached (DW_MEMCACHED_PORT).

#[path = "../../support/allowed.rs"]
mod allowed;

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use allowed::{Calibration, Expect};
use deadpool::managed::{self, Metrics, Pool, RecycleError, RecycleResult};
use deadpool::Runtime;
use dropwise::{
    Config, Ctx, Expect as ObservedExpect, Flavor, Race, Report, Settle, Verdict,
    explore_observed,
};
use tokio::sync::mpsc;

// ---- Part A: model ---------------------------------------------------------

struct Conn {
    requests: mpsc::UnboundedSender<String>,
    replies: mpsc::UnboundedReceiver<String>,
}

impl Conn {
    async fn call(&mut self, request: &str) -> Result<String, String> {
        self.requests.send(request.into()).map_err(|_| "server gone")?;
        self.replies.recv().await.ok_or_else(|| "server gone".into())
    }
}

type Log = Arc<Mutex<Vec<(String, String)>>>;

struct Model {
    log: Log,
    /// Recycle requires the reply to be PONG; otherwise any reply passes.
    strict: bool,
    /// Delay the server's next PING reply by this much (once).
    slow_ping: Arc<Mutex<Option<Duration>>>,
}

impl managed::Manager for Model {
    type Type = Conn;
    type Error = String;

    async fn create(&self) -> Result<Conn, String> {
        let (requests, mut inbox) = mpsc::unbounded_channel::<String>();
        let (outbox, replies) = mpsc::unbounded_channel();
        let (log, slow) = (self.log.clone(), self.slow_ping.clone());
        tokio::spawn(async move {
            while let Some(request) = inbox.recv().await {
                let reply = request.strip_prefix("ECHO ").map_or("PONG".to_string(), str::to_string);
                let pending = slow.lock().unwrap().take();
                let delay = match pending {
                    Some(d) if reply == "PONG" => d,
                    other => {
                        *slow.lock().unwrap() = other;
                        Duration::from_millis(1)
                    }
                };
                tokio::time::sleep(delay).await;
                log.lock().unwrap().push((request, reply.clone()));
                if outbox.send(reply).is_err() {
                    return;
                }
            }
        });
        Ok(Conn { requests, replies })
    }

    async fn recycle(&self, conn: &mut Conn, _: &Metrics) -> RecycleResult<String> {
        let reply = conn.call("PING").await.map_err(RecycleError::Backend)?;
        if self.strict && reply != "PONG" {
            return Err(RecycleError::message(format!("unexpected reply {reply:?}")));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    reply: Result<String, String>,
    server_answered: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// D1: the target is `Pool::get()`, so cuts land inside `recycle`.
    Get,
    /// D3: the target checks out an object and makes one request.
    Request,
}

async fn model_pool(strict: bool, recycle_timeout: Option<Duration>) -> (Pool<Model>, Log, Arc<Mutex<Option<Duration>>>) {
    let log = Log::default();
    let slow = Arc::new(Mutex::new(None));
    let pool = Pool::builder(Model { log: log.clone(), strict, slow_ping: slow.clone() })
        .max_size(1)
        .runtime(Runtime::Tokio1)
        .recycle_timeout(recycle_timeout)
        .build()
        .unwrap();
    drop(pool.get().await.unwrap()); // one idle object
    (pool, log, slow)
}

async fn model_survivor(pool: &Pool<Model>, log: &Log) -> Obs {
    let reply = match pool.get().await {
        Ok(mut conn) => conn.call("ECHO survivor").await,
        Err(e) => Err(format!("get: {e}")),
    };
    let server_answered = log.lock().unwrap().iter().any(|(q, a)| q == "ECHO survivor" && a == "survivor");
    Obs { reply, server_answered }
}

async fn model_scenario(ctx: Ctx, shape: Shape, strict: bool) -> Result<Obs, String> {
    let (pool, log, _) = model_pool(strict, None).await;
    match shape {
        Shape::Get => {
            if let Some(r) = ctx.target(pool.get()).await {
                drop(r.map_err(|e| e.to_string())?);
            }
        }
        Shape::Request => {
            let request = async {
                let mut conn = pool.get().await.map_err(|e| e.to_string())?;
                conn.call("ECHO borrower").await
            };
            if let Some(r) = ctx.target(request).await {
                r?;
            }
        }
    }
    Ok(model_survivor(&pool, &log).await)
}

fn model_config() -> Config {
    Config { races: vec![Race::Immediate, Race::AfterWake], scenario_timeout: Duration::from_secs(10), ..Config::default() }
}

/// One line per experiment. The field names keep the old `allowed.rs` format;
/// the counts now come from `Report` (trial verdicts only, baseline excluded).
fn summary(id: &str, report: &Report) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} verdicts(all runs) pre={} post={} neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        report.baseline_errors.len(),
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    )
}

fn model_rescan(id: &str, shape: Shape, strict: bool) {
    let report = explore_observed(&model_config(), ObservedExpect::Atomic, |ctx| {
        model_scenario(ctx, shape, strict)
    });
    println!("{report}");
    println!("{}", summary(id, &report));
}

#[test]
fn d1_get_cancelled_during_recycle() {
    model_rescan("D1 deadpool get, strict recycle", Shape::Get, true);
    model_rescan("D1 deadpool get, lax recycle", Shape::Get, false);
}

#[test]
fn d3_user_cancelled_mid_request() {
    model_rescan("D3 deadpool request, strict recycle", Shape::Request, true);
    model_rescan("D3 deadpool request, lax recycle", Shape::Request, false);
}

/// D2: deadpool's own `recycle_timeout` (100 ms) fires while the server holds
/// a PING reply for 150 ms. `pre`: that checkout never happens.
#[test]
fn d2_internal_recycle_timeout() {
    for strict in [true, false] {
        let run = |timed_out_get: bool| {
            tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap().block_on(
                async move {
                    let (pool, log, slow) = model_pool(strict, Some(Duration::from_millis(100))).await;
                    if timed_out_get {
                        *slow.lock().unwrap() = Some(Duration::from_millis(150));
                        // A recycle timeout discards the object and creates a new one.
                        drop(pool.get().await);
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    model_survivor(&pool, &log).await
                },
            )
        };
        let calibration = Calibration::new(vec![run(false), run(false)], Vec::new());
        let verdict = calibration.check(Expect::Atomic, &run(true));
        let label = if strict { "strict" } else { "lax" };
        println!("RESCAN D2 deadpool recycle_timeout, {label} recycle: verdict={verdict:?}");
    }
}

// ---- Part B: deadpool-memcached against memcached ---------------------------

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn key() -> String {
    format!("dp-{}-{}", std::process::id(), NEXT.fetch_add(1, SeqCst))
}

fn addr() -> String {
    format!("tcp://127.0.0.1:{}", std::env::var("DW_MEMCACHED_PORT").expect("DW_MEMCACHED_PORT not set"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McObs {
    session: Vec<Result<String, String>>,
    observer_saw_write: bool,
}

fn text(v: Result<Option<async_memcached::Value>, async_memcached::Error>) -> Result<String, String> {
    match v {
        Ok(Some(v)) => Ok(String::from_utf8_lossy(&v.data.unwrap_or_default()).into_owned()),
        Ok(None) => Ok("<miss>".into()),
        Err(e) => Err(format!("{e:?}").chars().take(40).collect()),
    }
}

async fn mc_scenario(ctx: Ctx, target: &str) -> Result<McObs, String> {
    use async_memcached::AsciiProtocol;
    let pool = deadpool_memcached::Pool::builder(deadpool_memcached::Manager::new(addr()))
        .max_size(1)
        .build()
        .map_err(|e| e.to_string())?;
    drop(pool.get().await.map_err(|e| e.to_string())?);
    let (own, a, b, warm) = (key(), key(), key(), key());
    {
        let mut direct = async_memcached::Client::new(addr()).await.map_err(|e| format!("{e:?}"))?;
        for (k, v) in [(&a, "value-a"), (&b, "value-b"), (&warm, "warm")] {
            direct.set(k, v, None, None).await.map_err(|e| format!("{e:?}"))?;
        }
    }
    // The target: an ordinary request through a checked-out object.
    let request = async {
        let mut conn = pool.get().await.map_err(|e| e.to_string())?;
        match target {
            "get" => text(conn.get(&warm).await),
            _ => conn.set(&warm, "rewritten", None, None).await.map(|()| "OK".into()).map_err(|e| format!("{e:?}")),
        }
    };
    if let Some(r) = ctx.target(request).await {
        r?;
    }
    let session = match pool.get().await {
        Ok(mut conn) => vec![
            conn.set(&own, "survivor", None, None).await.map(|()| "OK".into()).map_err(|e| format!("{e:?}")),
            text(conn.get(&a).await),
            text(conn.get(&b).await),
            text(conn.get(&own).await),
        ],
        Err(e) => vec![Err(e.to_string())],
    };
    let mut direct = async_memcached::Client::new(addr()).await.map_err(|e| format!("{e:?}"))?;
    let observer_saw_write = text(direct.get(&own).await) == Ok("survivor".into());
    Ok(McObs { session, observer_saw_write })
}

fn mc_config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

#[test]
fn d4_deadpool_memcached_user_cancelled_mid_request() {
    for target in ["get", "set"] {
        let report = explore_observed(&mc_config(), ObservedExpect::Atomic, |ctx| {
            mc_scenario(ctx, target)
        });
        println!("{report}");
        println!("{}", summary(&format!("D4 deadpool-memcached 0.5.0, user cancels {target}"), &report));
    }
}

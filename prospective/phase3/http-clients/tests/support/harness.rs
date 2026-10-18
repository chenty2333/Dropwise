//! The shared ladder skeleton for the phase 3 HTTP/1.1 targets.
//!
//! One scenario shape, one `Config`, four levels (plus M-residue) that differ
//! only in what happens after the cancelled request:
//!
//! - B1: the scenario ends right after the cancellation (`explore`);
//! - B2: retry the same request with the same arguments, judge `Ok` only;
//! - B3: a different request of the same kind, judge `Ok` only;
//! - M: the survivor sends a POST with a unique id, then a second GET. The
//!   independent observer is the server's request log, and the client's own
//!   response body must be the answer to *its* request (`explore_observed`,
//!   `Expect::Atomic`, residual bytes not in the judged value);
//! - M-residue: M through `dropwise::net::Proxy`, with the unread bytes in the
//!   client socket before the survivors added to the observation (Q3 only).
//!
//! Making the request wait for its reply uses the same means at every level:
//! the in-process server waits `header_delay` before the response header and
//! `body_delay` more before the body. A target can therefore be cancelled while
//! waiting for the header or while waiting for the body.
//!
//! `Target` is implemented once per client crate; the target-specific code is
//! only client construction and one complete request/response round.

#![allow(dead_code)]

use std::future::Future;
use std::marker::PhantomData;
use std::time::Duration;

use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};

#[path = "http_server.rs"]
pub mod http_server;

pub use http_server::{Server, ServerConfig};

/// Real time before the server writes the response header.
pub const HEADER_DELAY: Duration = Duration::from_millis(40);
/// Real time between the response header and the response body.
pub const BODY_DELAY: Duration = Duration::from_millis(80);
/// Fixed real-time wait between the cancellation and the first reuse, identical
/// at B2, B3 and M. Longer than `HEADER_DELAY + BODY_DELAY` so the abandoned
/// reply has had time to reach the client before the survivor starts.
pub const RECYCLE_WAIT: Duration = Duration::from_millis(200);
/// Bound on one survivor/retry request; a timeout counts as a failure.
pub const OP_TIMEOUT: Duration = Duration::from_secs(3);

pub fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 128,
        scenario_timeout: Duration::from_secs(30),
        settle: Settle { tokio_time: Duration::from_millis(50), ..Settle::default() },
        ..Config::default()
    }
}

/// One HTTP request: method, origin-form path, the unique `X-DW-Id` marker and
/// a request body. GET requests have an empty body.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: &'static str,
    pub path: String,
    pub id: String,
    pub body: String,
}

impl Req {
    pub fn get(path: &str, id: &str) -> Self {
        Self { method: "GET", path: path.into(), id: id.into(), body: String::new() }
    }

    pub fn post(path: &str, id: &str, body: &str) -> Self {
        Self { method: "POST", path: path.into(), id: id.into(), body: body.into() }
    }

    pub fn expected_reply(&self) -> String {
        format!("repl:{}{}", self.id, "x".repeat(http_server::BODY_PAD))
    }
}

/// The request shape cancelled at every level. L1 is a small GET (reply side
/// only); L2 is a POST with a 4 MiB body (request write side, then reply side).
/// Within one target all levels use the same shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    L1,
    L2,
}

/// Body size of the L2 target request.
pub const LARGE_BODY: usize = 16 << 20;

impl Shape {
    /// The cancelled request. L2's body is large enough that writing it yields
    /// `Pending` boundaries of its own.
    fn target(self) -> Req {
        let id = tag("A");
        match self {
            Shape::L1 => Req::get("/a", &id),
            Shape::L2 => Req::post("/a", &id, &large_body(&id, 'p', LARGE_BODY)),
        }
    }

    /// B3's different request: a different path and, for L2, a different body.
    fn different(self) -> Req {
        let id = tag("B");
        match self {
            Shape::L1 => Req::get("/b", &id),
            Shape::L2 => Req::post("/b", &id, &large_body(&id, 'q', LARGE_BODY / 4)),
        }
    }
}

fn large_body(id: &str, fill: char, size: usize) -> String {
    let mut body = format!("id={id};");
    body.extend(std::iter::repeat_n(fill, size));
    body
}

/// Implemented once per HTTP client crate. `call` is one complete request,
/// including reading the whole response body: that is the operation the user
/// would cancel.
pub trait Target {
    type Client;

    fn make(base_url: &str) -> impl Future<Output = Result<Self::Client, String>>;
    fn call(client: &Self::Client, req: &Req) -> impl Future<Output = Result<String, String>>;
}

/// Scenario-local state. Declared in reverse drop priority: the client is
/// dropped first, then the proxy, then the server.
pub struct Env<T: Target> {
    pub client: T::Client,
    pub proxy: Option<ProxyHolder>,
    pub server: Server,
    pub _target: PhantomData<T>,
}

/// `Proxy` is only compiled with the `net` feature; this wrapper keeps `Env`
/// usable without it.
/// Only compiled with the `net` feature. Without it `Env::proxy` is always
/// `None` and M-residue refuses to run.
#[cfg(feature = "net")]
pub type ProxyHolder = dropwise::net::Proxy;

#[cfg(not(feature = "net"))]
pub struct ProxyHolder;

pub async fn make_env<T: Target>(use_proxy: bool) -> Result<Env<T>, String> {
    let server = Server::start(ServerConfig {
        header_delay: HEADER_DELAY,
        body_delay: BODY_DELAY,
        large_body_read_delay: Duration::from_millis(150),
    })
    .await
        .map_err(|e| format!("server start: {e}"))?;
    let base = server.base_url();

    #[cfg(feature = "net")]
    let (base, proxy) = if use_proxy {
        let proxy = dropwise::net::Proxy::start(server.addr())
            .await
            .map_err(|e| format!("proxy start: {e}"))?;
        (format!("http://{}", proxy.addr()), Some(proxy))
    } else {
        (base, None)
    };

    #[cfg(not(feature = "net"))]
    let (base, proxy) = {
        if use_proxy {
            return Err("M-residue needs --features net".into());
        }
        (base, None)
    };

    let client = T::make(&base).await?;

    // Warm exactly one pooled connection and consume its reply completely.
    let warm = Req::get("/warm", "warm");
    match T::call(&client, &warm).await {
        Ok(body) if body == warm.expected_reply() => {}
        Ok(body) => return Err(format!("warm-up reply was {body:?}, expected {:?}", warm.expected_reply())),
        Err(e) => return Err(format!("warm-up failed: {e}")),
    }
    Ok(Env { client, proxy, server, _target: PhantomData })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Returned `Ok` with the response body the server wrote for this request.
    Correct,
    /// Returned `Ok` with a different body.
    Wrong,
    /// Returned an error.
    Error,
    /// Did not finish within `OP_TIMEOUT`.
    Hung,
}

#[derive(Debug, Clone)]
struct Attempt {
    outcome: Outcome,
    detail: String,
}

/// Keep a wrong body's diagnostic short: reply bodies are deliberately large.
fn short(text: &str) -> String {
    let mut out: String = text.chars().take(60).collect();
    if out.len() < text.len() {
        out.push_str(&format!("... ({} bytes)", text.len()));
    }
    out
}

/// One survivor attempt. The expected body is `repl:<id>`; `Ok` with a
/// different body is `Wrong`, not a success.
async fn attempt<T: Target>(client: &T::Client, req: &Req) -> Attempt {
    let expected = req.expected_reply();
    match tokio::time::timeout(OP_TIMEOUT, T::call(client, req)).await {
        Ok(Ok(body)) if body == expected => {
            Attempt { outcome: Outcome::Correct, detail: short(&body) }
        }
        Ok(Ok(body)) => Attempt {
            outcome: Outcome::Wrong,
            detail: format!("body {:?} != {:?}", short(&body), short(&expected)),
        },
        Ok(Err(e)) => Attempt { outcome: Outcome::Error, detail: e },
        Err(_) => Attempt { outcome: Outcome::Hung, detail: format!("timeout after {OP_TIMEOUT:?}") },
    }
}

/// What the scenario returns and Dropwise judges. `detail` is printed with a
/// violation but ignored by `PartialEq`: it contains per-run ids.
#[derive(Debug, Clone)]
pub struct Obs {
    pub b: Outcome,
    /// How many times the server logged B's unique id. Exactly one expected.
    pub b_seen: usize,
    /// The single B record has method POST and path /effect.
    pub b_ok: bool,
    pub c: Outcome,
    /// How many times the server logged C's unique id. Exactly one expected.
    pub c_seen: usize,
    /// The single C record has method GET and path /c.
    pub c_ok: bool,
    /// Unread bytes in the client socket before the survivors start
    /// (M-residue only; `None` for ordinary M).
    pub residue: Option<usize>,
    #[allow(dead_code)]
    pub detail: String,
}

impl PartialEq for Obs {
    fn eq(&self, other: &Self) -> bool {
        self.b == other.b
            && self.b_seen == other.b_seen
            && self.b_ok == other.b_ok
            && self.c == other.c
            && self.c_seen == other.c_seen
            && self.c_ok == other.c_ok
            && self.residue == other.residue
    }
}

/// B1: cancel the request, then stop. Judged only by `dropwise::explore`
/// (panic, liveness, obligation leak).
pub async fn scenario_b1<T: Target>(ctx: Ctx, shape: Shape) -> Result<(), String> {
    let env = make_env::<T>(false).await?;
    let req = shape.target();
    // A completed target is fine too: B1 judges only what cancellation leaves.
    let _ = ctx.target(T::call(&env.client, &req)).await;
    Ok(())
}

/// B2: cancel, wait the fixed gap, retry the same request with the same
/// arguments through the same pool; only `Ok` is judged.
pub async fn scenario_b2<T: Target>(ctx: Ctx, shape: Shape) -> Result<(), String> {
    let env = make_env::<T>(false).await?;
    let req = shape.target();
    match ctx.target(T::call(&env.client, &req)).await {
        Some(result) => {
            result.map_err(|e| format!("completed target in B2 failed: {e}"))?;
        }
        None => {
            tokio::time::sleep(RECYCLE_WAIT).await;
            match tokio::time::timeout(OP_TIMEOUT, T::call(&env.client, &req)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(format!("retry of the same request failed: {e}")),
                Err(_) => return Err("retry of the same request timed out".into()),
            }
        }
    }
    Ok(())
}

/// B3: cancel, wait the fixed gap, run a different request of the same kind
/// through the same pool; only `Ok` is judged.
pub async fn scenario_b3<T: Target>(ctx: Ctx, shape: Shape) -> Result<(), String> {
    let env = make_env::<T>(false).await?;
    let req = shape.target();
    match ctx.target(T::call(&env.client, &req)).await {
        Some(result) => {
            result.map_err(|e| format!("completed target in B3 failed: {e}"))?;
        }
        None => {
            tokio::time::sleep(RECYCLE_WAIT).await;
            let other = shape.different();
            match tokio::time::timeout(OP_TIMEOUT, T::call(&env.client, &other)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(format!("different request after cancellation failed: {e}")),
                Err(_) => return Err("different request after cancellation timed out".into()),
            }
        }
    }
    Ok(())
}

/// M: cancel, wait, then a side-effecting POST (B) and a GET (C) through the
/// same pool. Judged on the client's replies and the server's independent log;
/// `with_residue` adds the unread client bytes before B starts (M-residue).
pub async fn scenario_m<T: Target>(ctx: Ctx, with_residue: bool, shape: Shape) -> Result<Obs, String> {
    let env = make_env::<T>(with_residue).await?;
    let req = shape.target();
    let a_id = req.id.clone();
    if let Some(result) = ctx.target(T::call(&env.client, &req)).await {
        result.map_err(|e| format!("completed target in M failed: {e}"))?;
    }
    // The same fixed gap at every level; the server's reply has arrived by now.
    tokio::time::sleep(RECYCLE_WAIT).await;

    #[cfg(feature = "net")]
    let residue = if with_residue {
        let proxy = env.proxy.as_ref().expect("proxy exists for M-residue");
        let residue = proxy
            .residue()
            .map_err(|e| format!("residue: {e}"))?
            .into_iter()
            .map(|entry| entry.bytes)
            .max()
            .unwrap_or(0);
        MAX_RESIDUE.fetch_max(residue, std::sync::atomic::Ordering::SeqCst);
        Some(residue)
    } else {
        None
    };
    #[cfg(not(feature = "net"))]
    let residue = {
        if with_residue {
            return Err("M-residue needs --features net".into());
        }
        None
    };

    let b_id = tag("B");
    let c_id = tag("C");
    let b_req = Req::post("/effect", &b_id, &format!("marker={b_id}"));
    let mut detail = String::new();
    let b = attempt::<T>(&env.client, &b_req).await;
    detail.push_str(&format!("B {} {}; ", b.outcome as u8, b.detail));
    let b_records = env.server.records_with_id(&b_id);
    let b_seen = b_records.len();
    let b_ok = b_records.len() == 1 && b_records[0].method == "POST" && b_records[0].path == "/effect";

    let c_req = Req::get("/c", &c_id);
    let c = attempt::<T>(&env.client, &c_req).await;
    detail.push_str(&format!("C {} {}; ", c.outcome as u8, c.detail));
    let c_records = env.server.records_with_id(&c_id);
    let c_seen = c_records.len();
    let c_ok = c_records.len() == 1 && c_records[0].method == "GET" && c_records[0].path == "/c";

    let records = env.server.records();
    if std::env::var_os("DW_TRACE").is_some() {
        let conn = |id: &str| records.iter().find(|record| record.id == id).map(|record| record.connection);
        let warm_conn = records.iter().find(|record| record.id == "warm").map(|record| record.connection);
        println!(
            "M-TRACE a_seen={} warm_conn={warm_conn:?} a_conn={:?} b_conn={:?} c_conn={:?} \
             residue={residue:?} b={:?} b_seen={b_seen} b_ok={b_ok} c={:?} c_seen={c_seen} c_ok={c_ok}",
            conn(&a_id).is_some(),
            conn(&a_id),
            conn(&b_id),
            conn(&c_id),
            b.outcome,
            c.outcome,
        );
    }
    detail.push_str(&format!(
        "server={:?}",
        records.iter().map(|r| (r.method.clone(), r.path.clone(), r.id.clone())).collect::<Vec<_>>()
    ));
    Ok(Obs { b: b.outcome, b_seen, b_ok, c: c.outcome, c_seen, c_ok, residue, detail })
}

pub static MAX_RESIDUE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Per-run unique marker. `tag("B")` is unique within the process; the server
/// is per run, so it cannot collide with an earlier trial.
pub fn tag(letter: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{letter}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

pub fn run_b1<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b1::<T>(ctx, Shape::L1));
    crate::ladder::simple("B1", id, &report);
}

pub fn run_b2<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b2::<T>(ctx, Shape::L1));
    crate::ladder::simple("B2", id, &report);
}

pub fn run_b3<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b3::<T>(ctx, Shape::L1));
    crate::ladder::simple("B3", id, &report);
}

pub fn run_b1_l2<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b1::<T>(ctx, Shape::L2));
    crate::ladder::simple("B1-L2", id, &report);
}

pub fn run_b2_l2<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b2::<T>(ctx, Shape::L2));
    crate::ladder::simple("B2-L2", id, &report);
}

pub fn run_b3_l2<T: Target>(id: &str) {
    let report = explore(&config(), |ctx| scenario_b3::<T>(ctx, Shape::L2));
    crate::ladder::simple("B3-L2", id, &report);
}

/// Diagnostic only: show that consecutive uncancelled requests share one pooled
/// connection. A zero ladder result is only meaningful if the pool reuses.
pub fn check_reuse<T: Target>(id: &str) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("diagnostic runtime");
    runtime.block_on(async {
        let env = match make_env::<T>(false).await {
            Ok(env) => env,
            Err(e) => {
                println!("LADDER-REUSE {id} [{}]: setup failed: {e}", crate::ladder::variant());
                return;
            }
        };
        for letter in ["A", "B", "C"] {
            let req = Req::get("/reuse", &tag(letter));
            if let Err(e) = T::call(&env.client, &req).await {
                println!("LADDER-REUSE {id} [{}]: request failed: {e}", crate::ladder::variant());
                return;
            }
        }
        let records = env.server.records();
        let connections: Vec<_> = records.iter().map(|r| (r.path.clone(), r.connection)).collect();
        let reused = records
            .windows(2)
            .all(|pair| pair[0].connection == pair[1].connection);
        println!(
            "LADDER-REUSE {id} [{}]: requests={} reused={} {:?}",
            crate::ladder::variant(),
            records.len(),
            reused,
            connections
        );
    });
}

pub fn run_m<T: Target>(id: &str) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario_m::<T>(ctx, false, Shape::L1));
    crate::ladder::observed("M", id, &report);
}

pub fn run_m_l2<T: Target>(id: &str) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario_m::<T>(ctx, false, Shape::L2));
    crate::ladder::observed("M-L2", id, &report);
}

pub fn run_m_residue<T: Target>(id: &str) {
    #[cfg(feature = "net")]
    {
        MAX_RESIDUE.store(0, std::sync::atomic::Ordering::SeqCst);
        let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario_m::<T>(ctx, true, Shape::L1));
        println!(
            "LADDER-RESIDUE M-residue {id} [{}]: max_residue={}",
            crate::ladder::variant(),
            MAX_RESIDUE.load(std::sync::atomic::Ordering::SeqCst)
        );
        crate::ladder::observed("M-residue", id, &report);
    }
    #[cfg(not(feature = "net"))]
    {
        println!("LADDER M-residue {id} [{}]: skipped (needs --features net)", crate::ladder::variant());
    }
}

pub fn run_m_residue_l2<T: Target>(id: &str) {
    #[cfg(feature = "net")]
    {
        MAX_RESIDUE.store(0, std::sync::atomic::Ordering::SeqCst);
        let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario_m::<T>(ctx, true, Shape::L2));
        println!(
            "LADDER-RESIDUE M-residue-L2 {id} [{}]: max_residue={}",
            crate::ladder::variant(),
            MAX_RESIDUE.load(std::sync::atomic::Ordering::SeqCst)
        );
        crate::ladder::observed("M-residue-L2", id, &report);
    }
    #[cfg(not(feature = "net"))]
    {
        println!("LADDER M-residue-L2 {id} [{}]: skipped (needs --features net)", crate::ladder::variant());
    }
}

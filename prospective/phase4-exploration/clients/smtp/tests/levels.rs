//! Phase 4 exploration: async-smtp 0.10.2 direct handle, real GreenMail.
//!
//! This test target is compiled with `harness = false`; it is a small runner,
//! not a unit test. The ladder reporting is shared, read-only, from
//! `prospective/support/ladder.rs` via the path four levels up from this file:
//! clients/smtp/tests -> clients/smtp -> clients -> phase4-exploration ->
//! prospective.
//!
//! Public API used (docs.rs for 0.10.2 plus the crate's published example):
//!   let stream = BufStream::new(TcpStream::connect(addr).await?);
//!   let transport = SmtpTransport::new(SmtpClient::new(), stream).await?;
//!   let result: SmtpResult = transport.send(SendableEmail::new(env, bytes)).await;
//! `send` takes `&mut self`, so the transport is an ordinary direct handle that
//! lives outside the target future. No source of async-smtp is read here.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_smtp::error::SmtpResult;
use async_smtp::{Envelope, SendableEmail, SmtpClient, SmtpTransport};
use dropwise::net::{ConnectionStats, Proxy, ProxyOptions};
use dropwise::{
    explore, explore_observed, replay, Config, Ctx, Cut, Expect, Flavor, Plan, Race, Report, Settle,
};
use serde_json::Value;
use tokio::io::BufStream;
use tokio::net::TcpStream;

type Transport = SmtpTransport<BufStream<TcpStream>>;

const LEVELS: [&str; 4] = ["B1", "B2", "B3", "M1"];
const MESSAGE_DELAY: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Environment and the independent REST observer
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Env {
    smtp: String,
    http: String,
}

impl Env {
    fn from_process() -> Result<Self, String> {
        Ok(Self {
            smtp: std::env::var("DW_SMTP_ADDR")
                .map_err(|_| "DW_SMTP_ADDR is not set (host:port of GreenMail SMTP)".to_string())?,
            http: std::env::var("DW_HTTP_BASE")
                .map_err(|_| "DW_HTTP_BASE is not set (e.g. http://127.0.0.1:PORT)".to_string())?,
        })
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build()
}

/// GreenMail users are explicit. Each scenario run creates two fresh users so
/// that a mailbox cannot contain a message from an earlier run or calibration.
fn create_user(env: &Env, email: &str) -> Result<(), String> {
    let login = email.split('@').next().unwrap_or(email);
    let payload = serde_json::json!({
        "email": email,
        "login": login,
        "password": "pass",
    })
    .to_string();
    let url = format!("{}/api/user", env.http);
    match http_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .send_string(&payload)
    {
        Ok(response) => {
            let body = response.into_string().unwrap_or_default();
            println!("REST create_user {email}: {body}");
            Ok(())
        }
        Err(error) => Err(format!("create GreenMail user {email} via {url}: {error}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageView {
    subject: String,
    message_id: String,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    count: usize,
    messages: Vec<MessageView>,
}

/// Reads the mailbox through GreenMail's REST API, which bypasses async-smtp
/// and the loopback proxy. Per-run tokens are replaced by `<run>` so two
/// calibration samples compare equal.
fn observe(env: &Env, user: &str, token: &str) -> Result<Observation, String> {
    let url = format!("{}/api/user/{user}/messages", env.http);
    let body = http_agent()
        .get(&url)
        .call()
        .map_err(|error| format!("GET {url}: {error}"))?
        .into_string()
        .map_err(|error| format!("read GET {url}: {error}"))?;
    let values: Vec<Value> = serde_json::from_str(&body)
        .map_err(|error| format!("parse GET {url} body {body:?}: {error}"))?;

    let mut rows = Vec::with_capacity(values.len());
    for (fallback_uid, value) in values.iter().enumerate() {
        let uid = value
            .get("uid")
            .and_then(Value::as_str)
            .and_then(|uid| uid.parse::<u64>().ok())
            .unwrap_or(fallback_uid as u64);
        let subject = value
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace(token, "<run>");
        let message_id = value
            .get("Message-ID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace(token, "<run>");
        let mime = value
            .get("mimeMessage")
            .and_then(Value::as_str)
            .unwrap_or("");
        let raw_body = mime
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or(mime);
        let body = raw_body
            .trim_end_matches(['\r', '\n'])
            .replace(token, "<run>");
        rows.push((
            uid,
            MessageView {
                subject,
                message_id,
                body,
            },
        ));
    }
    rows.sort_by_key(|(uid, _)| *uid);
    Ok(Observation {
        count: rows.len(),
        messages: rows.into_iter().map(|(_, view)| view).collect(),
    })
}

// ---------------------------------------------------------------------------
// Setup, target and messages shared by every level
// ---------------------------------------------------------------------------

struct Session {
    // Declared before the proxy so the transport (the direct handle) is
    // dropped first at the end of a scenario.
    transport: Transport,
    proxy: Proxy,
    user_a: String,
    user_b: String,
}

#[derive(Clone)]
struct Mail {
    from: String,
    to: String,
    raw: Vec<u8>,
}

impl Mail {
    fn new(token: &str, tag: &str, from: &str, to: &str) -> Self {
        let subject = format!("p4-smtp {token} {tag}");
        let message_id = format!("<{token}.{tag}@dw.test>");
        let body = format!("body {token} {tag}");
        let raw = format!(
            "From: {from}\r\n\
             To: {to}\r\n\
             Subject: {subject}\r\n\
             Message-ID: {message_id}\r\n\
             Date: Thu, 01 Jan 1970 00:00:00 +0000\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             {body}\r\n"
        )
        .into_bytes();
        Self {
            from: from.to_string(),
            to: to.to_string(),
            raw,
        }
    }

    fn sendable(&self) -> Result<SendableEmail, String> {
        let from = self
            .from
            .parse()
            .map_err(|error| format!("parse From {:?}: {error:?}", self.from))?;
        let to = self
            .to
            .parse()
            .map_err(|error| format!("parse To {:?}: {error:?}", self.to))?;
        let envelope = Envelope::new(Some(from), vec![to])
            .map_err(|error| format!("Envelope::new: {error}"))?;
        Ok(SendableEmail::new(envelope, self.raw.clone()))
    }
}

async fn connect_transport(addr: SocketAddr) -> Result<Transport, String> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("TcpStream::connect {addr}: {error}"))?;
    SmtpTransport::new(SmtpClient::new(), BufStream::new(stream))
        .await
        .map_err(|error| format!("SmtpTransport::new: {error}"))
}

/// Setup is inside the scenario but outside the target future: a fresh GreenMail
/// mailbox pair, the loopback proxy with a 50 ms reply delay, a connected
/// BufStream, and one SmtpTransport. The transport is never moved into the
/// target future; only a borrow of it is.
async fn setup(env: &Env, token: &str) -> Result<Session, String> {
    let user_a = format!("{token}-a@example.com");
    let user_b = format!("{token}-b@example.com");
    create_user(env, &user_a)?;
    create_user(env, &user_b)?;

    let upstream: SocketAddr = env
        .smtp
        .parse()
        .map_err(|error| format!("parse DW_SMTP_ADDR {:?}: {error}", env.smtp))?;
    let proxy = Proxy::start_with(
        upstream,
        ProxyOptions {
            reply_delay: MESSAGE_DELAY,
        },
    )
    .await
    .map_err(|error| format!("Proxy::start_with {upstream}: {error}"))?;
    let transport = connect_transport(proxy.addr()).await?;
    println!(
        "SETUP token={token} smtp={} proxy={} upstream={upstream} transport={:p}",
        env.smtp,
        proxy.addr(),
        &transport as *const _
    );
    Ok(Session {
        transport,
        proxy,
        user_a,
        user_b,
    })
}

fn result_text(result: &SmtpResult) -> String {
    match result {
        Ok(response) => format!("Ok(code={}, message={:?})", response.code, response.message),
        Err(error) => format!("Err({error})"),
    }
}

/// Round-2 observation: the concrete code of each send after the target, not a
/// Debug string. `Timeout` and `Error` are first-class values so a timeout that
/// Dropwise would otherwise kill can still be compared with the baselines.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SendView {
    Code(u16),
    Timeout,
    Error(String),
}

impl SendView {
    fn from_result(result: &SmtpResult) -> Self {
        match result {
            Ok(response) => SendView::Code(response.code.to_string().parse().unwrap_or(0)),
            Err(error) => SendView::Error(error.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Observation2 {
    // The target result is deliberately not part of this observation: after a
    // cut it is absent, and the contract compares the survivor's effects. The
    // REST mailbox still records a late target effect.
    survivor: SendView,
    third: SendView,
    messages: Vec<MessageView>,
}

const ROUND2_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded send used only by the round-2 experiments. A dropped `send` future
/// can itself change the transport; bounding it makes that state observable
/// instead of turning the whole scenario into a 20 s liveness failure. The
/// original ladder scenarios above remain unbounded.
async fn send_bounded(transport: &mut Transport, mail: &Mail, label: &str) -> SendView {
    let started = Instant::now();
    println!(
        "ROUND2 {label} start elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let sendable = match mail.sendable() {
        Ok(sendable) => sendable,
        Err(error) => {
            println!("ROUND2 {label} build_error={error}");
            return SendView::Error(error);
        }
    };
    let view = match tokio::time::timeout(ROUND2_SEND_TIMEOUT, transport.send(sendable)).await {
        Ok(result) => {
            println!(
                "ROUND2 {label} done elapsed_ms={} result={}",
                started.elapsed().as_millis(),
                result_text(&result)
            );
            SendView::from_result(&result)
        }
        Err(_) => {
            println!(
                "ROUND2 {label} timeout elapsed_ms={}",
                started.elapsed().as_millis()
            );
            SendView::Timeout
        }
    };
    view
}

fn connection_text(stats: &[ConnectionStats]) -> String {
    let rows = stats
        .iter()
        .map(|stats| {
            format!(
                "client={} up={} down={}",
                stats.client, stats.to_upstream, stats.to_client
            )
        })
        .collect::<Vec<_>>();
    format!("connections={} {:?}", stats.len(), rows)
}

type Records = Arc<Mutex<Vec<String>>>;

fn record(records: &Records, line: String) {
    records.lock().unwrap().push(line);
}

fn print_records(records: &Records) {
    let lines = records.lock().unwrap().clone();
    for line in lines {
        println!("{line}");
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

async fn scenario_b1(env: Env, records: Records, ctx: Ctx) -> Result<(), String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw");
    let mut session = setup(&env, &token).await?;
    println!(
        "PHASE B1 setup_done token={token} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let target = Mail::new(&token, "target", &session.user_a, &session.user_a);

    let outcome = ctx.target(session.transport.send(target.sendable()?)).await;
    println!(
        "PHASE B1 target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    record(
        &records,
        format!(
            "B1-RUN target={target_text} {}",
            connection_text(&session.proxy.connections())
        ),
    );
    if let Some(Err(error)) = outcome {
        return Err(format!("target send failed before cancellation: {error}"));
    }
    // B1 stops here: no recovery, no survivor, no observer.
    Ok(())
}

async fn scenario_b2(env: Env, records: Records, ctx: Ctx) -> Result<(), String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw");
    let mut session = setup(&env, &token).await?;
    println!(
        "PHASE B2 setup_done token={token} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let mail = Mail::new(&token, "same", &session.user_a, &session.user_a);

    let outcome = ctx.target(session.transport.send(mail.sendable()?)).await;
    println!(
        "PHASE B2 target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        record(
            &records,
            format!(
                "B2-RUN target={target_text} survivor=skipped {}",
                connection_text(&session.proxy.connections())
            ),
        );
        return Err(format!("target send failed before cancellation: {error}"));
    }

    // Same arguments (same envelope and the same raw message bytes, including
    // Message-ID) on the same transport, without reset/NOOP/close/login.
    println!(
        "PHASE B2 survivor_start elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let survivor = session.transport.send(mail.sendable()?).await;
    println!(
        "PHASE B2 survivor_done elapsed_ms={} result={}",
        started.elapsed().as_millis(),
        result_text(&survivor)
    );
    let survivor_text = result_text(&survivor);
    let connections = connection_text(&session.proxy.connections());
    record(
        &records,
        format!("B2-RUN target={target_text} survivor={survivor_text} {connections}"),
    );
    match survivor {
        Ok(_) => Ok(()),
        Err(error) => Err(format!(
            "B2 survivor send on the same transport failed: {error}"
        )),
    }
}

async fn scenario_b3(env: Env, records: Records, ctx: Ctx) -> Result<(), String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw");
    let mut session = setup(&env, &token).await?;
    println!(
        "PHASE B3 setup_done token={token} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let target = Mail::new(&token, "target", &session.user_a, &session.user_a);
    // Different subject tag, Message-ID, recipient and body from the target.
    let survivor_mail = Mail::new(&token, "different", &session.user_a, &session.user_b);

    let outcome = ctx.target(session.transport.send(target.sendable()?)).await;
    println!(
        "PHASE B3 target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        record(
            &records,
            format!(
                "B3-RUN target={target_text} survivor=skipped {}",
                connection_text(&session.proxy.connections())
            ),
        );
        return Err(format!("target send failed before cancellation: {error}"));
    }

    println!(
        "PHASE B3 survivor_start elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let survivor = session.transport.send(survivor_mail.sendable()?).await;
    let survivor_text = result_text(&survivor);
    println!(
        "PHASE B3 survivor_done elapsed_ms={} result={survivor_text}",
        started.elapsed().as_millis()
    );
    let connections = connection_text(&session.proxy.connections());
    record(
        &records,
        format!(
            "B3-RUN target={target_text} survivor={survivor_text} recipient={} {connections}",
            session.user_b
        ),
    );
    match survivor {
        Ok(_) => Ok(()),
        Err(error) => Err(format!(
            "B3 survivor send on the same transport failed: {error}"
        )),
    }
}

async fn scenario_m1(env: Env, records: Records, ctx: Ctx) -> Result<Observation, String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw");
    let mut session = setup(&env, &token).await?;
    println!(
        "PHASE M1 setup_done token={token} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let target = Mail::new(&token, "target", &session.user_a, &session.user_a);
    let survivor_mail = Mail::new(&token, "survivor", &session.user_a, &session.user_a);

    let outcome = ctx.target(session.transport.send(target.sendable()?)).await;
    println!(
        "PHASE M1 target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        record(
            &records,
            format!(
                "M1-RUN target={target_text} survivor=skipped {}",
                connection_text(&session.proxy.connections())
            ),
        );
        return Err(format!("target send failed before cancellation: {error}"));
    }

    println!(
        "PHASE M1 survivor_start elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let survivor = session.transport.send(survivor_mail.sendable()?).await;
    let survivor_text = result_text(&survivor);
    println!(
        "PHASE M1 survivor_done elapsed_ms={} result={survivor_text}",
        started.elapsed().as_millis()
    );
    let connections = connection_text(&session.proxy.connections());
    record(
        &records,
        format!("M1-RUN target={target_text} survivor={survivor_text} {connections}"),
    );
    match survivor {
        Ok(_) => {}
        Err(error) => {
            return Err(format!(
                "M1 survivor send on the same transport failed: {error}"
            ))
        }
    }

    let observation = observe(&env, &session.user_a, &token)?;
    println!(
        "PHASE M1 observe_done elapsed_ms={}",
        started.elapsed().as_millis()
    );
    record(
        &records,
        format!(
            "M1-OBS mailbox={} observation={observation:?}",
            session.user_a
        ),
    );
    Ok(observation)
}

// ---------------------------------------------------------------------------
// Round 2: M1 with codes + third send, timeout diagnosis, recovery contrast
// ---------------------------------------------------------------------------

async fn scenario_m1_round2(env: Env, records: Records, ctx: Ctx) -> Result<Observation2, String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw-r2");
    let mut session = setup(&env, &token).await?;
    println!(
        "PHASE M1R2 setup_done token={token} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    let target = Mail::new(&token, "target", &session.user_a, &session.user_a);
    let survivor_mail = Mail::new(&token, "survivor", &session.user_a, &session.user_a);
    let third_mail = Mail::new(&token, "third", &session.user_a, &session.user_a);

    let outcome = ctx.target(session.transport.send(target.sendable()?)).await;
    println!(
        "PHASE M1R2 target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        record(
            &records,
            format!(
                "M1R2-RUN target={target_text} survivor=skipped third=skipped {}",
                connection_text(&session.proxy.connections())
            ),
        );
        return Err(format!("target send failed before cancellation: {error}"));
    }

    let survivor = send_bounded(&mut session.transport, &survivor_mail, "M1R2 survivor").await;
    let third = send_bounded(&mut session.transport, &third_mail, "M1R2 third").await;
    let connections = connection_text(&session.proxy.connections());
    let mailbox = observe(&env, &session.user_a, &token)?;
    let observation = Observation2 {
        survivor: survivor.clone(),
        third: third.clone(),
        messages: mailbox.messages.clone(),
    };
    record(
        &records,
        format!(
            "M1R2-RUN target={target_text} survivor={survivor:?} third={third:?} mailbox={mailbox:?} {connections}"
        ),
    );
    record(
        &records,
        format!(
            "M1R2-OBS mailbox={} observation={observation:?}",
            session.user_a
        ),
    );
    println!(
        "PHASE M1R2 observe_done elapsed_ms={}",
        started.elapsed().as_millis()
    );
    Ok(observation)
}

async fn scenario_diag(env: Env, records: Records, ctx: Ctx) -> Result<(), String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw-diag");
    let mut session = setup(&env, &token).await?;
    let target = Mail::new(&token, "target", &session.user_a, &session.user_a);
    let survivor_mail = Mail::new(&token, "survivor", &session.user_a, &session.user_a);
    let third_mail = Mail::new(&token, "third", &session.user_a, &session.user_a);
    let outcome = ctx.target(session.transport.send(target.sendable()?)).await;
    println!(
        "DIAG setup_token={token} target_cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        return Err(format!("target send failed before cancellation: {error}"));
    }
    let survivor = send_bounded(&mut session.transport, &survivor_mail, "DIAG survivor").await;
    let third = send_bounded(&mut session.transport, &third_mail, "DIAG third").await;
    let mailbox = observe(&env, &session.user_a, &token)?;
    record(
        &records,
        format!(
            "DIAG-RUN target={target_text} survivor={survivor:?} third={third:?} mailbox={mailbox:?} {}",
            connection_text(&session.proxy.connections())
        ),
    );
    Ok(())
}

/// The fixed-dependency contrast uses the same target cut, then:
/// second and third sends on the old transport (both bounded), drop/reconnect,
/// and a recovery send on a fresh transport. This separates "explicitly refused
/// stale transport" from "silently returned 354 / hung".
async fn scenario_poison(env: Env, records: Records, ctx: Ctx) -> Result<(), String> {
    let started = Instant::now();
    let token = dropwise::support::unique("dw-pois");
    let Session {
        mut transport,
        proxy,
        user_a,
        user_b: _,
    } = setup(&env, &token).await?;
    let target = Mail::new(&token, "target", &user_a, &user_a);
    let second_mail = Mail::new(&token, "survivor", &user_a, &user_a);
    let third_mail = Mail::new(&token, "third", &user_a, &user_a);
    let recovery_mail = Mail::new(&token, "recovery", &user_a, &user_a);

    let outcome = ctx.target(transport.send(target.sendable()?)).await;
    println!(
        "PHASE POISON target_done cancelled={} elapsed_ms={}",
        outcome.is_none(),
        started.elapsed().as_millis()
    );
    let target_text = match &outcome {
        None => "cancelled".to_string(),
        Some(result) => result_text(result),
    };
    if let Some(Err(error)) = &outcome {
        return Err(format!("target send failed before cancellation: {error}"));
    }

    let second = send_bounded(&mut transport, &second_mail, "POISON second old").await;
    let third = send_bounded(&mut transport, &third_mail, "POISON third old").await;
    let old_connections = connection_text(&proxy.connections());
    drop(transport);

    let mut fresh = connect_transport(proxy.addr()).await?;
    let recovery = send_bounded(&mut fresh, &recovery_mail, "POISON recovery fresh").await;
    let mailbox = observe(&env, &user_a, &token)?;
    let connections = connection_text(&proxy.connections());
    record(
        &records,
        format!(
            "POISON-RUN target={target_text} second={second:?} third={third:?} recovery={recovery:?} mailbox={mailbox:?} old_{old_connections} after_{connections}"
        ),
    );
    if recovery != SendView::Code(250) {
        return Err(format!(
            "recovery on a fresh transport failed: {recovery:?}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config shared by all four levels
// ---------------------------------------------------------------------------

fn config() -> Config {
    Config {
        max_runs: 64,
        max_cancellations: 1,
        races: vec![Race::Immediate, Race::AfterWake],
        repeat_at_site: false,
        gap_choices: Vec::new(),
        flavor: Flavor::MultiThread { workers: 1 },
        // The requested Settle::ZERO does not exist in Dropwise 0.1.0; this is
        // the same thing spelled as an explicit zero-tokio-time/zero-real-time
        // Settle. The watchdog only bounds a stuck settle phase.
        settle: Settle {
            tokio_time: Duration::ZERO,
            real_time: Duration::ZERO,
            watchdog: Duration::from_secs(10),
        },
        scenario_timeout: Duration::from_secs(20),
    }
}

// ---------------------------------------------------------------------------
// Level runner
// ---------------------------------------------------------------------------

fn report_text(report: &Report) {
    println!("----- report display -----");
    println!("{report}");
    println!("----- report diagnostics -----");
    println!("{}", report.diagnostics());
}

fn failed(report: &Report) -> bool {
    !report.baseline_errors.is_empty() || report.violations().next().is_some()
}

fn run_level(env: Env, level: &str, rep: u32) -> Result<(), String> {
    let config = config();
    let records: Records = Arc::default();
    let id = format!("rep{rep}");
    println!(
        "LEVEL start level={level} rep={rep} flavor=MultiThread workers=1 races=Immediate+AfterWake \
         max_runs=64 settle=ZERO timeout=20s reply_delay=50ms"
    );

    let clean = match level {
        "B1" => {
            let env = env.clone();
            let records_for_closure = Arc::clone(&records);
            let report = explore(&config, move |ctx| {
                let env = env.clone();
                let records = Arc::clone(&records_for_closure);
                async move { scenario_b1(env, records, ctx).await }
            });
            report_text(&report);
            print_records(&records);
            ladder::simple(level, &id, &report);
            !failed(&report)
        }
        "B2" => {
            let env = env.clone();
            let records_for_closure = Arc::clone(&records);
            let report = explore(&config, move |ctx| {
                let env = env.clone();
                let records = Arc::clone(&records_for_closure);
                async move { scenario_b2(env, records, ctx).await }
            });
            report_text(&report);
            print_records(&records);
            ladder::simple(level, &id, &report);
            !failed(&report)
        }
        "B3" => {
            let env = env.clone();
            let records_for_closure = Arc::clone(&records);
            let report = explore(&config, move |ctx| {
                let env = env.clone();
                let records = Arc::clone(&records_for_closure);
                async move { scenario_b3(env, records, ctx).await }
            });
            report_text(&report);
            print_records(&records);
            ladder::simple(level, &id, &report);
            !failed(&report)
        }
        "M1" => {
            let env = env.clone();
            let records_for_closure = Arc::clone(&records);
            let report = explore_observed(&config, Expect::Atomic, move |ctx| {
                let env = env.clone();
                let records = Arc::clone(&records_for_closure);
                async move { scenario_m1(env, records, ctx).await }
            });
            report_text(&report);
            print_records(&records);
            ladder::observed(level, &id, &report);
            !failed(&report)
        }
        other => {
            return Err(format!(
                "unknown level {other:?}; expected one of {LEVELS:?}"
            ))
        }
    };

    if clean {
        Ok(())
    } else {
        Err(format!(
            "level {level} rep {rep} produced a baseline error or violation"
        ))
    }
}

// ---------------------------------------------------------------------------
// Pre-ladder evidence: no cancellation, reuse/identity, pre/post calibration
// ---------------------------------------------------------------------------

fn expected_subjects(tags: &[&str]) -> Vec<String> {
    let mut subjects = tags
        .iter()
        .map(|tag| format!("p4-smtp <run> {tag}"))
        .collect::<Vec<_>>();
    subjects.sort();
    subjects
}

fn check_observation(label: &str, observation: &Observation, tags: &[&str]) -> Result<(), String> {
    let mut expected = expected_subjects(tags);
    expected.sort();
    let mut actual = observation
        .messages
        .iter()
        .map(|message| message.subject.clone())
        .collect::<Vec<_>>();
    actual.sort();
    if observation.count != expected.len() || actual != expected {
        return Err(format!(
            "{label}: expected {} messages with subjects {expected:?}, got {observation:?}",
            expected.len()
        ));
    }
    Ok(())
}

async fn prep_nocancel(env: Env) -> Result<(), String> {
    let token = dropwise::support::unique("prep-nocancel");
    let mut session = setup(&env, &token).await?;
    let first = Mail::new(&token, "nocancel-one", &session.user_a, &session.user_a);
    let second = Mail::new(&token, "nocancel-two", &session.user_a, &session.user_a);
    let transport_ptr = &session.transport as *const _;

    let first_result = session.transport.send(first.sendable()?).await;
    println!("PREP-NOCANCEL first={}", result_text(&first_result));
    let second_result = session.transport.send(second.sendable()?).await;
    println!("PREP-NOCANCEL second={}", result_text(&second_result));
    println!(
        "PREP-NOCANCEL transport_ptr={transport_ptr:p} same_object={} {}",
        transport_ptr == &session.transport as *const _,
        connection_text(&session.proxy.connections())
    );

    let observation = observe(&env, &session.user_a, &token)?;
    println!("PREP-NOCANCEL observation={observation:?}");
    check_observation(
        "PREP-NOCANCEL",
        &observation,
        &["nocancel-one", "nocancel-two"],
    )?;
    if !first_result.is_ok() || !second_result.is_ok() {
        return Err("PREP-NOCANCEL: a send returned Err".to_string());
    }
    Ok(())
}

async fn prep_reuse(env: Env) -> Result<(), String> {
    let token = dropwise::support::unique("prep-reuse");
    let mut session = setup(&env, &token).await?;
    let first = Mail::new(&token, "reuse-one", &session.user_a, &session.user_a);
    let second = Mail::new(&token, "reuse-two", &session.user_a, &session.user_a);
    let transport_ptr = &session.transport as *const _;
    let before = session.proxy.connections();

    let first_result = session.transport.send(first.sendable()?).await;
    let after_first = session.proxy.connections();
    let second_result = session.transport.send(second.sendable()?).await;
    let after_second = session.proxy.connections();

    println!("PREP-REUSE first={}", result_text(&first_result));
    println!("PREP-REUSE second={}", result_text(&second_result));
    println!(
        "PREP-REUSE transport_ptr={transport_ptr:p} same_object={}",
        transport_ptr == &session.transport as *const _
    );
    println!("PREP-REUSE before={}", connection_text(&before));
    println!("PREP-REUSE after_first={}", connection_text(&after_first));
    println!("PREP-REUSE after_second={}", connection_text(&after_second));

    let same_client = after_second.len() == 1
        && after_first.len() == 1
        && after_second.first().map(|stats| stats.client)
            == after_first.first().map(|stats| stats.client);
    let one_connection = after_second.len() == 1 && before.len() <= 1;
    let bytes_grew = after_second.first().map(|stats| stats.to_upstream)
        > after_first.first().map(|stats| stats.to_upstream);
    println!(
        "PREP-REUSE same_tcp_connection={same_client} one_connection={one_connection} \
         bytes_grew={bytes_grew}"
    );

    let observation = observe(&env, &session.user_a, &token)?;
    println!("PREP-REUSE observation={observation:?}");
    check_observation("PREP-REUSE", &observation, &["reuse-one", "reuse-two"])?;
    if !same_client || !one_connection || !bytes_grew {
        return Err("PREP-REUSE: the two sends did not reuse one TCP connection".to_string());
    }
    Ok(())
}

async fn prep_calibration(env: Env) -> Result<(), String> {
    // pre: the target is never started (the same shape Dropwise uses for its
    // pre calibration); the survivor sends one message.
    let pre_token = dropwise::support::unique("prep-pre");
    let mut pre_session = setup(&env, &pre_token).await?;
    let pre_survivor = Mail::new(
        &pre_token,
        "survivor",
        &pre_session.user_a,
        &pre_session.user_a,
    );
    let pre_send = pre_session.transport.send(pre_survivor.sendable()?).await;
    println!("PREP-CALIBRATION pre_survivor={}", result_text(&pre_send));
    let pre = observe(&env, &pre_session.user_a, &pre_token)?;
    println!("PREP-CALIBRATION pre={pre:?}");
    check_observation("PREP-CALIBRATION pre", &pre, &["survivor"])?;

    // post: target then survivor on the same transport.
    let post_token = dropwise::support::unique("prep-post");
    let mut post_session = setup(&env, &post_token).await?;
    let post_target = Mail::new(
        &post_token,
        "target",
        &post_session.user_a,
        &post_session.user_a,
    );
    let post_survivor = Mail::new(
        &post_token,
        "survivor",
        &post_session.user_a,
        &post_session.user_a,
    );
    let post_target_send = post_session.transport.send(post_target.sendable()?).await;
    println!(
        "PREP-CALIBRATION post_target={}",
        result_text(&post_target_send)
    );
    let post_survivor_send = post_session.transport.send(post_survivor.sendable()?).await;
    println!(
        "PREP-CALIBRATION post_survivor={}",
        result_text(&post_survivor_send)
    );
    let post = observe(&env, &post_session.user_a, &post_token)?;
    println!("PREP-CALIBRATION post={post:?}");
    check_observation("PREP-CALIBRATION post", &post, &["target", "survivor"])?;

    let distinguishable = pre != post;
    println!(
        "PREP-CALIBRATION distinguishable={distinguishable} pre_count={} post_count={}",
        pre.count, post.count
    );
    if !distinguishable {
        return Err("PREP-CALIBRATION: pre and post observations are equal".to_string());
    }
    if !post_target_send.is_ok() || !post_survivor_send.is_ok() {
        return Err("PREP-CALIBRATION: a post send returned Err".to_string());
    }
    Ok(())
}

fn check_round2_expected(
    label: &str,
    observation: &Observation,
    tags: &[&str],
) -> Result<(), String> {
    let mut expected = expected_subjects(tags);
    expected.sort();
    let mut actual = observation
        .messages
        .iter()
        .map(|message| message.subject.clone())
        .collect::<Vec<_>>();
    actual.sort();
    if observation.count != expected.len() || actual != expected {
        return Err(format!(
            "{label}: expected {} messages with subjects {expected:?}, got {observation:?}",
            expected.len()
        ));
    }
    Ok(())
}

/// Explicit pre/post evidence for the round-2 observation. The pre side never
/// starts the target; the post side starts and completes it, then sends the
/// survivor and third messages. Both sides must use the same 2 s bound as M1R2.
async fn prep_round2(env: Env) -> Result<(), String> {
    let pre_token = dropwise::support::unique("prep-r2-pre");
    let mut pre_session = setup(&env, &pre_token).await?;
    let pre_survivor = Mail::new(
        &pre_token,
        "survivor",
        &pre_session.user_a,
        &pre_session.user_a,
    );
    let pre_third = Mail::new(
        &pre_token,
        "third",
        &pre_session.user_a,
        &pre_session.user_a,
    );
    let pre_survivor_view = send_bounded(
        &mut pre_session.transport,
        &pre_survivor,
        "R2-CAL pre survivor",
    )
    .await;
    let pre_third_view =
        send_bounded(&mut pre_session.transport, &pre_third, "R2-CAL pre third").await;
    let pre_observation = observe(&env, &pre_session.user_a, &pre_token)?;
    check_round2_expected("R2-CAL pre", &pre_observation, &["survivor", "third"])?;
    if pre_survivor_view != SendView::Code(250) || pre_third_view != SendView::Code(250) {
        return Err(format!(
            "R2-CAL pre: expected 250/250, got survivor={pre_survivor_view:?} third={pre_third_view:?}"
        ));
    }
    println!(
        "ROUND2-CAL pre survivor={pre_survivor_view:?} third={pre_third_view:?} observation={pre_observation:?}"
    );

    let post_token = dropwise::support::unique("prep-r2-post");
    let mut post_session = setup(&env, &post_token).await?;
    let post_target = Mail::new(
        &post_token,
        "target",
        &post_session.user_a,
        &post_session.user_a,
    );
    let post_survivor = Mail::new(
        &post_token,
        "survivor",
        &post_session.user_a,
        &post_session.user_a,
    );
    let post_third = Mail::new(
        &post_token,
        "third",
        &post_session.user_a,
        &post_session.user_a,
    );
    let post_target_view = send_bounded(
        &mut post_session.transport,
        &post_target,
        "R2-CAL post target",
    )
    .await;
    let post_survivor_view = send_bounded(
        &mut post_session.transport,
        &post_survivor,
        "R2-CAL post survivor",
    )
    .await;
    let post_third_view = send_bounded(
        &mut post_session.transport,
        &post_third,
        "R2-CAL post third",
    )
    .await;
    let post_observation = observe(&env, &post_session.user_a, &post_token)?;
    check_round2_expected(
        "R2-CAL post",
        &post_observation,
        &["target", "survivor", "third"],
    )?;
    let expected = SendView::Code(250);
    if post_target_view != expected || post_survivor_view != expected || post_third_view != expected
    {
        return Err(format!(
            "R2-CAL post: expected three 250 codes, got target={post_target_view:?} survivor={post_survivor_view:?} third={post_third_view:?}"
        ));
    }
    println!(
        "ROUND2-CAL post target={post_target_view:?} survivor={post_survivor_view:?} third={post_third_view:?} observation={post_observation:?}"
    );
    println!(
        "ROUND2-CAL distinguishable=true pre_count={} post_count={}",
        pre_observation.count, post_observation.count
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Round-2 runners
// ---------------------------------------------------------------------------

fn run_round2_m1(env: Env, rep: u32) -> Result<(), String> {
    let config = config();
    let records: Records = Arc::default();
    println!(
        "ROUND2-M1 start rep={rep} flavor=MultiThread workers=1 races=Immediate+AfterWake \
         max_runs=64 settle=ZERO timeout=20s send_bound=2s reply_delay=50ms"
    );
    let env_for_closure = env.clone();
    let records_for_closure = Arc::clone(&records);
    let report = explore_observed(&config, Expect::Atomic, move |ctx| {
        let env = env_for_closure.clone();
        let records = Arc::clone(&records_for_closure);
        async move { scenario_m1_round2(env, records, ctx).await }
    });
    report_text(&report);
    print_records(&records);
    ladder::observed("M1R2", &format!("rep{rep}"), &report);
    if failed(&report) {
        Err("round2 M1 produced a baseline error or violation".to_string())
    } else {
        Ok(())
    }
}

fn run_round2_diag(env: Env) -> Result<(), String> {
    for (race, label) in [
        (Race::Immediate, "Immediate"),
        (Race::AfterWake, "AfterWake"),
    ] {
        let plan = Plan::new(vec![Cut {
            target: 0,
            after_pending: 3,
            race,
        }]);
        let records: Records = Arc::default();
        println!("ROUND2-DIAG plan=#0@3:{label} send_bound=2s");
        let env_for_closure = env.clone();
        let records_for_closure = Arc::clone(&records);
        let report = replay(&config(), &plan, move |ctx| {
            let env = env_for_closure.clone();
            let records = Arc::clone(&records_for_closure);
            async move { scenario_diag(env, records, ctx).await }
        })
        .map_err(|error| format!("replay #0@3:{label} rejected: {error}"))?;
        println!("ROUND2-DIAG report=#0@3:{label}");
        report_text(&report);
        print_records(&records);
        let realized = report.trials.iter().all(|trial| trial.is_realized());
        println!(
            "ROUND2-DIAG realized=#0@3:{label}:{realized} trials={}",
            report.trials.len()
        );
    }
    Ok(())
}

fn run_round2_poison(env: Env, rep: u32) -> Result<(), String> {
    let records: Records = Arc::default();
    println!("ROUND2-POISON start rep={rep} all generated cuts send_bound=2s");
    let env_for_closure = env.clone();
    let records_for_closure = Arc::clone(&records);
    let report = explore(&config(), move |ctx| {
        let env = env_for_closure.clone();
        let records = Arc::clone(&records_for_closure);
        async move { scenario_poison(env, records, ctx).await }
    });
    report_text(&report);
    print_records(&records);
    println!(
        "ROUND2-POISON summary rep={rep} plans={} violations={} unrealized={} baseline_errors={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.baseline_errors.len()
    );
    if failed(&report) {
        Err("round2 poison contrast had a baseline error or violation".to_string())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build prep runtime")
        .block_on(future)
}

fn usage() -> String {
    "usage: levels nocancel | reuse | calibration | round2-calibration | round2-diagnose | \
     level <B1|B2|B3|M1> <rep> | round2-m1 <rep> | round2-poison <rep>"
        .to_string()
}

fn run(args: &[String]) -> Result<(), String> {
    let _ = env_logger::try_init();
    let env = Env::from_process()?;
    match args {
        [command] => match command.as_str() {
            "nocancel" => block_on(prep_nocancel(env)),
            "reuse" => block_on(prep_reuse(env)),
            "calibration" => block_on(prep_calibration(env)),
            "round2-calibration" => block_on(prep_round2(env)),
            "round2-diagnose" => run_round2_diag(env),
            _ => Err(usage()),
        },
        [command, level, rep] if command == "level" => {
            if !LEVELS.contains(&level.as_str()) {
                return Err(format!(
                    "unknown level {level:?}; expected one of {LEVELS:?}"
                ));
            }
            let rep: u32 = rep
                .parse()
                .map_err(|error| format!("parse rep {rep:?}: {error}"))?;
            run_level(env, level, rep)
        }
        [command, rep] if command == "round2-m1" || command == "round2-poison" => {
            let rep: u32 = rep
                .parse()
                .map_err(|error| format!("parse rep {rep:?}: {error}"))?;
            match command.as_str() {
                "round2-m1" => run_round2_m1(env, rep),
                "round2-poison" => run_round2_poison(env, rep),
                _ => unreachable!(),
            }
        }
        _ => Err(usage()),
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&args) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("ERROR: {error}");
            std::process::exit(1);
        }
    }
}

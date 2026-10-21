//! Phase 4 exploration: async-imap 0.12.0 direct Session, cancelled SELECT INBOX.
//!
//! Scenario, Pending mechanism and level definitions are preregistered in
//! `SCENARIO.md` next to this crate. This binary has two phases:
//!   imap-explore preflight   no-cancel, reuse/identity, manual pre/post calibration
//!   imap-explore ladder N    B1, B2, B3, M1 once, for repetition N
//!
//! The real crate is used unchanged. The only network intermediary is
//! `dropwise::net::Proxy` with a 50 ms server->client reply delay; the SMTP seed
//! and the GreenMail REST observer are direct and bypass both the proxy and
//! async-imap.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_imap::error::Error as ImapError;
use async_imap::types::{Fetch, Seq};
use async_imap::{Client, Session};
use dropwise::net::{ConnectionStats, Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Report, Settle};
use futures_util::TryStreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const OP_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_DELAY: Duration = Duration::from_millis(50);
const IMAP_USER: &str = "imapuser";
const IMAP_PASS: &str = "pass";
const MAILBOX: &str = "INBOX";
const SEED_BODY: &str = "p4-imap-seed-body";
const SURVIVOR_BODY: &str = "p4-imap-survivor-body";

#[derive(Debug, Clone, Copy)]
struct Ports {
    smtp: u16,
    imap: u16,
    rest: u16,
}

impl Ports {
    fn from_env() -> Result<Self, String> {
        fn one(name: &str) -> Result<u16, String> {
            std::env::var(name)
                .map_err(|_| format!("missing environment variable {name}"))?
                .parse()
                .map_err(|e| format!("bad {name}: {e}"))
        }
        Ok(Self {
            smtp: one("P4_SMTP_PORT")?,
            imap: one("P4_IMAP_PORT")?,
            rest: one("P4_REST_PORT")?,
        })
    }
}

fn imap_error_class(e: &ImapError) -> String {
    match e {
        ImapError::Io(_) => "Io".into(),
        ImapError::Bad(_) => "Bad".into(),
        ImapError::No(_) => "No".into(),
        ImapError::ConnectionLost => "ConnectionLost".into(),
        ImapError::Parse(_) => "Parse".into(),
        ImapError::Validate(_) => "Validate".into(),
        ImapError::Append => "Append".into(),
        _ => "Other".into(),
    }
}

fn mailbox_summary(m: &async_imap::types::Mailbox) -> String {
    format!(
        "exists={} recent={} unseen={:?} uid_next={:?} uid_validity={:?} flags={:?}",
        m.exists, m.recent, m.unseen, m.uid_next, m.uid_validity, m.flags
    )
}

fn message(subject: &str, body: &str, message_id: &str) -> String {
    format!(
        "From: seed@example.com\r\n\
         To: {IMAP_USER}@example.com\r\n\
         Subject: {subject}\r\n\
         Message-ID: <{message_id}>\r\n\
         Date: Tue, 20 Oct 2026 00:00:00 +0000\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=us-ascii\r\n\
         \r\n\
         {body}\r\n"
    )
}

// ---------------------------------------------------------------------------
// Independent writer (SMTP) and observer (GreenMail REST). Both are direct;
// neither goes through the proxy or async-imap.
// ---------------------------------------------------------------------------

async fn smtp_reply(reader: &mut BufReader<TcpStream>, expected: u16) -> Result<(), String> {
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("smtp read: {e}"))?;
        if line.is_empty() {
            return Err("smtp connection closed".into());
        }
        let code: u16 = line
            .get(..3)
            .ok_or_else(|| format!("short smtp line {line:?}"))?
            .parse()
            .map_err(|_| format!("bad smtp line {line:?}"))?;
        let more = line.as_bytes().get(3) == Some(&b'-');
        if code != expected {
            return Err(format!("smtp expected {expected}, got {line:?}"));
        }
        if !more {
            return Ok(());
        }
    }
}

async fn smtp_seed(
    ports: Ports,
    subject: &str,
    body: &str,
    message_id: &str,
) -> Result<(), String> {
    let stream = TcpStream::connect(("127.0.0.1", ports.smtp))
        .await
        .map_err(|e| format!("smtp connect: {e}"))?;
    let mut reader = BufReader::new(stream);
    smtp_reply(&mut reader, 220).await?;
    let w = reader.get_mut();
    w.write_all(b"EHLO p4imap.local\r\n")
        .await
        .map_err(|e| format!("smtp EHLO: {e}"))?;
    smtp_reply(&mut reader, 250).await?;
    let w = reader.get_mut();
    w.write_all(b"MAIL FROM:<seed@example.com>\r\n")
        .await
        .map_err(|e| format!("smtp MAIL: {e}"))?;
    smtp_reply(&mut reader, 250).await?;
    let w = reader.get_mut();
    w.write_all(format!("RCPT TO:<{IMAP_USER}@example.com>\r\n").as_bytes())
        .await
        .map_err(|e| format!("smtp RCPT: {e}"))?;
    smtp_reply(&mut reader, 250).await?;
    let w = reader.get_mut();
    w.write_all(b"DATA\r\n")
        .await
        .map_err(|e| format!("smtp DATA: {e}"))?;
    smtp_reply(&mut reader, 354).await?;
    let text = message(subject, body, message_id);
    let w = reader.get_mut();
    w.write_all(text.as_bytes())
        .await
        .map_err(|e| format!("smtp body: {e}"))?;
    w.write_all(b".\r\n")
        .await
        .map_err(|e| format!("smtp terminator: {e}"))?;
    smtp_reply(&mut reader, 250).await?;
    let w = reader.get_mut();
    w.write_all(b"QUIT\r\n")
        .await
        .map_err(|e| format!("smtp QUIT: {e}"))?;
    let _ = smtp_reply(&mut reader, 221).await;
    Ok(())
}

#[derive(Debug, Clone)]
struct RestMessage {
    #[allow(dead_code)]
    uid: String,
    subject: String,
    mime: String,
}

async fn rest_messages(ports: Ports) -> Result<Vec<RestMessage>, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", ports.rest))
        .await
        .map_err(|_| "observer-connect".to_string())?;
    let request = format!(
        "GET /api/user/{IMAP_USER}@example.com/messages HTTP/1.0\r\nHost: 127.0.0.1:{}\r\n\r\n",
        ports.rest
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "observer-io".to_string())?;
    let mut buf = Vec::new();
    match tokio::time::timeout(OP_TIMEOUT, stream.read_to_end(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return Err("observer-io".into()),
        Err(_) => return Err("observer-timeout".into()),
    }
    let sep = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "observer-http".to_string())?;
    let head = String::from_utf8_lossy(&buf[..sep]);
    if !head.starts_with("HTTP/1.0 200") && !head.starts_with("HTTP/1.1 200") {
        return Err("observer-http".into());
    }
    let mut body = &buf[sep + 4..];
    let content_length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    if let Some(length) = content_length {
        body = body.get(..length).ok_or_else(|| "observer-http".to_string())?;
    }
    let values: Vec<Value> = serde_json::from_slice(body).map_err(|_| "observer-json".to_string())?;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(RestMessage {
            uid: value
                .get("uid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            subject: value
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            mime: value
                .get("mimeMessage")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObserverView {
    run_subjects: Vec<String>,
    run_message_count: usize,
    seed_body: bool,
    survivor_body: bool,
}

fn observer_view(msgs: &[RestMessage], run_id: &str, survivor_subject: &str) -> ObserverView {
    let prefix = format!("s-{run_id}-");
    let seed_subject = format!("s-{run_id}-seed");
    let mut run_subjects = Vec::new();
    let mut seed_body = false;
    let mut survivor_body = false;
    for msg in msgs {
        if msg.subject.starts_with(&prefix) {
            run_subjects.push(msg.subject.replace(run_id, "RUN"));
            if msg.subject == seed_subject && msg.mime.contains(SEED_BODY) {
                seed_body = true;
            }
            if msg.subject == survivor_subject && msg.mime.contains(SURVIVOR_BODY) {
                survivor_body = true;
            }
        }
    }
    run_subjects.sort();
    ObserverView {
        run_message_count: run_subjects.len(),
        run_subjects,
        seed_body,
        survivor_body,
    }
}

async fn observe(
    ports: Ports,
    run_id: &str,
    survivor_subject: &str,
) -> (Result<ObserverView, String>, String) {
    match rest_messages(ports).await {
        Ok(msgs) => {
            let total = msgs.len();
            let view = observer_view(&msgs, run_id, survivor_subject);
            let raw = format!(
                "rest_total={total} run_count={} subjects={:?} seed_body={} survivor_body={}",
                view.run_message_count, view.run_subjects, view.seed_body, view.survivor_body
            );
            (Ok(view), raw)
        }
        Err(e) => (Err(e.clone()), format!("rest error {e}")),
    }
}

async fn observe_subject_count(ports: Ports, subject: &str) -> (Result<usize, String>, String) {
    match rest_messages(ports).await {
        Ok(msgs) => {
            let count = msgs.iter().filter(|m| m.subject == subject).count();
            (Ok(count), format!("rest_total={} subject={subject:?} count={count}", msgs.len()))
        }
        Err(e) => (Err(e.clone()), format!("rest error {e}")),
    }
}

// ---------------------------------------------------------------------------
// Session setup and operations. The Session is created in setup, outside the
// target future, and lives in a Box so its address is stable across awaits.
// ---------------------------------------------------------------------------

struct Env {
    run_id: String,
    seed_subject: String,
    proxy: Proxy,
    session: Box<Session<TcpStream>>,
}

async fn setup(ports: Ports) -> Result<Env, String> {
    let run_id = dropwise::support::unique("p4imap");
    let seed_subject = format!("s-{run_id}-seed");
    let seed_id = format!("{run_id}-seed@example.com");
    tokio::time::timeout(OP_TIMEOUT, smtp_seed(ports, &seed_subject, SEED_BODY, &seed_id))
        .await
        .map_err(|_| "smtp seed timed out".to_string())??;

    let upstream: SocketAddr = format!("127.0.0.1:{}", ports.imap)
        .parse()
        .map_err(|e| format!("bad upstream address: {e}"))?;
    let proxy = Proxy::start_with(upstream, ProxyOptions { reply_delay: REPLY_DELAY })
        .await
        .map_err(|e| format!("proxy start: {e}"))?;
    let stream = tokio::time::timeout(OP_TIMEOUT, TcpStream::connect(proxy.addr()))
        .await
        .map_err(|_| "imap connect timed out".to_string())?
        .map_err(|e| format!("imap connect: {e}"))?;
    let mut client = Client::new(stream);
    let greeting = tokio::time::timeout(OP_TIMEOUT, client.read_response())
        .await
        .map_err(|_| "greeting timed out".to_string())?
        .map_err(|e| format!("greeting: {e}"))?;
    if greeting.is_none() {
        return Err("imap server closed before greeting".into());
    }
    let session = match tokio::time::timeout(OP_TIMEOUT, client.login(IMAP_USER, IMAP_PASS)).await {
        Ok(Ok(session)) => session,
        Ok(Err((e, _client))) => return Err(format!("login failed: {e}")),
        Err(_) => return Err("login timed out".into()),
    };
    Ok(Env {
        run_id,
        seed_subject,
        proxy,
        session: Box::new(session),
    })
}

async fn select_inbox(session: &mut Session<TcpStream>) -> (Result<String, String>, String) {
    match tokio::time::timeout(OP_TIMEOUT, session.select(MAILBOX)).await {
        Ok(Ok(mailbox)) => (Ok(mailbox_summary(&mailbox)), "Ok".into()),
        Ok(Err(e)) => (Err(imap_error_class(&e)), format!("{e}")),
        Err(_) => (Err("Timeout".into()), "select timed out".into()),
    }
}

async fn append_message(
    session: &mut Session<TcpStream>,
    subject: &str,
    body: &str,
    message_id: &str,
) -> (Result<(), String>, String) {
    let content = message(subject, body, message_id);
    match tokio::time::timeout(OP_TIMEOUT, session.append(MAILBOX, None, None, content)).await {
        Ok(Ok(())) => (Ok(()), "Ok".into()),
        Ok(Err(e)) => (Err(imap_error_class(&e)), format!("{e}")),
        Err(_) => (Err("Timeout".into()), "append timed out".into()),
    }
}

async fn search_subject(
    session: &mut Session<TcpStream>,
    subject: &str,
) -> (Result<usize, String>, Result<Vec<Seq>, String>, String) {
    let query = format!("SUBJECT \"{subject}\"");
    match tokio::time::timeout(OP_TIMEOUT, session.search(query)).await {
        Ok(Ok(set)) => {
            let mut seqs: Vec<Seq> = set.into_iter().collect();
            seqs.sort_by_key(|seq| seq.to_string());
            let raw = seqs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            (Ok(seqs.len()), Ok(seqs), format!("seqs=[{raw}]"))
        }
        Ok(Err(e)) => (
            Err(imap_error_class(&e)),
            Err(imap_error_class(&e)),
            format!("{e}"),
        ),
        Err(_) => (
            Err("Timeout".into()),
            Err("Timeout".into()),
            "search timed out".into(),
        ),
    }
}

async fn fetch_from(
    session: &mut Session<TcpStream>,
    seqs: &Result<Vec<Seq>, String>,
) -> (Result<String, String>, String) {
    let seq = match seqs {
        Ok(seqs) => match seqs.first().cloned() {
            Some(seq) => seq,
            None => return (Err("NoMatch".into()), "no seq to fetch".into()),
        },
        Err(e) => return (Err(e.clone()), "not attempted: search failed".into()),
    };
    let fetch = async {
        let stream = session.fetch(seq.to_string(), "BODY.PEEK[TEXT]").await?;
        let messages: Vec<Fetch> = stream.try_collect().await?;
        Ok::<Vec<Fetch>, ImapError>(messages)
    };
    match tokio::time::timeout(OP_TIMEOUT, fetch).await {
        Ok(Ok(messages)) => match messages.iter().find_map(|m| m.text()) {
            Some(bytes) => (
                Ok(String::from_utf8_lossy(bytes).trim().to_string()),
                format!("seq={seq} text_bytes={}", bytes.len()),
            ),
            None => (Err("NoText".into()), format!("seq={seq} no TEXT section")),
        },
        Ok(Err(e)) => (Err(imap_error_class(&e)), format!("seq={seq} {e}")),
        Err(_) => (Err("Timeout".into()), format!("seq={seq} fetch timed out")),
    }
}

fn conn_stats(stats: &[ConnectionStats]) -> String {
    if stats.is_empty() {
        return "none".into();
    }
    stats
        .iter()
        .map(|c| {
            format!(
                "client={} up={} down={}",
                c.client, c.to_upstream, c.to_client
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// ---------------------------------------------------------------------------
// Preflight: no-cancel, reuse/identity, manual pre/post calibration.
// ---------------------------------------------------------------------------

async fn no_cancel(ports: Ports) -> Result<(), String> {
    let mut env = setup(ports).await?;
    println!(
        "NOCANCEL setup run_id={} seed={} session_ptr={:#x} stream_ptr={:#x} proxy={}",
        env.run_id,
        env.seed_subject,
        &*env.session as *const _ as usize,
        env.session.get_ref() as *const TcpStream as usize,
        conn_stats(&env.proxy.connections())
    );
    let (select, select_raw) = select_inbox(&mut env.session).await;
    let subject = format!("s-{}-survivor", env.run_id);
    let (append, append_raw) = append_message(
        &mut env.session,
        &subject,
        SURVIVOR_BODY,
        &format!("{}@example.com", subject),
    )
    .await;
    let (search, seqs, search_raw) = search_subject(&mut env.session, &subject).await;
    let (fetch, fetch_raw) = fetch_from(&mut env.session, &seqs).await;
    let (view, view_raw) = observe(ports, &env.run_id, &subject).await;
    println!(
        "NOCANCEL select={select:?} raw={select_raw:?} append={append:?} raw={append_raw:?}"
    );
    println!("NOCANCEL search={search:?} raw={search_raw:?} fetch={fetch:?} raw={fetch_raw:?}");
    println!("NOCANCEL observer={view:?} raw={view_raw:?}");
    println!(
        "NOCANCEL proxy_after={}",
        conn_stats(&env.proxy.connections())
    );
    if select.is_err() || append.is_err() || search.is_err() || fetch.is_err() {
        return Err("no-cancel path did not complete cleanly".into());
    }
    match view {
        Ok(v) if v.run_message_count == 2 && v.seed_body && v.survivor_body => Ok(()),
        other => Err(format!("observer did not see seed+survivor: {other:?}")),
    }
}

async fn reuse_identity(ports: Ports) -> Result<(), String> {
    let mut env = setup(ports).await?;
    let session_ptr = &*env.session as *const _ as usize;
    let stream_ptr = env.session.get_ref() as *const TcpStream as usize;
    println!(
        "REUSE start run_id={} session_ptr={session_ptr:#x} stream_ptr={stream_ptr:#x} conns={} stats=[{}]",
        env.run_id,
        env.proxy.connections().len(),
        conn_stats(&env.proxy.connections())
    );
    let (s1, r1) = select_inbox(&mut env.session).await;
    println!(
        "REUSE after_cmd1 select={s1:?} raw={r1:?} conns={} stats=[{}]",
        env.proxy.connections().len(),
        conn_stats(&env.proxy.connections())
    );
    let subject = format!("s-{}-reuse", env.run_id);
    let (s2, r2) = append_message(
        &mut env.session,
        &subject,
        SURVIVOR_BODY,
        &format!("{}@example.com", subject),
    )
    .await;
    println!(
        "REUSE after_cmd2 append={s2:?} raw={r2:?} conns={} stats=[{}]",
        env.proxy.connections().len(),
        conn_stats(&env.proxy.connections())
    );
    let (s3, seqs3, r3) = search_subject(&mut env.session, &subject).await;
    println!(
        "REUSE after_cmd3 search={s3:?} raw={r3:?} seqs={seqs3:?} conns={} stats=[{}]",
        env.proxy.connections().len(),
        conn_stats(&env.proxy.connections())
    );
    println!(
        "REUSE end session_ptr={:#x} stream_ptr={:#x} same_session={} same_stream={} conns={}",
        &*env.session as *const _ as usize,
        env.session.get_ref() as *const TcpStream as usize,
        (&*env.session as *const _ as usize) == session_ptr,
        (env.session.get_ref() as *const TcpStream as usize) == stream_ptr,
        env.proxy.connections().len()
    );
    if env.proxy.connections().len() != 1 {
        return Err(format!(
            "reuse diagnostic: {} proxy connections, expected 1; the survivor path would not be the same TCP stream",
            env.proxy.connections().len()
        ));
    }
    if s1.is_err() || s2.is_err() || s3.is_err() || s3.as_ref().is_ok_and(|n| *n != 1) {
        return Err("reuse diagnostic: three commands did not all complete with the expected result".into());
    }
    Ok(())
}

async fn calibration_side(ports: Ports, pre: bool) -> Result<(), String> {
    let mut env = setup(ports).await?;
    let select = if pre {
        None
    } else {
        Some(select_inbox(&mut env.session).await)
    };
    let subject = format!("s-{}-survivor", env.run_id);
    let (append, append_raw) = append_message(
        &mut env.session,
        &subject,
        SURVIVOR_BODY,
        &format!("{}@example.com", subject),
    )
    .await;
    let (search, seqs, search_raw) = search_subject(&mut env.session, &subject).await;
    let (fetch, fetch_raw) = fetch_from(&mut env.session, &seqs).await;
    let (view, view_raw) = observe(ports, &env.run_id, &subject).await;
    println!(
        "CALIBRATION side={} select={select:?} append={append:?} raw={append_raw:?} search={search:?} raw={search_raw:?} fetch={fetch:?} raw={fetch_raw:?} observer={view:?} observer_raw={view_raw:?}",
        if pre { "pre" } else { "post" }
    );
    if append.is_err() {
        return Err(format!("calibration {}: APPEND failed: {append:?}", if pre { "pre" } else { "post" }));
    }
    match view {
        Ok(v) if v.run_message_count == 2 && v.seed_body && v.survivor_body => Ok(()),
        other => Err(format!(
            "calibration {}: observer did not see seed+survivor: {other:?}",
            if pre { "pre" } else { "post" }
        )),
    }
}

async fn preflight_async(ports: Ports) -> Result<(), String> {
    println!("PREFLIGHT 1/3 no-cancel select+append+search+fetch+observer");
    no_cancel(ports).await?;
    println!("PREFLIGHT 2/3 reuse/identity three sequential commands");
    reuse_identity(ports).await?;
    println!("PREFLIGHT 3/3 manual pre/post calibration");
    calibration_side(ports, true).await?;
    calibration_side(ports, false).await?;
    println!("PREFLIGHT PASS");
    Ok(())
}

// ---------------------------------------------------------------------------
// Triage probe (added after the first ladder run; not part of the ladder).
// Uses the public API only: drop the SELECT future with tokio::time::timeout,
// then either APPEND immediately (A) or drain the queued responses first (B).
// ---------------------------------------------------------------------------

async fn probe(ports: Ports) -> Result<(), String> {
    println!("PROBE A: drop SELECT, do not drain, APPEND immediately");
    {
        let mut env = setup(ports).await?;
        let outcome = tokio::time::timeout(Duration::from_millis(40), env.session.select(MAILBOX)).await;
        println!(
            "PROBE-A dropped={} proxy=[{}]",
            outcome.is_err(),
            conn_stats(&env.proxy.connections())
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        println!("PROBE-A proxy_after_delay=[{}]", conn_stats(&env.proxy.connections()));
        let subject = format!("s-{}-probe-a", env.run_id);
        let (append, raw) = append_message(
            &mut env.session,
            &subject,
            SURVIVOR_BODY,
            &format!("{}@example.com", subject),
        )
        .await;
        println!(
            "PROBE-A append={append:?} raw={raw:?} proxy=[{}]",
            conn_stats(&env.proxy.connections())
        );
        for index in 0..10 {
            match tokio::time::timeout(Duration::from_millis(300), env.session.read_response()).await {
                Ok(Ok(Some(data))) => println!(
                    "PROBE-A queued[{index}] request_id={:?} parsed={:?}",
                    data.request_id(),
                    data.parsed()
                ),
                Ok(Ok(None)) => {
                    println!("PROBE-A queued[{index}] eof");
                    break;
                }
                Ok(Err(e)) => {
                    println!("PROBE-A queued[{index}] error={e}");
                    break;
                }
                Err(_) => {
                    println!("PROBE-A queued[{index}] read timed out");
                    break;
                }
            }
        }
        let (count, raw) = observe_subject_count(ports, &subject).await;
        println!("PROBE-A observer={count:?} raw={raw:?}");
    }

    println!("PROBE B: drop SELECT, drain the queued SELECT responses, APPEND");
    {
        let mut env = setup(ports).await?;
        let outcome = tokio::time::timeout(Duration::from_millis(40), env.session.select(MAILBOX)).await;
        println!("PROBE-B dropped={}", outcome.is_err());
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut drained = 0usize;
        loop {
            match tokio::time::timeout(Duration::from_millis(300), env.session.read_response()).await {
                Ok(Ok(Some(data))) => {
                    let done = matches!(data.parsed(), async_imap::imap_proto::Response::Done { .. });
                    println!(
                        "PROBE-B drained[{drained}] request_id={:?} done={done} parsed={:?}",
                        data.request_id(),
                        data.parsed()
                    );
                    drained += 1;
                    if done {
                        break;
                    }
                }
                Ok(Ok(None)) => {
                    println!("PROBE-B eof while draining");
                    break;
                }
                Ok(Err(e)) => {
                    println!("PROBE-B read error while draining: {e}");
                    break;
                }
                Err(_) => {
                    println!("PROBE-B no more queued responses after {drained}");
                    break;
                }
            }
        }
        let subject = format!("s-{}-probe-b", env.run_id);
        let (append, raw) = append_message(
            &mut env.session,
            &subject,
            SURVIVOR_BODY,
            &format!("{}@example.com", subject),
        )
        .await;
        let (count, obs_raw) = observe_subject_count(ports, &subject).await;
        println!(
            "PROBE-B drained_total={drained} append={append:?} raw={raw:?} observer={count:?} observer_raw={obs_raw:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ladder.
// ---------------------------------------------------------------------------

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle {
            tokio_time: Duration::ZERO,
            ..Settle::default()
        },
        ..Config::default()
    }
}

fn print_diagnostics(level: &str, rep: u32, report: &Report) {
    println!("--- DIAGNOSTICS {level} rep={rep} ---");
    print!("{}", report.diagnostics());
    for gap in report.coverage_gaps() {
        println!("COVERAGE-GAP {level} rep={rep}: {gap}");
    }
    for note in &report.notes {
        println!("NOTE {level} rep={rep}: {note}");
    }
}

fn print_actuals(level: &str, report: &Report, entries: &Arc<Mutex<Vec<String>>>) {
    let entries = entries.lock().unwrap();
    if let Some(baseline) = entries.first() {
        println!("{level}-BASELINE actual={baseline}");
    }
    for (index, trial) in report.trials.iter().enumerate() {
        if let Some(actual) = entries.get(index + 1) {
            println!(
                "{level}-ACTUAL plan={} realized={} actual={}",
                trial.plan_label(),
                trial.is_realized(),
                actual
            );
        }
    }
}

fn run_b1(ports: Ports, rep: u32) {
    let report = explore(&config(), move |ctx| async move {
        let mut env = setup(ports).await?;
        let selected = ctx.target(env.session.select(MAILBOX)).await;
        println!(
            "B1-RUN cancelled={} target={:?}",
            selected.is_none(),
            selected.map(|r| r.map(|m| m.exists))
        );
        Ok(())
    });
    ladder::simple("B1", "SELECT_INBOX", &report);
    print_diagnostics("B1", rep, &report);
}

fn run_b2(ports: Ports, rep: u32) {
    let entries = Arc::new(Mutex::new(Vec::new()));
    let entries_for_run = entries.clone();
    let report = explore(&config(), move |ctx| {
        let entries = entries_for_run.clone();
        async move {
            let mut env = setup(ports).await?;
            let selected = ctx.target(env.session.select(MAILBOX)).await;
            match selected {
                Some(Ok(mailbox)) => {
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("baseline Ok({})", mailbox_summary(&mailbox)));
                }
                Some(Err(e)) => {
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("baseline Err({}) {e}", imap_error_class(&e)));
                }
                None => {
                    let (retry, raw) = select_inbox(&mut env.session).await;
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("cancelled retry={retry:?} raw={raw:?}"));
                    if let Err(e) = retry {
                        return Err(format!("B2 retry failed: {e} raw={raw}"));
                    }
                }
            }
            Ok(())
        }
    });
    ladder::simple("B2", "SELECT_INBOX", &report);
    print_actuals("B2", &report, &entries);
    print_diagnostics("B2", rep, &report);
}

fn run_b3(ports: Ports, rep: u32) {
    let entries = Arc::new(Mutex::new(Vec::new()));
    let entries_for_run = entries.clone();
    let report = explore(&config(), move |ctx| {
        let entries = entries_for_run.clone();
        async move {
            let mut env = setup(ports).await?;
            let selected = ctx.target(env.session.select(MAILBOX)).await;
            match selected {
                Some(Ok(mailbox)) => {
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("baseline Ok({})", mailbox_summary(&mailbox)));
                }
                Some(Err(e)) => {
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("baseline Err({}) {e}", imap_error_class(&e)));
                }
                None => {
                    let subject = format!("s-{}-b3", env.run_id);
                    let (append, raw) = append_message(
                        &mut env.session,
                        &subject,
                        SURVIVOR_BODY,
                        &format!("{}@example.com", subject),
                    )
                    .await;
                    let (observed, observed_raw) = observe_subject_count(ports, &subject).await;
                    entries.lock().unwrap().push(format!(
                        "cancelled append={append:?} raw={raw:?} observer={observed:?} observer_raw={observed_raw:?}"
                    ));
                    if let Err(e) = append {
                        return Err(format!("B3 append failed: {e} raw={raw}"));
                    }
                }
            }
            Ok(())
        }
    });
    ladder::simple("B3", "APPEND_INBOX", &report);
    print_actuals("B3", &report, &entries);
    print_diagnostics("B3", rep, &report);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    append: Result<(), String>,
    search: Result<usize, String>,
    fetch_text: Result<String, String>,
    observer_subjects: Result<Vec<String>, String>,
    observer_seed_body: bool,
    observer_survivor_body: bool,
    observer_run_message_count: usize,
}

fn run_m1(ports: Ports, rep: u32) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx| async move {
        let mut env = setup(ports).await?;
        let selected = ctx.target(env.session.select(MAILBOX)).await;
        let target_cancelled = selected.is_none();
        let subject = format!("s-{}-survivor", env.run_id);
        let (append, append_raw) = append_message(
            &mut env.session,
            &subject,
            SURVIVOR_BODY,
            &format!("{}@example.com", subject),
        )
        .await;
        let (search, seqs, search_raw) = search_subject(&mut env.session, &subject).await;
        let (fetch, fetch_raw) = fetch_from(&mut env.session, &seqs).await;
        let (view, view_raw) = observe(ports, &env.run_id, &subject).await;
        let (observer_subjects, observer_seed_body, observer_survivor_body, observer_run_count) =
            match &view {
                Ok(v) => (
                    Ok(v.run_subjects.clone()),
                    v.seed_body,
                    v.survivor_body,
                    v.run_message_count,
                ),
                Err(e) => (Err(e.clone()), false, false, 0),
            };
        let obs = Obs {
            append,
            search,
            fetch_text: fetch,
            observer_subjects,
            observer_seed_body,
            observer_survivor_body,
            observer_run_message_count: observer_run_count,
        };
        println!(
            "M1-RUN cancelled={target_cancelled} select_exists={:?} append_raw={append_raw:?} search_raw={search_raw:?} fetch_raw={fetch_raw:?} observer_raw={view_raw:?} obs={obs:?}",
            selected.map(|r| r.map(|m| m.exists))
        );
        Ok(obs)
    });
    ladder::observed("M1", "SELECT_INBOX", &report);
    print_diagnostics("M1", rep, &report);
}

fn run_ladder(ports: Ports, rep: u32) {
    println!("=== LADDER REPETITION {rep} START ===");
    println!("CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0");
    run_b1(ports, rep);
    run_b2(ports, rep);
    run_b3(ports, rep);
    run_m1(ports, rep);
    println!("=== LADDER REPETITION {rep} END ===");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let phase = args.next().unwrap_or_else(|| "preflight".into());
    let rep: u32 = args
        .next()
        .unwrap_or_else(|| "0".into())
        .parse()
        .unwrap_or(0);
    let ports = match Ports::from_env() {
        Ok(ports) => ports,
        Err(e) => {
            eprintln!("SETUP-FAILURE {e}");
            std::process::exit(2);
        }
    };
    let mut out = String::new();
    let result = match phase.as_str() {
        "preflight" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build preflight runtime");
            rt.block_on(preflight_async(ports))
        }
        "probe" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build probe runtime");
            rt.block_on(probe(ports))
        }
        "ladder" => {
            run_ladder(ports, rep);
            Ok(())
        }
        other => Err(format!("unknown phase {other:?}")),
    };
    match result {
        Ok(()) => {}
        Err(e) => {
            let _ = writeln!(out, "PHASE-FAILURE phase={phase} rep={rep}: {e}");
            print!("{out}");
            eprintln!("PHASE-FAILURE phase={phase} rep={rep}: {e}");
            std::process::exit(1);
        }
    }
}

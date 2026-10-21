//! Phase 4 exploration, round 2: async-imap 0.12.0 direct Session.
//!
//! Round 1 showed that a cancelled SELECT leaves its replies in the socket and
//! that the next APPEND reads a stale untagged reply instead of the APPEND
//! continuation. Round 2 does three things with a fresh GreenMail container:
//!
//!   chain N [reverse]  cancel SELECT, then either retry SELECT -> APPEND ->
//!                      SEARCH/FETCH (default) or APPEND -> retry SELECT ->
//!                      SEARCH/FETCH (reverse), and record raw bytes, flags,
//!                      observer view and connection identity;
//!   ladder N           the round-1 B1/B2/B3/M1 ladder, re-run with round-2
//!                      names so it is not folded into round-1 denominators;
//!   patch-behavior     normal/rejected/EOF/stale-SELECT APPEND cases, with a
//!                      transcript of the bytes the client actually read;
//!   search-unselected  minimal reproduction of SEARCH returning Ok(0) while
//!                      GreenMail rejects it in AUTHENTICATED state.
//!
//! The binary is built twice by ../run-round2.sh: once against the published
//! async-imap 0.12.0 and once against the round-2 dependency copy with the
//! APPEND-continuation patch. There is no compatibility framework here.

#[path = "../../../../../support/ladder.rs"]
mod ladder;

use std::fmt::{Debug, Write as _};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_imap::error::Error as ImapError;
use async_imap::types::{Fetch, Seq};
use async_imap::{Client, Session};
use dropwise::net::{ConnectionStats, Proxy, ProxyOptions};
use dropwise::{explore, explore_observed, Config, Expect, Flavor, Race, Report, Settle};
use futures_util::TryStreamExt;
use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::net::{TcpListener, TcpStream};

const OP_TIMEOUT: Duration = Duration::from_secs(5);
const REVERSE_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_DELAY: Duration = Duration::from_millis(50);
const IMAP_USER: &str = "imapuser";
const IMAP_PASS: &str = "pass";
const MAILBOX: &str = "INBOX";
const BAD_MAILBOX: &str = "NoSuchBox-round2";
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
        "exists={} recent={} unseen={:?} uid_next={:?} uid_validity={:?} flags={:?} permanent_flags={:?}",
        m.exists, m.recent, m.unseen, m.uid_next, m.uid_validity, m.flags, m.permanent_flags
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
// Raw-byte transcript wrapper. It only observes the TcpStream that the
// published async-imap already owns; it does not change protocol handling.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct TeeStream {
    inner: TcpStream,
    log: Arc<Mutex<Vec<u8>>>,
}

impl TeeStream {
    fn new(inner: TcpStream, log: Arc<Mutex<Vec<u8>>>) -> Self {
        Self { inner, log }
    }
}

fn note_bytes(log: &Arc<Mutex<Vec<u8>>>, direction: u8, bytes: &[u8]) {
    let mut guard = log.lock().unwrap();
    guard.push(b'\n');
    guard.push(direction);
    guard.push(b'>');
    guard.extend_from_slice(bytes);
}

impl AsyncRead for TeeStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = result {
            let after = buf.filled().len();
            if after > before {
                let chunk = buf.filled()[before..after].to_vec();
                note_bytes(&this.log, b'S', &chunk);
            }
        }
        result
    }
}

impl AsyncWrite for TeeStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = result {
            if n > 0 {
                note_bytes(&this.log, b'C', &buf[..n]);
            }
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn log_len(log: &Arc<Mutex<Vec<u8>>>) -> usize {
    log.lock().unwrap().len()
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        match byte {
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(byte as char),
            _ => {
                let _ = write!(out, "\\x{byte:02x}");
            }
        }
        if out.len() > 12_000 {
            out.push_str("...<truncated>");
            break;
        }
    }
    out
}

fn transcript(log: &Arc<Mutex<Vec<u8>>>, from: usize) -> String {
    let guard = log.lock().unwrap();
    let slice = guard.get(from..).unwrap_or(&[]);
    escape_bytes(slice)
}

// ---------------------------------------------------------------------------
// Independent writer (SMTP) and observer (GreenMail REST), both direct.
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
        body = body
            .get(..length)
            .ok_or_else(|| "observer-http".to_string())?;
    }
    let values: Vec<Value> =
        serde_json::from_slice(body).map_err(|_| "observer-json".to_string())?;
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
            (
                Ok(count),
                format!(
                    "rest_total={} subject={subject:?} count={count}",
                    msgs.len()
                ),
            )
        }
        Err(e) => (Err(e.clone()), format!("rest error {e}")),
    }
}

// ---------------------------------------------------------------------------
// Sessions. One TeeStream variant records bytes; the plain variant is used by
// the ladder so it stays identical to round 1.
// ---------------------------------------------------------------------------

struct EnvTee {
    run_id: String,
    seed_subject: String,
    proxy: Proxy,
    session: Box<Session<TeeStream>>,
    log: Arc<Mutex<Vec<u8>>>,
}

struct EnvPlain {
    run_id: String,
    #[allow(dead_code)]
    seed_subject: String,
    #[allow(dead_code)]
    proxy: Proxy,
    session: Box<Session<TcpStream>>,
}

async fn begin(ports: Ports) -> Result<(String, String, Proxy), String> {
    let run_id = dropwise::support::unique("p4imap-r2");
    let seed_subject = format!("s-{run_id}-seed");
    let seed_id = format!("{run_id}-seed@example.com");
    tokio::time::timeout(
        OP_TIMEOUT,
        smtp_seed(ports, &seed_subject, SEED_BODY, &seed_id),
    )
    .await
    .map_err(|_| "smtp seed timed out".to_string())??;

    let upstream: std::net::SocketAddr = format!("127.0.0.1:{}", ports.imap)
        .parse()
        .map_err(|e| format!("bad upstream address: {e}"))?;
    let proxy = Proxy::start_with(
        upstream,
        ProxyOptions {
            reply_delay: REPLY_DELAY,
        },
    )
    .await
    .map_err(|e| format!("proxy start: {e}"))?;
    Ok((run_id, seed_subject, proxy))
}

async fn login_tee(
    stream: TcpStream,
    log: Arc<Mutex<Vec<u8>>>,
) -> Result<Session<TeeStream>, String> {
    let tee = TeeStream::new(stream, log);
    let mut client = Client::new(tee);
    let greeting = tokio::time::timeout(OP_TIMEOUT, client.read_response())
        .await
        .map_err(|_| "greeting timed out".to_string())?
        .map_err(|e| format!("greeting: {e}"))?;
    if greeting.is_none() {
        return Err("imap server closed before greeting".into());
    }
    match tokio::time::timeout(OP_TIMEOUT, client.login(IMAP_USER, IMAP_PASS)).await {
        Ok(Ok(session)) => Ok(session),
        Ok(Err((e, _client))) => Err(format!("login failed: {e}")),
        Err(_) => Err("login timed out".into()),
    }
}

async fn setup_tee(ports: Ports) -> Result<EnvTee, String> {
    let (run_id, seed_subject, proxy) = begin(ports).await?;
    let stream = tokio::time::timeout(OP_TIMEOUT, TcpStream::connect(proxy.addr()))
        .await
        .map_err(|_| "imap connect timed out".to_string())?
        .map_err(|e| format!("imap connect: {e}"))?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let session = login_tee(stream, log.clone()).await?;
    Ok(EnvTee {
        run_id,
        seed_subject,
        proxy,
        session: Box::new(session),
        log,
    })
}

async fn setup_plain(ports: Ports) -> Result<EnvPlain, String> {
    let (run_id, seed_subject, proxy) = begin(ports).await?;
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
    Ok(EnvPlain {
        run_id,
        seed_subject,
        proxy,
        session: Box::new(session),
    })
}

// ---------------------------------------------------------------------------
// Operations. All of them are bounded by an explicit timeout in this harness;
// async-imap itself has no command timeout.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectInfo {
    exists: u32,
    recent: u32,
    unseen: Option<u32>,
    uid_next: Option<u32>,
    uid_validity: Option<u32>,
    flags_len: usize,
    flags: String,
    permanent_flags_len: usize,
    permanent_flags: String,
    ms: u128,
}

impl SelectInfo {
    fn from_mailbox(m: &async_imap::types::Mailbox, ms: u128) -> Self {
        Self {
            exists: m.exists,
            recent: m.recent,
            unseen: m.unseen,
            uid_next: m.uid_next,
            uid_validity: m.uid_validity,
            flags_len: m.flags.len(),
            flags: format!("{:?}", m.flags),
            permanent_flags_len: m.permanent_flags.len(),
            permanent_flags: format!("{:?}", m.permanent_flags),
            ms,
        }
    }
}

async fn select_inbox<T>(
    session: &mut Session<T>,
    timeout: Duration,
) -> (Result<SelectInfo, String>, String, u128)
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let start = Instant::now();
    match tokio::time::timeout(timeout, session.select(MAILBOX)).await {
        Ok(Ok(mailbox)) => {
            let elapsed = start.elapsed().as_millis();
            (
                Ok(SelectInfo::from_mailbox(&mailbox, elapsed)),
                "Ok".into(),
                elapsed,
            )
        }
        Ok(Err(e)) => (
            Err(imap_error_class(&e)),
            format!("{e}"),
            start.elapsed().as_millis(),
        ),
        Err(_) => (
            Err("Timeout".into()),
            "select timed out".into(),
            start.elapsed().as_millis(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn append_to<T>(
    session: &mut Session<T>,
    mailbox: &str,
    flags: Option<&str>,
    internaldate: Option<&str>,
    subject: &str,
    body: &str,
    message_id: &str,
    timeout: Duration,
) -> (Result<(), String>, String, u128)
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let content = message(subject, body, message_id);
    let start = Instant::now();
    match tokio::time::timeout(
        timeout,
        session.append(mailbox, flags, internaldate, content),
    )
    .await
    {
        Ok(Ok(())) => (Ok(()), "Ok".into(), start.elapsed().as_millis()),
        Ok(Err(e)) => (
            Err(imap_error_class(&e)),
            format!("{e}"),
            start.elapsed().as_millis(),
        ),
        Err(_) => (
            Err("Timeout".into()),
            "append timed out".into(),
            start.elapsed().as_millis(),
        ),
    }
}

async fn append_message<T>(
    session: &mut Session<T>,
    subject: &str,
    timeout: Duration,
) -> (Result<(), String>, String, u128)
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    append_to(
        session,
        MAILBOX,
        None,
        None,
        subject,
        SURVIVOR_BODY,
        &format!("{subject}@example.com"),
        timeout,
    )
    .await
}

async fn search_subject<T>(
    session: &mut Session<T>,
    subject: &str,
    timeout: Duration,
) -> (
    Result<usize, String>,
    Result<Vec<Seq>, String>,
    String,
    u128,
)
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let query = format!("SUBJECT \"{subject}\"");
    let start = Instant::now();
    match tokio::time::timeout(timeout, session.search(query)).await {
        Ok(Ok(set)) => {
            let mut seqs: Vec<Seq> = set.into_iter().collect();
            seqs.sort_by_key(|seq| seq.to_string());
            let raw = seqs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            (
                Ok(seqs.len()),
                Ok(seqs),
                format!("seqs=[{raw}]"),
                start.elapsed().as_millis(),
            )
        }
        Ok(Err(e)) => {
            let class = imap_error_class(&e);
            (
                Err(class.clone()),
                Err(class),
                format!("{e}"),
                start.elapsed().as_millis(),
            )
        }
        Err(_) => (
            Err("Timeout".into()),
            Err("Timeout".into()),
            "search timed out".into(),
            start.elapsed().as_millis(),
        ),
    }
}

async fn fetch_from<T>(
    session: &mut Session<T>,
    seqs: &Result<Vec<Seq>, String>,
    timeout: Duration,
) -> (Result<String, String>, String, u128)
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let seq = match seqs {
        Ok(seqs) => match seqs.first().cloned() {
            Some(seq) => seq,
            None => return (Err("NoMatch".into()), "no seq to fetch".into(), 0),
        },
        Err(e) => return (Err(e.clone()), "not attempted: search failed".into(), 0),
    };
    let start = Instant::now();
    let fetch = async {
        let stream = session.fetch(seq.to_string(), "BODY.PEEK[TEXT]").await?;
        let messages: Vec<Fetch> = stream.try_collect().await?;
        Ok::<Vec<Fetch>, ImapError>(messages)
    };
    match tokio::time::timeout(timeout, fetch).await {
        Ok(Ok(messages)) => match messages.iter().find_map(|m| m.text()) {
            Some(bytes) => (
                Ok(String::from_utf8_lossy(bytes).trim().to_string()),
                format!("seq={seq} text_bytes={}", bytes.len()),
                start.elapsed().as_millis(),
            ),
            None => (
                Err("NoText".into()),
                format!("seq={seq} no TEXT section"),
                start.elapsed().as_millis(),
            ),
        },
        Ok(Err(e)) => (
            Err(imap_error_class(&e)),
            format!("seq={seq} {e}"),
            start.elapsed().as_millis(),
        ),
        Err(_) => (
            Err("Timeout".into()),
            format!("seq={seq} fetch timed out"),
            start.elapsed().as_millis(),
        ),
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

fn tee_identity(env: &EnvTee) -> String {
    format!(
        "session_ptr={:#x} stream_ptr={:#x}",
        &*env.session as *const _ as usize,
        &env.session.get_ref().inner as *const TcpStream as usize
    )
}

// ---------------------------------------------------------------------------
// Preflight: one no-cancel path through the transcript wrapper.
// ---------------------------------------------------------------------------

async fn preflight_async(ports: Ports) -> Result<(), String> {
    let mut env = setup_tee(ports).await?;
    println!(
        "ROUND2-PREFLIGHT run={} seed={} proxy=[{}] conns={} {}",
        env.run_id,
        env.seed_subject,
        conn_stats(&env.proxy.connections()),
        env.proxy.connections().len(),
        tee_identity(&env)
    );
    let select = select_inbox(&mut env.session, OP_TIMEOUT).await;
    println!(
        "ROUND2-PREFLIGHT select={:?} raw={:?} ms={}",
        select.0, select.1, select.2
    );
    let subject = format!("s-{}-survivor", env.run_id);
    let append = append_message(&mut env.session, &subject, OP_TIMEOUT).await;
    let search = search_subject(&mut env.session, &subject, OP_TIMEOUT).await;
    let fetch = fetch_from(&mut env.session, &search.1, OP_TIMEOUT).await;
    let view = observe(ports, &env.run_id, &subject).await;
    println!(
        "ROUND2-PREFLIGHT append={:?} raw={:?} ms={}",
        append.0, append.1, append.2
    );
    println!(
        "ROUND2-PREFLIGHT search={:?} raw={:?} ms={} fetch={:?} raw={:?} ms={}",
        search.0, search.2, search.3, fetch.0, fetch.1, fetch.2
    );
    println!(
        "ROUND2-PREFLIGHT observer={:?} raw={:?} proxy_after=[{}] conns={}",
        view.0,
        view.1,
        conn_stats(&env.proxy.connections()),
        env.proxy.connections().len()
    );
    if select.0.is_err() || append.0.is_err() || search.0.is_err() || fetch.0.is_err() {
        return Err("round2 preflight did not complete cleanly".into());
    }
    match view.0 {
        Ok(v) if v.run_message_count == 2 && v.seed_body && v.survivor_body => Ok(()),
        other => Err(format!(
            "round2 preflight observer did not see seed+survivor: {other:?}"
        )),
    }
}

// ---------------------------------------------------------------------------
// Recovery chain. The target is a SELECT INBOX whose future is dropped at a
// realized Pending. There is deliberately no reconnect/reset between steps.
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

fn run_chain(ports: Ports, rep: u32, reverse: bool) {
    let mode = if reverse { "reverse" } else { "recovery" };
    let entries = Arc::new(Mutex::new(Vec::new()));
    let entries_for_run = entries.clone();
    let report = explore(&config(), move |ctx| {
        let entries = entries_for_run.clone();
        async move {
            let mut env = setup_tee(ports).await?;
            let before = select_inbox(&mut env.session, OP_TIMEOUT).await;
            let before_info = before.0.clone();
            let target = ctx.target(env.session.select(MAILBOX)).await;
            if let Some(result) = target {
                let text = match result {
                    Ok(mailbox) => format!("target=Ok({})", mailbox_summary(&mailbox)),
                    Err(e) => format!("target=Err({}) {e}", imap_error_class(&e)),
                };
                entries
                    .lock()
                    .unwrap()
                    .push(format!("baseline {} before={:?}", text, before_info));
                return Ok(());
            }

            let subject = format!("s-{}-survivor", env.run_id);
            let proxy_before = format!(
                "conns={} stats=[{}]",
                env.proxy.connections().len(),
                conn_stats(&env.proxy.connections())
            );
            let (retry, retry_tx, append, append_tx, search, fetch, observer) = if reverse {
                let append_off = log_len(&env.log);
                let append = append_message(&mut env.session, &subject, OP_TIMEOUT).await;
                let append_tx = transcript(&env.log, append_off);
                let retry_off = log_len(&env.log);
                let retry = select_inbox(&mut env.session, REVERSE_TIMEOUT).await;
                let retry_tx = transcript(&env.log, retry_off);
                let search = search_subject(&mut env.session, &subject, REVERSE_TIMEOUT).await;
                let fetch = fetch_from(&mut env.session, &search.1, REVERSE_TIMEOUT).await;
                let observer = observe(ports, &env.run_id, &subject).await;
                (retry, retry_tx, append, append_tx, search, fetch, observer)
            } else {
                let retry_off = log_len(&env.log);
                let retry = select_inbox(&mut env.session, OP_TIMEOUT).await;
                let retry_tx = transcript(&env.log, retry_off);
                let append_off = log_len(&env.log);
                let append = append_message(&mut env.session, &subject, OP_TIMEOUT).await;
                let append_tx = transcript(&env.log, append_off);
                let search = search_subject(&mut env.session, &subject, OP_TIMEOUT).await;
                let fetch = fetch_from(&mut env.session, &search.1, OP_TIMEOUT).await;
                let observer = observe(ports, &env.run_id, &subject).await;
                (retry, retry_tx, append, append_tx, search, fetch, observer)
            };

            let proxy_after = format!(
                "conns={} stats=[{}]",
                env.proxy.connections().len(),
                conn_stats(&env.proxy.connections())
            );
            println!(
                "CHAIN-TX run={} mode={mode} retry_tx={:?} append_tx={:?}",
                env.run_id, retry_tx, append_tx
            );
            entries.lock().unwrap().push(format!(
                "mode={mode} before={:?} retry={:?} retry_raw={:?} retry_ms={} append={:?} append_raw={:?} append_ms={} search={:?} search_raw={:?} search_ms={} fetch={:?} fetch_raw={:?} fetch_ms={} observer={:?} observer_raw={:?} proxy_before=[{}] proxy_after=[{}] {}",
                before_info,
                retry.0,
                retry.1,
                retry.2,
                append.0,
                append.1,
                append.2,
                search.0,
                search.2,
                search.3,
                fetch.0,
                fetch.1,
                fetch.2,
                observer.0,
                observer.1,
                proxy_before,
                proxy_after,
                tee_identity(&env)
            ));
            Ok(())
        }
    });

    println!(
        "CHAIN mode={mode} rep={rep}: plans={} violations={} unrealized={} unsettled={} baseline_errors={} exhaustive={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.unsettled().count(),
        report.baseline_errors.len(),
        report.exhaustive
    );
    for gap in report.coverage_gaps() {
        println!("CHAIN-GAP mode={mode} rep={rep}: {gap}");
    }
    {
        let entries = entries.lock().unwrap();
        for (index, trial) in report.trials.iter().enumerate() {
            if let Some(actual) = entries.get(index + 1) {
                println!(
                    "CHAIN-ACTUAL mode={mode} rep={rep} plan={} realized={} cancelled={} actual={}",
                    trial.plan_label(),
                    trial.is_realized(),
                    trial.cancelled,
                    actual
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Round-2 rerun of the round-1 ladder. Same Config, levels, arguments and
// judgement rules; only the level names/denominators are new.
// ---------------------------------------------------------------------------

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
        let mut env = setup_plain(ports).await?;
        let selected = ctx.target(env.session.select(MAILBOX)).await;
        println!(
            "B1-RUN run={} cancelled={} target={:?}",
            env.run_id,
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
            let mut env = setup_plain(ports).await?;
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
                    let (retry, raw, ms) = select_inbox(&mut env.session, OP_TIMEOUT).await;
                    let failed = retry.is_err();
                    entries
                        .lock()
                        .unwrap()
                        .push(format!("cancelled retry={retry:?} raw={raw:?} ms={ms}"));
                    if failed {
                        return Err(format!("B2 retry failed: {retry:?} raw={raw}"));
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
            let mut env = setup_plain(ports).await?;
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
                    let (append, raw, ms) =
                        append_message(&mut env.session, &subject, OP_TIMEOUT).await;
                    let (observed, observed_raw) = observe_subject_count(ports, &subject).await;
                    let failed = append.is_err();
                    entries.lock().unwrap().push(format!(
                        "cancelled append={append:?} raw={raw:?} ms={ms} observer={observed:?} observer_raw={observed_raw:?}"
                    ));
                    if failed {
                        return Err(format!("B3 append failed: {append:?} raw={raw}"));
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
        let mut env = setup_plain(ports).await?;
        let selected = ctx.target(env.session.select(MAILBOX)).await;
        let target_cancelled = selected.is_none();
        let subject = format!("s-{}-survivor", env.run_id);
        let (append, append_raw, append_ms) =
            append_message(&mut env.session, &subject, OP_TIMEOUT).await;
        let (search, seqs, search_raw, search_ms) =
            search_subject(&mut env.session, &subject, OP_TIMEOUT).await;
        let (fetch, fetch_raw, fetch_ms) = fetch_from(&mut env.session, &seqs, OP_TIMEOUT).await;
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
            "M1-RUN run={} cancelled={} select_exists={:?} append_raw={append_raw:?} append_ms={append_ms} search_raw={search_raw:?} search_ms={search_ms} fetch_raw={fetch_raw:?} fetch_ms={fetch_ms} observer_raw={view_raw:?} obs={obs:?}",
            env.run_id,
            target_cancelled,
            selected.map(|r| r.map(|m| m.exists))
        );
        Ok(obs)
    });
    ladder::observed("M1", "SELECT_INBOX", &report);
    print_diagnostics("M1", rep, &report);
}

fn run_ladder(ports: Ports, rep: u32) {
    println!("=== ROUND2 LADDER REPETITION {rep} START ===");
    println!("CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0");
    run_b1(ports, rep);
    run_b2(ports, rep);
    run_b3(ports, rep);
    run_m1(ports, rep);
    println!("=== ROUND2 LADDER REPETITION {rep} END ===");
}

// ---------------------------------------------------------------------------
// APPEND patch behaviour on the real server and on scripted servers.
// ---------------------------------------------------------------------------

fn patch_case(
    case: &str,
    detail: String,
    result: &str,
    elapsed_ms: u128,
    observer: &str,
    tx: &str,
) {
    println!(
        "PATCH-CASE case={case} result={result} elapsed_ms={elapsed_ms} observer={observer} detail={detail} tx={tx:?}"
    );
}

async fn patch_normal(ports: Ports, selected: bool) -> Result<(), String> {
    let mut env = setup_tee(ports).await?;
    let select = if selected {
        let s = select_inbox(&mut env.session, OP_TIMEOUT).await;
        format!("select={:?}", s.0)
    } else {
        "select=not-sent".to_string()
    };
    let subject = format!(
        "s-{}-normal-{}",
        env.run_id,
        if selected { "sel" } else { "unsel" }
    );
    let offset = log_len(&env.log);
    let (append, raw, ms) = append_message(&mut env.session, &subject, OP_TIMEOUT).await;
    let tx = transcript(&env.log, offset);
    let (observer, observer_raw) = observe_subject_count(ports, &subject).await;
    let case = if selected {
        "normal-selected"
    } else {
        "normal-unselected"
    };
    patch_case(
        case,
        format!("{select} append_raw={raw:?} observer_raw={observer_raw:?}"),
        &format!("{append:?}"),
        ms,
        &format!("{observer:?}"),
        &tx,
    );
    Ok(())
}

async fn patch_reject(
    ports: Ports,
    case: &str,
    mailbox: &str,
    flags: Option<&str>,
    internaldate: Option<&str>,
) -> Result<(), String> {
    let mut env = setup_tee(ports).await?;
    let select = select_inbox(&mut env.session, OP_TIMEOUT).await;
    let subject = format!("s-{}-{}", env.run_id, case);
    let offset = log_len(&env.log);
    let (append, raw, ms) = append_to(
        &mut env.session,
        mailbox,
        flags,
        internaldate,
        &subject,
        SURVIVOR_BODY,
        &format!("{subject}@example.com"),
        OP_TIMEOUT,
    )
    .await;
    let tx = transcript(&env.log, offset);
    let (observer, observer_raw) = observe_subject_count(ports, &subject).await;
    patch_case(
        case,
        format!(
            "select={:?} mailbox={mailbox:?} flags={flags:?} internaldate={internaldate:?} append_raw={raw:?} observer_raw={observer_raw:?}",
            select.0
        ),
        &format!("{append:?}"),
        ms,
        &format!("{observer:?}"),
        &tx,
    );
    Ok(())
}

async fn patch_stale_select(ports: Ports, reject: bool) -> Result<(), String> {
    let mut env = setup_tee(ports).await?;
    let outcome =
        tokio::time::timeout(Duration::from_millis(40), env.session.select(MAILBOX)).await;
    let dropped = outcome.is_err();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let subject = format!(
        "s-{}-stale-{}",
        env.run_id,
        if reject { "reject" } else { "append" }
    );
    let offset = log_len(&env.log);
    let (append, raw, ms) = if reject {
        append_to(
            &mut env.session,
            BAD_MAILBOX,
            None,
            None,
            &subject,
            SURVIVOR_BODY,
            &format!("{subject}@example.com"),
            OP_TIMEOUT,
        )
        .await
    } else {
        append_message(&mut env.session, &subject, OP_TIMEOUT).await
    };
    let tx = transcript(&env.log, offset);
    let (observer, observer_raw) = observe_subject_count(ports, &subject).await;
    let case = if reject {
        "stale-select-reject"
    } else {
        "stale-select-append"
    };
    patch_case(
        case,
        format!(
            "dropped={dropped} mailbox={:?} append_raw={raw:?} observer_raw={observer_raw:?}",
            if reject { BAD_MAILBOX } else { MAILBOX }
        ),
        &format!("{append:?}"),
        ms,
        &format!("{observer:?}"),
        &tx,
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum FakeMode {
    Eof,
    OwnNo,
    StaleContinue,
}

async fn fake_serve(socket: TcpStream, mode: FakeMode) {
    let mut reader = BufReader::new(socket);
    if reader
        .get_mut()
        .write_all(b"* OK fake round2 server ready\r\n")
        .await
        .is_err()
    {
        return;
    }
    loop {
        let mut line = String::new();
        let read = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(_) => return,
        };
        if read == 0 {
            return;
        }
        let upper = line.to_ascii_uppercase();
        let tag = line
            .split_whitespace()
            .next()
            .unwrap_or("A0001")
            .to_string();
        if upper.contains(" LOGIN ") {
            if reader
                .get_mut()
                .write_all(format!("{tag} OK logged in\r\n").as_bytes())
                .await
                .is_err()
            {
                return;
            }
        } else if upper.contains(" APPEND ") {
            match mode {
                FakeMode::Eof => return,
                FakeMode::OwnNo => {
                    let _ = reader
                        .get_mut()
                        .write_all(format!("{tag} NO rejected by fake server\r\n").as_bytes())
                        .await;
                    return;
                }
                FakeMode::StaleContinue => {
                    let _ = reader
                        .get_mut()
                        .write_all(b"+ OK\r\nA9999 OK stale completion for another command\r\n")
                        .await;
                    // Keep the connection open and consume the literal/content.
                    // No response for the new APPEND will ever arrive.
                }
            }
        }
    }
}

async fn setup_fake(mode: FakeMode) -> Result<Session<TcpStream>, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("fake bind: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("fake local_addr: {e}"))?;
    tokio::spawn(async move {
        if let Ok((socket, _)) = listener.accept().await {
            fake_serve(socket, mode).await;
        }
    });
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("fake connect: {e}"))?;
    let mut client = Client::new(stream);
    let greeting = tokio::time::timeout(OP_TIMEOUT, client.read_response())
        .await
        .map_err(|_| "fake greeting timed out".to_string())?
        .map_err(|e| format!("fake greeting: {e}"))?;
    if greeting.is_none() {
        return Err("fake server closed before greeting".into());
    }
    match tokio::time::timeout(OP_TIMEOUT, client.login(IMAP_USER, IMAP_PASS)).await {
        Ok(Ok(session)) => Ok(session),
        Ok(Err((e, _client))) => Err(format!("fake login failed: {e}")),
        Err(_) => Err("fake login timed out".into()),
    }
}

async fn patch_fake(case: &str, mode: FakeMode) -> Result<(), String> {
    let mut session = setup_fake(mode).await?;
    let subject = format!("s-{}-{}", dropwise::support::unique("p4imap-r2"), case);
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        session.append(
            MAILBOX,
            None,
            None,
            message(&subject, SURVIVOR_BODY, &format!("{subject}@example.com")),
        ),
    )
    .await;
    let elapsed = start.elapsed().as_millis();
    let (text, detail) = match result {
        Ok(Ok(())) => ("Ok".to_string(), "server accepted".to_string()),
        Ok(Err(e)) => (
            format!("Err({})", imap_error_class(&e)),
            format!("server error {e}"),
        ),
        Err(_) => (
            "Timeout".to_string(),
            "caller-side 2 s timeout; server kept the connection open".to_string(),
        ),
    };
    patch_case(
        case,
        format!("fake_mode={mode:?} {detail}"),
        &text,
        elapsed,
        "not-queried",
        "not-captured (direct fake connection)",
    );
    Ok(())
}

async fn patch_behavior(ports: Ports) -> Result<(), String> {
    patch_normal(ports, true).await?;
    patch_normal(ports, false).await?;
    patch_reject(ports, "reject-nonexistent", BAD_MAILBOX, None, None).await?;
    patch_reject(ports, "reject-bad-flags", MAILBOX, Some(r"(\Bogus)"), None).await?;
    patch_reject(ports, "reject-bad-date", MAILBOX, None, Some("not-a-date")).await?;
    patch_stale_select(ports, false).await?;
    patch_stale_select(ports, true).await?;
    patch_fake("fake-eof", FakeMode::Eof).await?;
    patch_fake("fake-own-no", FakeMode::OwnNo).await?;
    patch_fake("fake-stale-continuation", FakeMode::StaleContinue).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal reproduction, deliberately not part of any ladder.
// ---------------------------------------------------------------------------

async fn search_unselected(ports: Ports) -> Result<(), String> {
    let mut env = setup_tee(ports).await?;
    let subject = format!("s-{}-search-unselected", env.run_id);
    let first = search_subject(&mut env.session, &subject, OP_TIMEOUT).await;
    println!(
        "SEARCH-UNSELECTED phase=before-select search={:?} raw={:?} seqs={:?} ms={}",
        first.0, first.2, first.1, first.3
    );
    let select = select_inbox(&mut env.session, OP_TIMEOUT).await;
    println!(
        "SEARCH-UNSELECTED phase=select select={:?} raw={:?} ms={}",
        select.0, select.1, select.2
    );
    let second = search_subject(&mut env.session, &subject, OP_TIMEOUT).await;
    println!(
        "SEARCH-UNSELECTED phase=after-select search={:?} raw={:?} seqs={:?} ms={}",
        second.0, second.2, second.1, second.3
    );
    println!(
        "SEARCH-UNSELECTED expected=before-select Ok(0) despite GreenMail BAD; after-select Ok(0) is a genuine empty result"
    );
    Ok(())
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
        "chain" => {
            run_chain(ports, rep, false);
            Ok(())
        }
        "chain-reverse" => {
            run_chain(ports, rep, true);
            Ok(())
        }
        "ladder" => {
            run_ladder(ports, rep);
            Ok(())
        }
        "patch-behavior" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build patch-behavior runtime");
            rt.block_on(patch_behavior(ports))
        }
        "search-unselected" => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build search-unselected runtime");
            rt.block_on(search_unselected(ports))
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

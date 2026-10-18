//! Consequence check for the residual SMTP reply described in RESCAN.md,
//! section 6.6. A pooled lettre connection whose `send` was cancelled early can
//! keep one unread server reply; the next `send` then reads every reply one
//! command late. This experiment asks whether that makes a failure reply
//! invisible to the caller.
//!
//! Setup: a minimal in-process SMTP server that can reject a recipient at RCPT
//! (`550`) or a message after DATA (`554`), and records which subjects it
//! stored or rejected. The client is lettre 0.11.23 through its pool. The SMTP
//! connection goes through `dropwise::net::Proxy`, so the scenario can record
//! the unread bytes before the first survivor send. The cancelled message A is
//! 16 bytes, like L1 in RESCAN.md; the survivor B is the message whose failure
//! we care about, and C is a normal message sent afterwards.
//!
//! Two variants:
//! * `PostDataReject`: B is accepted at RCPT and DATA, then rejected after its
//!   body. On a clean connection `send(B)` returns `Err(554)`.
//! * `RcptReject`: B's recipient is rejected at RCPT. On a clean connection
//!   `send(B)` returns `Err(550)`.
//!
//! The judged observation contains B's result, whether the server stored or
//! rejected B, and C's result. The residue is recorded separately as
//! diagnostic evidence, so the residue alone does not count as a consequence.
//! `explore_observed(Expect::Atomic)`
//! compares each cancellation plan with the same scenario where the target was
//! skipped (`pre`) or completed (`post`). A plan where B returns `Ok` while the
//! server rejected it is neither, and is reported as a violation.
//!
//! Linux only and only with `net`: the residue check uses /proc.

#![cfg(all(feature = "net", target_os = "linux"))]

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use dropwise::net::Proxy;
use dropwise::{
    explore_observed, Config, Ctx, Expect, Flavor, Race, Report, Settle, Verdict,
};
use lettre::transport::smtp::PoolConfig;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// Subject the server is configured to reject after DATA.
const REJECT_SUBJECT: &str = "reject-after-data";

/// Pause before every command reply. A loopback server otherwise answers
/// before the client polls, so the number of Pending boundaries (and therefore
/// the explored plan set) would vary with scheduling. This is the same
/// "slow in-process server" Pending source used elsewhere in the scans.
const REPLY_DELAY: Duration = Duration::from_millis(10);

#[derive(Default)]
struct ServerLog {
    /// Subjects of messages the server accepted after DATA.
    stored: Vec<String>,
    /// Subjects (or `rcpt`) the server rejected.
    rejected: Vec<String>,
}

struct Server {
    addr: std::net::SocketAddr,
    log: std::sync::Arc<std::sync::Mutex<ServerLog>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A small SMTP server: enough commands for lettre's plain, unauthenticated
/// transport. It rejects `reject@example.com` at RCPT and any message whose
/// subject is `reject-after-data` after DATA.
async fn start_server() -> Result<Server, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(e)?;
    let addr = listener.local_addr().map_err(e)?;
    let log = std::sync::Arc::new(std::sync::Mutex::new(ServerLog::default()));
    let task_log = log.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else { break };
            let log = task_log.clone();
            tokio::spawn(async move { serve(socket, log).await });
        }
    });
    Ok(Server { addr, log, task })
}

async fn send_reply<W: tokio::io::AsyncWrite + Unpin>(write: &mut W, bytes: &[u8]) {
    tokio::time::sleep(REPLY_DELAY).await;
    let _ = write.write_all(bytes).await;
}

async fn serve(socket: TcpStream, log: std::sync::Arc<std::sync::Mutex<ServerLog>>) {
    let (read, mut write) = socket.into_split();
    let mut reader = BufReader::new(read);
    let _ = write.write_all(b"220 localhost ESMTP\r\n").await;
    let mut line = String::new();
    let mut in_data = false;
    let mut accepted_rcpt = false;
    let mut body = Vec::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if in_data {
            if line == ".\r\n" {
                in_data = false;
                let subject = subject_of(&body);
                let reject = subject == REJECT_SUBJECT;
                let reply: &[u8] = if reject {
                    log.lock().unwrap().rejected.push(subject);
                    b"554 5.7.1 rejected after DATA\r\n"
                } else {
                    log.lock().unwrap().stored.push(subject);
                    b"250 2.0.0 queued\r\n"
                };
                send_reply(&mut write, reply).await;
                body.clear();
                accepted_rcpt = false;
                continue;
            }
            body.extend_from_slice(line.as_bytes());
            continue;
        }
        match line.get(..4).unwrap_or("").to_ascii_uppercase().as_str() {
            "EHLO" | "HELO" => {
                send_reply(&mut write, b"250 localhost\r\n").await;
            }
            "MAIL" => {
                accepted_rcpt = false;
                send_reply(&mut write, b"250 2.1.0 Ok\r\n").await;
            }
            "RCPT" => {
                if line.to_ascii_lowercase().contains("reject@example.com") {
                    log.lock().unwrap().rejected.push("rcpt".into());
                    send_reply(&mut write, b"550 5.1.1 no such user\r\n").await;
                } else {
                    accepted_rcpt = true;
                    send_reply(&mut write, b"250 2.1.5 Ok\r\n").await;
                }
            }
            "DATA" => {
                if accepted_rcpt {
                    in_data = true;
                    send_reply(&mut write, b"354 End data with <CR><LF>.<CR><LF>\r\n").await;
                } else {
                    send_reply(&mut write, b"503 5.5.1 no valid recipients\r\n").await;
                }
            }
            "NOOP" => {
                send_reply(&mut write, b"250 2.0.0 Ok\r\n").await;
            }
            "RSET" => {
                accepted_rcpt = false;
                send_reply(&mut write, b"250 2.0.0 Ok\r\n").await;
            }
            "QUIT" => {
                send_reply(&mut write, b"221 2.0.0 Bye\r\n").await;
                return;
            }
            _ => {
                send_reply(&mut write, b"500 5.5.1 unknown command\r\n").await;
            }
        }
    }
}

fn subject_of(body: &[u8]) -> String {
    String::from_utf8_lossy(body)
        .lines()
        .find_map(|line| line.strip_prefix("Subject: "))
        .map(|subject| subject.trim().to_string())
        .unwrap_or_default()
}

fn mailer(addr: std::net::SocketAddr) -> AsyncSmtpTransport<Tokio1Executor> {
    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(addr.ip().to_string())
        .port(addr.port())
        .pool_config(PoolConfig::new().max_size(1))
        .build()
}

fn email(to: &str, subject: &str, body_len: usize) -> Message {
    Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to(to.parse().unwrap())
        .subject(subject)
        .body("x".repeat(body_len))
        .unwrap()
}

/// A send under a 3 s bound. `Ok` carries the response code, so a wrong code
/// (354 instead of 250) is part of the observation.
async fn send_once(
    mailer: &AsyncSmtpTransport<Tokio1Executor>,
    message: Message,
) -> Result<u16, String> {
    match tokio::time::timeout(Duration::from_secs(3), mailer.send(message)).await {
        Ok(Ok(response)) => Ok(u16::from(response.code())),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("hung".into()),
    }
}

#[derive(Clone, Copy)]
enum Variant {
    /// B is rejected after its body (`554`).
    PostDataReject,
    /// B's recipient is rejected at RCPT (`550`).
    RcptReject,
}

/// Diagnostic evidence of the misalignment: residue seen before B, paired
/// with B's result. It is recorded on the side and deliberately *not* part of
/// the judged observation, so a residue alone does not count as a consequence.
static RESIDUE_SAMPLES: Mutex<BTreeSet<(usize, Result<u16, String>)>> =
    Mutex::new(BTreeSet::new());

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// B's send result: response code, error string, or `hung`.
    b: Result<u16, String>,
    b_stored: bool,
    b_rejected: bool,
    /// C is a normal message sent after B.
    c: Result<u16, String>,
    c_stored: bool,
}

async fn scenario(ctx: Ctx, variant: Variant) -> Result<Obs, String> {
    let server = start_server().await?;
    let proxy = Proxy::start(server.addr).await.map_err(e)?;
    let mailer = mailer(proxy.addr());
    // Warm one pooled connection.
    mailer.send(email("warm@example.com", "W", 1)).await.map_err(e)?;
    // A is the cancelled message; 16 bytes keeps the shape of L1.
    if let Some(result) = ctx.target(mailer.send(email("a@example.com", "A", 16))).await {
        result.map_err(e)?;
    }
    // Let a reply already on the wire reach the client buffer, then measure it
    // before the first survivor send reuses the connection.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let residue = proxy
        .residue()
        .map_err(e)?
        .into_iter()
        .map(|entry| entry.bytes)
        .max()
        .unwrap_or(0);

    let (to, subject) = match variant {
        Variant::PostDataReject => ("b@example.com", REJECT_SUBJECT),
        Variant::RcptReject => ("reject@example.com", "B-rcpt"),
    };
    let b = send_once(&mailer, email(to, subject, 16)).await;
    RESIDUE_SAMPLES.lock().unwrap().insert((residue, b.clone()));
    let c = send_once(&mailer, email("c@example.com", "C", 16)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (stored, rejected) = {
        let log = server.log.lock().unwrap();
        (log.stored.clone(), log.rejected.clone())
    };
    Ok(Obs {
        b,
        b_stored: stored.iter().any(|stored| stored == subject),
        b_rejected: !rejected.is_empty(),
        c,
        c_stored: stored.iter().any(|stored| stored == "C"),
    })
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_runs: 128,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}

fn summary(id: &str, report: &Report) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} \
         verdicts(all runs) pre={} post={} mixed={} neither={} unknown={}",
        report.trials.len(),
        report.violations().count(),
        report.unrealized().count(),
        report.exhaustive,
        report.baseline_errors.len(),
        report.verdict_count(Verdict::Pre),
        report.verdict_count(Verdict::Post),
        report.verdict_count(Verdict::Mixed),
        report.verdict_count(Verdict::Neither),
        report.verdict_count(Verdict::Unknown),
    )
}

fn run(id: &str, variant: Variant) {
    RESIDUE_SAMPLES.lock().unwrap().clear();
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario(ctx, variant));
    println!("{report}");
    let variant_label = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("{id} [{variant_label}]"), &report));
    let samples = RESIDUE_SAMPLES.lock().unwrap();
    let nonzero: Vec<_> = samples.iter().filter(|(residue, _)| *residue > 0).collect();
    println!(
        "RESIDUE {id} [{variant_label}]: nonzero_runs={} (residue, B result)={nonzero:?}",
        nonzero.len()
    );
}

/// The dangerous case: on a clean connection B returns `Err(554)`; on a
/// misaligned connection it may return `Ok(354)` even though the server
/// rejected it.
#[test]
fn reject_after_data() {
    run("M1 lettre reject-after-data", Variant::PostDataReject);
}

/// The case from the original speculation: B's RCPT is rejected with 550. The
/// 550 is read one command later, but the API may still return an error.
#[test]
fn reject_at_rcpt() {
    run("M2 lettre reject-at-RCPT", Variant::RcptReject);
}

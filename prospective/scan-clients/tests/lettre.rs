//! lettre 0.11.23 pooled async SMTP transport against Mailpit (RESCAN.md, section 6).
//! Prints one RESCAN line per experiment; no outcome asserts.
//!
//! A cancelled `send` drops its `PooledConnection`, which recycles the connection
//! unless `has_broken()`; the flag is set only when a command fails. Before reuse
//! the pool sends NOOP and accepts any positive reply. Survivor: two more emails
//! through the same pool; independent observer: the subjects Mailpit stored.
//! Environment: DW_SMTP_PORT, DW_SMTP_API_PORT (loopback Mailpit), SCAN_VARIANT (label only).

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{explore_observed, Config, Ctx, Expect, Flavor, Race, Report, Settle, Verdict};
use lettre::transport::smtp::PoolConfig;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn port(var: &str) -> u16 {
    std::env::var(var).unwrap_or_else(|_| panic!("{var} not set")).parse().unwrap()
}

/// Minimal HTTP/1.1 request to Mailpit's API; returns the response body.
async fn api(method: &str, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port("DW_SMTP_API_PORT"))).await.unwrap();
    let req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default()
}

static RUN: AtomicUsize = AtomicUsize::new(0);

/// Subjects of the stored messages of run `run` (tagged `<letter>-<pid>-<run>`),
/// without the tag, sorted. Earlier runs' late messages are ignored.
async fn stored_subjects(run: usize) -> Vec<String> {
    let body = api("GET", "/api/v1/messages?limit=500").await;
    let tag = format!("-{}-{run}", std::process::id());
    let mut subjects: Vec<String> = body
        .split("\"Subject\":\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|subject| subject.strip_suffix(&tag).map(str::to_string))
        .collect();
    subjects.sort();
    subjects
}

fn email(to: &str, subject: &str, body_len: usize) -> Message {
    Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to(to.parse().unwrap())
        .subject(subject)
        .body("x".repeat(body_len))
        .unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// What lettre reported for the two survivor emails.
    reported: Vec<&'static str>,
    /// What the server stored (independent observer).
    stored: Vec<String>,
}

async fn scenario(ctx: Ctx, body_len: usize) -> Result<Obs, String> {
    let run = RUN.fetch_add(1, SeqCst);
    let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(port("DW_SMTP_PORT"))
        .pool_config(PoolConfig::new().max_size(1))
        .build();
    // Warm one pooled connection.
    mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
    if let Some(r) = ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await {
        r.map_err(|e| e.to_string())?;
    }
    // Let the dropped connection be recycled before the next send.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut reported = Vec::new();
    for (to, subject) in [("b@example.com", "B"), ("c@example.com", "C")] {
        let sent = tokio::time::timeout(Duration::from_secs(3), mailer.send(email(to, &tagged(subject), 16))).await;
        reported.push(match sent {
            Ok(Ok(_)) => "ok",
            Ok(Err(_)) => "error",
            Err(_) => "hung",
        });
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stored = stored_subjects(run).await;
    Ok(Obs { reported, stored })
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

/// One line per experiment, for the rescan log. The field names keep the old
/// `allowed.rs` format; the counts now come from `Report` (trial verdicts only).
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

fn rescan(id: &str, body_len: usize) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario(ctx, body_len));
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("{id} [{variant}]"), &report));
}

#[test]
fn l1_send_cancelled_small_message() {
    rescan("L1 lettre pooled send, 16-byte body", 16);
}

#[test]
fn l2_send_cancelled_large_message() {
    // Large enough that writing the body itself returns Pending.
    rescan("L2 lettre pooled send, 8 MiB body", 8 << 20);
}

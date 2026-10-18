//! New dimension: residual bytes in the pooled client socket before the
//! survivor runs (RESCAN.md, section 6). The Mailpit SMTP connection goes
//! through `dropwise::net::Proxy`, and after the cancelled `send` the test asks
//! the proxy how many bytes are still unread in the client's receive buffer,
//! before B and C are attempted.
//!
//! This is a separate experiment, not part of the L1/L2 control group. Body
//! sizes mirror the control group: L1 (16-byte body, cancelling inside the
//! `354` wait) and L2 (8 MiB body, where writing the body itself also returns
//! Pending). Linux only, and only with the `net` feature. Environment:
//! DW_SMTP_PORT, DW_SMTP_API_PORT, SCAN_VARIANT (label only).

#![cfg(all(feature = "net", target_os = "linux"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use dropwise::net::Proxy;
use dropwise::{
    explore_observed, Config, Ctx, Expect, Flavor, Race, Report, Settle, Verdict,
};
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
    /// Bytes still unread in the client socket(s) before the survivors start.
    residue: usize,
    reported: Vec<&'static str>,
    stored: Vec<String>,
}

async fn scenario(ctx: Ctx, body_len: usize, max_residue: Arc<AtomicUsize>) -> Result<Obs, String> {
    let run = RUN.fetch_add(1, SeqCst);
    let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());

    let upstream = SocketAddr::from(([127, 0, 0, 1], port("DW_SMTP_PORT")));
    let proxy = Proxy::start(upstream).await.map_err(|e| e.to_string())?;
    let proxy_addr = proxy.addr();
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(proxy_addr.ip().to_string())
        .port(proxy_addr.port())
        .pool_config(PoolConfig::new().max_size(1))
        .build();
    mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
    if let Some(r) = ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await {
        r.map_err(|e| e.to_string())?;
    }
    // Let a reply that is already on the wire reach the client buffer, then
    // inspect it before any survivor reuses the connection.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let residue = proxy
        .residue()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|entry| entry.bytes)
        .max()
        .unwrap_or(0);
    max_residue.fetch_max(residue, SeqCst);

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
    Ok(Obs { residue, reported, stored })
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

/// One line per experiment. The field names keep the rescan format; the counts
/// come from `Report` (trial verdicts only).
fn summary(id: &str, report: &Report, max_residue: usize) -> String {
    format!(
        "RESCAN {id}: plans={} violations={} unrealized={} exhaustive={} baseline_errors={} verdicts(all runs) pre={} post={} neither={} unknown={} max_residue={max_residue}",
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

#[test]
fn l1_residue_before_survivors() {
    let max_residue = Arc::new(AtomicUsize::new(0));
    let seen = max_residue.clone();
    let report = explore_observed(&config(), Expect::Atomic, |ctx| {
        let seen = seen.clone();
        scenario(ctx, 16, seen)
    });
    println!("new dimension: residue before survivors, via dropwise::net::Proxy");
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!(
        "{}",
        summary(
            &format!("L1-residue lettre pooled send [{variant}]"),
            &report,
            max_residue.load(SeqCst)
        )
    );
}

#[test]
fn l2_residue_before_survivors() {
    let max_residue = Arc::new(AtomicUsize::new(0));
    let seen = max_residue.clone();
    let report = explore_observed(&config(), Expect::Atomic, |ctx| {
        let seen = seen.clone();
        scenario(ctx, 8 * 1024 * 1024, seen)
    });
    println!("new dimension: residue before survivors, via dropwise::net::Proxy");
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!(
        "{}",
        summary(
            &format!("L2-residue lettre pooled send [{variant}]"),
            &report,
            max_residue.load(SeqCst)
        )
    );
}

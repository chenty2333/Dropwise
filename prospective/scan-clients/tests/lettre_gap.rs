//! New dimension: the schedulable gap between a cancelled pooled send and the
//! next reuse (RESCAN.md, section 6). L1's fixed `sleep(50ms)` is replaced by
//! `ctx.gap(Gap::Sleep(50ms))`, and `Config::gap_choices` explores
//! `[Now, Yield(1), Sleep(50ms)]`.
//!
//! This is a separate experiment, not part of the L1/L2 control group. It uses
//! the same Mailpit setup and run tagging; environment: DW_SMTP_PORT,
//! DW_SMTP_API_PORT, SCAN_VARIANT (label only).

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{
    explore_observed, Config, Ctx, Expect, Flavor, Gap, Race, Report, Settle, Verdict,
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
    reported: Vec<&'static str>,
    stored: Vec<String>,
}

async fn scenario(ctx: Ctx, body_len: usize) -> Result<Obs, String> {
    let run = RUN.fetch_add(1, SeqCst);
    let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
        .port(port("DW_SMTP_PORT"))
        .pool_config(PoolConfig::new().max_size(1))
        .build();
    mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
    if let Some(r) = ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await {
        r.map_err(|e| e.to_string())?;
    }
    // The schedulable gap replaces L1's fixed `sleep(50ms)`.
    ctx.gap(Gap::Sleep(Duration::from_millis(50))).await;
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
        gap_choices: vec![Gap::Now, Gap::Yield(1), Gap::Sleep(Duration::from_millis(50))],
        ..Config::default()
    }
}

/// One line per experiment. The field names keep the rescan format; the counts
/// come from `Report` (trial verdicts only).
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

#[test]
fn l1_gap_send_cancelled_small_message() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario(ctx, 16));
    println!("new dimension: gap choices [Now, Yield(1), Sleep(50ms)]");
    println!("{report}");
    let variant = std::env::var("SCAN_VARIANT").unwrap_or_else(|_| "release".into());
    println!("{}", summary(&format!("L1-gap lettre pooled send [{variant}]"), &report));
}

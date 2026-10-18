//! Cancellation-ablation ladder for the two lettre scenarios (RESCAN.md,
//! section 6.7): B1 judges only the cancellation, B2 retries the same message,
//! B3 sends a different message and checks only `Ok`, M is the existing
//! `explore_observed(Expect::Atomic)` survivor plus the stored-subject
//! observer, and M-residue adds the unread bytes in the pooled client socket.
//!
//! Every level keeps the fixed 50 ms sleep before a reuse and otherwise uses
//! the same scenario skeleton and `Config` as `lettre.rs`. M-residue copies
//! `lettre_residue.rs` and is Linux-only behind the `net` feature. Environment:
//! DW_SMTP_PORT, DW_SMTP_API_PORT (loopback Mailpit), SCAN_VARIANT (label only).

#[path = "../../support/ladder.rs"]
mod ladder;

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
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

fn mailer(host: &str, port: u16) -> AsyncSmtpTransport<Tokio1Executor> {
    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(port)
        .pool_config(PoolConfig::new().max_size(1))
        .build()
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

fn b1(id: &str, body_len: usize) {
    let report = explore(&config(), move |ctx| async move {
        let run = RUN.fetch_add(1, SeqCst);
        let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());
        let mailer = mailer("127.0.0.1", port("DW_SMTP_PORT"));
        // Warm one pooled connection.
        mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
        // Ignore a completed target's result: B1 judges only the cancellation.
        let _ = ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await;
        Ok(())
    });
    ladder::simple("B1", id, &report);
}

fn b2(id: &str, body_len: usize) {
    let report = explore(&config(), move |ctx| async move {
        let run = RUN.fetch_add(1, SeqCst);
        let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());
        let mailer = mailer("127.0.0.1", port("DW_SMTP_PORT"));
        mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
        match ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await {
            Some(result) => {
                result.map_err(|e| e.to_string())?;
            }
            None => {
                // Let the dropped connection be recycled, then retry the same message.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let sent = tokio::time::timeout(
                    Duration::from_secs(3),
                    mailer.send(email("a@example.com", &tagged("A"), body_len)),
                )
                .await;
                match sent {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => return Err(e.to_string()),
                    Err(_) => return Err("retry of cancelled send timed out".into()),
                }
            }
        }
        Ok(())
    });
    ladder::simple("B2", id, &report);
}

fn b3(id: &str, body_len: usize) {
    let report = explore(&config(), move |ctx| async move {
        let run = RUN.fetch_add(1, SeqCst);
        let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());
        let mailer = mailer("127.0.0.1", port("DW_SMTP_PORT"));
        mailer.send(email("warm@example.com", &tagged("W"), 1)).await.map_err(|e| e.to_string())?;
        match ctx.target(mailer.send(email("a@example.com", &tagged("A"), body_len))).await {
            Some(result) => {
                result.map_err(|e| e.to_string())?;
            }
            None => {
                // A different message of the same kind; only `Ok` is judged.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let sent = tokio::time::timeout(
                    Duration::from_secs(3),
                    mailer.send(email("b@example.com", &tagged("B"), 16)),
                )
                .await;
                match sent {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => return Err(e.to_string()),
                    Err(_) => return Err("different send after cancellation timed out".into()),
                }
            }
        }
        Ok(())
    });
    ladder::simple("B3", id, &report);
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
    let mailer = mailer("127.0.0.1", port("DW_SMTP_PORT"));
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

fn m(id: &str, body_len: usize) {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| scenario(ctx, body_len));
    ladder::observed("M", id, &report);
}

#[cfg(all(feature = "net", target_os = "linux"))]
mod residue {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use dropwise::net::Proxy;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Obs {
        /// Bytes still unread in the client socket(s) before the survivors start.
        residue: usize,
        reported: Vec<&'static str>,
        stored: Vec<String>,
    }

    pub async fn scenario(ctx: Ctx, body_len: usize, max_residue: Arc<AtomicUsize>) -> Result<Obs, String> {
        let run = RUN.fetch_add(1, SeqCst);
        let tagged = |letter: &str| format!("{letter}-{}-{run}", std::process::id());

        let upstream = SocketAddr::from(([127, 0, 0, 1], port("DW_SMTP_PORT")));
        let proxy = Proxy::start(upstream).await.map_err(|e| e.to_string())?;
        let proxy_addr = proxy.addr();
        let mailer = mailer(&proxy_addr.ip().to_string(), proxy_addr.port());
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
}

#[test]
fn l1_b1() {
    b1("L1", 16);
}

#[test]
fn l1_b2() {
    b2("L1", 16);
}

#[test]
fn l1_b3() {
    b3("L1", 16);
}

#[test]
fn l1_m() {
    m("L1", 16);
}

#[cfg(all(feature = "net", target_os = "linux"))]
#[test]
fn l1_m_residue() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    let max_residue = Arc::new(AtomicUsize::new(0));
    let seen = max_residue.clone();
    let report = explore_observed(&config(), Expect::Atomic, |ctx| {
        residue::scenario(ctx, 16, seen.clone())
    });
    // The same scenario as lettre_residue.rs, with the residue in the observation.
    println!(
        "LADDER-RESIDUE M-residue L1 [{}]: max_residue={}",
        ladder::variant(),
        max_residue.load(SeqCst)
    );
    ladder::observed("M-residue", "L1", &report);
}

#[test]
fn l2_b1() {
    // Large enough that writing the body itself returns Pending.
    b1("L2", 8 << 20);
}

#[test]
fn l2_b2() {
    b2("L2", 8 << 20);
}

#[test]
fn l2_b3() {
    b3("L2", 8 << 20);
}

#[test]
fn l2_m() {
    m("L2", 8 << 20);
}

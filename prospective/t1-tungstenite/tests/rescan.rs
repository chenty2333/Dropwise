//! Post-hoc rescan of T1 (tokio-tungstenite 0.30.0), see prospective/RESCAN.md.
//! Separate from the frozen study; prints one RESCAN line per experiment.
//!
//! The frozen scenarios cancelled only `next()` and judged only what the
//! reader itself received. Here the peer is the independent observer, and the
//! targets include sending (no documented guarantee) and the handshake.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::task::{Context, Poll};
use std::time::Duration;

use dropwise::{Config, Ctx, Expect, Race, Report, Verdict, explore_observed};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::{WebSocketStream, accept_async, client_async};

fn config() -> Config {
    Config {
        races: vec![Race::Immediate, Race::AfterWake],
        scenario_timeout: Duration::from_secs(10),
        ..Config::default()
    }
}

fn label(m: &Message) -> Option<String> {
    match m {
        Message::Text(t) => Some(format!("{}x{}", t.chars().next().unwrap_or('-'), t.len())),
        Message::Binary(b) => Some(format!("bin{}", b.len())),
        _ => None,
    }
}

/// The peer's view: data messages in order, pong payloads, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Peer {
    data: Vec<String>,
    pongs: Vec<String>,
    error: Option<String>,
}

async fn observe_peer(mut ws: WebSocketStream<DuplexStream>) -> Peer {
    let mut peer = Peer { data: Vec::new(), pongs: Vec::new(), error: None };
    while let Some(item) = ws.next().await {
        match item {
            Ok(Message::Pong(p)) => peer.pongs.push(String::from_utf8_lossy(&p).into_owned()),
            Ok(m) => peer.data.extend(label(&m)),
            Err(e) => {
                peer.error = Some(e.to_string());
                break;
            }
        }
    }
    peer
}

async fn close<S: AsyncRead + AsyncWrite + Unpin>(mut ws: WebSocketStream<S>) {
    let _ = ws.close(None).await;
    while let Some(Ok(_)) = ws.next().await {}
}

/// R1.1 / R1.2: `send` of a 300-byte message over a 32-byte pipe, so the write
/// returns Pending until the peer reads. After a cancelled send the caller
/// either retries it (R1.1) or gives up (R1.2), then sends a second message.
async fn send_scenario(ctx: Ctx, retry: bool, silent: Arc<AtomicUsize>) -> Result<Peer, String> {
    let (client_io, server_io) = tokio::io::duplex(32);
    let peer = tokio::spawn(async move {
        observe_peer(WebSocketStream::from_raw_socket(server_io, Role::Server, None).await).await
    });
    let mut ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    let first = Message::text("a".repeat(300));
    let sent = ctx.target(ws.send(first.clone())).await;
    let cancelled = sent.is_none();
    match sent {
        Some(r) => r.map_err(|e| format!("send: {e}"))?,
        None if retry => ws.send(first).await.map_err(|e| format!("retry: {e}"))?,
        None => {}
    }
    ws.send(Message::text("b".repeat(300))).await.map_err(|e| format!("second send: {e}"))?;
    close(ws).await;
    let peer = peer.await.map_err(|e| e.to_string())?;
    // The caller was told "cancelled" (no retry), yet the peer got the first
    // message. This is also false in the `pre` calibration, where the target is
    // skipped before its first poll and nothing was written.
    if cancelled && !retry && peer.data.iter().any(|m| m == "ax300") {
        silent.fetch_add(1, SeqCst);
    }
    Ok(peer)
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

fn send_rescan(id: &str, retry: bool) {
    let expect = if retry { Expect::Completed } else { Expect::Atomic };
    let silent_commits = Arc::new(AtomicUsize::new(0));
    let report = explore_observed(&config(), expect, |ctx| {
        let silent = silent_commits.clone();
        send_scenario(ctx, retry, silent)
    });
    println!("{report}");
    println!(
        "{} cancelled-but-delivered={}",
        summary(id, &report),
        silent_commits.load(SeqCst)
    );
}

#[test]
fn r1_1_send_cancelled_then_retried() {
    send_rescan("R1.1 send+retry", true);
}

#[test]
fn r1_2_send_cancelled_then_abandoned() {
    send_rescan("R1.2 send+abandon", false);
}

/// Client transport whose every write first returns Pending once, so the pong
/// that `next()` queues after a ping is flushed across a Pending boundary.
struct Stutter {
    inner: DuplexStream,
    ready: bool,
}

impl AsyncRead for Stutter {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stutter {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if !self.ready {
            self.ready = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.ready = false;
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// What both sides saw in R1.3.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PingPong {
    client_data: Vec<String>,
    peer: Peer,
}

/// R1.3: the reader keeps reading after every cancelled `next()` (C1's own
/// usage). "Never read" is not a state this caller allows.
async fn pong_scenario(ctx: Ctx) -> Result<PingPong, String> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let mut ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        for m in [Message::Ping("p1".into()), Message::text("m1"), Message::Ping("p2".into()), Message::text("m2")] {
            if ws.send(m).await.is_err() {
                break;
            }
        }
        observe_peer(ws).await
    });
    let mut ws = WebSocketStream::from_raw_socket(Stutter { inner: client_io, ready: false }, Role::Client, None).await;
    let mut client_data = Vec::new();
    while client_data.len() < 2 {
        match ctx.target(ws.next()).await {
            Some(Some(Ok(m))) => client_data.extend(label(&m)),
            Some(Some(Err(e))) => return Err(format!("next: {e}")),
            Some(None) => break,
            None => {} // cancelled: keep reading, as C1 describes
        }
    }
    close(ws).await;
    let peer = peer.await.map_err(|e| e.to_string())?;
    Ok(PingPong { client_data, peer })
}

#[test]
fn r1_3_pong_flush_across_cancelled_next() {
    let report = explore_observed(&config(), Expect::Completed, pong_scenario);
    println!("{report}");
    println!("{}", summary("R1.3 pong", &report));
}

/// What the second client and the server saw in R1.4.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Served {
    second_client_echo: Result<String, String>,
    handshakes_accepted: usize,
}

/// R1.4: a sequential accept loop (one connection at a time, as simple servers
/// do). The first client's handshake is the target; the second client must
/// still be served.
async fn handshake_scenario(ctx: Ctx) -> Result<Served, String> {
    let (connections, mut incoming) = tokio::sync::mpsc::unbounded_channel::<DuplexStream>();
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let server = tokio::spawn(async move {
        while let Some(io) = incoming.recv().await {
            let Ok(mut ws) = accept_async(io).await else { continue };
            server_accepted.fetch_add(1, SeqCst);
            while let Some(Ok(m)) = ws.next().await {
                if m.is_text() && ws.send(m).await.is_err() {
                    break;
                }
            }
        }
    });
    let (first, first_server) = tokio::io::duplex(64 * 1024);
    connections.send(first_server).map_err(|_| "server gone")?;
    if let Some(r) = ctx.target(client_async("ws://localhost/", first)).await {
        let (ws, _) = r.map_err(|e| format!("first handshake: {e}"))?;
        close(ws).await;
    }
    let (second, second_server) = tokio::io::duplex(64 * 1024);
    connections.send(second_server).map_err(|_| "server gone")?;
    let second_client_echo = async {
        let (mut ws, _) = client_async("ws://localhost/", second).await.map_err(|e| e.to_string())?;
        ws.send(Message::text("hello")).await.map_err(|e| e.to_string())?;
        let reply = match ws.next().await {
            Some(Ok(m)) => m.into_text().map(|t| t.to_string()).map_err(|e| e.to_string()),
            other => Err(format!("{other:?}")),
        };
        close(ws).await;
        reply
    }
    .await;
    drop(connections);
    server.await.map_err(|e| e.to_string())?;
    Ok(Served { second_client_echo, handshakes_accepted: accepted.load(SeqCst) })
}

#[test]
fn r1_4_handshake_cancelled() {
    let report = explore_observed(&config(), Expect::Atomic, handshake_scenario);
    println!("{report}");
    println!("{}", summary("R1.4 handshake", &report));
}

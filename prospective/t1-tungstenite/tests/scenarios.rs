//! Pre-registered scenarios S1.1-S1.6 for T1 (tokio-tungstenite 0.30.0), contract C1:
//! a reader that drops `next()` futures at arbitrary Pending boundaries and keeps reading
//! receives exactly the data messages the peer sent, in order, with no loss or duplication,
//! and the stream stays usable.
//!
//! Written before the first run (PREREGISTRATION.md, section 4). Tests do not assert on the
//! outcome; each writes the full Dropwise report to runs/t1/<scenario>/<RUN>.txt for triage.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use dropwise::{explore, Config, Ctx, Race, Report};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};

/// Frozen configuration (PREREGISTRATION.md, section 4.2).
fn config() -> Config {
    Config {
        races: vec![Race::Immediate, Race::Reschedule(1), Race::After(Duration::from_millis(1))],
        max_cancellations: 1,
        scenario_timeout: Duration::from_secs(10),
        ..Config::default()
    }
}

fn record(scenario: &str, report: &Report) {
    let run = std::env::var("RUN").unwrap_or_else(|_| "dev".into());
    let dir = format!("{}/../runs/t1/{scenario}", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/{run}.txt"), report.to_string()).unwrap();
    println!("{scenario}: {} plans, {} violations, {} unrealized, exhaustive={}\n{report}",
        report.trials.len(), report.violations().count(), report.unrealized().count(), report.exhaustive);
}

/// Server-side transport that writes one byte per call and yields in between, so the
/// client sees every frame split across many reads (S1.4).
struct Trickle {
    inner: DuplexStream,
    ready: bool,
}

impl AsyncRead for Trickle {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Trickle {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        if !self.ready {
            self.ready = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.ready = false;
        Pin::new(&mut self.inner).poll_write(cx, &buf[..buf.len().min(1)])
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Clone)]
struct Plan {
    /// What the server sends, in order (data, control and raw frames).
    send: Vec<Message>,
    /// The data messages the client must receive, in order.
    expect: Vec<Message>,
    close: bool,
    trickle: bool,
}

fn is_data(m: &Message) -> bool {
    matches!(m, Message::Text(_) | Message::Binary(_))
}

async fn scenario(ctx: Ctx, plan: Plan) -> Result<(), String> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let send = plan.send.clone();
    let close = plan.close;
    let trickle = plan.trickle;
    tokio::spawn(async move {
        macro_rules! serve {
            ($io:expr) => {{
                let mut ws = WebSocketStream::from_raw_socket($io, Role::Server, None).await;
                for m in send {
                    if ws.send(m).await.is_err() {
                        return;
                    }
                }
                if close {
                    let _ = ws.close(None).await;
                    while let Some(Ok(_)) = ws.next().await {}
                } else {
                    let _ = done_rx.await; // keep the connection open until the client is done
                }
            }};
        }
        if trickle { serve!(Trickle { inner: server_io, ready: false }) } else { serve!(server_io) }
    });

    let mut ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    let mut got: Vec<Message> = Vec::new();
    loop {
        if !plan.close && got.len() == plan.expect.len() {
            break;
        }
        let (next, mut preempt) = ctx.race(ws.next());
        tokio::select! {
            biased;
            item = next => match item {
                Some(Ok(m)) if is_data(&m) => got.push(m),
                Some(Ok(_)) => {} // ping / pong / close / frame: not data
                Some(Err(e)) => return Err(format!("next() returned an error after {} data messages: {e}", got.len())),
                None => break,
            },
            _ = &mut preempt => {}
        }
    }
    let _ = done_tx.send(());
    if got != plan.expect {
        let summary = |v: &[Message]| v.iter().map(|m| format!("{}B", m.len())).collect::<Vec<_>>().join(",");
        return Err(format!("received [{}], expected [{}]", summary(&got), summary(&plan.expect)));
    }
    Ok(())
}

fn text(i: usize) -> Message {
    Message::text(format!("message-{i}"))
}

#[test]
fn s1_1_small_messages() {
    let mut send = Vec::new();
    for i in 0..6 {
        send.push(text(i));
        send.push(Message::binary(vec![i as u8; 16]));
    }
    let plan = Plan { expect: send.clone(), send, close: false, trickle: false };
    record("s1_1", &explore(&config(), |c| scenario(c, plan.clone())));
}

#[test]
fn s1_2_large_messages() {
    let send = vec![Message::binary(vec![0xA5; 300 * 1024]), text(1), Message::binary(vec![0x5A; 300 * 1024])];
    let plan = Plan { expect: send.clone(), send, close: false, trickle: false };
    record("s1_2", &explore(&config(), |c| scenario(c, plan.clone())));
}

#[test]
fn s1_3_fragmented_messages() {
    let frag = |data: &str, op: OpCode, fin: bool| Message::Frame(Frame::message(data.to_owned().into_bytes(), op, fin));
    let send = vec![
        frag("hel", OpCode::Data(Data::Text), false),
        frag("lo ", OpCode::Data(Data::Continue), false),
        frag("world", OpCode::Data(Data::Continue), true),
        text(2),
        frag("ab", OpCode::Data(Data::Binary), false),
        frag("cd", OpCode::Data(Data::Continue), true),
    ];
    let expect = vec![Message::text("hello world"), text(2), Message::binary(b"abcd".to_vec())];
    let plan = Plan { send, expect, close: false, trickle: false };
    record("s1_3", &explore(&config(), |c| scenario(c, plan.clone())));
}

#[test]
fn s1_4_byte_by_byte() {
    let send = vec![text(0), Message::binary(vec![7; 300]), text(2)];
    let plan = Plan { expect: send.clone(), send, close: false, trickle: true };
    record("s1_4", &explore(&config(), |c| scenario(c, plan.clone())));
}

#[test]
fn s1_5_interleaved_pings() {
    let send = vec![
        Message::Ping(b"p0".to_vec().into()),
        text(0),
        Message::Ping(b"p1".to_vec().into()),
        Message::binary(vec![1; 32]),
        Message::Ping(b"p2".to_vec().into()),
        text(2),
    ];
    let expect: Vec<Message> = send.iter().filter(|m| is_data(m)).cloned().collect();
    let plan = Plan { send, expect, close: false, trickle: false };
    record("s1_5", &explore(&config(), |c| scenario(c, plan.clone())));
}

#[test]
fn s1_6_close_at_end() {
    let send = vec![text(0), Message::binary(vec![2; 64]), text(2)];
    let plan = Plan { expect: send.clone(), send, close: true, trickle: false };
    record("s1_6", &explore(&config(), |c| scenario(c, plan.clone())));
}

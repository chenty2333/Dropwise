#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dropwise::net::{ConnectionStats, Proxy, ProxyOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const REQUEST: &[u8] = b"ping";
const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
const CHUNKS: [&[u8]; 3] = [b"first.", b"second.", b"third."];

fn reply_delay(delay: Duration) -> ProxyOptions {
    ProxyOptions { reply_delay: delay }
}

/// Mock upstream: accepts connections, reads one request, writes one reply, and
/// then holds the connection open until the test runtime shuts down.
async fn start_upstream(reply: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock upstream address");
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept mock client");
            tokio::spawn(async move {
                let mut request = vec![0u8; REQUEST.len()];
                socket.read_exact(&mut request).await.expect("read request");
                socket.write_all(reply).await.expect("write reply");
                std::future::pending::<()>().await;
            });
        }
    });
    addr
}

/// Mock upstream that writes `CHUNKS` as separate writes, with a short pause
/// between them so the proxy can hand them to the client one chunk at a time.
async fn start_chunked_upstream() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock upstream address");
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept mock client");
            tokio::spawn(async move {
                let mut request = vec![0u8; REQUEST.len()];
                socket.read_exact(&mut request).await.expect("read request");
                for chunk in CHUNKS {
                    socket.write_all(chunk).await.expect("write chunk");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                std::future::pending::<()>().await;
            });
        }
    });
    addr
}

async fn connect_through(proxy: &Proxy) -> (TcpStream, SocketAddr) {
    let client = TcpStream::connect(proxy.addr()).await.expect("connect to proxy");
    let addr = client.local_addr().expect("client local address");
    (client, addr)
}

/// Poll `connections` until the counters satisfy `done` and return them.
async fn wait_for_stats(
    proxy: &Proxy,
    client: SocketAddr,
    done: impl Fn(&ConnectionStats) -> bool,
) -> ConnectionStats {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = None;
    loop {
        if let Some(stats) = proxy.connections().into_iter().find(|stats| stats.client == client) {
            if done(&stats) {
                return stats;
            }
            last = Some(stats);
        }
        assert!(Instant::now() < deadline, "byte counts did not settle: {last:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn total_chunks() -> usize {
    CHUNKS.iter().map(|chunk| chunk.len()).sum()
}

/// A read issued while the proxy holds the reply must return `Pending`, then
/// see the same bytes in the same order and count them exactly once.
#[tokio::test]
async fn held_reply_makes_the_client_read_pending() {
    let upstream = start_upstream(REPLY).await;
    let proxy = Proxy::start_with(upstream, reply_delay(Duration::from_millis(1000)))
        .await
        .expect("start proxy");
    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");

    // The upstream has already written the reply; the proxy is holding it.
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_millis(100), client.read(&mut buf)).await;
    assert!(read.is_err(), "the read completed although the proxy should hold the reply");

    let mut reply = vec![0u8; REPLY.len()];
    tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut reply))
        .await
        .expect("timed out waiting for the delayed reply")
        .expect("read the delayed reply");
    assert_eq!(reply, REPLY);

    let stats = wait_for_stats(&proxy, client_addr, |stats| {
        stats.to_upstream == REQUEST.len() as u64 && stats.to_client == REPLY.len() as u64
    })
    .await;
    assert_eq!(stats.to_upstream, REQUEST.len() as u64);
    assert_eq!(stats.to_client, REPLY.len() as u64);
}

/// Every chunk is held individually, but the byte stream keeps its order and
/// content and the counters add up to exactly what was forwarded.
#[tokio::test]
async fn held_reply_preserves_chunk_order_and_byte_counts() {
    let upstream = start_chunked_upstream().await;
    let proxy = Proxy::start_with(upstream, reply_delay(Duration::from_millis(100)))
        .await
        .expect("start proxy");
    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");

    let mut seen = Vec::new();
    for chunk in CHUNKS {
        let mut received = vec![0u8; chunk.len()];
        tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut received))
            .await
            .expect("timed out waiting for a delayed chunk")
            .expect("read a delayed chunk");
        seen.extend_from_slice(&received);
    }
    assert_eq!(seen, CHUNKS.concat());

    let stats = wait_for_stats(&proxy, client_addr, |stats| {
        stats.to_upstream == REQUEST.len() as u64 && stats.to_client == total_chunks() as u64
    })
    .await;
    assert_eq!(stats.to_client, total_chunks() as u64);
}

/// While a reply is held in the proxy it has not reached the client socket, so
/// `residue` reports 0; it reports the bytes once the delay elapses.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn residue_does_not_see_a_held_reply() {
    let upstream = start_upstream(REPLY).await;
    let proxy = Proxy::start_with(upstream, reply_delay(Duration::from_millis(1000)))
        .await
        .expect("start proxy");
    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");

    // The request reached the upstream, so it has had a chance to send the
    // reply; the proxy is now inside the delay.
    wait_for_stats(&proxy, client_addr, |stats| stats.to_upstream == REQUEST.len() as u64).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let held = stats_to_client(&proxy, client_addr);
    assert_eq!(held, 0, "the reply was already forwarded, so the delay had elapsed");
    let during = residue(&proxy, client_addr);
    assert_eq!(during, 0, "residue saw bytes that the proxy is still holding");

    wait_for_stats(&proxy, client_addr, |stats| stats.to_client == REPLY.len() as u64).await;
    let after = residue(&proxy, client_addr);
    assert_eq!(after, REPLY.len(), "residue missed the forwarded reply");
}

#[cfg(target_os = "linux")]
fn stats_to_client(proxy: &Proxy, client: SocketAddr) -> u64 {
    proxy
        .connections()
        .into_iter()
        .find(|stats| stats.client == client)
        .map_or(0, |stats| stats.to_client)
}

#[cfg(target_os = "linux")]
fn residue(proxy: &Proxy, client: SocketAddr) -> usize {
    proxy
        .residue()
        .expect("read residue")
        .into_iter()
        .find(|entry| entry.client == client)
        .map_or(0, |entry| entry.bytes)
}

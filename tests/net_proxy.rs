#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dropwise::net::{ConnectionStats, Proxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

/// Mock upstream: accepts connections, reads one request, writes one reply, and
/// then holds the connection open until the test runtime shuts down.
async fn start_upstream() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock upstream address");
    let _server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.expect("accept mock client");
            tokio::spawn(async move {
                let mut request = vec![0u8; REQUEST.len()];
                socket.read_exact(&mut request).await.expect("read request");
                socket.write_all(REPLY).await.expect("write reply");
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

async fn read_reply(client: &mut TcpStream) -> Vec<u8> {
    let mut reply = vec![0u8; REPLY.len()];
    tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut reply))
        .await
        .expect("timed out waiting for the mock reply")
        .expect("read the mock reply");
    assert_eq!(reply, REPLY);
    reply
}

/// Poll `residue` until `done` accepts the value or the timeout expires, and
/// return the last observed value.
#[cfg(target_os = "linux")]
async fn wait_for_residue(proxy: &Proxy, client: SocketAddr, done: impl Fn(usize) -> bool) -> usize {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let bytes = proxy
            .residue()
            .expect("read residue")
            .into_iter()
            .find(|entry| entry.client == client)
            .map_or(0, |entry| entry.bytes);
        if done(bytes) || Instant::now() >= deadline {
            return bytes;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Poll `connections` until the counters satisfy `done` and return them.
async fn wait_for_stats(
    proxy: &Proxy,
    client: SocketAddr,
    done: impl Fn(&ConnectionStats) -> bool,
) -> ConnectionStats {
    let deadline = Instant::now() + Duration::from_secs(2);
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn residue_sees_reply_the_client_has_not_read() {
    let upstream = start_upstream().await;
    let proxy = Proxy::start(upstream).await.expect("start proxy");
    assert_eq!(
        proxy.addr().ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "proxy must bind loopback only"
    );

    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");

    // Do not read the reply. It stays in the client socket's receive buffer,
    // where `residue` must see it.
    let bytes = wait_for_residue(&proxy, client_addr, |n| n > 0).await;
    assert!(bytes > 0, "no unread reply visible in the client socket");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn residue_is_zero_after_the_client_reads_the_reply() {
    let upstream = start_upstream().await;
    let proxy = Proxy::start(upstream).await.expect("start proxy");
    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");

    // Wait until the reply is buffered, then consume it completely.
    let before = wait_for_residue(&proxy, client_addr, |n| n > 0).await;
    assert!(before > 0, "reply was never buffered in the client socket");
    read_reply(&mut client).await;

    // Allow forwarding to settle before checking.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let after = wait_for_residue(&proxy, client_addr, |n| n == 0).await;
    assert_eq!(after, 0, "residue still reports unread bytes after the client read the reply");
}

/// A client using a blocking socket: asking for the residue of an empty
/// receive buffer must return 0, not wait for data.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn residue_does_not_block_on_a_blocking_client_socket() {
    use std::io::{Read, Write};

    let upstream = start_upstream().await;
    let proxy = Proxy::start(upstream).await.expect("start proxy");
    let mut client = std::net::TcpStream::connect(proxy.addr()).expect("connect to proxy");
    let client_addr = client.local_addr().expect("client local address");
    client.write_all(REQUEST).expect("send request");

    let before = wait_for_residue(&proxy, client_addr, |n| n == REPLY.len()).await;
    assert_eq!(before, REPLY.len(), "the whole reply is counted");
    let mut reply = vec![0u8; REPLY.len()];
    client.read_exact(&mut reply).expect("read the buffered reply");
    assert_eq!(reply, REPLY);
    // Empty buffer on a blocking socket: a blocking read would hang here.
    assert_eq!(wait_for_residue(&proxy, client_addr, |n| n == 0).await, 0);
}

#[tokio::test]
async fn byte_counts_match_forwarded_traffic() {
    let upstream = start_upstream().await;
    let proxy = Proxy::start(upstream).await.expect("start proxy");
    let (mut client, client_addr) = connect_through(&proxy).await;
    client.write_all(REQUEST).await.expect("send request");
    read_reply(&mut client).await;

    let stats = wait_for_stats(&proxy, client_addr, |stats| {
        stats.to_upstream == REQUEST.len() as u64 && stats.to_client == REPLY.len() as u64
    })
    .await;
    assert_eq!(stats.client, client_addr);
    assert_eq!(stats.to_upstream, REQUEST.len() as u64);
    assert_eq!(stats.to_client, REPLY.len() as u64);
}

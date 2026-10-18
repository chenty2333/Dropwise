//! In-process HTTP/1.1 server used by every phase 3 HTTP-client target.
//!
//! One instance per scenario run, on `127.0.0.1:0`. It keeps connections alive,
//! answers one request at a time in arrival order, and records every received
//! request before answering it. The record list is the independent observer:
//! the client API never sees it.
//!
//! A reply is sent in two steps so the client can be made to wait for the
//! response header and, separately, for the body without any client-specific
//! trick:
//!
//! 1. after `header_delay` real time: status line, headers, `\r\n\r\n`;
//! 2. after `body_delay` more: the body.
//!
//! Both delays use `tokio::time::sleep`, which is real time only under
//! `Flavor::MultiThread`; the phase 3 configuration uses that flavor.
//!
//! The reply body is `repl:<X-DW-Id header>` and the reply header
//! `X-DW-Reply` carries the same value, so a client can check it received the
//! answer to *its* request rather than a previous one.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// One request as seen by the server. `id` is the client-supplied `X-DW-Id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub method: String,
    pub path: String,
    pub id: String,
    pub body: String,
    /// Accept order of the client connection, starting at 0.
    pub connection: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerConfig {
    /// Real time between receiving a complete request and writing the
    /// response header.
    pub header_delay: Duration,
    /// Real time between writing the response header and writing the body.
    pub body_delay: Duration,
    /// Real time the server waits after reading the request header before
    /// reading a request body larger than 1 MiB. Without it the loopback
    /// buffers absorb the whole L2 body and the client never returns `Pending`
    /// while writing.
    pub large_body_read_delay: Duration,
}

/// Bytes appended after `repl:<id>` in every reply body. Large enough that
/// reading the body yields several `Pending` boundaries for clients that may
/// otherwise buffer a small body in one read (isahc in particular).
pub const BODY_PAD: usize = 256 * 1024;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            header_delay: Duration::from_millis(40),
            body_delay: Duration::from_millis(80),
            large_body_read_delay: Duration::from_millis(150),
        }
    }
}

pub struct Server {
    addr: std::net::SocketAddr,
    records: Arc<Mutex<Vec<Record>>>,
    accept_task: JoinHandle<()>,
}

impl Server {
    pub async fn start(config: ServerConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let records = Arc::new(Mutex::new(Vec::new()));
        let accept_task = tokio::spawn(accept_loop(listener, config, Arc::clone(&records)));
        Ok(Self { addr, records, accept_task })
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every request received so far, in arrival order.
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }

    /// Requests whose `X-DW-Id` equals `id`.
    pub fn records_with_id(&self, id: &str) -> Vec<Record> {
        self.records().into_iter().filter(|record| record.id == id).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: TcpListener, config: ServerConfig, records: Arc<Mutex<Vec<Record>>>) {
    let mut connection = 0usize;
    loop {
        let Ok((stream, _)) = listener.accept().await else { return };
        tokio::spawn(serve(stream, config, Arc::clone(&records), connection));
        connection += 1;
    }
}

async fn serve(
    mut stream: TcpStream,
    config: ServerConfig,
    records: Arc<Mutex<Vec<Record>>>,
    connection: usize,
) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Read up to and including the header terminator.
        let header_end = loop {
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                break pos;
            }
            if !read_more(&mut stream, &mut buf).await {
                return;
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let mut content_length = 0usize;
        let mut close = false;
        let mut expect_continue = false;
        let mut id = String::new();
        for line in lines {
            let Some((name, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                "connection" if value.eq_ignore_ascii_case("close") => close = true,
                "expect" if value.eq_ignore_ascii_case("100-continue") => expect_continue = true,
                "x-dw-id" => id = value.to_string(),
                _ => {}
            }
        }

        // isahc/curl asks before sending a body larger than 1 KiB and would
        // otherwise stall for its 1 s fallback. A real HTTP/1.1 server answers.
        if expect_continue {
            if stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await.is_err() {
                return;
            }
            if stream.flush().await.is_err() {
                return;
            }
        }
        if content_length > (1 << 20) {
            tokio::time::sleep(config.large_body_read_delay).await;
        }
        let total = header_end + 4 + content_length;
        while buf.len() < total {
            if !read_more(&mut stream, &mut buf).await {
                return;
            }
        }
        // Keep only a prefix: the L2 shape sends megabytes of request body and
        // the observer never needs the padding.
        let body: String = String::from_utf8_lossy(&buf[header_end + 4..total])
            .chars()
            .take(256)
            .collect();
        buf.drain(..total);

        // Record before replying: a request the server processed is visible even
        // if the client is gone by the time the reply is written.
        records.lock().unwrap().push(Record {
            method,
            path,
            id: id.clone(),
            body,
            connection,
        });

        let mut reply = format!("repl:{id}");
        reply.push_str(&"x".repeat(BODY_PAD));
        tokio::time::sleep(config.header_delay).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-DW-Reply: {}\r\nConnection: {}\r\n\r\n",
            reply.len(),
            id,
            if close { "close" } else { "keep-alive" },
        );
        if stream.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        if stream.flush().await.is_err() {
            return;
        }
        tokio::time::sleep(config.body_delay).await;
        if stream.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
        if stream.flush().await.is_err() {
            return;
        }
        if close {
            return;
        }
    }
}

async fn read_more(stream: &mut TcpStream, buf: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 8192];
    match stream.read(&mut chunk).await {
        Ok(0) | Err(_) => false,
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            true
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

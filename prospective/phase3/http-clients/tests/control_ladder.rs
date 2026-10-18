//! Positive control for the shared HTTP ladder: a deliberately buggy "pool"
//! that returns a connection to the next caller without draining the previous
//! reply. It is not a phase 3 target; it only proves that the harness's server,
//! survivor observation and residue measurement can report a desynchronised
//! connection. Not run by `run-http.sh`.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::time::Duration;

use harness::{Req, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Buggy {
    address: String,
    slot: Rc<RefCell<Option<TcpStream>>>,
}

async fn connect(address: &str) -> io::Result<TcpStream> {
    TcpStream::connect(address).await
}

impl Target for Buggy {
    type Client = Buggy;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let address = base_url
            .strip_prefix("http://")
            .ok_or_else(|| format!("unexpected base URL {base_url:?}"))?
            .to_string();
        Ok(Buggy { address, slot: Rc::new(RefCell::new(None)) })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let mut stream = match client.slot.borrow_mut().take() {
            Some(stream) => stream,
            None => connect(&client.address).await.map_err(|e| e.to_string())?,
        };
        let head = format!(
            "{} {} HTTP/1.1\r\nHost: dw.local\r\nX-DW-Id: {}\r\nContent-Length: {}\r\n\r\n",
            req.method,
            req.path,
            req.id,
            req.body.len(),
        );
        let mut request = head.into_bytes();
        request.extend_from_slice(req.body.as_bytes());
        stream.write_all(&request).await.map_err(|e| e.to_string())?;

        // BUG (on purpose): the connection goes back to the "pool" before the
        // reply is read. A cancellation between here and the read leaves the
        // reply owed to the next caller.
        client.slot.replace(Some(stream));
        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut stream = client.slot.borrow_mut().take().expect("connection was stored");
        let reply = read_reply(&mut stream).await;
        client.slot.replace(Some(stream));
        reply
    }
}

async fn read_reply(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed before the header".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let length: usize = head
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < length {
        let mut chunk = vec![0u8; length - body.len()];
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed before the body".into());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    String::from_utf8(body).map_err(|e| e.to_string())
}

#[test]
fn control_m() {
    harness::run_m::<Buggy>("CONTROL");
}

#[test]
fn control_m_residue() {
    harness::run_m_residue::<Buggy>("CONTROL");
}

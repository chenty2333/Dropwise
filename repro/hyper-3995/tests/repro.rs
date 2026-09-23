//! hyper#3995: when the HTTP/1 client dispatcher (the `Connection` future) is dropped
//! mid-body, e.g. by runtime shutdown, the body sender is dropped without an error, so
//! the response body ends early and looks complete.
//!
//! The server sends a Content-Length: 10 response in two halves. Contract: collecting
//! the body yields all 10 bytes or an error, never a short Ok. Dropwise drops the
//! dispatcher at each of its Pending boundaries.
//!
//! EXPECT=buggy (hyper 1.9.0) expects a violation; EXPECT=fixed (1.10.0) expects none.

use std::time::Duration;

use bytes::Bytes;
use dropwise::{explore, Config, Ctx};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn scenario(ctx: Ctx) -> Result<(), String> {
    let (client_io, mut server_io) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        let _ = server_io.read(&mut buf).await; // the request
        let _ = server_io
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n01234")
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = server_io.write_all(b"56789").await;
        std::future::pending::<()>().await; // keep the connection open
    });

    let (mut sender, conn) = http1::handshake(TokioIo::new(client_io))
        .await
        .map_err(|e| e.to_string())?;
    let c = ctx.clone();
    // The dispatcher normally runs in its own task; that task is what shutdown drops.
    tokio::spawn(async move { c.target(conn).await });

    let req = hyper::Request::get("/").body(Empty::<Bytes>::new()).unwrap();
    let resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(_) => return Ok(()), // dispatcher gone before the response: an error is fine
    };
    match resp.into_body().collect().await {
        Ok(body) => {
            let n = body.to_bytes().len();
            if n == 10 { Ok(()) } else { Err(format!("body ended Ok after {n} of 10 bytes")) }
        }
        Err(_) => Ok(()), // an error is an honest outcome
    }
}

#[test]
fn dispatcher_drop_mid_body() {
    let expect = std::env::var("EXPECT").unwrap_or_else(|_| "buggy".into());
    let report = explore(&Config::default(), scenario);
    println!("EXPECT={expect}\n{report}");
    assert!(report.baseline_errors.is_empty() && report.exhaustive, "{report}");
    assert!(report.unrealized().next().is_none() && report.unsettled().next().is_none(), "{report}");
    match expect.as_str() {
        "buggy" => assert!(report.violations().next().is_some(), "expected a violation:\n{report}"),
        _ => assert!(report.is_clean(), "expected no violation:\n{report}"),
    }
}

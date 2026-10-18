//! Target 1: hyper-util 0.1.21's legacy `Client` connection pool (hyper 1.12
//! merged into it per targets.md). One target operation: a complete HTTP/1.1
//! GET (request, response header and response body). There is no
//! "establish session state" call in this client, so no second operation.
//!
//! Environment: none beyond loopback; the in-process server lives in
//! `tests/support/http_server.rs`.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use bytes::Bytes;
use harness::{Req, Target};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

struct Hyper {
    inner: Client<HttpConnector, Full<Bytes>>,
    base: String,
}

impl Target for Hyper {
    type Client = Hyper;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let inner = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(1)
            .build_http::<Full<Bytes>>();
        Ok(Hyper { inner, base: base_url.to_string() })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let uri = format!("{}{}", client.base, req.path);
        let request = hyper::Request::builder()
            .method(req.method)
            .uri(uri)
            .header("X-DW-Id", &req.id)
            .body(Full::new(Bytes::from(req.body.clone())))
            .map_err(|e| e.to_string())?;
        let response = client.inner.request(request).await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        if !status.is_success() {
            return Err(format!("status {status}"));
        }
        String::from_utf8(body.to_vec()).map_err(|e| e.to_string())
    }
}

#[test]
fn b1() {
    harness::run_b1::<Hyper>("HYPER");
}

#[test]
fn b2() {
    harness::run_b2::<Hyper>("HYPER");
}

#[test]
fn b3() {
    harness::run_b3::<Hyper>("HYPER");
}

#[test]
fn m() {
    harness::run_m::<Hyper>("HYPER");
}

#[test]
fn m_residue() {
    harness::run_m_residue::<Hyper>("HYPER");
}

#[test]
fn reuse() {
    harness::check_reuse::<Hyper>("HYPER_UTIL");
}

#[test]
fn l2_b1() {
    harness::run_b1_l2::<Hyper>("HYPER");
}

#[test]
fn l2_b2() {
    harness::run_b2_l2::<Hyper>("HYPER");
}

#[test]
fn l2_b3() {
    harness::run_b3_l2::<Hyper>("HYPER");
}

#[test]
fn l2_m() {
    harness::run_m_l2::<Hyper>("HYPER");
}

#[test]
fn l2_m_residue() {
    harness::run_m_residue_l2::<Hyper>("HYPER");
}

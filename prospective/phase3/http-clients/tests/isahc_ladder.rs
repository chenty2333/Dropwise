//! Target 6: isahc 2.0.1 over libcurl's connection cache. One target
//! operation: a complete GET (request, response header and body). libcurl
//! decides when the connection is reusable; isahc exposes no
//! session-establishing call for plain HTTP.
//!
//! The client is built with one cached connection per host and a one-entry
//! connection cache so reuse is visible.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use harness::{Req, Target};

struct Isahc {
    inner: isahc::HttpClient,
    base: String,
}

use isahc::AsyncReadResponseExt;

impl Target for Isahc {
    type Client = Isahc;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let inner = isahc::HttpClient::builder()
            .max_connections_per_host(1)
            .connection_cache_size(1)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Isahc { inner, base: base_url.to_string() })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let url = format!("{}{}", client.base, req.path);
        let request = isahc::Request::builder()
            .method(req.method)
            .uri(url)
            .header("X-DW-Id", req.id.as_str())
            .body(req.body.clone())
            .map_err(|e| e.to_string())?;
        let mut response = client.inner.send_async(request).await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.bytes().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("status {status}"));
        }
        String::from_utf8(body).map_err(|e| e.to_string())
    }
}

#[test]
fn b1() {
    harness::run_b1::<Isahc>("ISAHc");
}

#[test]
fn b2() {
    harness::run_b2::<Isahc>("ISAHc");
}

#[test]
fn b3() {
    harness::run_b3::<Isahc>("ISAHc");
}

#[test]
fn m() {
    harness::run_m::<Isahc>("ISAHc");
}

#[test]
fn m_residue() {
    harness::run_m_residue::<Isahc>("ISAHc");
}

#[test]
fn reuse() {
    harness::check_reuse::<Isahc>("ISAHC");
}

#[test]
fn l2_b1() {
    harness::run_b1_l2::<Isahc>("ISAHc");
}

#[test]
fn l2_b2() {
    harness::run_b2_l2::<Isahc>("ISAHc");
}

#[test]
fn l2_b3() {
    harness::run_b3_l2::<Isahc>("ISAHc");
}

#[test]
fn l2_m() {
    harness::run_m_l2::<Isahc>("ISAHc");
}

#[test]
fn l2_m_residue() {
    harness::run_m_residue_l2::<Isahc>("ISAHc");
}

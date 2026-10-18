//! Target 2: reqwest 0.13.5. One target operation: a complete HTTP/1.1 GET
//! (request, response header and body) through reqwest's own pool. No
//! session-establishing call exists for a plain-HTTP client.
//!
//! Plain HTTP only, HTTP/2 and TLS off, one idle connection per host.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use harness::{Req, Target};

struct Reqwest {
    inner: reqwest::Client,
    base: String,
}

impl Target for Reqwest {
    type Client = Reqwest;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let inner = reqwest::Client::builder()
            .http1_only()
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Reqwest { inner, base: base_url.to_string() })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let url = format!("{}{}", client.base, req.path);
        let mut builder = client
            .inner
            .request(req.method.parse().map_err(|e| format!("method: {e}"))?, &url)
            .header("X-DW-Id", &req.id);
        if req.method == "POST" {
            builder = builder.body(req.body.clone());
        }
        let response = builder.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("status {status}"));
        }
        Ok(body)
    }
}

#[test]
fn b1() {
    harness::run_b1::<Reqwest>("REQWEST");
}

#[test]
fn b2() {
    harness::run_b2::<Reqwest>("REQWEST");
}

#[test]
fn b3() {
    harness::run_b3::<Reqwest>("REQWEST");
}

#[test]
fn m() {
    harness::run_m::<Reqwest>("REQWEST");
}

#[test]
fn m_residue() {
    harness::run_m_residue::<Reqwest>("REQWEST");
}

#[test]
fn reuse() {
    harness::check_reuse::<Reqwest>("REQWEST");
}

#[test]
fn l2_b1() {
    harness::run_b1_l2::<Reqwest>("REQWEST");
}

#[test]
fn l2_b2() {
    harness::run_b2_l2::<Reqwest>("REQWEST");
}

#[test]
fn l2_b3() {
    harness::run_b3_l2::<Reqwest>("REQWEST");
}

#[test]
fn l2_m() {
    harness::run_m_l2::<Reqwest>("REQWEST");
}

#[test]
fn l2_m_residue() {
    harness::run_m_residue_l2::<Reqwest>("REQWEST");
}

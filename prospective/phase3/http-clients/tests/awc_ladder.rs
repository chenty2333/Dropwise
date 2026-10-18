//! Target 7: awc 3.10.0's HTTP/1.1 connection pool. One target operation: a
//! complete GET (request, response header and body). No session-establishing
//! call exists for a plain-HTTP client.
//!
//! Compression/cookies/TLS are off; the default connector and pool are used.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use std::rc::Rc;

use harness::{Req, Target};

/// awc's default connector uses `spawn_local` for DNS/connect, so its client
/// must be created and used inside a `LocalSet`. Dropwise's scenario future is
/// not `Send`, so a per-client `LocalSet` works here.
struct AwcClient {
    inner: awc::Client,
    local: Rc<tokio::task::LocalSet>,
    base: String,
}

struct Awc;

impl Target for Awc {
    type Client = AwcClient;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let local = Rc::new(tokio::task::LocalSet::new());
        let inner = local
            .run_until(async { awc::Client::builder().finish() })
            .await;
        Ok(AwcClient { inner, local, base: base_url.to_string() })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let inner = client.inner.clone();
        let url = format!("{}{}", client.base, req.path);
        let method = req.method.parse().map_err(|e| format!("method: {e}"))?;
        let id = req.id.clone();
        let body = req.body.clone();
        client
            .local
            .run_until(async move {
                let request = inner.request(method, &url).insert_header(("X-DW-Id", id.as_str()));
                let mut response = if body.is_empty() {
                    request.send().await.map_err(|e| e.to_string())?
                } else {
                    request.send_body(body).await.map_err(|e| e.to_string())?
                };
                let status = response.status();
                let bytes = response.body().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!("status {status}"));
                }
                String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())
            })
            .await
    }
}

#[test]
fn b1() {
    harness::run_b1::<Awc>("AWC");
}

#[test]
fn b2() {
    harness::run_b2::<Awc>("AWC");
}

#[test]
fn b3() {
    harness::run_b3::<Awc>("AWC");
}

#[test]
fn m() {
    harness::run_m::<Awc>("AWC");
}

#[test]
fn m_residue() {
    harness::run_m_residue::<Awc>("AWC");
}

#[test]
fn reuse() {
    harness::check_reuse::<Awc>("AWC");
}

#[test]
fn l2_b1() {
    harness::run_b1_l2::<Awc>("AWC");
}

#[test]
fn l2_b2() {
    harness::run_b2_l2::<Awc>("AWC");
}

#[test]
fn l2_b3() {
    harness::run_b3_l2::<Awc>("AWC");
}

#[test]
fn l2_m() {
    harness::run_m_l2::<Awc>("AWC");
}

#[test]
fn l2_m_residue() {
    harness::run_m_residue_l2::<Awc>("AWC");
}

//! Target 4: pingora-core 0.9.0's HTTP/1 client session plus its
//! pingora-pool-backed `Connector` (pingora-core and pingora-pool merged per
//! targets.md). One target operation: a complete GET (request, response
//! header and body), then the session is released to the pool.
//!
//! Plain HTTP only; TLS is not compiled in. Pool size 1 so the warm connection
//! is the reused one.

#[path = "../../../support/ladder.rs"]
mod ladder;

#[path = "support/harness.rs"]
mod harness;

use harness::{Req, Target};
use pingora_core::connectors::http::v1::Connector;
use pingora_core::connectors::ConnectorOptions;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::RequestHeader;

struct Pingora {
    connector: Connector,
    /// `host:port` of the server or proxy.
    address: String,
}

impl Target for Pingora {
    type Client = Pingora;

    async fn make(base_url: &str) -> Result<Self::Client, String> {
        let address = base_url
            .strip_prefix("http://")
            .ok_or_else(|| format!("unexpected base URL {base_url:?}"))?
            .to_string();
        let connector = Connector::new(Some(ConnectorOptions::new(1)));
        Ok(Pingora { connector, address })
    }

    async fn call(client: &Self::Client, req: &Req) -> Result<String, String> {
        let peer = HttpPeer::new(client.address.as_str(), false, String::new());
        let (mut session, _reused) =
            client.connector.get_http_session(&peer).await.map_err(|e| e.to_string())?;

        let size = Some(req.body.len());
        let mut header = RequestHeader::build(req.method, req.path.as_bytes(), size)
            .map_err(|e| e.to_string())?;
        header.append_header("Host", "dw.local").map_err(|e| e.to_string())?;
        header.append_header("X-DW-Id", req.id.as_str()).map_err(|e| e.to_string())?;

        session.write_request_header(Box::new(header)).await.map_err(|e| e.to_string())?;
        if !req.body.is_empty() {
            session.write_body(req.body.as_bytes()).await.map_err(|e| e.to_string())?;
        }
        session.finish_body().await.map_err(|e| e.to_string())?;
        session.read_response().await.map_err(|e| e.to_string())?;
        let status = session.get_status().map(|status| status.as_u16());
        let mut body = Vec::new();
        while let Some(chunk) = session.read_body_bytes().await.map_err(|e| e.to_string())? {
            body.extend_from_slice(&chunk);
        }
        client.connector.release_http_session(session, &peer, None).await;
        if status != Some(200) {
            return Err(format!("status {status:?}"));
        }
        String::from_utf8(body).map_err(|e| e.to_string())
    }
}

#[test]
fn b1() {
    harness::run_b1::<Pingora>("PINGORA");
}

#[test]
fn b2() {
    harness::run_b2::<Pingora>("PINGORA");
}

#[test]
fn b3() {
    harness::run_b3::<Pingora>("PINGORA");
}

#[test]
fn m() {
    harness::run_m::<Pingora>("PINGORA");
}

#[test]
fn m_residue() {
    harness::run_m_residue::<Pingora>("PINGORA");
}

#[test]
fn reuse() {
    harness::check_reuse::<Pingora>("PINGORA");
}

#[test]
fn l2_b1() {
    harness::run_b1_l2::<Pingora>("PINGORA");
}

#[test]
fn l2_b2() {
    harness::run_b2_l2::<Pingora>("PINGORA");
}

#[test]
fn l2_b3() {
    harness::run_b3_l2::<Pingora>("PINGORA");
}

#[test]
fn l2_m() {
    harness::run_m_l2::<Pingora>("PINGORA");
}

#[test]
fn l2_m_residue() {
    harness::run_m_residue_l2::<Pingora>("PINGORA");
}

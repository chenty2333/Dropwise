//! Candidate recovery actions, executed against the real server.  The action
//! set is deliberately modest: reuse without any protocol action, consume one
//! pending reply (fast or slow), finish an interrupted write then consume the
//! reply, or retire the connection.  Every "drain" action falls back to a
//! reconnect if the drain fails, because a recovery strategy that can leave a
//! suspect socket in use is not a strategy.

use crate::lab::raw::{RawSky, ReuseRule};
use crate::proto::sky::{CutPoint, SkyConfig};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Reuse,
    Drain,
    DrainSlow,
    FinishDrain,
    Retire,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Reuse => "reuse",
            Action::Drain => "drain",
            Action::DrainSlow => "drain_slow",
            Action::FinishDrain => "finish_drain",
            Action::Retire => "retire",
        }
    }

    pub fn plan(self) -> &'static str {
        match self {
            Action::Reuse => "discard_local",
            Action::Drain => "drain",
            Action::DrainSlow => "drain_slow",
            Action::FinishDrain => "finish_write_drain",
            Action::Retire => "retire",
        }
    }

    pub const ALL: [Action; 5] = [
        Action::Reuse,
        Action::Drain,
        Action::DrainSlow,
        Action::FinishDrain,
        Action::Retire,
    ];
}

#[derive(Clone, Debug)]
pub struct ActionResult {
    pub action: Action,
    pub observed: Option<std::collections::BTreeMap<String, String>>,
    pub safe: bool,
    pub latency_us: u64,
    pub reconnects: i64,
    pub side_effect: Option<u64>,
    pub note: String,
}

/// Apply the action and return the connection, ready for the successor.
pub async fn apply(action: Action, raw: &mut RawSky, timeout: Duration) -> String {
    match action {
        Action::Reuse => String::new(),
        Action::Drain => match raw.drain_one(timeout).await {
            Ok(()) => String::new(),
            Err(e) => {
                format!("drain failed ({e}); reconnect={:?}", raw.reconnect().await)
            }
        },
        Action::DrainSlow => match raw.drain_one(timeout).await {
            Ok(()) => String::new(),
            Err(e) => {
                format!(
                    "slow drain failed ({e}); reconnect={:?}",
                    raw.reconnect().await
                )
            }
        },
        Action::FinishDrain => match raw.finish_write_drain(timeout).await {
            Ok(()) => String::new(),
            Err(e) => {
                format!(
                    "finish+drain failed ({e}); reconnect={:?}",
                    raw.reconnect().await
                )
            }
        },
        Action::Retire => match raw.reconnect().await {
            Ok(()) => String::new(),
            Err(e) => format!("reconnect failed: {e}"),
        },
    }
}

/// A raw connection plus the state that was injected into it.
pub struct Run {
    pub raw: RawSky,
    pub proxy: Option<crate::lab::proxy::DelayProxy>,
}

pub struct Injection {
    pub natural: Option<(crate::lab::proxy::ProxyMode, Duration)>,
    pub cut: Option<CutPoint>,
    pub write_chunk: Option<usize>,
}

/// Connect, prime through a separate direct connection, then reproduce the
/// injected state on the tested connection.
pub async fn prepare(
    trial: usize,
    space: &str,
    server: std::net::SocketAddr,
    pass: &str,
    injection: Injection,
    cut_sql: &str,
    cut_params: &[u8],
) -> Result<Run, String> {
    let Injection {
        natural,
        cut,
        write_chunk,
    } = injection;
    let proxy = match natural {
        None => None,
        Some((mode, _)) => Some(
            crate::lab::proxy::DelayProxy::start(server, mode)
                .await
                .map_err(|e| format!("proxy: {e}"))?,
        ),
    };
    let port = proxy
        .as_ref()
        .map(|p| p.addr.port())
        .unwrap_or(server.port());
    let cfg = SkyConfig::loopback(port, "root", pass);
    let mut raw = RawSky::connect(trial as u64, cfg, ReuseRule::Immediate)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    // Prime over a direct connection so the tested connection's replies are
    // only the cut operation's and the successor's.
    let direct_cfg = SkyConfig::loopback(server.port(), "root", pass);
    let mut direct = RawSky::connect(0, direct_cfg, ReuseRule::Immediate)
        .await
        .map_err(|e| format!("prime connect: {e}"))?;
    for (which, value) in [('a', 111u64), ('b', 222u64)] {
        let key = format!("{space}_{which}{trial}");
        let mut p = crate::proto::sky::encode_param_str(&key);
        p.extend(crate::proto::sky::encode_param_uint(value));
        let s = format!("insert into {space}.kv(?, ?)");
        direct
            .query(&s, &p)
            .await
            .map_err(|e| format!("prime {which}: {e}"))?;
    }
    raw.write_chunk = write_chunk;
    if natural.is_none() {
        raw.cut = cut;
    }
    let cut_res = match natural {
        None => Some(raw.query(cut_sql, cut_params).await),
        Some((_, cancel_after)) => {
            let fut = raw.query(cut_sql, cut_params);
            tokio::pin!(fut);
            tokio::select! {
                r = &mut fut => Some(r),
                _ = tokio::time::sleep(cancel_after) => None,
            }
        }
    };
    // The scripted fault is one-shot for this state; never let it leak into
    // the action or the successor.
    raw.cut = None;
    if std::env::var_os("RULE_DEBUG").is_some() {
        let (off, len, inb) = raw.io_state();
        eprintln!(
            "prepare: cut_res_ok={} out={off}/{len} in={inb} last_cut={:?}",
            matches!(cut_res, Some(Ok(_))),
            raw.last_cut
        );
    }
    match cut_res {
        None | Some(Err(crate::proto::sky::SkyError::Cut(_))) => {}
        Some(Ok(_)) => return Err("cut did not fire: operation completed".into()),
        Some(Err(e)) => return Err(format!("unexpected failure, not a realized cut: {e}")),
    }
    Ok(Run { raw, proxy })
}

pub fn now() -> Instant {
    Instant::now()
}

/// A tiny helper for the successor measurement (used by the rule lab).
pub async fn successor(
    raw: &mut RawSky,
    space: &str,
    trial: usize,
    timeout: Duration,
) -> (String, i64) {
    let (s, p) = (
        format!("select * from {space}.kv where id = ?"),
        crate::proto::sky::encode_param_str(&format!("{space}_a{trial}")),
    );
    match tokio::time::timeout(timeout, raw.query(&s, &p)).await {
        Ok(Ok(crate::proto::sky::Reply::Row(vals))) => match vals.get(1).and_then(|v| v.as_u64()) {
            Some(111) => ("ok".into(), 111),
            Some(v) => ("stale".into(), v as i64),
            None => ("error".into(), -1),
        },
        Ok(Ok(crate::proto::sky::Reply::Empty)) => ("empty".into(), -1),
        Ok(Ok(crate::proto::sky::Reply::Error(c))) => (format!("error({c})"), -(c as i64)),
        Ok(Ok(crate::proto::sky::Reply::Rows(_))) => ("error".into(), -5),
        Ok(Ok(crate::proto::sky::Reply::Value(_))) => ("error".into(), -6),
        Ok(Err(_)) => ("error".into(), -3),
        Err(_) => ("timeout".into(), -4),
    }
}

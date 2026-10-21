//! Framed request/reply protocol (Skyhash/2 as spoken by Skytable 0.8.x).
//!
//! The point of this adapter is the *contract*, not the SQL dialect:
//!
//! * one request produces exactly one reply frame, in order, with no request id;
//! * a request frame is length-prefixed, so a prefix of a frame is never a
//!   complete request on its own;
//! * a reply frame is self-delimiting, so a complete reply can be drained
//!   without knowing the request;
//! * no protocol operation resets a desynchronised stream.
//!
//! Therefore the resource is reusable after an abandoned operation only when
//! (a) the whole request was written and exactly one whole reply frame was
//! consumed under exclusive ownership, or (b) the adapter can finish the write
//! and then consume the reply, which is permitted only for an effect contract
//! that allows the operation to have applied.  A cancelled request whose write
//! is only partially on the wire is never completed by default: the caller's
//! cancellation is not an authorisation to send the rest.
//!
//! The codec is intentionally strict.  A parse error, an unexpected extra
//! frame, or a partial write that cannot be completed all end in `Failed`, not
//! in "probably fine".

use crate::features::Features;
use crate::guard::{Cause, Discharge, DischargeKind, Guard, Qualification, Ticket};
use crate::pool::{GuardedSession, Manager, ValidateOutcome};
use crate::recovery::{EffectContract, RecoveryAttempt, RecoveryContext};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const DEFAULT_DRAIN: Duration = Duration::from_millis(120);
pub const SLOW_DRAIN: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub enum SkyError {
    Refused(crate::guard::Refusal),
    Io(std::io::Error),
    Protocol(String),
    Server(u16),
    /// The scripted cancellation point fired.  The ticket was abandoned with
    /// `Cause::Cancelled`; the connection is suspect and must be recovered.
    Cut(CutSnapshot),
    Closed,
}

impl fmt::Display for SkyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkyError::Refused(r) => write!(f, "refused: {r}"),
            SkyError::Io(e) => write!(f, "io: {e}"),
            SkyError::Protocol(s) => write!(f, "protocol: {s}"),
            SkyError::Server(code) => write!(f, "server error {code}"),
            SkyError::Cut(s) => write!(f, "cut at {s:?}"),
            SkyError::Closed => f.write_str("connection closed"),
        }
    }
}

impl std::error::Error for SkyError {}

/// Deterministic cancellation point for the lab.  This is fault injection, not
/// a claim about what a real caller does; it lets a state be reproduced exactly
/// so the same post-cancellation state can be replayed under different recovery
/// policies.
#[derive(Clone, Copy, Debug, Default)]
pub struct CutPoint {
    pub after_write_bytes: Option<usize>,
    pub after_read_bytes: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct CutSnapshot {
    pub write_bytes: usize,
    pub write_state: &'static str,
    pub read_bytes: usize,
    pub read_state: &'static str,
    pub fired_after: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkyValue {
    Null,
    Bool(bool),
    Uint(u64),
    Sint(i64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
    List(Vec<SkyValue>),
}

impl SkyValue {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            SkyValue::Uint(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            SkyValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Empty,
    Error(u16),
    Row(Vec<SkyValue>),
    Rows(Vec<Vec<SkyValue>>),
    Value(SkyValue),
}

impl Reply {
    pub fn is_empty(&self) -> bool {
        matches!(self, Reply::Empty)
    }

    pub fn error_code(&self) -> Option<u16> {
        match self {
            Reply::Error(e) => Some(*e),
            _ => None,
        }
    }
}

/// Encode one query with literal parameter bytes.  The caller supplies the
/// encoded parameters (as produced by [`encode_param_str`] / [`encode_param_uint`])
/// so the lab can use exactly the same bytes as the official client.
pub fn encode_query(sql: &str, params: &[u8]) -> Vec<u8> {
    let window = sql.len();
    let payload_len = window + params.len();
    // window digits + '\n' + payload
    let total = window.to_string().len() + 1 + payload_len;
    let mut out = Vec::with_capacity(total + 4);
    out.push(b'S');
    out.extend_from_slice(total.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(window.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(sql.as_bytes());
    out.extend_from_slice(params);
    out
}

pub fn encode_param_str(s: &str) -> Vec<u8> {
    let mut out = vec![0x06];
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(s.as_bytes());
    out
}

pub fn encode_param_uint(v: u64) -> Vec<u8> {
    let mut out = vec![0x02];
    out.extend_from_slice(v.to_string().as_bytes());
    out.push(b'\n');
    out
}

/// Try to parse one complete reply from the front of `buf`.  Returns `None`
/// when more bytes are needed.  The parse is restarted from the frame start on
/// each call, which makes a partial read trivially resumable: no parser state
/// lives outside `buf`.
pub fn parse_reply(buf: &[u8]) -> Result<Option<(Reply, usize)>, SkyError> {
    let mut cur = Cursor { b: buf, i: 0 };
    match cur.reply() {
        Ok(Some(reply)) => Ok(Some((reply, cur.i))),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }

    /// Decimal length terminated by `\n`.
    fn lfs(&mut self) -> Result<Option<usize>, SkyError> {
        let start = self.i;
        let mut value: usize = 0;
        let mut digits = 0;
        loop {
            let Some(b) = self.byte() else {
                self.i = start;
                return Ok(None);
            };
            if b == b'\n' {
                if digits == 0 {
                    return Err(SkyError::Protocol("empty length".into()));
                }
                return Ok(Some(value));
            }
            if !b.is_ascii_digit() || digits > 10 {
                return Err(SkyError::Protocol(format!("bad length byte {b:#x}")));
            }
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as usize))
                .ok_or_else(|| SkyError::Protocol("length overflow".into()))?;
            digits += 1;
        }
    }

    fn value(&mut self, code: u8, depth: usize) -> Result<Option<SkyValue>, SkyError> {
        if depth > 32 {
            return Err(SkyError::Protocol("value nesting too deep".into()));
        }
        Ok(Some(match code {
            0x00 => SkyValue::Null,
            0x01 => match self.byte() {
                Some(b) if b <= 1 => SkyValue::Bool(b == 1),
                Some(b) => return Err(SkyError::Protocol(format!("bad bool {b}"))),
                None => return Ok(None),
            },
            0x02..=0x0b => {
                // Numeric values are decimal digits terminated by LF; they do
                // not carry a length prefix (only strings/binary do).
                let start = self.i;
                let mut end = None;
                while let Some(b) = self.b.get(self.i) {
                    if *b == b'\n' {
                        end = Some(self.i);
                        self.i += 1;
                        break;
                    }
                    self.i += 1;
                }
                let Some(end) = end else {
                    self.i = start;
                    return Ok(None);
                };
                let text = std::str::from_utf8(&self.b[start..end])
                    .map_err(|_| SkyError::Protocol("non-utf8 scalar".into()))?;
                if text.is_empty() {
                    return Err(SkyError::Protocol("empty scalar".into()));
                }
                match code {
                    0x02..=0x05 => {
                        let v: u64 = text
                            .parse()
                            .map_err(|_| SkyError::Protocol("bad uint".into()))?;
                        SkyValue::Uint(v)
                    }
                    0x06..=0x09 => {
                        let v: i64 = text
                            .parse()
                            .map_err(|_| SkyError::Protocol("bad sint".into()))?;
                        SkyValue::Sint(v)
                    }
                    _ => {
                        let v: f64 = text
                            .parse()
                            .map_err(|_| SkyError::Protocol("bad float".into()))?;
                        SkyValue::Float(v)
                    }
                }
            }
            0x0c => {
                let Some(len) = self.lfs()? else {
                    return Ok(None);
                };
                let Some(raw) = self.take(len) else {
                    return Ok(None);
                };
                SkyValue::Bin(raw.to_vec())
            }
            0x0d => {
                let Some(len) = self.lfs()? else {
                    return Ok(None);
                };
                let Some(raw) = self.take(len) else {
                    return Ok(None);
                };
                SkyValue::Str(
                    std::str::from_utf8(raw)
                        .map_err(|_| SkyError::Protocol("non-utf8 string".into()))?
                        .to_owned(),
                )
            }
            0x0e => {
                let Some(count) = self.lfs()? else {
                    return Ok(None);
                };
                let mut items = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    let Some(c) = self.byte() else {
                        return Ok(None);
                    };
                    let Some(v) = self.value(c, depth + 1)? else {
                        return Ok(None);
                    };
                    items.push(v);
                }
                SkyValue::List(items)
            }
            other => {
                return Err(SkyError::Protocol(format!("unknown value code {other:#x}")));
            }
        }))
    }

    fn values(&mut self, count: usize, depth: usize) -> Result<Option<Vec<SkyValue>>, SkyError> {
        let mut out = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let Some(code) = self.byte() else {
                return Ok(None);
            };
            let Some(v) = self.value(code, depth + 1)? else {
                return Ok(None);
            };
            out.push(v);
        }
        Ok(Some(out))
    }

    fn reply(&mut self) -> Result<Option<Reply>, SkyError> {
        let Some(code) = self.byte() else {
            return Ok(None);
        };
        Ok(Some(match code {
            0x10 => {
                let Some(raw) = self.take(2) else {
                    return Ok(None);
                };
                Reply::Error(u16::from_le_bytes([raw[0], raw[1]]))
            }
            0x11 => {
                let Some(count) = self.lfs()? else {
                    return Ok(None);
                };
                let Some(items) = self.values(count, 0)? else {
                    return Ok(None);
                };
                Reply::Row(items)
            }
            0x12 => Reply::Empty,
            0x13 => {
                let Some(streams) = self.lfs()? else {
                    return Ok(None);
                };
                let Some(size) = self.lfs()? else {
                    return Ok(None);
                };
                let mut rows = Vec::with_capacity(streams.min(64));
                for _ in 0..streams {
                    let Some(row) = self.values(size, 0)? else {
                        return Ok(None);
                    };
                    rows.push(row);
                }
                Reply::Rows(rows)
            }
            0x0f => return Err(SkyError::Protocol("reserved response code 0x0f".into())),
            other => {
                let Some(v) = self.value(other, 0)? else {
                    return Ok(None);
                };
                Reply::Value(v)
            }
        }))
    }
}

pub struct SkyConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub connect_timeout: Duration,
}

impl SkyConfig {
    pub fn loopback(port: u16, user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            password: password.into(),
            connect_timeout: Duration::from_secs(2),
        }
    }
}

struct SkyIo {
    stream: Option<TcpStream>,
    /// Bytes received and not yet consumed as a complete reply frame.
    inbuf: Vec<u8>,
    /// The whole request being written, and how much of it is on the wire.
    out: Vec<u8>,
    out_off: usize,
}

pub struct SkyConn {
    io: SkyIo,
    guard: Arc<Guard>,
    contract: EffectContract,
    pub cut: Option<CutPoint>,
    /// Optional bound on one write syscall, used by the lab to reach exact
    /// partial-write states.  `None` writes the rest of the request at once.
    pub write_chunk: Option<usize>,
    cut_snapshot: Option<CutSnapshot>,
    /// Set once a recovery attempt consumed the abandoned exchange's frame; a
    /// retry (or a cancellation of that recovery after the read) can finish
    /// the recovery without waiting for a second frame that will never come.
    recovered_frame: bool,
}

impl SkyConn {
    pub async fn connect(cfg: &SkyConfig) -> std::io::Result<Self> {
        let connect = TcpStream::connect((cfg.host.as_str(), cfg.port));
        let mut stream = match tokio::time::timeout(cfg.connect_timeout, connect).await {
            Ok(r) => r?,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "skytable connect timed out",
                ));
            }
        };
        let mut hs = Vec::new();
        hs.extend_from_slice(&[b'H', 0, 0, 0, 0, 0]);
        hs.extend_from_slice(cfg.user.len().to_string().as_bytes());
        hs.push(b'\n');
        hs.extend_from_slice(cfg.password.len().to_string().as_bytes());
        hs.push(b'\n');
        hs.extend_from_slice(cfg.user.as_bytes());
        hs.extend_from_slice(cfg.password.as_bytes());
        stream.write_all(&hs).await?;
        let mut resp = [0u8; 4];
        stream.read_exact(&mut resp).await?;
        match resp {
            [b'H', 0, 0, 0] => {}
            [b'H', 0, 1, code] => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("skytable handshake rejected (code {code})"),
                ));
            }
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad skytable handshake {other:?}"),
                ));
            }
        }
        if std::env::var_os("OG_SKY_DEBUG").is_some() {
            eprintln!("sky connect done (handshake consumed)");
        }
        Ok(Self {
            io: SkyIo {
                stream: Some(stream),
                inbuf: Vec::with_capacity(256),
                out: Vec::new(),
                out_off: 0,
            },
            guard: Arc::new(Guard::new()),
            contract: EffectContract::MustNotApply,
            cut: None,
            write_chunk: None,
            cut_snapshot: None,
            recovered_frame: false,
        })
    }

    pub fn with_contract(mut self, contract: EffectContract) -> Self {
        self.contract = contract;
        self
    }

    pub fn guard(&self) -> &Arc<Guard> {
        &self.guard
    }

    pub fn cut_snapshot(&self) -> Option<&CutSnapshot> {
        self.cut_snapshot.as_ref()
    }

    pub fn clear_cut(&mut self) {
        self.cut = None;
        self.cut_snapshot = None;
    }

    /// Run one query.  The future arms the ticket at its first poll before any
    /// byte is written, records write/read progress on the ticket, and
    /// discharges only after a complete reply frame was consumed.
    pub async fn query(&mut self, sql: &str) -> Result<Reply, SkyError> {
        self.query_with(sql, &[]).await
    }

    pub async fn query_with(&mut self, sql: &str, params: &[u8]) -> Result<Reply, SkyError> {
        if self.io.stream.is_none() {
            return Err(SkyError::Closed);
        }
        let guard = self.guard.clone();
        let mut ticket = guard.begin().map_err(SkyError::Refused)?;
        ticket.arm();
        ticket.tag("op", "query");
        ticket.tag(
            "contract",
            match self.contract {
                EffectContract::AtomicEffectMayApply => "may_apply",
                EffectContract::MustNotApply => "must_not_apply",
            },
        );
        ticket.set("write_bytes", 0usize);
        ticket.tag("write", "none");
        ticket.set("read_bytes", 0usize);
        ticket.tag("read", "none");
        self.recovered_frame = false;
        self.io.out = encode_query(sql, params);
        self.io.out_off = 0;
        let write_result = self.flush_out(&mut ticket).await;
        if let Err(e) = write_result {
            let cause = match &e {
                SkyError::Io(_) | SkyError::Closed => Cause::Io,
                SkyError::Cut(_) => Cause::Cancelled,
                _ => Cause::Protocol,
            };
            ticket.abandon(cause);
            return Err(e);
        }
        let read_result = self.read_reply(&mut ticket).await;
        match read_result {
            Ok(reply) => {
                let kind = if reply.error_code().is_some() {
                    DischargeKind::ServerError
                } else {
                    DischargeKind::Complete
                };
                let discharge = Discharge::new(
                    guard.obj(),
                    ticket.epoch(),
                    ticket.op(),
                    kind,
                    "sky.reply_frame",
                );
                if let Err(e) = ticket.settle(discharge) {
                    // The ledger disagrees with the protocol.  The object may
                    // not be reused; leave it suspect (the ticket's Drop does
                    // that) and report a protocol error.
                    return Err(SkyError::Protocol(format!("settlement rejected: {e}")));
                }
                self.io.out.clear();
                self.io.out_off = 0;
                Ok(reply)
            }
            Err(e) => {
                let cause = match &e {
                    SkyError::Io(_) | SkyError::Closed => Cause::Io,
                    SkyError::Protocol(_) => Cause::Protocol,
                    SkyError::Cut(_) => Cause::Cancelled,
                    _ => Cause::Other("query"),
                };
                ticket.abandon(cause);
                Err(e)
            }
        }
    }

    async fn flush_out(&mut self, ticket: &mut Ticket<'_>) -> Result<(), SkyError> {
        loop {
            if self.io.out_off >= self.io.out.len() {
                ticket.tag("write", "complete");
                return Ok(());
            }
            if let Some(cut) = self.cut
                && cut.after_write_bytes == Some(self.io.out_off)
            {
                return Err(self.fire_cut("write", ticket));
            }
            let (_, state) = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                let end = match self.write_chunk {
                    Some(chunk) => (io.out_off + chunk).min(io.out.len()),
                    None => io.out.len(),
                };
                let n = stream
                    .write(&io.out[io.out_off..end])
                    .await
                    .map_err(SkyError::Io)?;
                if n == 0 {
                    ticket.tag("write", "eof");
                    return Err(SkyError::Closed);
                }
                io.out_off += n;
                ticket.set("write_bytes", io.out_off);
                let state = if io.out_off == io.out.len() {
                    "complete"
                } else {
                    "partial"
                };
                ticket.tag("write", state);
                (io.out_off, state)
            };
            if let Some(cut) = self.cut
                && let Some(at) = cut.after_write_bytes
                && self.io.out_off >= at
            {
                return Err(self.fire_cut("write", ticket));
            }
            let _ = state;
        }
    }

    fn fire_cut(&mut self, fired_after: &'static str, ticket: &Ticket<'_>) -> SkyError {
        let write_state = if self.io.out_off == 0 {
            "none"
        } else if self.io.out_off == self.io.out.len() {
            "complete"
        } else {
            "partial"
        };
        let read_state = if self.io.inbuf.is_empty() {
            "none"
        } else {
            "partial"
        };
        let snapshot = CutSnapshot {
            write_bytes: self.io.out_off,
            write_state,
            read_bytes: self.io.inbuf.len(),
            read_state,
            fired_after,
        };
        // The ticket is abandoned by the caller with Cause::Cancelled.
        let _ = ticket;
        self.cut_snapshot = Some(snapshot.clone());
        // One-shot: recovery and the successor must run without the scripted
        // fault still armed.
        self.cut = None;
        SkyError::Cut(snapshot)
    }

    async fn read_reply(&mut self, ticket: &mut Ticket<'_>) -> Result<Reply, SkyError> {
        loop {
            // A zero-byte read cut abandons before reading the reply, even if
            // the reply is already buffered.
            if let Some(cut) = self.cut
                && cut.after_read_bytes == Some(0)
            {
                return Err(self.fire_cut("read", ticket));
            }
            if let Some((reply, used)) = parse_reply(&self.io.inbuf)? {
                self.io.inbuf.drain(..used);
                ticket.tag("read", "complete");
                ticket.set("read_bytes", used);
                return Ok(reply);
            }
            if let Some(cut) = self.cut
                && cut.after_read_bytes == Some(self.io.inbuf.len())
            {
                return Err(self.fire_cut("read", ticket));
            }
            let n = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                let mut tmp = [0u8; 512];
                let n = stream.read(&mut tmp).await.map_err(SkyError::Io)?;
                io.inbuf.extend_from_slice(&tmp[..n]);
                if n == 0 {
                    ticket.tag("read", "eof");
                    return Err(SkyError::Closed);
                }
                n
            };
            ticket.set("read_bytes", self.io.inbuf.len());
            ticket.tag("read", "partial");
            if std::env::var_os("OG_SKY_DEBUG").is_some() {
                let m = self.io.inbuf.len().min(8);
                eprintln!(
                    "sky read n={n} len={} head={:02x?}",
                    self.io.inbuf.len(),
                    &self.io.inbuf[..m]
                );
            }
            if let Some(cut) = self.cut
                && cut.after_read_bytes.is_some()
                && self.io.inbuf.len() >= cut.after_read_bytes.unwrap_or(usize::MAX)
            {
                return Err(self.fire_cut("read", ticket));
            }
            let _ = n;
        }
    }

    /// Health probe used only for a clean session.
    pub async fn validate(&mut self) -> ValidateOutcome {
        match self.query("sysctl report status").await {
            Ok(Reply::Empty) => ValidateOutcome::Healthy,
            Ok(_) => ValidateOutcome::Unhealthy(Cause::Other("unexpected_probe_reply")),
            Err(_) => ValidateOutcome::Unhealthy(Cause::Protocol),
        }
    }

    /// Protocol recovery.  All progress lives in `self.io`; dropping this
    /// future at an await leaves a state that the next attempt can continue.
    pub async fn recover(&mut self, ctx: &RecoveryContext) -> RecoveryAttempt {
        let r = self.recover_inner(ctx).await;
        if std::env::var_os("OG_RECOVERY_DEBUG").is_some()
            && !matches!(r, RecoveryAttempt::Recovered(_))
        {
            eprintln!(
                "sky recover plan={} result={:?} out={}/{} in={} phase={:?}",
                ctx.plan,
                r,
                self.io.out_off,
                self.io.out.len(),
                self.io.inbuf.len(),
                self.guard.phase()
            );
            let n = self.io.inbuf.len().min(64);
            eprintln!("sky recover inbuf[0..{n}]={:02x?}", &self.io.inbuf[..n]);
        }
        r
    }

    async fn recover_inner(&mut self, ctx: &RecoveryContext) -> RecoveryAttempt {
        let plan = ctx.plan;
        if plan == "discard_local" {
            // An I/O failure before any byte was counted can still mean the
            // socket is dead.  The byte count alone is not protocol evidence.
            if ctx.cause == Cause::Io {
                return RecoveryAttempt::Failed(Cause::Other("discard_local_io_broken"));
            }
            // Nothing left the process and nothing was read: there is no
            // exchange to drain and no reason to retire the object.  This is
            // only true because the adapter counted bytes, not because the
            // cancellation "probably" happened early.
            if self.io.out_off == 0 && self.io.inbuf.is_empty() {
                self.io.out.clear();
                self.io.out_off = 0;
                return RecoveryAttempt::recovered(ctx, "sky.nothing_written");
            }
            return RecoveryAttempt::Failed(Cause::Other("discard_local_with_bytes"));
        }
        let (allow_finish, timeout) = match plan {
            "drain" => (false, DEFAULT_DRAIN.min(ctx.remaining)),
            "drain_slow" => (false, SLOW_DRAIN.min(ctx.remaining)),
            "finish_write_drain" => (true, DEFAULT_DRAIN.min(ctx.remaining)),
            "finish_write_drain_slow" => (true, SLOW_DRAIN.min(ctx.remaining)),
            other => {
                return RecoveryAttempt::Failed(Cause::Other(match other {
                    "retire" => "policy_retire",
                    _ => "unknown_plan",
                }));
            }
        };
        if !allow_finish && self.io.out_off != self.io.out.len() {
            // The request is only partially on the wire.  Draining a reply
            // would wait for a request that was never sent; reusing the stream
            // would leave a prefix in front of the next request.
            return RecoveryAttempt::Failed(Cause::Other("partial_write_unrecoverable"));
        }
        if allow_finish && self.io.out_off < self.io.out.len() {
            // Use the contract recorded when the abandoned operation armed, not
            // the connection's current builder setting.
            if ctx.contract != EffectContract::AtomicEffectMayApply {
                return RecoveryAttempt::Failed(Cause::Other("contract_forbids_finish"));
            }
            // Finish exactly the original request; never a different one.
            while self.io.out_off < self.io.out.len() {
                let (stream, out, off) = {
                    let io = &mut self.io;
                    match io.stream.as_mut() {
                        Some(s) => (s, &io.out[..], &mut io.out_off),
                        None => return RecoveryAttempt::Failed(Cause::Io),
                    }
                };
                match tokio::time::timeout(timeout, stream.write(&out[*off..])).await {
                    Ok(Ok(0)) => return RecoveryAttempt::Failed(Cause::Io),
                    Ok(Ok(n)) => *off += n,
                    Ok(Err(_)) => return RecoveryAttempt::Failed(Cause::Io),
                    Err(_) => return RecoveryAttempt::Unresolved(Cause::Timeout),
                }
            }
        }
        if self.recovered_frame {
            // A previous attempt consumed the frame but was cancelled before
            // minting the witness.  The protocol position is already restored,
            // but only if no trailing bytes arrived since then.
            self.recovered_frame = false;
            if !self.io.inbuf.is_empty() {
                return RecoveryAttempt::Failed(Cause::Other("extra_bytes_after_reply"));
            }
            self.io.out.clear();
            self.io.out_off = 0;
            return RecoveryAttempt::recovered(ctx, "sky.frame_consumed_earlier");
        }
        loop {
            match parse_reply(&self.io.inbuf) {
                Ok(Some((_reply, used))) => {
                    self.io.inbuf.drain(..used);
                    if !self.io.inbuf.is_empty() {
                        return RecoveryAttempt::Failed(Cause::Other("extra_bytes_after_reply"));
                    }
                    // Record the progress before yielding: if the recovery
                    // future dies before minting the witness, the next attempt
                    // must not wait for another frame.
                    self.recovered_frame = true;
                    self.recovered_frame = false;
                    self.io.out.clear();
                    self.io.out_off = 0;
                    return RecoveryAttempt::recovered(ctx, "sky.frame_drained");
                }
                Ok(None) => {}
                Err(_) => return RecoveryAttempt::Failed(Cause::Protocol),
            }
            let read = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return RecoveryAttempt::Failed(Cause::Io);
                };
                let mut tmp = [0u8; 512];
                tokio::time::timeout(timeout, stream.read(&mut tmp))
                    .await
                    .map(|r| {
                        r.inspect(|&n| {
                            io.inbuf.extend_from_slice(&tmp[..n]);
                        })
                    })
            };
            match read {
                Ok(Ok(0)) => return RecoveryAttempt::Failed(Cause::Io),
                Ok(Ok(_)) => {}
                Ok(Err(_)) => return RecoveryAttempt::Failed(Cause::Io),
                Err(_) => {
                    // If a complete frame arrived in the same read (possible
                    // when a timeout fires late), consume it before reporting.
                    match parse_reply(&self.io.inbuf) {
                        Ok(Some((_reply, used))) => {
                            self.io.inbuf.drain(..used);
                            self.io.out.clear();
                            self.io.out_off = 0;
                            return RecoveryAttempt::recovered(ctx, "sky.frame_drained_late");
                        }
                        _ => return RecoveryAttempt::Unresolved(Cause::Timeout),
                    }
                }
            }
        }
    }
}

impl GuardedSession for SkyConn {
    fn guard(&self) -> &Arc<Guard> {
        &self.guard
    }

    fn close(&mut self) {
        // Dropping a tokio TcpStream closes the descriptor.  There is no async
        // shutdown in a synchronous Drop path; the peer sees a close/reset.
        self.io.stream.take();
    }

    fn effect_contract(&self) -> EffectContract {
        self.contract
    }
}

pub struct SkyManager {
    pub cfg: SkyConfig,
    pub contract: EffectContract,
}

impl SkyManager {
    pub fn new(cfg: SkyConfig, contract: EffectContract) -> Self {
        Self { cfg, contract }
    }
}

impl Manager<SkyConn> for SkyManager {
    fn name(&self) -> &'static str {
        "skytable-framed"
    }

    async fn connect(&self) -> std::io::Result<SkyConn> {
        Ok(SkyConn::connect(&self.cfg)
            .await?
            .with_contract(self.contract))
    }

    async fn validate(&self, session: &mut SkyConn) -> ValidateOutcome {
        session.validate().await
    }

    async fn recover(&self, session: &mut SkyConn, ctx: &RecoveryContext) -> RecoveryAttempt {
        session.recover(ctx).await
    }
}

/// The observation a policy sees for this protocol.  The adapter fills the
/// feature map at abandonment time; this helper is also used by the offline
/// dataset builder so the online and offline views cannot diverge.
pub fn sky_features(
    write_bytes: usize,
    write_total: usize,
    read_bytes: usize,
    extra: &[(&'static str, crate::features::Value)],
) -> Features {
    let mut f = Features::new();
    f.tag(
        "write",
        if write_bytes == 0 && write_total > 0 {
            "none"
        } else if write_bytes == write_total {
            "complete"
        } else {
            "partial"
        },
    );
    f.tag("read", if read_bytes == 0 { "none" } else { "partial" });
    f.set("write_bytes", write_bytes);
    f.set("read_bytes", read_bytes);
    for (k, v) in extra {
        f.set(k, *v);
    }
    f
}

#[allow(dead_code)]
fn _assert_qualification(_: Qualification) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_same_bytes_as_the_official_client() {
        // Captured from skytable 0.8.12 `Query::debug_encode_packet` for the
        // same SQL and parameters (see research/online/investigation-skytable.md).
        let mut params = encode_param_str("k");
        params.extend(encode_param_uint(0));
        assert_eq!(
            encode_query("insert into probe4.kv(?, ?)", &params),
            vec![
                0x53, 0x33, 0x37, 0x0a, 0x32, 0x37, 0x0a, b'i', b'n', b's', b'e', b'r', b't', b' ',
                b'i', b'n', b't', b'o', b' ', b'p', b'r', b'o', b'b', b'e', b'4', b'.', b'k', b'v',
                b'(', b'?', b',', b' ', b'?', b')', 0x06, 0x31, 0x0a, b'k', 0x02, 0x30, 0x0a
            ]
        );
    }

    #[test]
    fn parses_empty_error_row_and_rows() {
        assert_eq!(parse_reply(&[0x12]).unwrap(), Some((Reply::Empty, 1)));
        assert_eq!(
            parse_reply(&[0x10, 0x19, 0x00]).unwrap(),
            Some((Reply::Error(25), 3))
        );
        let row = [0x11, 0x32, 0x0a, 0x0d, 0x31, 0x0a, b'k', 0x05, 0x31, 0x0a];
        assert_eq!(
            parse_reply(&row).unwrap(),
            Some((
                Reply::Row(vec![SkyValue::Str("k".into()), SkyValue::Uint(1)]),
                row.len()
            ))
        );
        let rows = [0x13, 0x31, 0x0a, 0x31, 0x0a, 0x05, 0x31, 0x0a];
        assert_eq!(
            parse_reply(&rows).unwrap(),
            Some((Reply::Rows(vec![vec![SkyValue::Uint(1)]]), rows.len()))
        );
    }

    #[test]
    fn partial_frames_need_more_bytes_and_do_not_advance_external_state() {
        let full = [0x11, 0x32, 0x0a, 0x0d, 0x31, 0x0a, b'k', 0x05, 0x32, 0x0a];
        for cut in 0..full.len() - 1 {
            assert_eq!(parse_reply(&full[..cut]).unwrap(), None, "cut {cut}");
        }
        assert_eq!(
            parse_reply(&full).unwrap(),
            Some((
                Reply::Row(vec![SkyValue::Str("k".into()), SkyValue::Uint(2)]),
                10
            ))
        );
    }

    #[test]
    fn unknown_type_codes_and_bad_lengths_are_protocol_errors() {
        assert!(matches!(parse_reply(&[0x0f]), Err(SkyError::Protocol(_))));
        assert!(matches!(
            parse_reply(&[0x11, b'x', 0x0a]),
            Err(SkyError::Protocol(_))
        ));
        assert!(matches!(
            parse_reply(&[0x0d, b'x', 0x0a]),
            Err(SkyError::Protocol(_))
        ));
    }

    #[test]
    fn feature_names_match_the_adapter_report() {
        let f = sky_features(0, 10, 0, &[]);
        assert_eq!(f.name("write"), Some("none"));
        let f = sky_features(10, 10, 3, &[]);
        assert_eq!(f.name("write"), Some("complete"));
        assert_eq!(f.name("read"), Some("partial"));
        let f = sky_features(4, 10, 0, &[]);
        assert_eq!(f.name("write"), Some("partial"));
    }
}

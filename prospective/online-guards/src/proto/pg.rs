//! PostgreSQL 3.0 simple-query adapter.
//!
//! The interesting boundary is different from Skytable's.  PostgreSQL *does*
//! have a recovery command and a status byte, so the adapter can distinguish
//! "idle" from "inside a transaction" and from "aborted transaction" after a
//! cancelled exchange.  What it cannot distinguish is whether a cancelled
//! COMMIT took effect; that outcome stays indeterminate and is never turned
//! into success.
//!
//! The invariant for reuse is: the wire is exactly at a message boundary and
//! the server reported `ReadyForQuery` with status `I` (idle).  A status of
//! `T` or `E` is an open transaction; the adapter must issue ROLLBACK and
//! observe `I` before the object can be Ready.  `DISCARD ALL` is a session
//! reset, not a transaction reset: it is illegal inside a transaction and is
//! never used as a rollback substitute.

use crate::guard::{Cause, Discharge, DischargeKind, Guard, Qualification, Ticket};
use crate::pool::{GuardedSession, Manager, ValidateOutcome};
use crate::recovery::{EffectContract, RecoveryAttempt, RecoveryContext};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PG_DRAIN: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub enum PgError {
    Refused(crate::guard::Refusal),
    Io(std::io::Error),
    Protocol(String),
    Server(PgErrorInfo),
    Cut(CutSnapshot),
    Closed,
}

impl fmt::Display for PgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgError::Refused(r) => write!(f, "refused: {r}"),
            PgError::Io(e) => write!(f, "io: {e}"),
            PgError::Protocol(s) => write!(f, "protocol: {s}"),
            PgError::Server(e) => write!(f, "server {}: {}", e.code, e.message),
            PgError::Cut(s) => write!(f, "cut at {s:?}"),
            PgError::Closed => f.write_str("connection closed"),
        }
    }
}

impl std::error::Error for PgError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PgErrorInfo {
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PgStatus {
    #[default]
    Unknown,
    Idle,
    InTransaction,
    FailedTransaction,
}

impl PgStatus {
    fn from_byte(b: u8) -> Self {
        match b {
            b'I' => PgStatus::Idle,
            b'T' => PgStatus::InTransaction,
            b'E' => PgStatus::FailedTransaction,
            _ => PgStatus::Unknown,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            PgStatus::Idle => "idle",
            PgStatus::InTransaction => "in_transaction",
            PgStatus::FailedTransaction => "failed_transaction",
            PgStatus::Unknown => "unknown",
        }
    }
    pub fn open(self) -> bool {
        matches!(
            self,
            PgStatus::InTransaction | PgStatus::FailedTransaction
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct PgReply {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub command: Option<String>,
    pub error: Option<PgErrorInfo>,
    pub notices: Vec<PgErrorInfo>,
    pub status: PgStatus,
}

impl PgReply {
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

#[derive(Clone)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub connect_timeout: Duration,
}

impl PgConfig {
    pub fn loopback(port: u16, user: &str, database: &str) -> Self {
        Self {
            host: "127.0.0.1".into(),
            port,
            user: user.into(),
            database: database.into(),
            connect_timeout: Duration::from_secs(3),
        }
    }
}

struct PgIo {
    stream: Option<TcpStream>,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    out_off: usize,
}

pub struct PgConn {
    io: PgIo,
    guard: Arc<Guard>,
    contract: EffectContract,
    pub status: PgStatus,
    pub backend_pid: i32,
    pub cut: Option<CutPoint>,
    pub write_chunk: Option<usize>,
    cut_snapshot: Option<CutSnapshot>,
    /// `Some(kind)` when `io.out` is a recovery request that may already be on
    /// the wire.  A retry must not send it twice.
    out_kind: Option<&'static str>,
    /// Set once a recovery observed `I`; a retry can mint the witness without
    /// waiting for another ReadyForQuery.  This is per-process, per-attempt
    /// state; it is not a durable progress marker.
    recovered_ready: bool,
    /// A complete request has been written and its reply has not been read to
    /// ReadyForQuery.  `rollback`/`discard` must not be sent while this is
    /// true: they would consume the abandoned exchange's reply.
    pending: bool,
}

fn put_i32(v: &mut Vec<u8>, n: i32) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_i16(v: &mut Vec<u8>, n: i16) {
    v.extend_from_slice(&n.to_be_bytes());
}

fn put_cstr(v: &mut Vec<u8>, s: &str) {
    v.extend_from_slice(s.as_bytes());
    v.push(0);
}

pub fn encode_query(sql: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(sql.len() + 6);
    v.push(b'Q');
    put_i32(&mut v, (sql.len() + 5) as i32);
    put_cstr(&mut v, sql);
    v
}

pub fn encode_startup(cfg: &PgConfig) -> Vec<u8> {
    let mut body = Vec::new();
    put_i32(&mut body, 196608); // protocol 3.0
    put_cstr(&mut body, "user");
    put_cstr(&mut body, &cfg.user);
    put_cstr(&mut body, "database");
    put_cstr(&mut body, &cfg.database);
    put_cstr(&mut body, "client_encoding");
    put_cstr(&mut body, "UTF8");
    body.push(0);
    let mut v = Vec::new();
    put_i32(&mut v, (body.len() + 4) as i32);
    v.extend_from_slice(&body);
    v
}

fn take_i32(b: &[u8], i: &mut usize) -> Option<i32> {
    let end = i.checked_add(4)?;
    let s = b.get(*i..end)?;
    *i = end;
    Some(i32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn take_i16(b: &[u8], i: &mut usize) -> Option<i16> {
    let end = i.checked_add(2)?;
    let s = b.get(*i..end)?;
    *i = end;
    Some(i16::from_be_bytes([s[0], s[1]]))
}

fn take_cstr<'a>(b: &'a [u8], i: &mut usize) -> Option<&'a str> {
    let start = *i;
    let pos = b.get(start..)?.iter().position(|&c| c == 0)?;
    let end = start + pos;
    *i = end + 1;
    std::str::from_utf8(&b[start..end]).ok()
}

fn parse_error(payload: &[u8]) -> PgErrorInfo {
    let mut info = PgErrorInfo::default();
    let mut i = 0;
    while i < payload.len() {
        let code = payload[i];
        i += 1;
        if code == 0 {
            break;
        }
        let Some(value) = take_cstr(payload, &mut i) else {
            break;
        };
        match code {
            b'S' => info.severity = value.into(),
            b'C' => info.code = value.into(),
            b'M' => info.message = value.into(),
            _ => {}
        }
    }
    info
}

fn parse_row_description(payload: &[u8]) -> Result<Vec<String>, String> {
    let mut i = 0;
    let Some(n) = take_i16(payload, &mut i) else {
        return Err("short RowDescription".into());
    };
    let mut cols = Vec::with_capacity(n.max(0) as usize);
    for _ in 0..n {
        let Some(name) = take_cstr(payload, &mut i) else {
            return Err("short column name".into());
        };
        // table oid, attnum, type oid, typlen, typmod, format
        if payload.len() < i + 4 + 2 + 4 + 2 + 4 + 2 {
            return Err("short column description".into());
        }
        i += 18;
        cols.push(name.to_string());
    }
    Ok(cols)
}

fn parse_data_row(payload: &[u8]) -> Result<Vec<Option<String>>, String> {
    let mut i = 0;
    let Some(n) = take_i16(payload, &mut i) else {
        return Err("short DataRow".into());
    };
    let mut row = Vec::with_capacity(n.max(0) as usize);
    for _ in 0..n {
        let Some(len) = take_i32(payload, &mut i) else {
            return Err("short field length".into());
        };
        if len < 0 {
            row.push(None);
            continue;
        }
        let end = i + len as usize;
        let Some(raw) = payload.get(i..end) else {
            return Err("short field".into());
        };
        i = end;
        row.push(Some(String::from_utf8_lossy(raw).into_owned()));
    }
    Ok(row)
}

impl PgConn {
    pub async fn connect(cfg: &PgConfig) -> std::io::Result<Self> {
        let connect = TcpStream::connect((cfg.host.as_str(), cfg.port));
        let mut stream = match tokio::time::timeout(cfg.connect_timeout, connect).await {
            Ok(r) => r?,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "postgres connect timed out",
                ));
            }
        };
        stream.write_all(&encode_startup(cfg)).await?;
        let mut conn = Self {
            io: PgIo {
                stream: Some(stream),
                inbuf: Vec::new(),
                out: Vec::new(),
                out_off: 0,
            },
            guard: Arc::new(Guard::new()),
            contract: EffectContract::MustNotApply,
            status: PgStatus::Unknown,
            backend_pid: 0,
            cut: None,
            write_chunk: None,
            cut_snapshot: None,
            out_kind: None,
            recovered_ready: false,
            pending: false,
        };
        loop {
            let (ty, payload) = conn.read_frame().await.map_err(io_of)?;
            match ty {
                b'R' => {
                    let mut i = 0;
                    let code = take_i32(&payload, &mut i).unwrap_or(-1);
                    if code != 0 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            format!("postgres requested auth method {code}"),
                        ));
                    }
                }
                b'S' | b'N' => {}
                b'K' => {
                    let mut i = 0;
                    conn.backend_pid = take_i32(&payload, &mut i).unwrap_or(0);
                }
                b'E' => {
                    let info = parse_error(&payload);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("postgres startup error {}: {}", info.code, info.message),
                    ));
                }
                b'Z' => {
                    let status = payload.first().copied().unwrap_or(b'?');
                    conn.status = PgStatus::from_byte(status);
                    if conn.status == PgStatus::Unknown {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "postgres startup returned an unknown ReadyForQuery status",
                        ));
                    }
                    return Ok(conn);
                }
                other => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unexpected startup message {other:#x}"),
                    ));
                }
            }
        }
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

    async fn read_frame(&mut self) -> Result<(u8, Vec<u8>), PgError> {
        loop {
            if self.io.inbuf.len() >= 5 {
                let len = i32::from_be_bytes([
                    self.io.inbuf[1],
                    self.io.inbuf[2],
                    self.io.inbuf[3],
                    self.io.inbuf[4],
                ]);
                if len < 4 {
                    return Err(PgError::Protocol("bad frame length".into()));
                }
                let total = 1 + len as usize;
                if self.io.inbuf.len() >= total {
                    let ty = self.io.inbuf[0];
                    let payload = self.io.inbuf[5..total].to_vec();
                    self.io.inbuf.drain(..total);
                    return Ok((ty, payload));
                }
            }
            let n = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return Err(PgError::Closed);
                };
                stream.read_buf(&mut io.inbuf).await.map_err(PgError::Io)?
            };
            if n == 0 {
                return Err(PgError::Closed);
            }
        }
    }

    async fn flush_out(&mut self, ticket: Option<&mut Ticket<'_>>) -> Result<(), PgError> {
        let mut ticket = ticket;
        loop {
            if self.io.out_off >= self.io.out.len() {
                if let Some(t) = ticket.as_deref_mut() {
                    t.tag("write", "complete");
                }
                return Ok(());
            }
            if let Some(cut) = self.cut {
                if cut.after_write_bytes == Some(self.io.out_off) {
                    self.cut = None;
                    let snap = self.snapshot("write");
                    return Err(PgError::Cut(snap));
                }
            }
            let end = match self.write_chunk {
                Some(chunk) => (self.io.out_off + chunk).min(self.io.out.len()),
                None => self.io.out.len(),
            };
            let n = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return Err(PgError::Closed);
                };
                stream
                    .write(&io.out[io.out_off..end])
                    .await
                    .map_err(PgError::Io)?
            };
            if n == 0 {
                return Err(PgError::Closed);
            }
            self.io.out_off += n;
            if let Some(t) = ticket.as_deref_mut() {
                t.set("write_bytes", self.io.out_off);
                t.tag(
                    "write",
                    if self.io.out_off == self.io.out.len() {
                        "complete"
                    } else {
                        "partial"
                    },
                );
            }
            if let Some(at) = self.cut.and_then(|c| c.after_write_bytes) {
                if self.io.out_off >= at {
                    self.cut = None;
                    let snap = self.snapshot("write");
                    return Err(PgError::Cut(snap));
                }
            }
        }
    }

    fn snapshot(&mut self, fired_after: &'static str) -> CutSnapshot {
        let snap = CutSnapshot {
            write_bytes: self.io.out_off,
            write_state: if self.io.out_off == 0 {
                "none"
            } else if self.io.out_off == self.io.out.len() {
                "complete"
            } else {
                "partial"
            },
            read_bytes: self.io.inbuf.len(),
            read_state: if self.io.inbuf.is_empty() {
                "none"
            } else {
                "partial"
            },
            fired_after,
        };
        snap
    }

    /// Execute one simple query.  A server ErrorResponse is a complete
    /// exchange (the connection is still reusable after ReadyForQuery); the
    /// ticket is discharged with `ServerError`, not abandoned.
    pub async fn query(&mut self, sql: &str) -> Result<PgReply, PgError> {
        if self.io.stream.is_none() {
            return Err(PgError::Closed);
        }
        self.recovered_ready = false;
        let g = self.guard.clone();
        let mut t = g.begin().map_err(PgError::Refused)?;
        t.arm();
        t.tag("op", "query");
        t.tag(
            "contract",
            match self.contract {
                EffectContract::AtomicEffectMayApply => "may_apply",
                EffectContract::MustNotApply => "must_not_apply",
            },
        );
        t.tag("tx", self.status.code());
        t.set("write_bytes", 0usize);
        t.tag("write", "none");
        t.set("read_bytes", 0usize);
        t.tag("read", "none");
        self.io.out = encode_query(sql);
        self.io.out_off = 0;
        self.out_kind = Some("query");
        if let Err(e) = self.flush_out(Some(&mut t)).await {
            // A write cut can fire after the last byte reached the socket; the
            // request is then complete and its reply is pending even though the
            // write path returned an error.
            self.pending = self.io.out_off == self.io.out.len();
            let cause = match &e {
                PgError::Cut(_) => Cause::Cancelled,
                PgError::Io(_) | PgError::Closed => Cause::Io,
                _ => Cause::Protocol,
            };
            t.abandon(cause);
            return Err(e);
        }
        self.pending = true;
        let reply = match self.read_until_ready(Some(&mut t)).await {
            Ok(r) => r,
            Err(e) => {
                let cause = match &e {
                    PgError::Cut(_) => Cause::Cancelled,
                    PgError::Io(_) | PgError::Closed => Cause::Io,
                    PgError::Protocol(_) => Cause::Protocol,
                    _ => Cause::Other("query"),
                };
                t.abandon(cause);
                return Err(e);
            }
        };
        self.pending = false;
        self.status = reply.status;
        if reply.status.open() {
            self.guard.enter_standing("open_transaction");
        } else if reply.status == PgStatus::Idle {
            self.guard.clear_standing("open_transaction");
        }
        let kind = if reply.is_error() {
            DischargeKind::ServerError
        } else {
            DischargeKind::Complete
        };
        let d = Discharge::new(g.obj(), t.epoch(), t.op(), kind, "pg.ready_for_query");
        if let Err(e) = t.settle(d) {
            return Err(PgError::Protocol(format!("settlement rejected: {e}")));
        }
        self.io.out.clear();
        self.io.out_off = 0;
        self.out_kind = None;
        Ok(reply)
    }

    async fn read_until_ready(
        &mut self,
        mut ticket: Option<&mut Ticket<'_>>,
    ) -> Result<PgReply, PgError> {
        let mut reply = PgReply::default();
        loop {
            if let Some(cut) = self.cut {
                if let Some(at) = cut.after_read_bytes {
                    let fire = if at == 0 {
                        self.io.inbuf.is_empty()
                    } else {
                        self.io.inbuf.len() >= at
                    };
                    if fire {
                        self.cut = None;
                        let snap = self.snapshot("read");
                        return Err(PgError::Cut(snap));
                    }
                }
            }
            let (ty, payload) = self.read_frame().await?;
            if let Some(t) = ticket.as_deref_mut() {
                t.set("read_bytes", payload.len() + 5);
                t.tag("read", "partial");
            }
            match ty {
                b'T' => reply.columns = parse_row_description(&payload).map_err(PgError::Protocol)?,
                b'D' => reply
                    .rows
                    .push(parse_data_row(&payload).map_err(PgError::Protocol)?),
                b'C' => {
                    let mut i = 0;
                    reply.command = take_cstr(&payload, &mut i).map(|s| s.to_string());
                }
                b'E' => reply.error = Some(parse_error(&payload)),
                b'N' => reply.notices.push(parse_error(&payload)),
                b'S' | b'K' => {}
                b'Z' => {
                    let status = payload.first().copied().unwrap_or(b'?');
                    reply.status = PgStatus::from_byte(status);
                    if reply.status == PgStatus::Unknown {
                        // A ReadyForQuery with an unknown status is not evidence
                        // that the session is reusable.  Consume the frame but
                        // make the object unsafe.
                        self.pending = false;
                        self.status = PgStatus::Unknown;
                        return Err(PgError::Protocol(
                            "unknown_ready_status".to_string(),
                        ));
                    }
                    if let Some(t) = ticket.as_deref_mut() {
                        t.tag("read", "complete");
                        t.tag("tx_after", reply.status.code());
                    }
                    return Ok(reply);
                }
                other => {
                    return Err(PgError::Protocol(format!(
                        "unexpected message {other:#x} from server"
                    )));
                }
            }
        }
    }

    /// Drain the abandoned exchange to ReadyForQuery.  If the status is T/E,
    /// issue ROLLBACK and wait for I.  Everything is persisted in `io` and
    /// `status` so a cancelled recovery can be retried.
    /// Read protocol messages until ReadyForQuery, rejecting an unknown status.
    async fn read_to_ready(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), RecoveryAttempt> {
        loop {
            match tokio::time::timeout_at(deadline, self.read_frame()).await {
                Ok(Ok((b'Z', payload))) => {
                    let status = PgStatus::from_byte(payload.first().copied().unwrap_or(b'?'));
                    if status == PgStatus::Unknown {
                        self.status = PgStatus::Unknown;
                        return Err(RecoveryAttempt::Failed(Cause::Other(
                            "unknown_ready_status",
                        )));
                    }
                    self.status = status;
                    return Ok(());
                }
                Ok(Ok(_)) => {}
                Ok(Err(PgError::Closed)) => return Err(RecoveryAttempt::Failed(Cause::Io)),
                Ok(Err(PgError::Protocol(_))) => {
                    return Err(RecoveryAttempt::Failed(Cause::Protocol))
                }
                Ok(Err(_)) => return Err(RecoveryAttempt::Failed(Cause::Io)),
                Err(_) => return Err(RecoveryAttempt::Unresolved(Cause::Timeout)),
            }
        }
    }

    /// Drain the abandoned exchange to ReadyForQuery.  If the status is T/E,
    /// issue ROLLBACK and wait for I.  A successful reset also clears the
    /// standing transaction obligation: the adapter observed the rollback.
    async fn drain_and_reset(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), RecoveryAttempt> {
        if self.io.out_off < self.io.out.len() {
            return Err(RecoveryAttempt::Failed(Cause::Other(
                "partial_write_unrecoverable",
            )));
        }
        if self.status == PgStatus::Unknown {
            return Err(RecoveryAttempt::Failed(Cause::Other("unknown_status")));
        }
        // Only read an abandoned exchange if one is actually pending.  On a
        // clean standing obligation (a completed BEGIN returned to the pool)
        // there is no reply to drain; waiting for a Z would hang.
        if self.pending {
            self.read_to_ready(deadline).await?;
            self.pending = false;
            self.recovered_ready = true;
        }
        if self.status.open() {
            if self.out_kind != Some("rollback") {
                self.io.out = encode_query("ROLLBACK");
                self.io.out_off = 0;
                self.out_kind = Some("rollback");
            }
            if let Err(e) = self.flush_out(None).await {
                return Err(RecoveryAttempt::Failed(Cause::Other(match e {
                    PgError::Io(_) | PgError::Closed => "io",
                    _ => "protocol",
                })));
            }
            self.pending = true;
            self.read_to_ready(deadline).await?;
            self.pending = false;
            if self.status != PgStatus::Idle {
                return Err(RecoveryAttempt::Failed(Cause::Other("rollback_not_idle")));
            }
        }
        if self.status != PgStatus::Idle {
            return Err(RecoveryAttempt::Failed(Cause::Other("not_idle_after_reset")));
        }
        self.guard.clear_standing("open_transaction");
        self.recovered_ready = true;
        Ok(())
    }

    pub async fn recover(&mut self, ctx: &RecoveryContext) -> RecoveryAttempt {
        let timeout = PG_DRAIN.min(ctx.remaining);
        let deadline = tokio::time::Instant::now() + timeout;
        match ctx.plan {
            "drain_reset" => {
                if let Err(attempt) = self.drain_and_reset(deadline).await {
                    return attempt;
                }
                self.finish_recovery(ctx, "pg.ready_idle")
            }
            "finish_drain_reset" => {
                if self.io.out_off < self.io.out.len()
                    && ctx.contract != EffectContract::AtomicEffectMayApply
                {
                    return RecoveryAttempt::Failed(Cause::Other("contract_forbids_finish"));
                }
                if let Err(e) = self.finish_pending_write(deadline).await {
                    return e;
                }
                self.pending = self.io.out_off == self.io.out.len();
                if let Err(attempt) = self.drain_and_reset(deadline).await {
                    return attempt;
                }
                self.finish_recovery(ctx, "pg.ready_idle_after_finish")
            }
            "rollback" => {
                if self.pending {
                    // A complete request's reply is still unread.  Reading the
                    // first Z here would acknowledge the abandoned exchange and
                    // leave ROLLBACK's own reply pending.
                    return RecoveryAttempt::Failed(Cause::Other(
                        "pending_exchange_requires_drain",
                    ));
                }
                if self.io.out_off < self.io.out.len() {
                    return RecoveryAttempt::Failed(Cause::Other(
                        "partial_write_unrecoverable",
                    ));
                }
                if self.status == PgStatus::Unknown {
                    return RecoveryAttempt::Failed(Cause::Other("unknown_status"));
                }
                if self.out_kind != Some("rollback") {
                    self.io.out = encode_query("ROLLBACK");
                    self.io.out_off = 0;
                    self.out_kind = Some("rollback");
                }
                if let Err(e) = self.flush_out(None).await {
                    return RecoveryAttempt::Failed(Cause::Other(match e {
                        PgError::Io(_) | PgError::Closed => "io",
                        _ => "protocol",
                    }));
                }
                self.pending = true;
                if let Err(attempt) = self.read_to_ready(deadline).await {
                    return attempt;
                }
                self.pending = false;
                if self.status != PgStatus::Idle {
                    return RecoveryAttempt::Failed(Cause::Other("rollback_not_idle"));
                }
                self.guard.clear_standing("open_transaction");
                self.finish_recovery(ctx, "pg.rolled_back")
            }
            "discard" => {
                if self.pending {
                    return RecoveryAttempt::Failed(Cause::Other(
                        "pending_exchange_requires_drain",
                    ));
                }
                if self.status != PgStatus::Idle {
                    // DISCARD ALL inside a transaction is an error (25001) and
                    // leaves the server in E; it is not a rollback substitute.
                    return RecoveryAttempt::Failed(Cause::Other("discard_requires_idle"));
                }
                self.io.out = encode_query("DISCARD ALL");
                self.io.out_off = 0;
                self.out_kind = Some("discard");
                if let Err(e) = self.flush_out(None).await {
                    return RecoveryAttempt::Failed(Cause::Other(match e {
                        PgError::Io(_) | PgError::Closed => "io",
                        _ => "protocol",
                    }));
                }
                self.pending = true;
                if let Err(attempt) = self.read_to_ready(deadline).await {
                    return attempt;
                }
                self.pending = false;
                if self.status != PgStatus::Idle {
                    return RecoveryAttempt::Failed(Cause::Other("discard_not_idle"));
                }
                self.finish_recovery(ctx, "pg.discarded")
            }
            _ => RecoveryAttempt::Failed(Cause::Other("unknown_plan")),
        }
    }

    fn finish_recovery(&mut self, ctx: &RecoveryContext, why: &'static str) -> RecoveryAttempt {
        self.io.out.clear();
        self.io.out_off = 0;
        self.out_kind = None;
        self.recovered_ready = false;
        RecoveryAttempt::recovered(ctx, why)
    }

    async fn finish_pending_write(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), RecoveryAttempt> {
        while self.io.out_off < self.io.out.len() {
            let (stream, out, off) = {
                let io = &mut self.io;
                let Some(stream) = io.stream.as_mut() else {
                    return Err(RecoveryAttempt::Failed(Cause::Io));
                };
                (stream, &io.out[..], &mut io.out_off)
            };
            match tokio::time::timeout_at(deadline, stream.write(&out[*off..])).await {
                Ok(Ok(0)) => return Err(RecoveryAttempt::Failed(Cause::Io)),
                Ok(Ok(n)) => *off += n,
                Ok(Err(_)) => return Err(RecoveryAttempt::Failed(Cause::Io)),
                Err(_) => return Err(RecoveryAttempt::Unresolved(Cause::Timeout)),
            }
        }
        Ok(())
    }

    /// Health probe for a clean object.
    pub async fn validate(&mut self) -> ValidateOutcome {
        match self.query("SELECT 1").await {
            Ok(r) if !r.is_error() && r.status == PgStatus::Idle => ValidateOutcome::Healthy,
            _ => ValidateOutcome::Unhealthy(Cause::Other("pg_probe_failed")),
        }
    }
}

impl GuardedSession for PgConn {
    fn guard(&self) -> &Arc<Guard> {
        &self.guard
    }
    fn close(&mut self) {
        self.io.stream.take();
    }
    fn effect_contract(&self) -> EffectContract {
        self.contract
    }
}

impl Drop for PgConn {
    fn drop(&mut self) {
        self.io.stream.take();
    }
}

pub struct PgManager {
    pub cfg: PgConfig,
    pub contract: EffectContract,
}

impl PgManager {
    pub fn new(cfg: PgConfig, contract: EffectContract) -> Self {
        Self { cfg, contract }
    }
}

impl Manager<PgConn> for PgManager {
    fn name(&self) -> &'static str {
        "postgres-simple"
    }
    async fn connect(&self) -> std::io::Result<PgConn> {
        Ok(PgConn::connect(&self.cfg).await?.with_contract(self.contract))
    }
    async fn validate(&self, session: &mut PgConn) -> ValidateOutcome {
        session.validate().await
    }
    async fn recover(&self, session: &mut PgConn, ctx: &RecoveryContext) -> RecoveryAttempt {
        session.recover(ctx).await
    }
}

fn io_of(e: PgError) -> std::io::Error {
    match e {
        PgError::Io(e) => e,
        PgError::Closed => std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed"),
        other => std::io::Error::other(other.to_string()),
    }
}

/// Observation features exposed to the policy.
pub fn pg_features(status: PgStatus, write: &'static str, read: &'static str) -> crate::features::Features {
    let mut f = crate::features::Features::new();
    f.tag("write", write);
    f.tag("read", read);
    f.tag("tx", status.code());
    f
}

#[allow(dead_code)]
fn _q(_: Qualification) {}

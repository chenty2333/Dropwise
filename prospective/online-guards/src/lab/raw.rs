//! Baseline clients with the same wire codec but different invalidation rules.
//!
//! `RawSky` is deliberately not built on the ledger: it models the target
//! library (no guard, reuse the stream) and the two simple repairs:
//!
//! * `PoisonRetire`: a boolean poisoned flag; a poisoned connection is
//!   reconnected before its next use, never drained (the shape of the Skytable
//!   0.8.12 patch and of `has_broken`).
//! * `DrainOnly`: on the next use, try to consume exactly one pending reply;
//!   reuse if that succeeds, reconnect otherwise.  No object identity, no
//!   epoch, no quarantine and no ownership between the failed operation and
//!   the drain.

use crate::proto::sky::{
    CutPoint, CutSnapshot, Reply, SkyConfig, SkyError, encode_query, parse_reply,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReuseRule {
    /// Stock: write the next request and read whatever comes back.
    Immediate,
    /// One boolean; poisoned => reconnect.
    PoisonRetire,
    /// Try to drain one frame, then reuse; reconnect on failure.
    DrainOnly,
}

pub struct RawSky {
    pub id: u64,
    pub cfg: SkyConfig,
    pub rule: ReuseRule,
    stream: Option<TcpStream>,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    out_off: usize,
    pub poisoned: bool,
    pub reconnects: u64,
    pub cut: Option<CutPoint>,
    pub write_chunk: Option<usize>,
    pub last_cut: Option<CutSnapshot>,
    pub drains_ok: u64,
    pub drains_failed: u64,
}

impl RawSky {
    pub async fn connect(id: u64, cfg: SkyConfig, rule: ReuseRule) -> std::io::Result<Self> {
        let stream = TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
        let mut s = Self {
            id,
            cfg,
            rule,
            stream: Some(stream),
            inbuf: Vec::new(),
            out: Vec::new(),
            out_off: 0,
            poisoned: false,
            reconnects: 0,
            cut: None,
            write_chunk: None,
            last_cut: None,
            drains_ok: 0,
            drains_failed: 0,
        };
        s.handshake().await?;
        Ok(s)
    }

    async fn handshake(&mut self) -> std::io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "closed",
            ));
        };
        let mut hs = Vec::new();
        hs.extend_from_slice(&[b'H', 0, 0, 0, 0, 0]);
        hs.extend_from_slice(self.cfg.user.len().to_string().as_bytes());
        hs.push(b'\n');
        hs.extend_from_slice(self.cfg.password.len().to_string().as_bytes());
        hs.push(b'\n');
        hs.extend_from_slice(self.cfg.user.as_bytes());
        hs.extend_from_slice(self.cfg.password.as_bytes());
        stream.write_all(&hs).await?;
        let mut resp = [0u8; 4];
        stream.read_exact(&mut resp).await?;
        if resp != [b'H', 0, 0, 0] {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "handshake rejected",
            ));
        }
        self.inbuf.clear();
        self.out.clear();
        self.out_off = 0;
        self.poisoned = false;
        Ok(())
    }

    pub fn io_state(&self) -> (usize, usize, usize) {
        (self.out_off, self.out.len(), self.inbuf.len())
    }

    pub async fn reconnect(&mut self) -> std::io::Result<()> {
        self.stream.take();
        let stream = TcpStream::connect((self.cfg.host.as_str(), self.cfg.port)).await?;
        self.stream = Some(stream);
        self.reconnects += 1;
        self.handshake().await
    }

    /// Before reusing the stream under the selected rule.
    pub async fn prepare_reuse(&mut self, drain_timeout: Duration) -> bool {
        match self.rule {
            ReuseRule::Immediate => true,
            ReuseRule::PoisonRetire => {
                if self.poisoned {
                    match self.reconnect().await {
                        Ok(()) => false,
                        Err(_) => false,
                    }
                } else {
                    true
                }
            }
            ReuseRule::DrainOnly => {
                if self.poisoned {
                    match self.drain_one(drain_timeout).await {
                        Ok(()) => {
                            self.drains_ok += 1;
                            self.poisoned = false;
                            true
                        }
                        Err(_) => {
                            self.drains_failed += 1;
                            let _ = self.reconnect().await;
                            false
                        }
                    }
                } else {
                    true
                }
            }
        }
    }

    /// The raw operation.  Mirrors the stock client's buffer handling: the
    /// request buffer is replaced and the next write starts at offset zero,
    /// even if a previous request was only partially written.
    pub async fn query(&mut self, sql: &str, params: &[u8]) -> Result<Reply, SkyError> {
        if self.stream.is_none() {
            return Err(SkyError::Closed);
        }
        if self.rule != ReuseRule::Immediate {
            self.poisoned = true;
        }
        self.out = encode_query(sql, params);
        if std::env::var_os("RULE_DEBUG").is_some() {
            eprintln!("raw query sql={sql:?} out={:02x?}", self.out);
        }
        self.out_off = 0;
        loop {
            if self.out_off >= self.out.len() {
                break;
            }
            if let Some(cut) = self.cut
                && cut.after_write_bytes == Some(self.out_off)
            {
                return Err(self.fire_cut("write"));
            }
            let (stream, out, off) = {
                let Some(stream) = self.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                (stream, &self.out[..], &mut self.out_off)
            };
            let end = match self.write_chunk {
                Some(chunk) => (*off + chunk).min(out.len()),
                None => out.len(),
            };
            let n = stream.write(&out[*off..end]).await.map_err(SkyError::Io)?;
            if n == 0 {
                return Err(SkyError::Closed);
            }
            *off += n;
            if let Some(cut) = self.cut
                && let Some(at) = cut.after_write_bytes
                && *off >= at
            {
                return Err(self.fire_cut("write"));
            }
        }
        loop {
            // A cut at zero read bytes means "abandon before reading any of
            // the reply"; it must win over a reply that was already buffered.
            if let Some(cut) = self.cut
                && cut.after_read_bytes == Some(0)
            {
                return Err(self.fire_cut("read"));
            }
            if let Some((reply, used)) = parse_reply(&self.inbuf)? {
                self.inbuf.drain(..used);
                self.poisoned = false;
                return Ok(reply);
            }
            if let Some(cut) = self.cut
                && cut.after_read_bytes == Some(self.inbuf.len())
            {
                return Err(self.fire_cut("read"));
            }
            let n = {
                let Some(stream) = self.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                stream
                    .read_buf(&mut self.inbuf)
                    .await
                    .map_err(SkyError::Io)?
            };
            if n == 0 {
                return Err(SkyError::Closed);
            }
            if let Some(cut) = self.cut
                && let Some(at) = cut.after_read_bytes
                && self.inbuf.len() >= at
            {
                return Err(self.fire_cut("read"));
            }
        }
    }

    fn fire_cut(&mut self, fired_after: &'static str) -> SkyError {
        let snapshot = CutSnapshot {
            write_bytes: self.out_off,
            write_state: if self.out_off == 0 {
                "none"
            } else if self.out_off == self.out.len() {
                "complete"
            } else {
                "partial"
            },
            read_bytes: self.inbuf.len(),
            read_state: if self.inbuf.is_empty() {
                "none"
            } else {
                "partial"
            },
            fired_after,
        };
        self.last_cut = Some(snapshot.clone());
        self.cut = None;
        SkyError::Cut(snapshot)
    }

    /// Finish exactly the request that was interrupted, then consume its
    /// reply.  The request bytes are still in `self.out`; only the caller's
    /// authority (an effect contract permitting completion) makes this legal.
    pub async fn finish_write_drain(&mut self, timeout: Duration) -> Result<(), SkyError> {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.out_off < self.out.len() {
            let (stream, out, off) = {
                let Some(stream) = self.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                (stream, &self.out[..], &mut self.out_off)
            };
            match tokio::time::timeout_at(deadline, stream.write(&out[*off..])).await {
                Ok(Ok(0)) => return Err(SkyError::Closed),
                Ok(Ok(n)) => *off += n,
                Ok(Err(e)) => return Err(SkyError::Io(e)),
                Err(_) => {
                    return Err(SkyError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "finish write timeout",
                    )));
                }
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        self.drain_one(remaining).await
    }

    /// Consume exactly one frame, or fail.  This is the protocol-native repair
    /// without the ledger: it does not know whose reply it is draining.
    pub async fn drain_one(&mut self, timeout: Duration) -> Result<(), SkyError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some((_reply, used)) = parse_reply(&self.inbuf)? {
                self.inbuf.drain(..used);
                if !self.inbuf.is_empty() {
                    return Err(SkyError::Protocol("extra bytes after drained reply".into()));
                }
                self.out.clear();
                self.out_off = 0;
                return Ok(());
            }
            let read = {
                let Some(stream) = self.stream.as_mut() else {
                    return Err(SkyError::Closed);
                };
                tokio::time::timeout_at(deadline, stream.read_buf(&mut self.inbuf)).await
            };
            match read {
                Ok(Ok(0)) => return Err(SkyError::Closed),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(SkyError::Io(e)),
                Err(_) => {
                    return Err(SkyError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "drain timeout",
                    )));
                }
            }
        }
    }
}

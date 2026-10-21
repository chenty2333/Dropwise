//! Loopback TCP proxy with two fault modes used by the natural-cancellation
//! trials:
//!
//! * `Delay` holds every server->client chunk for a chosen time.  A client that
//!   cancels while the reply is held has written the whole request and read
//!   nothing.
//! * `LoseReply` forwards the 4-byte handshake reply of the first connection
//!   and then discards everything the server sends on it.  Reconnecting gives
//!   a healthy connection, so "the reply never comes on this connection" is
//!   distinguishable from "the server is slow".
//!
//! Neither mode is a claim about a particular server; they are fault injection
//! for late and lost completions, and the report labels them as such.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug)]
pub enum ProxyMode {
    Delay(Duration),
    LoseReply,
}

#[derive(Default)]
pub struct ProxyStats {
    pub client_to_server: AtomicU64,
    pub server_to_client: AtomicU64,
    pub connections: AtomicU64,
    pub discarded: AtomicU64,
}

pub struct DelayProxy {
    pub addr: SocketAddr,
    pub stats: Arc<ProxyStats>,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl DelayProxy {
    pub async fn start(upstream: SocketAddr, mode: ProxyMode) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let stats = Arc::new(ProxyStats::default());
        let (shutdown, rx) = watch::channel(false);
        let task = tokio::spawn(accept_loop(listener, upstream, mode, stats.clone(), rx));
        Ok(Self {
            addr,
            stats,
            shutdown,
            task,
        })
    }

    pub fn c2s(&self) -> u64 {
        self.stats.client_to_server.load(Ordering::Relaxed)
    }

    pub fn s2c(&self) -> u64 {
        self.stats.server_to_client.load(Ordering::Relaxed)
    }
}

impl Drop for DelayProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    upstream: SocketAddr,
    mode: ProxyMode,
    stats: Arc<ProxyStats>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => {
                let Ok((client, _)) = accepted else { return };
                let index = stats.connections.fetch_add(1, Ordering::Relaxed);
                let stats = stats.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    let Ok(server) = TcpStream::connect(upstream).await else { return };
                    let (mut cr, mut cw) = client.into_split();
                    let (mut sr, mut sw) = server.into_split();
                    let up = {
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            let mut buf = vec![0u8; 16 * 1024];
                            loop {
                                match cr.read(&mut buf).await {
                                    Ok(0) | Err(_) => {
                                        let _ = sw.shutdown().await;
                                        return;
                                    }
                                    Ok(n) => {
                                        if sw.write_all(&buf[..n]).await.is_err() {
                                            return;
                                        }
                                        stats.client_to_server.fetch_add(n as u64, Ordering::Relaxed);
                                    }
                                }
                            }
                        })
                    };
                    let mut shutdown2 = shutdown.clone();
                    let lose = matches!(mode, ProxyMode::LoseReply) && index == 0;
                    tokio::select! {
                        _ = shutdown2.changed() => {}
                        _ = async {
                            let mut buf = vec![0u8; 16 * 1024];
                            let mut forwarded = 0usize;
                            loop {
                                match sr.read(&mut buf).await {
                                    Ok(0) | Err(_) => {
                                        let _ = cw.shutdown().await;
                                        break;
                                    }
                                    Ok(n) => {
                                        if std::env::var_os("OG_PROXY_DEBUG").is_some() {
                                            eprintln!("proxy s2c read conn={index} n={n} head={:02x?}", &buf[..n.min(8)]);
                                        }
                                        if lose {
                                            let k = if forwarded < 4 {
                                                (4 - forwarded).min(n)
                                            } else {
                                                0
                                            };
                                            if k > 0 {
                                                if cw.write_all(&buf[..k]).await.is_err() {
                                                    break;
                                                }
                                                forwarded += k;
                                                stats.server_to_client.fetch_add(k as u64, Ordering::Relaxed);
                                            }
                                            stats.discarded.fetch_add((n - k) as u64, Ordering::Relaxed);
                                        } else {
                                            if std::env::var_os("OG_PROXY_DEBUG").is_some() {
                                                eprintln!(
                                                    "proxy s2c conn={index} n={n} forwarded={forwarded} head={:02x?}",
                                                    &buf[..n.min(8)]
                                                );
                                            }
                                            // The handshake reply is exactly four bytes and
                                            // is not a query reply: forward it immediately so
                                            // a reconnect is not made artificially expensive.
                                            let k = if forwarded < 4 {
                                                (4 - forwarded).min(n)
                                            } else {
                                                0
                                            };
                                            if k > 0 {
                                                if std::env::var_os("OG_PROXY_DEBUG").is_some() {
                                                    eprintln!("proxy handshake write conn={index} len={k}");
                                                }
                                                if cw.write_all(&buf[..k]).await.is_err() {
                                                    break;
                                                }
                                                forwarded += k;
                                                stats.server_to_client.fetch_add(k as u64, Ordering::Relaxed);
                                                if k == n {
                                                    continue;
                                                }
                                            }
                                            if let ProxyMode::Delay(d) = mode
                                                && !d.is_zero() {
                                                    tokio::time::sleep(d).await;
                                                }
                                            if std::env::var_os("OG_PROXY_DEBUG").is_some() {
                                                eprintln!(
                                                    "proxy write conn={index} len={} k={k}",
                                                    n - k
                                                );
                                            }
                                            if cw.write_all(&buf[k..n]).await.is_err() {
                                                break;
                                            }
                                            stats.server_to_client.fetch_add((n - k) as u64, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        } => {}
                    }
                    up.abort();
                });
            }
        }
    }
}

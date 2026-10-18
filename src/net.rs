//! Loopback-only TCP proxy for cancellation experiments.
//!
//! [`Proxy`] listens on `127.0.0.1:0` and forwards every accepted connection to
//! a fixed upstream address. It counts the bytes forwarded in each direction,
//! and [`Proxy::residue`] reports how many bytes wait in the client socket's
//! receive buffer without reading them.
//!
//! [`Proxy::start_with`] can hold every chunk read from the upstream for a
//! configurable real-time delay before writing it to the client. That makes an
//! operation return `Pending` while it waits for the reply without changing the
//! server, which is useful when the server has no `SLEEP`-like primitive.
//!
//! Only compiled with the `net` feature.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Byte counters for one accepted client connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionStats {
    /// Peer address of the client as recorded by `accept` on the proxy listener.
    pub client: SocketAddr,
    /// Bytes forwarded from the client to the upstream server.
    pub to_upstream: u64,
    /// Bytes forwarded from the upstream server to the client.
    pub to_client: u64,
}

/// Unread bytes in one client socket's receive buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionResidue {
    /// Peer address of the client as recorded by `accept` on the proxy listener.
    pub client: SocketAddr,
    /// Bytes pending in the receive buffer; 0 when it is empty or the socket
    /// is closed (see [`Proxy::residue`]).
    pub bytes: usize,
}

/// Options for [`Proxy::start_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyOptions {
    /// Real time to hold every chunk read from the upstream before writing it
    /// to the client. Zero forwards at once.
    ///
    /// The delay is wall-clock time, so this is meant for
    /// [`Flavor::MultiThread`](crate::Flavor::MultiThread); under
    /// [`Flavor::CurrentThread`](crate::Flavor::CurrentThread) Tokio time is
    /// paused and a sleep would never advance. The order and content of the
    /// bytes do not change; only their arrival is postponed. While a chunk is
    /// held it is still in the proxy, not in the client socket, so
    /// [`Proxy::residue`] does not see it.
    pub reply_delay: Duration,
}

/// A TCP proxy bound to `127.0.0.1`, forwarding to a fixed upstream address.
///
/// Dropping the proxy stops its accept loop and signals the connection tasks to
/// drop their sockets.
pub struct Proxy {
    addr: SocketAddr,
    state: Arc<State>,
    shutdown: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl Proxy {
    /// Start listening on `127.0.0.1:0` and forward each accepted connection to
    /// `upstream`.
    pub async fn start(upstream: SocketAddr) -> io::Result<Self> {
        Self::start_with(upstream, ProxyOptions::default()).await
    }

    /// Like [`Proxy::start`], with control over how replies are forwarded.
    pub async fn start_with(upstream: SocketAddr, options: ProxyOptions) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(State {
            listener_addr: addr,
            clients: Mutex::new(Vec::new()),
        });
        let (shutdown, shutdown_rx) = watch::channel(false);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            upstream,
            options,
            Arc::clone(&state),
            shutdown_rx,
        ));
        Ok(Self {
            addr,
            state,
            shutdown,
            accept_task,
        })
    }

    /// The loopback address clients connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Per client connection: client peer address, bytes forwarded to upstream,
    /// bytes forwarded to client.
    ///
    /// The entries are in accept order and are kept for the lifetime of the
    /// proxy, including connections whose client has since disconnected.
    pub fn connections(&self) -> Vec<ConnectionStats> {
        self.state
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|counters| ConnectionStats {
                client: counters.client,
                to_upstream: counters.to_upstream.load(Ordering::Relaxed),
                to_client: counters.to_client.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Per client connection: bytes still unread in the CLIENT socket's receive
    /// buffer.
    ///
    /// The result has one entry per connection accepted so far. On Linux this
    /// walks `/proc/self/fd` and identifies the client socket by its addresses
    /// (local address = the peer address recorded by `accept`, peer address =
    /// this proxy's listener address), then asks the kernel for the receive
    /// queue length with the `FIONREAD` ioctl. That neither reads the data nor
    /// blocks, whether the socket is in blocking or non-blocking mode. If the
    /// client socket is no longer open, its entry reports 0.
    ///
    /// On platforms other than Linux this returns an error with kind
    /// `Unsupported`.
    ///
    /// # Limits
    ///
    /// - Only meaningful while the client is idle (not reading) and the peer
    ///   has stopped sending. Otherwise data can arrive between the `/proc`
    ///   walk and the query.
    /// - The walk uses raw fd numbers. The process may close an fd and the
    ///   kernel reuse its number while the walk runs, so a recycled fd can be
    ///   mistaken for the client socket.
    /// - Server-push protocols (pub/sub, notifications) make the number
    ///   meaningless: the server sends data the client is not expected to
    ///   consume, and the proxy cannot tell that data apart from a reply.
    /// - With [`ProxyOptions::reply_delay`], a reply held in the proxy has not
    ///   reached the client socket yet and is not counted. The count rises once
    ///   the delay elapses and the chunk is written.
    pub fn residue(&self) -> io::Result<Vec<ConnectionResidue>> {
        #[cfg(target_os = "linux")]
        {
            linux::residue(&self.state)
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "residue requires Linux: it inspects client sockets through /proc/self/fd",
            ))
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // Wake the accept loop and every pump task, then make sure an accept
        // that is already queued cannot keep the loop alive.
        let _ = self.shutdown.send(true);
        self.accept_task.abort();
    }
}

struct State {
    listener_addr: SocketAddr,
    clients: Mutex<Vec<ClientCounters>>,
}

struct ClientCounters {
    client: SocketAddr,
    to_upstream: Arc<AtomicU64>,
    to_client: Arc<AtomicU64>,
}

async fn accept_loop(
    listener: TcpListener,
    upstream: SocketAddr,
    options: ProxyOptions,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => accepted,
        };
        let Ok((client, peer)) = accepted else { break };

        // Register the accepted address before connecting upstream so that
        // `residue` can find the client socket even if that connect fails.
        let to_upstream = Arc::new(AtomicU64::new(0));
        let to_client = Arc::new(AtomicU64::new(0));
        state.clients.lock().unwrap().push(ClientCounters {
            client: peer,
            to_upstream: Arc::clone(&to_upstream),
            to_client: Arc::clone(&to_client),
        });

        let Ok(upstream_stream) = TcpStream::connect(upstream).await else {
            continue;
        };

        let (client_read, client_write) = client.into_split();
        let (upstream_read, upstream_write) = upstream_stream.into_split();
        tokio::spawn(pump(
            client_read,
            upstream_write,
            to_upstream,
            shutdown.clone(),
            Duration::ZERO,
        ));
        tokio::spawn(pump(
            upstream_read,
            client_write,
            to_client,
            shutdown.clone(),
            options.reply_delay,
        ));
    }
}

/// Copy one direction until EOF, an error, or proxy shutdown, counting the
/// bytes that were successfully written to the destination.
///
/// When `reply_delay` is non-zero, every chunk read from the reader is held in
/// the proxy for that real-time delay before it is written. Reads stop while a
/// chunk is held, so the byte stream keeps its order.
async fn pump<R, W>(
    mut reader: R,
    mut writer: W,
    counter: Arc<AtomicU64>,
    mut shutdown: watch::Receiver<bool>,
    reply_delay: Duration,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if *shutdown.borrow() {
        return;
    }

    let mut buf = [0u8; 8 * 1024];
    'copy: loop {
        let n = tokio::select! {
            _ = shutdown.changed() => break,
            read = reader.read(&mut buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            },
        };

        if !reply_delay.is_zero() {
            tokio::select! {
                _ = shutdown.changed() => break 'copy,
                _ = tokio::time::sleep(reply_delay) => {}
            }
        }

        // Write the chunk in pieces so a proxy drop can interrupt a blocked
        // write, and count only bytes that actually reached the socket.
        let mut written = 0;
        while written < n {
            let result = tokio::select! {
                _ = shutdown.changed() => break 'copy,
                result = writer.write(&buf[written..n]) => result,
            };
            match result {
                Ok(0) | Err(_) => break 'copy,
                Ok(k) => {
                    written += k;
                    counter.fetch_add(k as u64, Ordering::Relaxed);
                }
            }
        }
    }

    // Propagate EOF so a half-closed client does not leave the upstream side
    // waiting for more input forever.
    let _ = writer.shutdown().await;
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::mem::ManuallyDrop;
    use std::net::{SocketAddr, TcpStream};
    use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

    use super::{ConnectionResidue, State};

    /// Snapshot the receive-buffer state of every accepted client socket.
    pub(super) fn residue(state: &State) -> io::Result<Vec<ConnectionResidue>> {
        let clients: Vec<SocketAddr> = state
            .clients
            .lock()
            .unwrap()
            .iter()
            .map(|counters| counters.client)
            .collect();

        let mut residues = Vec::with_capacity(clients.len());
        for client in clients {
            let bytes = receive_buffer_len(client, state.listener_addr)?;
            residues.push(ConnectionResidue { client, bytes });
        }
        Ok(residues)
    }

    /// `FIONREAD` on the fd whose addresses identify the client socket.
    ///
    /// Returns the pending byte count, and 0 both when the receive buffer is
    /// empty and when the socket is gone. Never consumes data, blocks, or
    /// changes fd flags.
    fn receive_buffer_len(client: SocketAddr, listener: SocketAddr) -> io::Result<usize> {
        let entries = std::fs::read_dir("/proc/self/fd")
            .map_err(|e| io::Error::new(e.kind(), format!("cannot read /proc/self/fd: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                io::Error::new(e.kind(), format!("cannot read /proc/self/fd entry: {e}"))
            })?;
            let Some(fd) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<RawFd>().ok())
            else {
                continue;
            };

            // SAFETY: `fd` comes from `/proc/self/fd` and is wrapped in
            // `ManuallyDrop`, so this borrow never closes it; it is only used
            // for address queries and `FIONREAD`, which neither read from it
            // nor touch its flags. Address checks reject every fd that is not
            // the client socket. The fd can still be closed and its number
            // reused while this walk runs; that race is documented on
            // `Proxy::residue`.
            let stream = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(fd) });

            // The client socket's local address is the address `accept`
            // recorded; its peer address is the proxy's listener address.
            if stream.local_addr().ok() != Some(client) || stream.peer_addr().ok() != Some(listener)
            {
                continue;
            }

            let mut pending: libc::c_int = 0;
            // SAFETY: `FIONREAD` stores the receive queue length in the
            // `c_int` it is given and has no other effect on the socket.
            let rc = unsafe { libc::ioctl(stream.as_raw_fd(), libc::FIONREAD, &mut pending) };
            if rc == -1 {
                let e = io::Error::last_os_error();
                return Err(io::Error::new(
                    e.kind(),
                    format!("FIONREAD on client socket {client} failed: {e}"),
                ));
            }
            return Ok(usize::try_from(pending).unwrap_or(0));
        }

        // No matching fd: the client socket has been closed, so it cannot hold
        // unread bytes any more.
        Ok(0)
    }
}

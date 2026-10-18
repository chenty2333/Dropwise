//! Test-only loopback TCP reply gate. Does not parse or simulate PostgreSQL.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::SeqCst},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub struct ReplyGate {
    pub port: u16,
    pub withheld: Arc<AtomicUsize>,
    hold: watch::Sender<bool>,
    proxy: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl ReplyGate {
    pub async fn start(backend_port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (hold, holding) = watch::channel(false);
        let withheld = Arc::new(AtomicUsize::new(0));
        let proxy = tokio::spawn(forward_with_reply_gate(
            listener,
            backend_port,
            holding,
            withheld.clone(),
        ));
        Ok(Self {
            port,
            withheld,
            hold,
            proxy,
        })
    }
    pub fn hold(&self, value: bool) {
        self.hold.send_replace(value);
    }
    pub async fn stop(&mut self) {
        self.hold(false);
        self.proxy.abort();
        let _ = (&mut self.proxy).await;
    }
}
impl Drop for ReplyGate {
    fn drop(&mut self) {
        self.proxy.abort();
    }
}

async fn forward_with_reply_gate(
    listener: TcpListener,
    backend_port: u16,
    mut hold: watch::Receiver<bool>,
    withheld: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let (client, _) = listener.accept().await?;
    let backend = TcpStream::connect(("127.0.0.1", backend_port)).await?;
    let (mut cr, mut cw) = client.into_split();
    let (mut br, mut bw) = backend.into_split();
    let upstream = async {
        tokio::io::copy(&mut cr, &mut bw).await?;
        bw.shutdown().await
    };
    let downstream = async {
        let mut bytes = [0; 8192];
        loop {
            let n = br.read(&mut bytes).await?;
            if n == 0 {
                return cw.shutdown().await;
            }
            if *hold.borrow() {
                withheld.fetch_add(n, SeqCst);
                hold.wait_for(|holding| !holding)
                    .await
                    .map_err(std::io::Error::other)?;
            }
            cw.write_all(&bytes[..n]).await?;
        }
    };
    tokio::try_join!(upstream, downstream)?;
    Ok(())
}

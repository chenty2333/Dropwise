//! Deep instrumentation for the round-3 skytable candidate (new in round 3,
//! deep pass). This binary does not define ladder levels; it produces the
//! request/reply trace the ladder cannot show.
//!
//! Modes:
//!   sky-deep direct <cut-ms> <reply-delay-ms> <rep>
//!   sky-deep pooled <cut-ms> <reply-delay-ms> <rep>
//!   sky-deep wire   <cut-ms> <reply-delay-ms> <rep>
//!
//! `direct`/`pooled` use `dropwise::net::Proxy` exactly like the scenario, so
//! `Proxy::residue` reports bytes already in the tested client's socket.
//! `wire` replaces the dropwise proxy with a logging forwarder that holds each
//! server->client chunk for the same delay and prints every chunk in hex; it is
//! an instrumentation run, not a scenario run.
//!
//! `pooled` reproduces the scenario target: the target future is
//! `pool.get()` + one update, so a short cut can land inside bb8's checkout
//! validation query (`sysctl report status`) rather than in the update.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use p4e_skytable::{
    OBSERVER_KEY, SURVIVOR_V, TARGET_V, create_model_query, create_space_query, insert_query,
    select_query, sysctl_query, update_query,
};
use skytable::response::Response;
use skytable::{Config, ConnectionAsync, Query};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static CONN_ID: AtomicUsize = AtomicUsize::new(0);
const Q_TIMEOUT: Duration = Duration::from_secs(2);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

#[derive(Clone)]
struct Srv {
    host: String,
    port: u16,
    user: String,
    pass: String,
}

impl Srv {
    fn from_env() -> Self {
        let addr = env("DW_SKY_SERVER");
        let (host, port) = addr.rsplit_once(':').expect("host:port");
        Self {
            host: host.into(),
            port: port.parse().expect("port"),
            user: env("DW_SKY_USER"),
            pass: env("DW_SKY_PASS"),
        }
    }
    fn config(&self) -> Config {
        Config::new(&self.host, self.port, &self.user, &self.pass)
    }
    fn addr(&self) -> SocketAddr {
        format!("{}:{}", self.host, self.port).parse().unwrap()
    }
}


fn ck_empty(r: Response) -> Result<(), String> {
    match r {
        Response::Empty => Ok(()),
        Response::Error(code) => Err(format!("server error {code}")),
        other => Err(format!("unexpected response {other:?}")),
    }
}

fn ck_row(r: Response) -> Result<u64, String> {
    match r {
        Response::Row(row) => row.into_first_as::<u64>().map_err(|err| format!("{err:?}")),
        Response::Error(code) => Err(format!("server error {code}")),
        Response::Empty => Err("row not found (empty response)".into()),
        other => Err(format!("unexpected response {other:?}")),
    }
}

fn check_empty(r: Result<Response, skytable::error::Error>) -> Result<(), String> {
    match r {
        Ok(Response::Empty) => Ok(()),
        Ok(Response::Error(code)) => Err(format!("server error {code}")),
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    }
}

fn check_row(r: Result<Response, skytable::error::Error>) -> Result<u64, String> {
    match r {
        Ok(Response::Row(row)) => row.into_first_as::<u64>().map_err(|err| format!("{err:?}")),
        Ok(Response::Error(code)) => Err(format!("server error {code}")),
        Ok(Response::Empty) => Err("row not found (empty response)".into()),
        Ok(other) => Err(format!("unexpected response {other:?}")),
        Err(err) => Err(format!("{err:?}")),
    }
}

async fn q(conn: &mut ConnectionAsync, query: &Query) -> Result<Response, String> {
    match tokio::time::timeout(Q_TIMEOUT, conn.query(query)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(err)) => Err(format!("{err:?}")),
        Err(_) => Err("timeout".into()),
    }
}

fn space() -> String {
    p4e_skytable::unique("dw_deep").replace('-', "_")
}

async fn setup_observer(srv: &Srv) -> Result<(ConnectionAsync, String), String> {
    let mut observer = srv.config().connect_async().await.map_err(e)?;
    let space = space();
    check_empty(observer.query(&create_space_query(&space)).await)?;
    check_empty(observer.query(&create_model_query(&space)).await)?;
    check_empty(observer.query(&insert_query(&space, OBSERVER_KEY, 0)).await)?;
    Ok((observer, space))
}

async fn observer_read(observer: &mut ConnectionAsync, space: &str) -> Result<u64, String> {
    check_row(observer.query(&select_query(space, OBSERVER_KEY)).await)
}

async fn cleanup(mut observer: ConnectionAsync, space: &str) {
    let mut del = Query::new(&format!("delete from {space}.kv where id = ?"));
    del.push_param(OBSERVER_KEY);
    let _ = observer.query(&del).await;
    let _ = observer.query(&drop_model_query(space)).await;
    let _ = observer.query(&drop_space_query(space)).await;
}

fn drop_model_query(space: &str) -> Query {
    Query::new(&format!("drop model {space}.kv"))
}
fn drop_space_query(space: &str) -> Query {
    Query::new(&format!("drop space {space}"))
}

async fn cut<F, T>(future: F, cut_ms: u64) -> (bool, Option<Result<T, String>>)
where
    F: std::future::Future<Output = Result<T, String>>,
{
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => (false, Some(result)),
        _ = tokio::time::sleep(Duration::from_millis(cut_ms)) => (true, None),
    }
}

fn now_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

async fn deep_direct(srv: &Srv, cut_ms: u64, delay: Duration, rep: usize) -> Result<(), String> {
    let (mut observer, space) = setup_observer(srv).await?;
    let proxy = dropwise::net::Proxy::start_with(
        srv.addr(),
        dropwise::net::ProxyOptions { reply_delay: delay },
    )
    .await
    .map_err(e)?;
    let tested = Config::new("127.0.0.1", proxy.addr().port(), &srv.user, &srv.pass);
    let mut conn = tested.connect_async().await.map_err(e)?;
    ck_empty(q(&mut conn, &sysctl_query()).await?)?;

    let start = Instant::now();
    println!("DEEP-BEGIN mode=direct rep={rep} cut_ms={cut_ms} delay_ms={} space={space}", delay.as_millis());
    println!("DEEP-REQ mode=direct stage=target t={:.3} packet={:02x?}", now_ms(start), update_query(&space, TARGET_V, OBSERVER_KEY).debug_encode_packet());
    let target = async { ck_empty(q(&mut conn, &update_query(&space, TARGET_V, OBSERVER_KEY)).await?) };
    let (cancelled, completed) = cut(target, cut_ms).await;
    let cut_at = now_ms(start);
    println!("DEEP-CUT mode=direct rep={rep} cancelled={cancelled} completed={completed:?} t={cut_at:.3}");
    println!("DEEP-RESIDUE mode=direct stage=cut entries={:?} max={:?}", proxy.residue(), proxy.max_residue());

    let before = observer_read(&mut observer, &space).await;
    println!("DEEP-OBS mode=direct stage=before value={before:?} t={:.3} residue={:?}", now_ms(start), proxy.residue());

    // Survivor starts as soon as the observer read returns (same order as the
    // ladder): update the same row, then read it back.
    let write_at = now_ms(start);
    let write = ck_empty(q(&mut conn, &update_query(&space, SURVIVOR_V, OBSERVER_KEY)).await?);
    println!("DEEP-SURVIVOR mode=direct op=write value={write:?} start={write_at:.3} end={:.3} residue={:?}", now_ms(start), proxy.residue());
    let own = ck_row(q(&mut conn, &select_query(&space, OBSERVER_KEY)).await?);
    println!("DEEP-SURVIVOR mode=direct op=own value={own:?} residue={:?}", proxy.residue());
    // Diagnostic extra read: the shift should make a second read see the
    // previous read's reply.
    let own2 = ck_row(q(&mut conn, &select_query(&space, OBSERVER_KEY)).await?);
    println!("DEEP-SURVIVOR mode=direct op=own2 value={own2:?} residue={:?}", proxy.residue());
    let visible = observer_read(&mut observer, &space).await;
    println!("DEEP-END mode=direct rep={rep} before={before:?} write={write:?} own={own:?} own2={own2:?} visible={visible:?} t={:.3}", now_ms(start));
    cleanup(observer, &space).await;
    Ok(())
}

async fn deep_pooled(srv: &Srv, cut_ms: u64, delay: Duration, rep: usize) -> Result<(), String> {
    let (mut observer, space) = setup_observer(srv).await?;
    let proxy = dropwise::net::Proxy::start_with(
        srv.addr(),
        dropwise::net::ProxyOptions { reply_delay: delay },
    )
    .await
    .map_err(e)?;
    let tested = Config::new("127.0.0.1", proxy.addr().port(), &srv.user, &srv.pass);
    let pool = skytable::pool::get_async(1, tested)
        .await
        .map_err(|err| format!("pool: {err:?}"))?;
    {
        let mut conn = pool.get().await.map_err(e)?;
        ck_empty(q(&mut conn, &sysctl_query()).await?)?;
    }
    println!("DEEP-BEGIN mode=pooled rep={rep} cut_ms={cut_ms} delay_ms={} space={space}", delay.as_millis());
    println!("DEEP-POOL mode=pooled stage=warm pool={:?}", pool.state());

    let start = Instant::now();
    // Target exactly as the scenario: checkout/validation inside the future.
    let target = async {
        let mut conn = pool.get().await.map_err(|err| format!("pool.get: {err:?}"))?;
        println!("DEEP-REQ mode=pooled stage=update-after-checkout t={:.3}", now_ms(start));
        ck_empty(q(&mut conn, &update_query(&space, TARGET_V, OBSERVER_KEY)).await?)
    };
    let (cancelled, completed) = cut(target, cut_ms).await;
    println!("DEEP-CUT mode=pooled rep={rep} cancelled={cancelled} completed={completed:?} t={:.3} residue={:?} pool={:?}", now_ms(start), proxy.residue(), pool.state());

    let before = observer_read(&mut observer, &space).await;
    println!("DEEP-OBS mode=pooled stage=before value={before:?} t={:.3} residue={:?}", now_ms(start), proxy.residue());

    // The next borrower: get() runs bb8's test_on_check_out -> is_valid ->
    // `sysctl report status` on the same slot before user code sees it.
    let get_at = now_ms(start);
    let mut conn = pool.get().await.map_err(e)?;
    println!("DEEP-CHECKOUT mode=pooled t_start={get_at:.3} t_end={:.3} pool={:?} residue={:?}", now_ms(start), pool.state(), proxy.residue());
    let write = ck_empty(q(&mut conn, &update_query(&space, SURVIVOR_V, OBSERVER_KEY)).await?);
    println!("DEEP-SURVIVOR mode=pooled op=write value={write:?} t={:.3} residue={:?} pool={:?}", now_ms(start), proxy.residue(), pool.state());
    let own = ck_row(q(&mut conn, &select_query(&space, OBSERVER_KEY)).await?);
    println!("DEEP-SURVIVOR mode=pooled op=own value={own:?} t={:.3} residue={:?} pool={:?}", now_ms(start), proxy.residue(), pool.state());
    let visible = observer_read(&mut observer, &space).await;
    println!("DEEP-END mode=pooled rep={rep} before={before:?} write={write:?} own={own:?} visible={visible:?} t={:.3} pool={:?}", now_ms(start), pool.state());
    cleanup(observer, &space).await;
    Ok(())
}

async fn wire_pump<R, W>(
    mut reader: R,
    mut writer: W,
    conn: usize,
    dir: &'static str,
    delay: Duration,
    start: Instant,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = [0u8; 8192];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let read_at = start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "WIRE conn={conn} dir={dir} phase=read t={read_at:.3} len={n} hex={}",
            hex(&buf[..n])
        );
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if writer.write_all(&buf[..n]).await.is_err() {
            break;
        }
        println!(
            "WIRE conn={conn} dir={dir} phase=write t={:.3} len={n}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    let _ = writer.shutdown().await;
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("")
}

async fn start_log_proxy(upstream: SocketAddr, delay: Duration, start: Instant) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let (client, _peer) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let server = match TcpStream::connect(upstream).await {
                Ok(stream) => stream,
                Err(_) => break,
            };
            let id = CONN_ID.fetch_add(1, SeqCst);
            println!("WIRE conn={id} phase=accept t={:.3}", start.elapsed().as_secs_f64() * 1000.0);
            let (client_r, client_w) = client.into_split();
            let (server_r, server_w) = server.into_split();
            tokio::spawn(wire_pump(client_r, server_w, id, "c2s", Duration::ZERO, start));
            tokio::spawn(wire_pump(server_r, client_w, id, "s2c", delay, start));
        }
    });
    Ok(addr)
}

async fn deep_wire(srv: &Srv, cut_ms: u64, delay: Duration, rep: usize) -> Result<(), String> {
    let (mut observer, space) = setup_observer(srv).await?;
    let start = Instant::now();
    let addr = start_log_proxy(srv.addr(), delay, start).await.map_err(e)?;
    let tested = Config::new("127.0.0.1", addr.port(), &srv.user, &srv.pass);
    let mut conn = tested.connect_async().await.map_err(e)?;
    ck_empty(q(&mut conn, &sysctl_query()).await?)?;
    println!("WIRE-BEGIN mode=wire rep={rep} cut_ms={cut_ms} delay_ms={} space={space}", delay.as_millis());

    let target = async { ck_empty(q(&mut conn, &update_query(&space, TARGET_V, OBSERVER_KEY)).await?) };
    let (cancelled, completed) = cut(target, cut_ms).await;
    println!("WIRE-CUT mode=wire rep={rep} cancelled={cancelled} completed={completed:?} t={:.3}", now_ms(start));

    let before = observer_read(&mut observer, &space).await;
    println!("WIRE-OBS mode=wire stage=before value={before:?} t={:.3}", now_ms(start));
    let write = ck_empty(q(&mut conn, &update_query(&space, SURVIVOR_V, OBSERVER_KEY)).await?);
    println!("WIRE-SURVIVOR mode=wire op=write value={write:?} t={:.3}", now_ms(start));
    let own = ck_row(q(&mut conn, &select_query(&space, OBSERVER_KEY)).await?);
    println!("WIRE-SURVIVOR mode=wire op=own value={own:?} t={:.3}", now_ms(start));
    let own2 = ck_row(q(&mut conn, &select_query(&space, OBSERVER_KEY)).await?);
    println!("WIRE-SURVIVOR mode=wire op=own2 value={own2:?} t={:.3}", now_ms(start));
    let visible = observer_read(&mut observer, &space).await;
    println!("WIRE-END mode=wire rep={rep} before={before:?} write={write:?} own={own:?} own2={own2:?} visible={visible:?} t={:.3}", now_ms(start));
    cleanup(observer, &space).await;
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let cut_ms: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(10);
    let delay_ms: u64 = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(50);
    let rep: usize = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(0);
    let srv = Srv::from_env();
    let delay = Duration::from_millis(delay_ms);
    let result = match mode {
        "direct" => deep_direct(&srv, cut_ms, delay, rep).await,
        "pooled" => deep_pooled(&srv, cut_ms, delay, rep).await,
        "wire" => deep_wire(&srv, cut_ms, delay, rep).await,
        other => Err(format!("unknown mode {other:?}; expected direct|pooled|wire")),
    };
    if let Err(err) = result {
        eprintln!("DEEP-ERROR {err}");
        std::process::exit(1);
    }
}

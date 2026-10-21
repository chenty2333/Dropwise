//! Lifecycle probe for a cancelled pipeline: does the next user see the
//! cancelled exchange or a fresh reply?  Used for both the stock client and
//! the dependency-copy patch; the binary itself only uses the public API.
//!
//! Modes: direct | pool2 | tls-direct | tls-pool
//! Second argument: fixed | original (fixed asserts the safe outcome).

use std::net::SocketAddr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use p4e_skytable::{
    create_model_query, create_space_query, drop_model_query, drop_space_query, insert_query,
    select_query, sysctl_query, unique, Server, OBSERVER_KEY, SEED_V,
};
use skytable::error::Error;
use skytable::pool::{ConnectionMgrTcp, ConnectionMgrTls};
use skytable::response::Response;
use skytable::{Config, ConnectionAsync, ConnectionTlsAsync, Pipeline, Query};

const CUT_MS: u64 = 10;
const DELAY_MS: u64 = 50;
const LIFE_KEY: &str = "lifecycle";
const LIFE_V: u64 = 4242;
const Q_TIMEOUT: Duration = Duration::from_secs(3);

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

struct Ctx {
    observer: ConnectionAsync,
    space: String,
    tcp: Config,
    tls: Config,
    cert: String,
    _tcp_proxy: Proxy,
    _tls_proxy: Proxy,
}

fn check_empty(r: Response) -> Result<(), String> {
    match r {
        Response::Empty => Ok(()),
        Response::Error(code) => Err(format!("server error {code}")),
        other => Err(format!("unexpected response {other:?}")),
    }
}

async fn setup() -> Result<Ctx, String> {
    let srv = Server::from_env()?;
    let tls_server: SocketAddr = env("DW_SKY_TLS_SERVER")?.parse().map_err(e)?;
    let cert = std::fs::read_to_string(env("DW_SKY_TLS_CERT")?).map_err(e)?;
    let mut observer = srv.config().connect_async().await.map_err(e)?;
    let space = unique("dw_life").replace('-', "_");
    check_empty(observer.query(&create_space_query(&space)).await.map_err(e)?)?;
    check_empty(observer.query(&create_model_query(&space)).await.map_err(e)?)?;
    check_empty(observer.query(&insert_query(&space, OBSERVER_KEY, SEED_V)).await.map_err(e)?)?;
    check_empty(observer.query(&insert_query(&space, LIFE_KEY, LIFE_V)).await.map_err(e)?)?;

    let tcp_upstream: SocketAddr = format!("{}:{}", srv.host, srv.port).parse().map_err(e)?;
    let delay = ProxyOptions { reply_delay: Duration::from_millis(DELAY_MS) };
    let tcp_proxy = Proxy::start_with(tcp_upstream, delay).await.map_err(e)?;
    let tls_proxy = Proxy::start_with(tls_server, delay).await.map_err(e)?;
    let tcp = Config::new("127.0.0.1", tcp_proxy.addr().port(), &srv.user, &srv.pass);
    let tls = Config::new("127.0.0.1", tls_proxy.addr().port(), &srv.user, &srv.pass);
    Ok(Ctx {
        observer,
        space,
        tcp,
        tls,
        cert,
        _tcp_proxy: tcp_proxy,
        _tls_proxy: tls_proxy,
    })
}

async fn cleanup(ctx: &mut Ctx) {
    let _ = ctx.observer.query(&drop_model_query(&ctx.space)).await;
    let _ = ctx.observer.query(&drop_space_query(&ctx.space)).await;
}

fn pipe(q: &Query) -> Pipeline {
    let mut p = Pipeline::new();
    p.push(q);
    p
}

async fn cut<F, T>(future: F, ms: u64) -> (bool, Option<T>)
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => (false, Some(result)),
        _ = tokio::time::sleep(Duration::from_millis(ms)) => (true, None),
    }
}

#[derive(Debug)]
enum Outcome {
    Fresh,
    Stale(u64),
    Blocked,
    Other(String),
}

fn classify(r: Result<Vec<Response>, Error>, expect: u64) -> Outcome {
    match r {
        Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::ConnectionAborted => {
            Outcome::Blocked
        }
        Err(err) => Outcome::Other(format!("{err:?}")),
        Ok(mut replies) => {
            if replies.len() != 1 {
                return Outcome::Other(format!("pipeline replies={}", replies.len()));
            }
            match replies.remove(0) {
                Response::Row(row) => match row.into_first_as::<u64>() {
                    Ok(v) if v == expect => Outcome::Fresh,
                    Ok(v) => Outcome::Stale(v),
                    Err(err) => Outcome::Other(format!("{err:?}")),
                },
                Response::Error(code) => Outcome::Other(format!("server error {code}")),
                other => Outcome::Other(format!("{other:?}")),
            }
        }
    }
}

fn report(label: &str, out: &Outcome, expect: &str) -> Result<(), String> {
    match out {
        Outcome::Stale(v) => println!("LIFECYCLE label={label} outcome=Stale stale_value={v}"),
        Outcome::Other(err) => println!("LIFECYCLE label={label} outcome=Other error={err:?}"),
        other => println!("LIFECYCLE label={label} outcome={other:?}"),
    }
    if expect != "fixed" {
        return Ok(());
    }
    let safe = match label {
        "direct" | "tls-direct" => matches!(out, Outcome::Blocked),
        _ => matches!(out, Outcome::Fresh | Outcome::Blocked),
    };
    if safe {
        Ok(())
    } else {
        Err(format!("{label}: unsafe outcome {out:?}"))
    }
}

async fn warm(conn: &mut ConnectionAsync) -> Result<(), String> {
    check_empty(
        tokio::time::timeout(Q_TIMEOUT, conn.query(&sysctl_query()))
            .await
            .map_err(|_| "warm-up timeout".to_string())?
            .map_err(e)?,
    )
}

async fn warm_tls(conn: &mut ConnectionTlsAsync) -> Result<(), String> {
    check_empty(
        tokio::time::timeout(Q_TIMEOUT, conn.query(&sysctl_query()))
            .await
            .map_err(|_| "TLS warm-up timeout".to_string())?
            .map_err(e)?,
    )
}

async fn cancel_pipe(conn: &mut ConnectionAsync, space: &str) -> Result<(), String> {
    let p = pipe(&select_query(space, OBSERVER_KEY));
    let (cancelled, _) = cut(
        tokio::time::timeout(Q_TIMEOUT, conn.execute_pipeline(&p)),
        CUT_MS,
    )
    .await;
    if cancelled {
        Ok(())
    } else {
        Err("first pipeline completed before the cut".into())
    }
}

async fn cancel_pipe_tls(conn: &mut ConnectionTlsAsync, space: &str) -> Result<(), String> {
    let p = pipe(&select_query(space, OBSERVER_KEY));
    let (cancelled, _) = cut(
        tokio::time::timeout(Q_TIMEOUT, conn.execute_pipeline(&p)),
        CUT_MS,
    )
    .await;
    if cancelled {
        Ok(())
    } else {
        Err("first TLS pipeline completed before the cut".into())
    }
}

async fn next_pipe(conn: &mut ConnectionAsync, space: &str) -> Outcome {
    let p = pipe(&select_query(space, LIFE_KEY));
    match tokio::time::timeout(Q_TIMEOUT, conn.execute_pipeline(&p)).await {
        Ok(r) => classify(r, LIFE_V),
        Err(_) => Outcome::Other("next pipeline timeout".into()),
    }
}

async fn next_pipe_tls(conn: &mut ConnectionTlsAsync, space: &str) -> Outcome {
    let p = pipe(&select_query(space, LIFE_KEY));
    match tokio::time::timeout(Q_TIMEOUT, conn.execute_pipeline(&p)).await {
        Ok(r) => classify(r, LIFE_V),
        Err(_) => Outcome::Other("next TLS pipeline timeout".into()),
    }
}

async fn direct(ctx: &Ctx, expect: &str) -> Result<(), String> {
    let mut conn = ctx.tcp.connect_async().await.map_err(e)?;
    warm(&mut conn).await?;
    cancel_pipe(&mut conn, &ctx.space).await?;
    let out = next_pipe(&mut conn, &ctx.space).await;
    report("direct", &out, expect)
}

async fn pool2(ctx: &Ctx, expect: &str) -> Result<(), String> {
    let pool = bb8::Pool::builder()
        .max_size(2)
        .test_on_check_out(false)
        .build(ConnectionMgrTcp::new(ctx.tcp.clone()))
        .await
        .map_err(e)?;
    let mut a = pool.get().await.map_err(e)?;
    let mut b = pool.get().await.map_err(e)?;
    warm(&mut a).await?;
    warm(&mut b).await?;

    cancel_pipe(&mut a, &ctx.space).await?;
    drop(a);

    // b is still checked out; this is the second live connection.
    let concurrent = next_pipe(&mut b, &ctx.space).await;
    println!("LIFECYCLE label=pool2-concurrent outcome={concurrent:?}");
    if !matches!(concurrent, Outcome::Fresh) {
        return Err(format!("pool2 concurrent connection unsafe: {concurrent:?}"));
    }

    // Only the cancelled connection is idle in the stock client; the fixed
    // client discarded it in has_broken, so this checkout is fresh.
    let mut c = pool.get().await.map_err(e)?;
    let out = next_pipe(&mut c, &ctx.space).await;
    drop(c);
    drop(b);
    report("pool2", &out, expect)
}

async fn tls_direct(ctx: &Ctx, expect: &str) -> Result<(), String> {
    let mut conn = ctx.tls.connect_tls_async(&ctx.cert).await.map_err(e)?;
    warm_tls(&mut conn).await?;
    cancel_pipe_tls(&mut conn, &ctx.space).await?;
    let out = next_pipe_tls(&mut conn, &ctx.space).await;
    report("tls-direct", &out, expect)
}

async fn tls_pool(ctx: &Ctx, expect: &str) -> Result<(), String> {
    let pool = bb8::Pool::builder()
        .max_size(1)
        .test_on_check_out(false)
        .build(ConnectionMgrTls::new(ctx.tls.clone(), ctx.cert.clone()))
        .await
        .map_err(e)?;
    let mut conn = pool.get().await.map_err(e)?;
    warm_tls(&mut conn).await?;
    cancel_pipe_tls(&mut conn, &ctx.space).await?;
    drop(conn);

    let mut next = pool.get().await.map_err(e)?;
    let out = next_pipe_tls(&mut next, &ctx.space).await;
    drop(next);
    report("tls-pool", &out, expect)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let expect = args.get(2).map(String::as_str).unwrap_or("fixed");
    let result = async {
        let mut ctx = setup().await?;
        let r = match mode {
            "direct" => direct(&ctx, expect).await,
            "pool2" => pool2(&ctx, expect).await,
            "tls-direct" => tls_direct(&ctx, expect).await,
            "tls-pool" => tls_pool(&ctx, expect).await,
            other => Err(format!("unknown mode {other:?}")),
        };
        cleanup(&mut ctx).await;
        r
    }
    .await;
    if let Err(err) = result {
        eprintln!("LIFECYCLE_ERROR {err}");
        std::process::exit(1);
    }
}

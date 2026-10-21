//! async-pop 1.1.3 direct `Client<S>`: round-3 cancellation ladder.
//!
//! Setup connects a tokio TcpStream through `dropwise::net::Proxy`
//! (reply_delay 50 ms) and creates the client with `async_pop::new`, which
//! reads the greeting and CAPA. The `Client<TcpStream>` is held outside every
//! target future and never reconnected.
//!
//! Target: `login` (the session-establishing exchange); when it completes, a
//! second marked target `retr(1)` runs on the same handle. B2 retries whichever
//! of the two was cancelled, with the same arguments. B3 issues a different
//! command (`capa`) on the same handle. M1's survivor does `stat` + `retr(1)`,
//! and the independent observer is GreenMail's REST mailbox view fetched over
//! its own HTTP connection (not through the proxy).
//!
//! Prints LADDER lines; no outcome asserts.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::net::SocketAddr;
use std::time::Duration;

use async_pop::Client;
use dropwise::net::{Proxy, ProxyOptions};
use dropwise::support::{bounded, unique};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const REPLY_DELAY: Duration = Duration::from_millis(50);
const BOUND: Duration = Duration::from_secs(2);
const MSG: usize = 1;
const SEED_SUBJECT: &str = "dw-r3-pop-seed";

type PopClient = Client<TcpStream>;

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

fn config() -> Config {
    Config {
        max_runs: 64,
        max_cancellations: 1,
        races: vec![Race::Immediate, Race::AfterWake],
        repeat_at_site: false,
        gap_choices: Vec::new(),
        flavor: Flavor::MultiThread { workers: 1 },
        settle: Settle {
            tokio_time: Duration::ZERO,
            real_time: Duration::ZERO,
            watchdog: Duration::from_secs(10),
        },
        scenario_timeout: Duration::from_secs(20),
    }
}

fn env_var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn addr(name: &str) -> Result<SocketAddr, String> {
    env_var(name)?
        .parse()
        .map_err(|err| format!("bad {name}: {err}"))
}

struct Env {
    /// Direct handle under test; created once per run, never reconnected.
    client: PopClient,
    proxy: Proxy,
    rest: SocketAddr,
    user: String,
    run: usize,
}

impl Env {
    async fn setup(run: usize) -> Result<Self, String> {
        let pop = addr("DW_POP_ADDR")?;
        let rest = addr("DW_POP_OBS")?;
        let user = env_var("DW_POP_USER")?;
        let proxy = Proxy::start_with(pop, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let stream = TcpStream::connect(proxy.addr()).await.map_err(e)?;
        let client = async_pop::new(stream)
            .await
            .map_err(|err| format!("client greeting/CAPA: {err}"))?;
        Ok(Self { client, proxy, rest, user, run })
    }

    fn snapshot(&self, label: &str) {
        let conns = self
            .proxy
            .connections()
            .iter()
            .map(|c| format!("{}/{}/{}", c.client, c.to_upstream, c.to_client))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "POP-SNAPSHOT run={} stage={label} proxy_connections={} conn=[{conns}]",
            self.run,
            self.proxy.connections().len(),
        );
    }

    /// Independent observer: the POP3 server's own HTTP state endpoint over a
    /// fresh connection, bypassing the proxy and the tested handle. It reports
    /// the server-owned mailbox (`messages`, `subject`), not the client view.
    async fn observer(&self) -> Result<(usize, bool), String> {
        let mut stream = TcpStream::connect(self.rest).await.map_err(e)?;
        let req = format!(
            "GET /state HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.rest
        );
        stream.write_all(req.as_bytes()).await.map_err(e)?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.map_err(e)?;
        let text = String::from_utf8_lossy(&buf);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
        let count = body
            .split("\"messages\":")
            .nth(1)
            .and_then(|tail| {
                tail.trim_start()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|digits| digits.parse().ok())
            })
            .ok_or_else(|| format!("observer body has no messages field: {body}"))?;
        let present = body.contains(SEED_SUBJECT);
        Ok((count, present))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// REST mailbox view after the cut, before the survivor: (count, subject).
    before: Result<(usize, bool), String>,
    /// Same-handle STAT: concrete "count/size" or error.
    stat: Result<String, String>,
    /// Same-handle RETR 1: (byte length, subject present) or error.
    retr: Result<(usize, bool), String>,
    /// REST mailbox view after the survivor.
    visible: Result<(usize, bool), String>,
}

async fn obs_after_cut(env: &mut Env) -> Obs {
    let before = bounded(BOUND, "REST observer before survivor", async {
        env.observer().await
    })
    .await;
    let stat = bounded(BOUND, "survivor STAT", async {
        env.client
            .stat()
            .await
            .map(|s| format!("{}/{}", s.counter(), s.size()))
            .map_err(e)
    })
    .await;
    env.snapshot("after_stat");
    let retr = bounded(BOUND, "survivor RETR 1", async {
        env.client
            .retr(MSG)
            .await
            .map(|bytes| {
                let subject = SEED_SUBJECT.as_bytes();
                let present = bytes
                    .windows(subject.len())
                    .any(|window| window == subject);
                (bytes.len(), present)
            })
            .map_err(e)
    })
    .await;
    env.snapshot("after_retr");
    let visible = bounded(BOUND, "REST observer after survivor", async {
        env.observer().await
    })
    .await;
    let obs = Obs { before, stat, retr, visible };
    println!(
        "POP-OBS run={} before={:?} stat={:?} retr={:?} visible={:?}",
        env.run, obs.before, obs.stat, obs.retr, obs.visible
    );
    obs
}

fn flat<T>(res: Result<T, async_pop::error::Error>) -> Result<T, String> {
    res.map_err(e)
}

/// Mark `login`; when it completes, mark `retr(1)` as a second target on the
/// same handle. Returns (login_cancelled, retr_cancelled).
async fn cut(env: &mut Env, ctx: &Ctx, label: &str) -> Result<(bool, bool), String> {
    let user = env.user.clone();
    let pass = env_var("DW_POP_PASS")?;
    let login = ctx
        .target(env.client.login(user.as_str(), pass.as_str()))
        .await
        .map(flat);
    let login_cancelled = login.is_none();
    println!(
        "POP-TARGET-1 {label} run={} login_cancelled={login_cancelled}",
        env.run
    );
    if let Some(Err(err)) = &login {
        return Err(format!("login failed in baseline: {err}"));
    }
    if login.is_none() {
        env.snapshot("after_login_cut");
        return Ok((true, false));
    }
    let retr = ctx.target(env.client.retr(MSG)).await.map(flat);
    let retr_cancelled = retr.is_none();
    println!(
        "POP-TARGET-2 {label} run={} retr_cancelled={retr_cancelled}",
        env.run
    );
    if let Some(Err(err)) = &retr {
        return Err(format!("retr failed in baseline: {err}"));
    }
    env.snapshot("after_targets");
    Ok((login_cancelled, retr_cancelled))
}

/// B1: cancel and stop. Judged by explore (panic/liveness/leak).
fn run_b1(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        cut(&mut env, &ctx, "B1").await?;
        Ok(())
    });
    ladder::simple("B1", "LOGIN_RETR", &report);
}

/// B2: retry whichever target was cancelled, same arguments, same handle.
fn run_b2(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let (login_cancelled, retr_cancelled) = cut(&mut env, &ctx, "B2").await?;
        let retry = if login_cancelled {
            let user = env.user.clone();
            let pass = env_var("DW_POP_PASS")?;
            bounded(BOUND, "B2 retry login", async {
                flat(env.client.login(user.as_str(), pass.as_str()).await).map(|_| ())
            })
            .await
        } else if retr_cancelled {
            bounded(BOUND, "B2 retry retr", async {
                flat(env.client.retr(MSG).await).map(|_| ())
            })
            .await
        } else {
            println!("B2-TARGET run={} baseline completed, no retry", env.run);
            return Ok(());
        };
        println!(
            "B2-RESULT run={} login_cancelled={login_cancelled} retr_cancelled={retr_cancelled} retry={retry:?}",
            env.run
        );
        env.snapshot("after_retry");
        if let Err(err) = &retry {
            return Err(format!("B2 same operation failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B2", "LOGIN_RETR", &report);
}

/// B3: different command (CAPA) on the same handle after the cancellation.
fn run_b3(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let (login_cancelled, retr_cancelled) = cut(&mut env, &ctx, "B3").await?;
        if !login_cancelled && !retr_cancelled {
            println!("B3-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let capa = bounded(BOUND, "B3 capa", async {
            env.client.capa().await.map(|_| ()).map_err(e)
        })
        .await;
        println!(
            "B3-RESULT run={} login_cancelled={login_cancelled} retr_cancelled={retr_cancelled} capa={capa:?}",
            env.run
        );
        env.snapshot("after_capa");
        if let Err(err) = &capa {
            return Err(format!("B3 different command failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B3", "CAPA", &report);
}

/// M1: `Expect::Atomic`, survivor STAT+RETR on the same handle, REST observer.
fn run_m1(run: usize) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        cut(&mut env, &ctx, "M1").await?;
        let obs = obs_after_cut(&mut env).await;
        Ok(obs)
    });
    ladder::observed("M1", "LOGIN_RETR", &report);
}

/// Manual calibration, independent of explore_observed's cache: skip or
/// complete login (and retr), then run the same survivor path.
async fn calibrate(login: bool, retr: bool) -> Result<Obs, String> {
    let mut env = Env::setup(0).await?;
    if login {
        let user = env.user.clone();
        let pass = env_var("DW_POP_PASS")?;
        flat(env.client.login(user.as_str(), pass.as_str()).await)?;
    }
    if retr {
        flat(env.client.retr(MSG).await)?;
    }
    let obs = obs_after_cut(&mut env).await;
    println!("POP-CAL login={login} retr={retr} obs={obs:?}");
    Ok(obs)
}

/// Uncanceled login+retr through the proxy, same-handle reuse, REST observer.
async fn preflight() -> Result<(), String> {
    let mut env = Env::setup(0).await?;
    env.snapshot("preflight_setup");
    let before = bounded(BOUND, "REST before", async { env.observer().await }).await?;
    let user = env.user.clone();
    let pass = env_var("DW_POP_PASS")?;
    let login = bounded(BOUND, "uncancelled login", async {
        flat(env.client.login(user.as_str(), pass.as_str()).await).map(|_| ())
    })
    .await?;
    let retr = bounded(BOUND, "uncancelled retr", async {
        flat(env.client.retr(MSG).await).map(|bytes| bytes.len())
    })
    .await?;
    let stat = bounded(BOUND, "same-handle stat", async {
        env.client
            .stat()
            .await
            .map(|s| format!("{}/{}", s.counter(), s.size()))
            .map_err(e)
    })
    .await?;
    let second = bounded(BOUND, "same-handle retr again", async {
        flat(env.client.retr(MSG).await).map(|bytes| bytes.len())
    })
    .await?;
    let visible = bounded(BOUND, "REST after", async { env.observer().await }).await?;
    env.snapshot("preflight_after");
    let conns = env.proxy.connections().len();
    println!(
        "PREFLIGHT login=Ok({login:?}) retr_len={retr} stat={stat} second_len={second} before={before:?} visible={visible:?} proxy_connections={conns}"
    );
    if conns != 1 {
        return Err(format!("tested handle opened {conns} proxy connections"));
    }
    if retr == 0 || second == 0 || retr != second || before != visible {
        return Err(format!(
            "preflight mismatch: retr_len={retr} second_len={second} before={before:?} visible={visible:?}"
        ));
    }
    println!("PREFLIGHT-OK same_handle=reused proxy_connections=1 observer_unchanged=true");
    Ok(())
}

fn usage() -> ! {
    eprintln!("usage: pop-ladder <preflight|calibrate-pre|calibrate-post|ladder <rep>>");
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| usage());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    match cmd.as_str() {
        "preflight" => {
            if let Err(err) = rt.block_on(preflight()) {
                eprintln!("PREFLIGHT-ERR {err}");
                std::process::exit(1);
            }
        }
        "calibrate-pre" | "calibrate-post" => {
            let post = cmd == "calibrate-post";
            if let Err(err) = rt.block_on(calibrate(post, post)) {
                eprintln!("CAL-ERR {err}");
                std::process::exit(1);
            }
        }
        "probe" => {
            let target: SocketAddr = env_var("DW_POP_PROBE").unwrap().parse().unwrap();
            let user = env_var("DW_POP_USER").unwrap();
            let pass = env_var("DW_POP_PASS").unwrap();
            let r = rt.block_on(async {
                println!("PROBE connect {target}");
                let stream = TcpStream::connect(target).await.map_err(e)?;
                println!("PROBE new");
                let mut client = async_pop::new(stream)
                    .await
                    .map_err(|err| format!("new: {err}"))?;
                println!("PROBE new ok; state={:?}", client.get_state());
                let login = bounded(Duration::from_secs(4), "probe login", async {
                    flat(client.login(user.as_str(), pass.as_str()).await).map(|_| ())
                })
                .await;
                println!("PROBE login={login:?}");
                if login.is_ok() {
                    let stat = bounded(Duration::from_secs(4), "probe stat", async {
                        client
                            .stat()
                            .await
                            .map(|s| format!("{}/{}", s.counter(), s.size()))
                            .map_err(e)
                    })
                    .await;
                    println!("PROBE stat={stat:?}");
                }
                Ok::<(), String>(())
            });
            if let Err(err) = r {
                eprintln!("PROBE-ERR {err}");
                std::process::exit(1);
            }
        }
        "ladder" => {
            let rep: u32 = args
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            println!(
                "=== POP LADDER REPETITION {rep} START (run {}) ===",
                unique("pop")
            );
            println!("CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0 proxy_reply_delay=50ms");
            run_b1(rep as usize);
            run_b2(rep as usize);
            run_b3(rep as usize);
            run_m1(rep as usize);
            println!("=== POP LADDER REPETITION {rep} END ===");
        }
        other => {
            eprintln!("unknown command {other}");
            usage();
        }
    }
}

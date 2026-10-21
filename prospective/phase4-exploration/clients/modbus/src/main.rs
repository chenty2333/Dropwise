//! tokio-modbus 0.17.0 direct `client::Context`: round-3 cancellation ladder.
//!
//! Setup opens an independent observer Context straight to the Python Modbus
//! server, resets four holding registers to [100, 0, 0, 0], then opens the
//! tested Context through `dropwise::net::Proxy` (reply_delay 50 ms) and warms
//! it up with one read. The tested `Context` is created once and held outside
//! every target future; it is never reconnected.
//!
//! Target: `write_single_register(1, 1)` (function 6). B2 repeats it, B3 does
//! `read_holding_registers(0, 2)` (function 3) through the same handle, M1's
//! survivor writes register 1 to 7 and reads it back, and the independent
//! observer Context reads the registers straight from the server.
//!
//! Prints LADDER lines; no outcome asserts.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::net::SocketAddr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::support::{bounded, unique};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use tokio_modbus::client::{tcp, Context};
use tokio_modbus::prelude::*;

const REPLY_DELAY: Duration = Duration::from_millis(50);
const BOUND: Duration = Duration::from_secs(2);
const REG: u16 = 1;
const OBS_REG: u16 = 0;
const SEED: [u16; 4] = [100, 0, 0, 0];
const TARGET_V: u16 = 1;
const SURVIVOR_V: u16 = 7;

fn e(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// tokio-modbus 0.17 returns `Result<Result<T, ExceptionCode>, Error>`: a
/// protocol exception is a separate layer from a transport/protocol error.
/// Flatten both into the scenario's string error so neither is swallowed.
fn flat<T>(res: Result<Result<T, tokio_modbus::ExceptionCode>, tokio_modbus::Error>) -> Result<T, String> {
    match res {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(code)) => Err(format!("modbus exception {code:?}")),
        Err(err) => Err(e(err)),
    }
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

fn server_addr() -> Result<SocketAddr, String> {
    std::env::var("DW_MODBUS_SERVER")
        .map_err(|_| "DW_MODBUS_SERVER is required".to_string())?
        .parse()
        .map_err(|err| format!("bad DW_MODBUS_SERVER: {err}"))
}

struct Env {
    /// Independent observer: its own Context straight to the server, no proxy.
    observer: Context,
    /// Pending mechanism for the tested connection only.
    proxy: Proxy,
    /// The direct handle under test; created once per run, never reconnected.
    tested: Context,
    run: usize,
}

impl Env {
    async fn setup(run: usize) -> Result<Self, String> {
        let server = server_addr()?;
        let mut observer = tcp::connect(server).await.map_err(e)?;
        flat(observer.write_multiple_registers(0, &SEED).await)
            .map_err(|err| format!("observer reset: {err}"))?;
        let proxy = Proxy::start_with(server, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let mut tested = tcp::connect(proxy.addr()).await.map_err(e)?;
        flat(tested.read_holding_registers(0, 1).await)
            .map_err(|err| format!("warm-up read: {err}"))?;
        Ok(Self { observer, proxy, tested, run })
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
            "MODBUS-SNAPSHOT run={} stage={label} proxy_connections={} conn=[{conns}]",
            self.run,
            self.proxy.connections().len(),
        );
    }

    async fn observer_read(&mut self) -> Result<Vec<u16>, String> {
        flat(self.observer.read_holding_registers(OBS_REG, 2).await)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// Independent observer read after the cut, before the survivor.
    before: Result<Vec<u16>, String>,
    /// Survivor's write of register 1 to 7 through the tested handle.
    write: Result<(), String>,
    /// Same-handle read-back of registers 0..2.
    own: Result<Vec<u16>, String>,
    /// Independent observer read after the survivor.
    visible: Result<Vec<u16>, String>,
}

async fn observed_after_cut(env: &mut Env) -> Obs {
    let before = bounded(BOUND, "observer read before survivor", async {
        env.observer_read().await
    })
    .await;
    let write = bounded(BOUND, "survivor write", async {
        flat(env.tested.write_single_register(REG, SURVIVOR_V).await)
    })
    .await;
    env.snapshot("after_survivor_write");
    let own = bounded(BOUND, "same-handle read-back", async {
        flat(env.tested.read_holding_registers(OBS_REG, 2).await)
    })
    .await;
    let visible = bounded(BOUND, "observer read after survivor", async {
        env.observer_read().await
    })
    .await;
    let obs = Obs { before, write, own, visible };
    println!(
        "MODBUS-OBS run={} before={:?} write={:?} own={:?} visible={:?}",
        env.run, obs.before, obs.write, obs.own, obs.visible
    );
    obs
}

fn summary(res: &Option<Result<(), String>>) -> String {
    match res {
        None => "cancelled".into(),
        Some(Ok(())) => "Ok(())".into(),
        Some(Err(err)) => format!("Err({err})"),
    }
}

/// B1: cancel the target, then stop. Judged by explore (panic/liveness/leak).
fn run_b1(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        env.snapshot("after_setup");
        let res = ctx
            .target(env.tested.write_single_register(REG, TARGET_V))
            .await
            .map(flat);
        println!("B1-TARGET run={} result={}", env.run, summary(&res));
        env.snapshot("after_target");
        if let Some(res) = res {
            res?;
        }
        Ok(())
    });
    ladder::simple("B1", "WRITE_REG", &report);
}

/// B2: after a cancelled target, repeat the same write on the same handle.
fn run_b2(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(env.tested.write_single_register(REG, TARGET_V))
            .await
            .map(flat);
        if let Some(res) = res {
            res?;
            println!("B2-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let retry = bounded(BOUND, "B2 retry", async {
            flat(env.tested.write_single_register(REG, TARGET_V).await)
        })
        .await;
        println!("B2-RESULT run={} retry={retry:?}", env.run);
        env.snapshot("after_retry");
        if let Err(err) = &retry {
            return Err(format!("B2 same write failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B2", "WRITE_REG", &report);
}

/// B3: after a cancelled target, a different operation (register read) on the
/// same handle.
fn run_b3(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(env.tested.write_single_register(REG, TARGET_V))
            .await
            .map(flat);
        if let Some(res) = res {
            res?;
            println!("B3-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let read = bounded(BOUND, "B3 read", async {
            flat(env.tested.read_holding_registers(OBS_REG, 2).await)
        })
        .await;
        println!("B3-RESULT run={} read={read:?}", env.run);
        env.snapshot("after_read");
        if let Err(err) = &read {
            return Err(format!("B3 different read failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B3", "READ_REGS", &report);
}

/// M1: `Expect::Atomic` with an independent observer and same-register survivor.
fn run_m1(run: usize) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(env.tested.write_single_register(REG, TARGET_V))
            .await
            .map(flat);
        let cancelled = res.is_none();
        if let Some(res) = res {
            res.map_err(|err| format!("baseline target: {err}"))?;
        }
        env.snapshot("after_target");
        println!("M1-TARGET run={} cancelled={cancelled}", env.run);
        let obs = observed_after_cut(&mut env).await;
        Ok(obs)
    });
    ladder::observed("M1", "WRITE_REG", &report);
}

/// Manual calibration evidence, independent of explore_observed's cache:
/// skip (`pre`) or complete (`post`) the target, then run the same survivor.
async fn calibrate(complete: bool) -> Result<Obs, String> {
    let mut env = Env::setup(0).await?;
    if complete {
        flat(env.tested.write_single_register(REG, TARGET_V).await)?;
    }
    let obs = observed_after_cut(&mut env).await;
    println!(
        "MODBUS-CAL side={} obs={:?}",
        if complete { "post" } else { "pre" },
        obs
    );
    Ok(obs)
}

/// Uncanceled target through the proxy, same-handle reuse, observer effect.
async fn preflight() -> Result<(), String> {
    let server = server_addr()?;
    let mut env = Env::setup(0).await?;
    env.snapshot("preflight_setup");
    let write = bounded(BOUND, "uncancelled target", async {
        flat(env.tested.write_single_register(REG, TARGET_V).await)
    })
    .await?;
    let observer = bounded(BOUND, "observer after uncancelled", async {
        env.observer_read().await
    })
    .await?;
    let own = bounded(BOUND, "same-handle readback", async {
        flat(env.tested.read_holding_registers(OBS_REG, 2).await)
    })
    .await?;
    let second = bounded(BOUND, "same-handle second request", async {
        flat(env.tested.read_holding_registers(OBS_REG, 2).await)
    })
    .await?;
    env.snapshot("preflight_after");
    let conns = env.proxy.connections().len();
    println!(
        "PREFLIGHT server={server} write=Ok({write:?}) observer={observer:?} own={own:?} second={second:?} proxy_connections={conns}"
    );
    let expected = vec![SEED[0], TARGET_V];
    if observer != expected || own != expected || second != expected || conns != 1 {
        return Err(format!(
            "preflight mismatch: observer={observer:?} own={own:?} second={second:?} proxy_connections={conns}"
        ));
    }
    if observer[1] == SEED[1] {
        return Err("uncancelled target did not change the observed register".into());
    }
    println!(
        "PREFLIGHT-OK observer_effect=register1:{}->{TARGET_V} same_handle=reused proxy_connections=1",
        SEED[1]
    );
    Ok(())
}

fn usage() -> ! {
    eprintln!("usage: modbus-ladder <preflight|calibrate-pre|calibrate-post|ladder <rep>>");
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
            let complete = cmd == "calibrate-post";
            if let Err(err) = rt.block_on(calibrate(complete)) {
                eprintln!("CAL-ERR {err}");
                std::process::exit(1);
            }
        }
        "probe-reject" => {
            let r = rt.block_on(async {
                let server = server_addr()?;
                let mut c = tcp::connect(server).await.map_err(e)?;
                let out_of_range = flat(c.read_holding_registers(99, 1).await);
                let normal_after = flat(c.read_holding_registers(0, 2).await);
                let write = flat(c.write_single_register(1, 7).await);
                drop(c);
                let mut fresh = tcp::connect(server).await.map_err(e)?;
                let after_reconnect = flat(fresh.read_holding_registers(0, 2).await);
                println!(
                    "PROBE-REJECT out_of_range={out_of_range:?} normal_after={normal_after:?} write={write:?} after_reconnect={after_reconnect:?}"
                );
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
                "=== MODBUS LADDER REPETITION {rep} START (run {}) ===",
                unique("modbus")
            );
            println!("CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 max_runs=64 scenario_timeout=20s settle=tokio0/real0 proxy_reply_delay=50ms server_reply_delay=0ms");
            run_b1(rep as usize);
            run_b2(rep as usize);
            run_b3(rep as usize);
            run_m1(rep as usize);
            println!("=== MODBUS LADDER REPETITION {rep} END ===");
        }
        other => {
            eprintln!("unknown command {other}");
            usage();
        }
    }
}

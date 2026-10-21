//! sqlx 0.8.6 `PgConnection` direct handle: round-4 cancellation ladder.
//!
//! Setup opens an independent observer `PgConnection` straight to PostgreSQL
//! (never through the proxy), creates/seed `dw_r4_sqlx` with a sentinel row
//! (`b3 = 3`) and three rows the target touches (`t1..t3 = 0`), then opens the
//! tested `PgConnection` through `dropwise::net::Proxy` (reply_delay 50 ms) and
//! warms it with one `SELECT 1`. The tested connection is created once and held
//! outside every target future; it is never reconnected or pooled.
//!
//! Target: one multi-row `INSERT ... ON CONFLICT (k) DO UPDATE SET v =
//! excluded.v` for t1..t3, executed with `sqlx::query(...).execute(&mut conn)`
//! (extended query protocol, persistent statement cache). It returns
//! rows_affected = 3 and changes the rows the independent observer reads.
//!
//! B2 repeats the same statement; B3 runs a different statement (`UPDATE b3`,
//! rows_affected 1) on the same handle; M1's survivor updates all four rows to
//! v = 7 with rows_affected 4, reads t1 back, and the independent observer reads
//! all rows before and after. The survivor deliberately writes every row the
//! target touched, so a late server-side application of the cancelled target
//! cannot make the final observer snapshot differ from the calibration.
//!
//! Prints LADDER lines; no outcome asserts.

#[path = "../../../../support/ladder.rs"]
mod ladder;

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use dropwise::net::{Proxy, ProxyOptions};
use dropwise::support::{bounded, unique};
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{Connection, Row};

const REPLY_DELAY: Duration = Duration::from_millis(50);
const BOUND: Duration = Duration::from_secs(2);

/// Target: three-row upsert, rows_affected 3, no RETURNING.
const TARGET_SQL: &str = "INSERT INTO dw_r4_sqlx (k, v) VALUES ('t1', 1), ('t2', 1), ('t3', 1) \
     ON CONFLICT (k) DO UPDATE SET v = excluded.v";
/// B2: exactly the same statement and arguments.
const B2_SQL: &str = TARGET_SQL;
/// B3: different statement, expected rows_affected 1 (a stale target
/// CommandComplete would carry 3).
const B3_SQL: &str = "UPDATE dw_r4_sqlx SET v = v WHERE k = 'b3'";
/// M1 survivor: all four rows to 7, expected rows_affected 4.
const SURVIVOR_SQL: &str =
    "UPDATE dw_r4_sqlx SET v = 7 WHERE k IN ('b3', 't1', 't2', 't3')";
const READ_T1_SQL: &str = "SELECT v FROM dw_r4_sqlx WHERE k = 't1'";
const READ_ALL_SQL: &str = "SELECT k, v FROM dw_r4_sqlx ORDER BY k";
const PING_SQL: &str = "SELECT 42";
const SEED_SQL: &str =
    "INSERT INTO dw_r4_sqlx (k, v) VALUES ('b3', 3), ('t1', 0), ('t2', 0), ('t3', 0)";

const TARGET_ROWS: u64 = 3;
const SURVIVOR_ROWS: u64 = 4;
const B3_ROWS: u64 = 1;
const SEED_ROWS: [(&str, i32); 4] = [("b3", 3), ("t1", 0), ("t2", 0), ("t3", 0)];
const SURVIVOR_ROWS_STATE: [(&str, i32); 4] =
    [("b3", 7), ("t1", 7), ("t2", 7), ("t3", 7)];

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

fn server_addr() -> Result<SocketAddr, String> {
    std::env::var("DW_PG_SERVER")
        .map_err(|_| "DW_PG_SERVER is required".to_string())?
        .parse()
        .map_err(|err| format!("bad DW_PG_SERVER: {err}"))
}

fn direct_options() -> Result<PgConnectOptions, String> {
    let url = std::env::var("DW_PG_URL").map_err(|_| "DW_PG_URL is required".to_string())?;
    PgConnectOptions::from_str(&url)
        .map_err(|err| format!("bad DW_PG_URL: {err}"))
        .map(|options| options.ssl_mode(PgSslMode::Disable))
}

struct Env {
    /// Independent observer: its own PgConnection straight to PostgreSQL.
    observer: PgConnection,
    /// Pending mechanism for the tested connection only.
    proxy: Proxy,
    /// The direct handle under test; created once per run, never reconnected.
    tested: PgConnection,
    run: usize,
}

impl Env {
    async fn setup(run: usize) -> Result<Self, String> {
        let base = direct_options()?;
        let mut observer = PgConnection::connect_with(&base)
            .await
            .map_err(|err| format!("observer connect: {err}"))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS dw_r4_sqlx (k text PRIMARY KEY, v int NOT NULL)",
        )
        .execute(&mut observer)
        .await
        .map_err(|err| format!("observer create table: {err}"))?;
        sqlx::query("DELETE FROM dw_r4_sqlx")
            .execute(&mut observer)
            .await
            .map_err(|err| format!("observer reset: {err}"))?;
        sqlx::query(SEED_SQL)
            .execute(&mut observer)
            .await
            .map_err(|err| format!("observer seed: {err}"))?;

        let server = server_addr()?;
        let proxy = Proxy::start_with(server, ProxyOptions { reply_delay: REPLY_DELAY })
            .await
            .map_err(e)?;
        let tested_options = base
            .clone()
            .host("127.0.0.1")
            .port(proxy.addr().port())
            .ssl_mode(PgSslMode::Disable);
        let mut tested = PgConnection::connect_with(&tested_options)
            .await
            .map_err(|err| format!("tested connect through proxy: {err}"))?;
        bounded(BOUND, "warm-up", async {
            sqlx::query("SELECT 1")
                .fetch_one(&mut tested)
                .await
                .map(|_| ())
                .map_err(e)
        })
        .await
        .map_err(|err| format!("warm-up: {err}"))?;
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
        let residue = self.proxy.max_residue();
        println!(
            "SQLX-SNAPSHOT run={} stage={label} proxy_connections={} conn=[{conns}] residue={residue:?}",
            self.run,
            self.proxy.connections().len(),
        );
    }

    async fn observer_rows(&mut self) -> Result<Vec<(String, i32)>, String> {
        let rows = sqlx::query(READ_ALL_SQL)
            .fetch_all(&mut self.observer)
            .await
            .map_err(e)?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<String, _>("k").map_err(e)?,
                    row.try_get::<i32, _>("v").map_err(e)?,
                ))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    /// Independent observer read after the cut, before the survivor.
    before: Result<Vec<(String, i32)>, String>,
    /// Survivor's update of all four rows to 7, as rows_affected.
    write: Result<u64, String>,
    /// Same-handle read-back of t1.
    own: Result<i32, String>,
    /// Independent observer read after the survivor.
    visible: Result<Vec<(String, i32)>, String>,
    /// Same-handle `SELECT 42` after everything else: connection usable.
    usable: Result<i32, String>,
}

async fn observed_after_cut(env: &mut Env) -> Obs {
    let before = bounded(BOUND, "observer read before survivor", async {
        env.observer_rows().await
    })
    .await;
    let write = bounded(BOUND, "survivor write", async {
        sqlx::query(SURVIVOR_SQL)
            .execute(&mut env.tested)
            .await
            .map(|result| result.rows_affected())
            .map_err(e)
    })
    .await;
    env.snapshot("after_survivor_write");
    let own = bounded(BOUND, "same-handle read-back", async {
        sqlx::query(READ_T1_SQL)
            .fetch_one(&mut env.tested)
            .await
            .map_err(e)
            .and_then(|row| row.try_get::<i32, _>("v").map_err(e))
    })
    .await;
    let visible = bounded(BOUND, "observer read after survivor", async {
        env.observer_rows().await
    })
    .await;
    let usable = bounded(BOUND, "same-handle usability probe", async {
        sqlx::query(PING_SQL)
            .fetch_one(&mut env.tested)
            .await
            .map_err(e)
            .and_then(|row| row.try_get::<i32, _>(0).map_err(e))
    })
    .await;
    let obs = Obs { before, write, own, visible, usable };
    println!(
        "SQLX-OBS run={} before={:?} write={:?} own={:?} visible={:?} usable={:?}",
        env.run, obs.before, obs.write, obs.own, obs.visible, obs.usable
    );
    obs
}

fn summary(res: &Option<Result<u64, String>>) -> String {
    match res {
        None => "cancelled".into(),
        Some(Ok(rows)) => format!("Ok(rows_affected={rows})"),
        Some(Err(err)) => format!("Err({err})"),
    }
}

/// Print every realised plan, its cut sites and (for M1) the verdict. The
/// shared ladder helper prints only aggregate counts; the fragment records the
/// plan labels and the source lines the cuts landed on.
fn dump_trials(level: &str, id: &str, report: &dropwise::Report) {
    for (index, trial) in report.trials.iter().enumerate() {
        let sites = trial
            .sites
            .iter()
            .map(|site| {
                site.map_or_else(
                    || "not-created".to_string(),
                    |location| format!("{}:{}", location.file(), location.line()),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "SQLX-TRIAL {level} {id} index={index} plan={} outcomes={:?} cancelled={} \
             verdict={:?} settled={} sites=[{sites}]",
            trial.plan_label(),
            trial.outcomes,
            trial.cancelled,
            trial.verdict,
            trial.settled,
        );
        if let Some(comparison) = &trial.comparison {
            println!("SQLX-COMPARISON {level} {id} index={index} {comparison}");
        }
    }
    for note in &report.notes {
        println!("SQLX-NOTE {level} {id}: {note}");
    }
}

/// B1: cancel the target, then stop. Judged by explore (panic/liveness/leak).
fn run_b1(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        env.snapshot("after_setup");
        let res = ctx
            .target(bounded(BOUND, "B1 target", async {
                sqlx::query(TARGET_SQL)
                    .execute(&mut env.tested)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(e)
            }))
            .await;
        println!("B1-TARGET run={} result={}", env.run, summary(&res));
        env.snapshot("after_target");
        if let Some(res) = res {
            res.map_err(|err| format!("baseline target: {err}"))?;
        }
        Ok(())
    });
    ladder::simple("B1", "UPSERT3", &report);
    dump_trials("B1", "UPSERT3", &report);
}

/// B2: after a cancelled target, repeat the same upsert on the same handle.
fn run_b2(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(bounded(BOUND, "B2 target", async {
                sqlx::query(TARGET_SQL)
                    .execute(&mut env.tested)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(e)
            }))
            .await;
        if let Some(res) = res {
            res.map_err(|err| format!("baseline target: {err}"))?;
            println!("B2-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let retry = bounded(BOUND, "B2 retry", async {
            sqlx::query(B2_SQL)
                .execute(&mut env.tested)
                .await
                .map(|result| result.rows_affected())
                .map_err(e)
        })
        .await;
        println!("B2-RESULT run={} retry={retry:?}", env.run);
        env.snapshot("after_retry");
        if let Err(err) = &retry {
            return Err(format!("B2 same upsert failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B2", "UPSERT3", &report);
    dump_trials("B2", "UPSERT3", &report);
}

/// B3: after a cancelled target, a different statement on the same handle.
fn run_b3(run: usize) {
    let report = explore(&config(), move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(bounded(BOUND, "B3 target", async {
                sqlx::query(TARGET_SQL)
                    .execute(&mut env.tested)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(e)
            }))
            .await;
        if let Some(res) = res {
            res.map_err(|err| format!("baseline target: {err}"))?;
            println!("B3-TARGET run={} baseline completed", env.run);
            return Ok(());
        }
        let other = bounded(BOUND, "B3 other statement", async {
            sqlx::query(B3_SQL)
                .execute(&mut env.tested)
                .await
                .map(|result| result.rows_affected())
                .map_err(e)
        })
        .await;
        println!(
            "B3-RESULT run={} expected_rows={B3_ROWS} rows_affected={other:?}",
            env.run
        );
        env.snapshot("after_other");
        if let Err(err) = &other {
            return Err(format!("B3 different statement failed after cancellation: {err}"));
        }
        Ok(())
    });
    ladder::simple("B3", "UPDATE_B3", &report);
    dump_trials("B3", "UPDATE_B3", &report);
}

/// M1: `Expect::Atomic` with an independent observer and same-row survivor.
fn run_m1(run: usize) {
    let report = explore_observed(&config(), Expect::Atomic, move |ctx: Ctx| async move {
        let mut env = Env::setup(run).await?;
        let res = ctx
            .target(bounded(BOUND, "M1 target", async {
                sqlx::query(TARGET_SQL)
                    .execute(&mut env.tested)
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(e)
            }))
            .await;
        let cancelled = res.is_none();
        if let Some(res) = res {
            let rows = res.map_err(|err| format!("baseline target: {err}"))?;
            if rows != TARGET_ROWS {
                return Err(format!("baseline target rows_affected {rows}, expected {TARGET_ROWS}"));
            }
        }
        env.snapshot("after_target");
        println!("M1-TARGET run={} cancelled={cancelled}", env.run);
        let obs = observed_after_cut(&mut env).await;
        Ok(obs)
    });
    ladder::observed("M1", "UPSERT3", &report);
    dump_trials("M1", "UPSERT3", &report);
}

/// Manual calibration evidence, independent of explore_observed's cache:
/// skip (`pre`) or complete (`post`) the target, then run the same survivor.
async fn calibrate(complete: bool) -> Result<Obs, String> {
    let mut env = Env::setup(0).await?;
    if complete {
        let rows = bounded(BOUND, "calibration target", async {
            sqlx::query(TARGET_SQL)
                .execute(&mut env.tested)
                .await
                .map(|result| result.rows_affected())
                .map_err(e)
        })
        .await?;
        if rows != TARGET_ROWS {
            return Err(format!("calibration target rows_affected {rows}, expected {TARGET_ROWS}"));
        }
    }
    let obs = observed_after_cut(&mut env).await;
    println!(
        "SQLX-CAL side={} obs={:?}",
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

    let initial = bounded(BOUND, "initial observer read", async {
        env.observer_rows().await
    })
    .await?;
    let target = bounded(BOUND, "uncancelled target", async {
        sqlx::query(TARGET_SQL)
            .execute(&mut env.tested)
            .await
            .map(|result| result.rows_affected())
            .map_err(e)
    })
    .await?;
    let after = bounded(BOUND, "observer after target", async {
        env.observer_rows().await
    })
    .await?;
    let write = bounded(BOUND, "survivor write", async {
        sqlx::query(SURVIVOR_SQL)
            .execute(&mut env.tested)
            .await
            .map(|result| result.rows_affected())
            .map_err(e)
    })
    .await?;
    let own = bounded(BOUND, "same-handle readback", async {
        sqlx::query(READ_T1_SQL)
            .fetch_one(&mut env.tested)
            .await
            .map_err(e)
            .and_then(|row| row.try_get::<i32, _>("v").map_err(e))
    })
    .await?;
    let second = bounded(BOUND, "same-handle second read", async {
        sqlx::query(READ_T1_SQL)
            .fetch_one(&mut env.tested)
            .await
            .map_err(e)
            .and_then(|row| row.try_get::<i32, _>("v").map_err(e))
    })
    .await?;
    let all = bounded(BOUND, "observer after survivor", async {
        env.observer_rows().await
    })
    .await?;
    env.snapshot("preflight_after");

    let expected_initial: Vec<(String, i32)> =
        SEED_ROWS.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let expected_after: Vec<(String, i32)> =
        [("b3", 3), ("t1", 1), ("t2", 1), ("t3", 1)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
    let expected_all: Vec<(String, i32)> = SURVIVOR_ROWS_STATE
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();
    let conns = env.proxy.connections().len();
    println!(
        "PREFLIGHT server={server} initial={initial:?} target_rows={target} after={after:?} \
         write_rows={write} own={own} second={second} all={all:?} proxy_connections={conns}"
    );
    if initial != expected_initial {
        return Err(format!("preflight initial rows {initial:?} != {expected_initial:?}"));
    }
    if target != TARGET_ROWS {
        return Err(format!("preflight target rows {target} != {TARGET_ROWS}"));
    }
    if after != expected_after {
        return Err(format!("preflight observer after target {after:?} != {expected_after:?}"));
    }
    if write != SURVIVOR_ROWS || own != 7 || second != 7 {
        return Err(format!(
            "preflight survivor mismatch: write_rows={write} own={own} second={second}"
        ));
    }
    if all != expected_all {
        return Err(format!("preflight final rows {all:?} != {expected_all:?}"));
    }
    if conns != 1 {
        return Err(format!("preflight proxy_connections={conns}, expected 1"));
    }
    println!(
        "PREFLIGHT-OK observer_effect=t1..t3:0->1 survivor=t1..t3,b3:->7 same_handle=reused proxy_connections=1"
    );
    Ok(())
}

fn usage() -> ! {
    eprintln!("usage: sqlx-ladder <preflight|calibrate-pre|calibrate-post|ladder <rep>>");
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
        "ladder" => {
            let rep: u32 = args
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            println!(
                "=== SQLX LADDER REPETITION {rep} START (run {}) ===",
                unique("sqlx")
            );
            println!(
                "CONFIG flavor=MultiThread(1) races=[Immediate,AfterWake] max_cancellations=1 \
                 max_runs=64 scenario_timeout=20s settle=tokio0/real0 proxy_reply_delay=50ms \
                 target=UPSERT3 rows=3"
            );
            run_b1(rep as usize);
            run_b2(rep as usize);
            run_b3(rep as usize);
            run_m1(rep as usize);
            println!("=== SQLX LADDER REPETITION {rep} END ===");
        }
        other => {
            eprintln!("unknown command {other}");
            usage();
        }
    }
}

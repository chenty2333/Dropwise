//! Real PostgreSQL 18 probe: a cancelled BEGIN, a cancelled mid-reply SELECT,
//! a completed BEGIN returned with a standing obligation, and the naive
//! no-read reuse that shifts replies by one exchange.
//!
//! Usage: pg_probe <port> [user] [database]

use online_guards::guard::{Cause, RecoveryLimits, Suspect};
use online_guards::policy::{Plan, RecoveryPolicy};
use online_guards::pool::{Pool, PoolConfig};
use online_guards::proto::pg::{CutPoint, PgConfig, PgConn, PgError, PgManager, PgStatus};
use online_guards::recovery::{RecoveryAttempt, RecoveryContext};
use online_guards::{EffectContract, GuardedSession};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct RawPg {
    stream: TcpStream,
    inbuf: Vec<u8>,
}

impl RawPg {
    async fn connect(cfg: &PgConfig) -> std::io::Result<Self> {
        let mut stream = TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
        stream
            .write_all(&online_guards::proto::pg::encode_startup(cfg))
            .await?;
        let mut raw = Self {
            stream,
            inbuf: Vec::new(),
        };
        loop {
            let (t, _) = raw.frame().await?;
            if t == b'Z' {
                return Ok(raw);
            }
            if t == b'E' {
                return Err(std::io::Error::other("startup error"));
            }
        }
    }
    async fn send(&mut self, sql: &str) -> std::io::Result<()> {
        self.stream
            .write_all(&online_guards::proto::pg::encode_query(sql))
            .await
    }
    async fn frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        loop {
            if self.inbuf.len() >= 5 {
                let len = i32::from_be_bytes([
                    self.inbuf[1],
                    self.inbuf[2],
                    self.inbuf[3],
                    self.inbuf[4],
                ]) as usize;
                if self.inbuf.len() > len {
                    let ty = self.inbuf[0];
                    let payload = self.inbuf[5..len + 1].to_vec();
                    self.inbuf.drain(..len + 1);
                    return Ok((ty, payload));
                }
            }
            let n = self.stream.read_buf(&mut self.inbuf).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "closed",
                ));
            }
        }
    }
    /// Read through the next ReadyForQuery, returning the last CommandComplete
    /// tag and the status byte.
    async fn read_exchange(&mut self) -> std::io::Result<(String, char)> {
        let mut tag = String::new();
        loop {
            let (t, p) = self.frame().await?;
            match t {
                b'C' => {
                    tag = String::from_utf8_lossy(&p[..p.len().saturating_sub(1)]).into_owned();
                }
                b'Z' => return Ok((tag, p.first().copied().unwrap_or(b'?') as char)),
                b'E' => {
                    let mut i = 0;
                    let mut msg = String::new();
                    while i < p.len() && p[i] != 0 {
                        let code = p[i];
                        i += 1;
                        let start = i;
                        while i < p.len() && p[i] != 0 {
                            i += 1;
                        }
                        if code == b'M' {
                            msg = String::from_utf8_lossy(&p[start..i]).into_owned();
                        }
                        i += 1;
                    }
                    return Ok((format!("ERROR {msg}"), 'E'));
                }
                _ => {}
            }
        }
    }
}

async fn stat(cfg: &PgConfig, pid: i32) -> String {
    match PgConn::connect(cfg).await {
        Ok(mut c) => {
            let sql = format!("select state from pg_stat_activity where pid = {pid}");
            match c.query(&sql).await {
                Ok(r) => r
                    .rows
                    .first()
                    .and_then(|r| r.first().cloned().flatten())
                    .unwrap_or_else(|| "<null>".into()),
                Err(e) => format!("stat error {e}"),
            }
        }
        Err(e) => format!("stat connect error {e}"),
    }
}

async fn count(cfg: &PgConfig, key: &str) -> i64 {
    let mut c = PgConn::connect(cfg).await.expect("admin connect");
    let sql = format!("select count(*) from og_probe where k = '{key}'");
    c.query(&sql)
        .await
        .ok()
        .and_then(|r| r.rows.first().and_then(|r| r.first().cloned().flatten()))
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1)
}

fn ctx_for(
    conn: &PgConn,
    guard: &online_guards::guard::Guard,
    permit: &online_guards::guard::RecoveryPermit<'_>,
    plan: &'static str,
) -> RecoveryContext {
    ctx_for_cause(conn, guard, permit, plan, Cause::Cancelled)
}

fn ctx_for_cause(
    conn: &PgConn,
    guard: &online_guards::guard::Guard,
    permit: &online_guards::guard::RecoveryPermit<'_>,
    plan: &'static str,
    cause: Cause,
) -> RecoveryContext {
    let _ = conn;
    RecoveryContext {
        obj: guard.obj(),
        epoch: permit.epoch(),
        attempt: permit.attempt(),
        plan,
        limits: RecoveryLimits::default(),
        features: online_guards::features::Features::new(),
        remaining: Duration::from_secs(2),
        write_state: "unknown",
        read_state: "unknown",
        cause,
        contract: conn.effect_contract(),
    }
}

async fn recover_manual_cause(conn: &mut PgConn, plan: &'static str, cause: Cause) -> String {
    let guard = conn.guard().clone();
    let permit = guard.claim_recovery().expect("claim");
    let ctx = ctx_for_cause(conn, &guard, &permit, plan, cause);
    let attempt = conn.recover(&ctx).await;
    match attempt {
        RecoveryAttempt::Recovered(w) => {
            let s = format!("Recovered({})", w.evidence);
            permit.recovered(w).expect("permit recovered");
            s
        }
        RecoveryAttempt::Failed(c) => {
            permit.failed(c);
            format!("Failed({c:?})")
        }
        RecoveryAttempt::Unresolved(c) => {
            permit.unresolved(c);
            format!("Unresolved({c:?})")
        }
    }
}

async fn recover_manual(conn: &mut PgConn, plan: &'static str) -> String {
    let guard = conn.guard().clone();
    let permit = guard.claim_recovery().expect("claim");
    let ctx = ctx_for(conn, &guard, &permit, plan);
    let attempt = conn.recover(&ctx).await;
    match attempt {
        RecoveryAttempt::Recovered(w) => {
            let s = format!("Recovered({})", w.evidence);
            permit.recovered(w).expect("permit recovered");
            s
        }
        RecoveryAttempt::Failed(c) => {
            permit.failed(c);
            format!("Failed({c:?})")
        }
        RecoveryAttempt::Unresolved(c) => {
            permit.unresolved(c);
            format!("Unresolved({c:?})")
        }
    }
}

struct PgPolicy;
impl RecoveryPolicy for PgPolicy {
    fn choose(&self, s: &Suspect) -> Plan {
        // A completed BEGIN returned to the pool has no reply pending: a
        // rollback is the whole recovery.  A cancelled statement whose reply
        // was never read must be drained first, even when a transaction is
        // standing; otherwise the first Z read here belongs to the abandoned
        // exchange and ROLLBACK's reply is left for the successor.
        if s.standing.is_some() && s.features.name("read") == Some("complete") {
            return Plan::Run("rollback");
        }
        match s.features.name("write") {
            Some("complete") => Plan::Run("drain_reset"),
            Some("partial") => Plan::Run("retire"),
            _ => Plan::Run("rollback"),
        }
    }
    fn describe(&self) -> String {
        "pg-drain-reset".into()
    }
}

struct RetirePolicy;
impl RecoveryPolicy for RetirePolicy {
    fn choose(&self, _s: &Suspect) -> Plan {
        Plan::Retire
    }
    fn describe(&self) -> String {
        "retire-on-suspect".into()
    }
}

struct ProbePolicy;
impl RecoveryPolicy for ProbePolicy {
    fn choose(&self, s: &Suspect) -> Plan {
        // A completed BEGIN returned to the pool is a standing obligation with
        // no pending reply: a rollback is the whole cleanup.
        if s.standing.is_some() && s.features.name("read") == Some("complete") {
            return Plan::Run("rollback");
        }
        // A complete write with no complete reply is a pending exchange: a
        // bounded probe is the action that can distinguish a delayed reply
        // from a dead or desynchronised connection.  A partial write is not
        // probe-able; retiring is the only authority-preserving decision.
        if s.features.name("write") == Some("complete")
            && s.features.name("read") != Some("complete")
        {
            Plan::Run("probe_reset")
        } else {
            Plan::Retire
        }
    }
    fn describe(&self) -> String {
        "probe-pending-then-reset".into()
    }
}

/// One recovery decision under a deliberately delayed reply.  The delay is a
/// client-read injection and is never placed in the policy features; the plan
/// is chosen from the same `Suspect` record the pool would use.
async fn policy_trial(
    cfg: &PgConfig,
    strategy: &'static str,
    policy: &dyn RecoveryPolicy,
    delay: Duration,
    budget: Duration,
) -> String {
    let mut c = PgConn::connect(cfg).await.expect("policy trial connect");
    let pid = c.backend_pid;
    c.query("BEGIN").await.expect("policy trial begin");
    c.cut = Some(CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(0),
    });
    let cut = c.query("SELECT 1").await;
    assert!(cut.is_err(), "the cut must abandon the SELECT");
    c.read_delay_once = Some(delay);
    let before = stat(cfg, pid).await;
    let start = std::time::Instant::now();
    let mut session = Some(c);
    let suspect = session
        .as_ref()
        .expect("live session")
        .guard()
        .suspect()
        .expect("the cut must leave a suspect object");
    let selected = policy.choose(&suspect);
    let (outcome, recovered, evidence) = if selected == Plan::Retire {
        session.take();
        (
            "closed+reconnect".to_string(),
            false,
            "policy=retire".to_string(),
        )
    } else {
        let Plan::Run(plan) = selected else {
            unreachable!()
        };
        let c = session.as_mut().expect("live session");
        let guard = c.guard().clone();
        let permit = guard.claim_recovery().expect("recovery claim");
        let ctx = RecoveryContext {
            obj: guard.obj(),
            epoch: permit.epoch(),
            attempt: permit.attempt(),
            plan,
            limits: RecoveryLimits {
                max_attempts: 1,
                deadline: Duration::from_millis(500),
                probe_budget: budget,
            },
            features: online_guards::features::Features::new(),
            remaining: Duration::from_millis(400),
            write_state: "complete",
            read_state: "none",
            cause: Cause::Cancelled,
            contract: c.effect_contract(),
        };
        let attempt = c.recover(&ctx).await;
        match attempt {
            RecoveryAttempt::Recovered(w) => {
                let evidence = w.evidence.to_string();
                permit.recovered(w).expect("permit recovered");
                ("Recovered".to_string(), true, evidence)
            }
            RecoveryAttempt::Failed(cause) => {
                permit.failed(cause);
                (format!("Failed({cause:?})"), false, String::new())
            }
            RecoveryAttempt::Unresolved(cause) => {
                permit.unresolved(cause);
                (format!("Unresolved({cause:?})"), false, String::new())
            }
        }
    };
    let successor = if recovered {
        matches!(
            session
                .as_mut()
                .expect("recovered session")
                .query("SELECT 1")
                .await,
            Ok(r) if r.status == PgStatus::Idle
        )
    } else {
        drop(session.take());
        let mut fresh = PgConn::connect(cfg).await.expect("retire connect");
        let ok = fresh
            .query("SELECT 1")
            .await
            .map(|r| r.status == PgStatus::Idle)
            .unwrap_or(false);
        fresh.close();
        ok
    };
    let elapsed = start.elapsed().as_millis();
    let after = stat(cfg, pid).await;
    if let Some(mut c) = session {
        if recovered {
            let _ = c.query("ROLLBACK").await;
        }
        c.close();
    }
    format!(
        "{strategy:>10} {:>5}ms {:>5}ms {:>18} {:>5} {:>7} {:>8}ms  before={before} after={after}  {evidence}",
        delay.as_millis(),
        budget.as_millis(),
        outcome,
        if successor { "ok" } else { "FAIL" },
        if recovered { "same" } else { "new" },
        elapsed,
    )
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("PG_PORT").ok())
        .expect("usage: pg_probe <port>")
        .parse()
        .unwrap();
    let user = std::env::args().nth(2).unwrap_or_else(|| "postgres".into());
    let db = std::env::args().nth(3).unwrap_or_else(|| "postgres".into());
    let cfg = PgConfig::loopback(port, &user, &db);
    let mut admin = PgConn::connect(&cfg).await.expect("admin");
    admin
        .query("drop table if exists og_probe")
        .await
        .expect("drop");
    admin
        .query("create table og_probe(k text primary key, v int)")
        .await
        .expect("create");
    println!("server: postgres healthy, pid={}", admin.backend_pid);

    // ---- 1. naive no-read reuse: replies shift by one exchange -------------
    {
        let mut raw = RawPg::connect(&cfg).await.expect("raw connect");
        raw.send("begin").await.unwrap();
        raw.send("insert into og_probe values ('naive', 1)")
            .await
            .unwrap();
        let (tag, status) = raw.read_exchange().await.unwrap();
        println!("naive after INSERT request: tag={tag:?} status={status}");
        // The client believes the INSERT's reply is its own; it never reads the
        // real INSERT reply.  It closes without a COMMIT, so the server rolls
        // back a transaction the client did not know it was in.
        drop(raw);
        tokio::time::sleep(Duration::from_millis(100)).await;
        println!("naive row after close: {}", count(&cfg, "naive").await);
    }

    // ---- 2. guarded: cut BEGIN after the write, before the reply ----------
    {
        let mut c = PgConn::connect(&cfg).await.expect("connect");
        let pid = c.backend_pid;
        c.cut = Some(CutPoint {
            after_write_bytes: None,
            after_read_bytes: Some(0),
        });
        let e = c.query("begin").await;
        println!(
            "guard begin: error={:?} phase={:?} status={}",
            e.as_ref().err().map(|e| e.to_string()),
            c.guard().phase(),
            c.status.code()
        );
        let refused = c.query("insert into og_probe values ('guard', 2)").await;
        println!(
            "guard successor while suspect: {}",
            match refused {
                Err(PgError::Refused(r)) => format!("refused ({r})"),
                other => format!("UNEXPECTED {other:?}"),
            }
        );
        println!("server state before recovery: {}", stat(&cfg, pid).await);
        let plan = "drain_reset";
        let attempt = recover_manual(&mut c, plan).await;
        println!(
            "guard recovery: {} phase={:?} status={} post-recovery server state: {}",
            attempt,
            c.guard().phase(),
            c.status.code(),
            stat(&cfg, pid).await
        );
        c.clear_cut();
        c.guard().clear_standing("open_transaction");
        let r = c
            .query("insert into og_probe values ('guard', 2)")
            .await
            .expect("insert");
        println!(
            "guard autocommit insert: command={:?} status={}",
            r.command,
            r.status.code()
        );
        println!("guard row present: {}", count(&cfg, "guard").await);
    }

    // ---- 3. completed BEGIN has a standing obligation ----------------------
    {
        let mut c = PgConn::connect(&cfg).await.expect("connect");
        let r = c.query("begin").await.expect("begin");
        println!(
            "standing begin: status={} standing={:?} qualification={:?}",
            r.status.code(),
            c.guard().standing(),
            c.guard().qualification()
        );
        let attempt = recover_manual(&mut c, "rollback").await;
        println!(
            "standing rollback: {} standing={:?} qualification={:?}",
            attempt,
            c.guard().standing(),
            c.guard().qualification()
        );
        let r = c
            .query("insert into og_probe values ('standing', 3)")
            .await
            .expect("insert");
        println!(
            "after cleanup insert: status={} rows={}",
            r.status.code(),
            count(&cfg, "standing").await
        );
    }

    // ---- 4. mid-reply SELECT cut ------------------------------------------
    {
        let mut c = PgConn::connect(&cfg).await.expect("connect");
        c.query("insert into og_probe values ('mid', 4)")
            .await
            .expect("prime");
        c.cut = Some(CutPoint {
            after_write_bytes: None,
            after_read_bytes: Some(1),
        });
        let e = c.query("select * from og_probe order by k").await;
        println!(
            "guard select cut: {:?} phase={:?}",
            e.as_ref().err().map(|e| e.to_string()),
            c.guard().phase()
        );
        let attempt = recover_manual(&mut c, "drain_reset").await;
        println!("guard select recovery: {}", attempt);
        c.clear_cut();
        let r = c
            .query("select count(*) from og_probe")
            .await
            .expect("count");
        println!(
            "successor after select recovery: rows={:?} status={}",
            r.rows,
            r.status.code()
        );
        let _ = c.query("begin").await;
        let _ = c.query("rollback").await;
    }

    // ---- 5. partial BEGIN: retire vs finish -------------------------------
    for finish in [false, true] {
        let mut c = PgConn::connect(&cfg)
            .await
            .expect("connect")
            .with_contract(if finish {
                EffectContract::AtomicEffectMayApply
            } else {
                EffectContract::MustNotApply
            });
        c.write_chunk = Some(3);
        c.cut = Some(CutPoint {
            after_write_bytes: Some(3),
            after_read_bytes: None,
        });
        let e = c.query("begin").await;
        let plan = if finish {
            "finish_drain_reset"
        } else {
            "drain_reset"
        };
        let attempt = recover_manual(&mut c, plan).await;
        println!(
            "partial begin finish={finish}: err={:?} recovery={} status={} phase={:?}",
            e.as_ref().err().map(|e| e.to_string()),
            attempt,
            c.status.code(),
            c.guard().phase()
        );
    }

    // ---- 5b. a rollback plan must not acknowledge a pending COMMIT --------
    {
        let mut c = PgConn::connect(&cfg)
            .await
            .expect("pending rollback connect");
        c.query("BEGIN").await.expect("pending begin");
        println!(
            "\npending rollback: before cut status={:?} standing={:?}",
            c.status,
            c.guard().standing()
        );
        c.cut = Some(CutPoint {
            after_write_bytes: None,
            after_read_bytes: Some(0),
        });
        let e = c.query("COMMIT").await.expect_err("commit cut");
        println!("pending rollback: cut={e:?} status={:?}", c.status);
        let r = recover_manual(&mut c, "rollback").await;
        println!(
            "pending rollback: plan=rollback result={r} phase={:?}",
            c.guard().phase()
        );
        assert!(
            c.guard().retired(),
            "a rollback plan must not claim recovery while a reply is pending"
        );
    }

    // ---- 5c. drain_reset handles the same pending COMMIT ------------------
    {
        let mut c = PgConn::connect(&cfg).await.expect("pending drain connect");
        c.query("BEGIN").await.expect("pending drain begin");
        c.cut = Some(CutPoint {
            after_write_bytes: None,
            after_read_bytes: Some(0),
        });
        let _ = c.query("COMMIT").await.expect_err("commit cut");
        let r = recover_manual(&mut c, "drain_reset").await;
        println!(
            "pending drain_reset: result={r} status={:?} standing={:?} phase={:?}",
            c.status,
            c.guard().standing(),
            c.guard().phase()
        );
        assert!(r.starts_with("Recovered"), "drain_reset must recover");
        let q = c
            .query(
                "insert into og_probe(k, v) values ('pd', 7) on conflict (k) do update set v = 7",
            )
            .await
            .expect("successor after pending drain");
        println!(
            "pending drain_reset successor: command={:?} status={:?}",
            q.command, q.status
        );
        assert_eq!(q.status, PgStatus::Idle);
    }

    // ---- 5d. bounded probe vs blind drain vs retire ------------------------
    {
        println!("\npolicy trials: pending SELECT reply delayed on the client read path");
        println!(
            "{:>10} {:>7} {:>8} {:>18} {:>5} {:>7} {:>9}  server state",
            "strategy", "delay", "budget", "outcome", "ok", "socket", "elapsed"
        );
        for delay in [20u64, 100, 1000] {
            for (strategy, budget) in [
                ("retire", 0u64),
                ("drain", 0),
                ("probe_50", 50),
                ("probe_150", 150),
            ] {
                println!(
                    "{}",
                    policy_trial(
                        &cfg,
                        strategy,
                        match strategy {
                            "retire" => &RetirePolicy as &dyn RecoveryPolicy,
                            "drain" => &PgPolicy as &dyn RecoveryPolicy,
                            _ => &ProbePolicy as &dyn RecoveryPolicy,
                        },
                        Duration::from_millis(delay),
                        Duration::from_millis(budget)
                    )
                    .await
                );
            }
        }
    }

    // ---- 5e. an I/O-suspect object with no pending reply must round-trip ---
    {
        let mut c = PgConn::connect(&cfg).await.expect("io probe connect");
        c.query("BEGIN").await.expect("io probe begin");
        let mut ticket = c.guard().begin().expect("begin ticket");
        ticket.arm();
        ticket.abandon(Cause::Io);
        assert!(c.guard().suspect().is_some());
        // The server is actually healthy, but the adapter saw an I/O failure
        // and has no queued reply.  The old drain path would mint a witness
        // from local state; the probe path pays one round trip first.
        c.read_delay_once = Some(Duration::from_millis(100));
        let start = std::time::Instant::now();
        let r = recover_manual_cause(&mut c, "drain_reset", Cause::Io).await;
        let elapsed = start.elapsed().as_millis();
        println!(
            "io-no-pending probe: result={r} elapsed={elapsed}ms status={} phase={:?}",
            c.status.code(),
            c.guard().phase()
        );
        assert!(r.starts_with("Recovered"), "the probe must recover");
        assert!(elapsed >= 90, "recovery must include the probe round trip");
    }

    // ---- 6. pool integration ----------------------------------------------
    {
        let pool = Pool::new(
            PgManager::new(cfg.clone(), EffectContract::MustNotApply),
            PoolConfig {
                name: "pg-lab",
                max_size: 1,
                recovery_capacity: 1,
                validate_on_checkout: false,
                policy: std::sync::Arc::new(PgPolicy),
                acquire_timeout: Some(Duration::from_secs(2)),
                retry_delay: Duration::from_millis(20),
                ..PoolConfig::default()
            },
        );
        {
            let mut lease = pool.acquire().await.expect("acquire");
            lease.cut = Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            });
            let e = lease.query("begin").await;
            println!(
                "pool lease begin cut: {:?}",
                e.as_ref().err().map(|e| e.to_string())
            );
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        let snap = pool.snapshot();
        println!(
            "pool after cut: recovery_enqueued={} succeeded={} failed={} retired_suspect={}",
            snap.recovery_enqueued,
            snap.recovery_succeeded,
            snap.recovery_failed,
            snap.retired_suspect
        );
        {
            let mut lease = pool.acquire().await.expect("reacquire");
            let r = lease
                .query("insert into og_probe values ('pool', 5)")
                .await
                .expect("pool insert");
            println!(
                "pool successor insert: status={} command={:?}",
                r.status.code(),
                r.command
            );
        }
        println!("pool row present: {}", count(&cfg, "pool").await);
        pool.shutdown(Duration::from_secs(1)).await;
    }

    // ---- 6b. the same decision through the pool's recovery worker ----------
    {
        let pool = Pool::new(
            PgManager::new(cfg.clone(), EffectContract::MustNotApply),
            PoolConfig {
                name: "pg-probe-policy",
                max_size: 1,
                recovery_capacity: 1,
                validate_on_checkout: false,
                policy: std::sync::Arc::new(ProbePolicy),
                acquire_timeout: Some(Duration::from_secs(2)),
                retry_delay: Duration::from_millis(10),
                ..PoolConfig::default()
            },
        );
        let pid_before;
        {
            let mut lease = pool.acquire().await.expect("probe acquire");
            pid_before = lease.backend_pid;
            lease.cut = Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            });
            let _ = lease.query("begin").await.expect_err("cut begin");
            lease.read_delay_once = Some(Duration::from_millis(20));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let snap = pool.snapshot();
        println!(
            "pool probe policy: enqueued={} succeeded={} failed={} retired={}",
            snap.recovery_enqueued,
            snap.recovery_succeeded,
            snap.recovery_failed,
            snap.retired_suspect
        );
        let mut lease = pool.acquire().await.expect("probe reacquire");
        let r = lease.query("SELECT 1").await.expect("probe successor");
        println!(
            "pool probe successor: status={} same_backend={}",
            r.status.code(),
            lease.backend_pid == pid_before
        );
        pool.shutdown(Duration::from_secs(1)).await;
    }

    // ---- 7. status is T after a completed BEGIN; DISCARD ALL fails there --
    {
        let mut c = PgConn::connect(&cfg).await.expect("connect");
        let _ = c.query("begin").await;
        let r = c.query("discard all").await.expect("discard");
        println!(
            "discard inside tx: error={:?} status={}",
            r.error
                .as_ref()
                .map(|e| (e.code.clone(), e.message.clone())),
            r.status.code()
        );
        let _ = c.query("rollback").await;
    }
    println!("done");
}

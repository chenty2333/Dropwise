//! Load and tail-latency comparison on a real Skytable instance.
//!
//! Each worker repeatedly selects a key that must be 111.  Every `fault_every`
//! operations it instead selects a key whose value is 222 with a scripted cut
//! before the first read byte; the next operation's expected value is still
//! 111, so a 222 reply is a silent misassociation.  This is scripted fault
//! injection (not a natural cancellation), and it is labelled as such.
//!
//! Baselines share the same workload and fault schedule:
//!   b0 - immediate reuse (no guard)
//!   b1 - poison/retire (reconnect)
//!   b2 - drain one frame, reconnect on failure
//!   b3 - guard + pool + quarantine + recovery worker
//!
//! The B3 timing includes acquire/return; the raw baselines have no pool.

use online_guards::lab::policy::ByteStatePolicy;
use online_guards::lab::raw::{RawSky, ReuseRule};
use online_guards::pool::{Pool, PoolConfig};
use online_guards::proto::sky::{CutPoint, Reply, SkyConfig, SkyManager, encode_param_str};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const A_VAL: u64 = 111;
const B_VAL: u64 = 222;

#[derive(Default)]
struct Stats {
    ok: u64,
    stale: u64,
    error: u64,
    timeout: u64,
    faults: u64,
    reconnects: u64,
    lat: Vec<u64>,
}

impl Stats {
    fn merge(&mut self, other: Stats) {
        self.ok += other.ok;
        self.stale += other.stale;
        self.error += other.error;
        self.timeout += other.timeout;
        self.faults += other.faults;
        self.reconnects += other.reconnects;
        self.lat.extend(other.lat);
    }
    fn pct(&self, p: f64) -> u64 {
        if self.lat.is_empty() {
            return 0;
        }
        let mut v = self.lat.clone();
        v.sort_unstable();
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[idx]
    }
}

fn key(space: &str, which: char) -> String {
    format!("{space}_{which}")
}

async fn select(
    raw: &mut RawSky,
    space: &str,
    which: char,
) -> Result<Option<u64>, online_guards::proto::sky::SkyError> {
    let (sql, params) = (
        format!("select * from {space}.kv where id = ?"),
        encode_param_str(&key(space, which)),
    );
    match raw.query(&sql, &params).await? {
        Reply::Row(vals) => Ok(vals.get(1).and_then(|v| v.as_u64())),
        _ => Ok(None),
    }
}

async fn raw_worker(
    server: SocketAddr,
    pass: String,
    space: String,
    rule: ReuseRule,
    fault_every: u64,
    stop: Arc<AtomicBool>,
) -> Stats {
    let mut st = Stats::default();
    let mut raw =
        match RawSky::connect(0, SkyConfig::loopback(server.port(), "root", &pass), rule).await {
            Ok(r) => r,
            Err(_) => {
                st.error += 1;
                return st;
            }
        };
    let mut i = 0u64;
    while !stop.load(Ordering::Relaxed) {
        i += 1;
        let fault = fault_every > 0 && i.is_multiple_of(fault_every);
        let started = Instant::now();
        if fault {
            st.faults += 1;
            // The cut targets the 222 key; it must not be associated with the
            // next 111 request.
            let (sql, params) = (
                format!("select * from {space}.kv where id = ?"),
                encode_param_str(&key(&space, 'b')),
            );
            raw.cut = Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            });
            let r = raw.query(&sql, &params).await;
            if r.is_err() {
                let _ = raw.prepare_reuse(Duration::from_millis(120)).await;
            }
            st.lat.push(started.elapsed().as_micros() as u64);
            continue;
        }
        match select(&mut raw, &space, 'a').await {
            Ok(Some(A_VAL)) => st.ok += 1,
            Ok(Some(B_VAL)) => st.stale += 1,
            Ok(_) => st.error += 1,
            Err(e) => {
                let s = e.to_string();
                if s.contains("timeout") {
                    st.timeout += 1;
                } else {
                    st.error += 1;
                }
                let _ = raw.prepare_reuse(Duration::from_millis(120)).await;
            }
        }
        st.lat.push(started.elapsed().as_micros() as u64);
    }
    st.reconnects = raw.reconnects as u64;
    st
}

async fn pool_worker(
    pool: Arc<Pool<online_guards::proto::sky::SkyConn, SkyManager>>,
    space: String,
    fault_every: u64,
    stop: Arc<AtomicBool>,
) -> Stats {
    let mut st = Stats::default();
    let mut i = 0u64;
    while !stop.load(Ordering::Relaxed) {
        i += 1;
        let fault = fault_every > 0 && i.is_multiple_of(fault_every);
        let started = Instant::now();
        let mut lease = match pool.acquire().await {
            Ok(l) => l,
            Err(_) => {
                st.error += 1;
                st.lat.push(started.elapsed().as_micros() as u64);
                continue;
            }
        };
        if fault {
            st.faults += 1;
            lease.cut = Some(CutPoint {
                after_write_bytes: None,
                after_read_bytes: Some(0),
            });
            let (sql, params) = (
                format!("select * from {space}.kv where id = ?"),
                encode_param_str(&key(&space, 'b')),
            );
            let r = lease.query_with(&sql, &params).await;
            if r.is_ok() {
                // The cut did not fire (e.g. the reply was already buffered).
                st.error += 1;
            }
            drop(lease);
            st.lat.push(started.elapsed().as_micros() as u64);
            continue;
        }
        let (sql, params) = (
            format!("select * from {space}.kv where id = ?"),
            encode_param_str(&key(&space, 'a')),
        );
        match lease.query_with(&sql, &params).await {
            Ok(Reply::Row(vals)) => match vals.get(1).and_then(|v| v.as_u64()) {
                Some(A_VAL) => st.ok += 1,
                Some(B_VAL) => st.stale += 1,
                _ => st.error += 1,
            },
            Ok(_) => st.error += 1,
            Err(_) => st.error += 1,
        }
        drop(lease);
        st.lat.push(started.elapsed().as_micros() as u64);
    }
    st
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SKY_PORT").ok())
        .expect("usage: throughput <port> [seconds] [concurrency] [fault_every]")
        .parse()
        .unwrap();
    let seconds: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let conc: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let fault_every: u64 = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let pass = std::env::var("SKY_PASS").unwrap_or_else(|_| "online-guards-pass".into());
    let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let space = format!("tp{}", std::process::id());
    {
        let cfg = SkyConfig::loopback(port, "root", &pass);
        let mut c = online_guards::proto::sky::SkyConn::connect(&cfg)
            .await
            .expect("connect");
        c.query(&format!("create space {space}"))
            .await
            .expect("space");
        c.query(&format!("create model {space}.kv(id: string, v: uint64)"))
            .await
            .expect("model");
        for (w, v) in [('a', A_VAL), ('b', B_VAL)] {
            let mut p = encode_param_str(&key(&space, w));
            p.extend(online_guards::proto::sky::encode_param_uint(v));
            c.query_with(&format!("insert into {space}.kv(?, ?)"), &p)
                .await
                .expect("prime");
        }
    }
    println!("mode conc={conc} fault_every={fault_every} seconds={seconds}");
    println!(
        "| mode | ops | ops/s | ok | stale | err | timeout | faults | reconnects | p50 us | p95 us | p99 us | max us |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for mode in ["b0", "b1", "b2", "b3"] {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        let pool = if mode == "b3" {
            Some(Arc::new(Pool::new(
                SkyManager::new(
                    SkyConfig::loopback(port, "root", &pass),
                    online_guards::recovery::EffectContract::MustNotApply,
                ),
                PoolConfig {
                    name: "tp",
                    max_size: conc,
                    recovery_capacity: conc,
                    validate_on_checkout: false,
                    policy: Arc::new(ByteStatePolicy {
                        allow_finish_write: false,
                    }),
                    acquire_timeout: Some(Duration::from_secs(3)),
                    retry_delay: Duration::from_millis(5),
                    ..PoolConfig::default()
                },
            )))
        } else {
            None
        };
        let rule = match mode {
            "b0" => ReuseRule::Immediate,
            "b1" => ReuseRule::PoisonRetire,
            _ => ReuseRule::DrainOnly,
        };
        let start = Instant::now();
        for _ in 0..conc {
            let pool = pool.clone();
            let sp = space.clone();
            let st = stop.clone();
            let pass2 = pass.clone();
            handles.push(tokio::spawn(async move {
                if let Some(p) = pool {
                    pool_worker(p, sp, fault_every, st).await
                } else {
                    raw_worker(server, pass2, sp, rule, fault_every, st).await
                }
            }));
        }
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        stop.store(true, Ordering::Relaxed);
        let mut total = Stats::default();
        for h in handles {
            if let Ok(s) = h.await {
                total.merge(s);
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let ops = total.ok + total.stale + total.error + total.timeout + total.faults;
        println!(
            "| {mode} | {ops} | {:.0} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            ops as f64 / elapsed,
            total.ok,
            total.stale,
            total.error,
            total.timeout,
            total.faults,
            total.reconnects,
            total.pct(0.50),
            total.pct(0.95),
            total.pct(0.99),
            total.lat.iter().max().copied().unwrap_or(0),
        );
        if let Some(p) = pool {
            let s = p.snapshot();
            println!(
                "  pool snapshot: connects={} retired_suspect={} recovery_ok={} failed={} unresolved={} queue_full={}",
                s.connects,
                s.retired_suspect,
                s.recovery_succeeded,
                s.recovery_failed,
                s.recovery_unresolved,
                s.queue_full
            );
            p.shutdown(Duration::from_millis(300)).await;
        }
    }
}

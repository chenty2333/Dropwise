//! JSONL trial records and a small aggregator.  No serde: every field written
//! here is a number or a string escaped by `json_str`.

use std::collections::BTreeMap;
use std::io::Write;

#[derive(Clone, Debug, Default)]
pub struct TrialRecord {
    pub scenario: String,
    pub baseline: String,
    pub trial: usize,
    pub contract: String,
    pub cut_write: i64,
    pub cut_read: i64,
    pub write_state: String,
    pub read_state: String,
    pub suspect: bool,
    pub plan: String,
    pub recovery: String,
    pub recovered: bool,
    pub reconnects: i64,
    pub conns: i64,
    pub drains_ok: i64,
    pub drains_failed: i64,
    /// "ok" | "stale" | "error" | "timeout" | "empty" | "skipped"
    pub successor: String,
    pub expected: i64,
    pub got: i64,
    pub latency_us: u64,
    pub note: String,
}

impl TrialRecord {
    /// A reply that cannot belong to the successor: a different value, or an
    /// Empty reply where a row was required.  Operational errors/timeouts are
    /// counted separately because they are not silent misassociation.
    pub fn unsafe_successor(&self) -> bool {
        matches!(self.successor.as_str(), "stale" | "empty")
    }

    pub fn successor_ok(&self) -> bool {
        self.successor == "ok"
    }

    pub fn json(&self) -> String {
        format!(
            "{{\"scenario\":{},\"baseline\":{},\"trial\":{},\"contract\":{},\"cut_write\":{},\"cut_read\":{},\"write_state\":{},\"read_state\":{},\"suspect\":{},\"plan\":{},\"recovery\":{},\"recovered\":{},\"reconnects\":{},\"conns\":{},\"drains_ok\":{},\"drains_failed\":{},\"successor\":{},\"expected\":{},\"got\":{},\"latency_us\":{},\"note\":{}}}",
            json_str(&self.scenario),
            json_str(&self.baseline),
            self.trial,
            json_str(&self.contract),
            self.cut_write,
            self.cut_read,
            json_str(&self.write_state),
            json_str(&self.read_state),
            self.suspect,
            json_str(&self.plan),
            json_str(&self.recovery),
            self.recovered,
            self.reconnects,
            self.conns,
            self.drains_ok,
            self.drains_failed,
            json_str(&self.successor),
            self.expected,
            self.got,
            self.latency_us,
            json_str(&self.note),
        )
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn write_jsonl(path: &str, records: &[TrialRecord]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    for r in records {
        writeln!(f, "{}", r.json())?;
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Aggregate {
    pub n: usize,
    pub unsafe_successors: usize,
    pub successor_ok: usize,
    pub successor_error: usize,
    pub successor_timeout: usize,
    pub recovered: usize,
    pub reconnects: i64,
    pub latency_us: Vec<u64>,
}

impl Aggregate {
    pub fn pct(&self, n: usize) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            100.0 * n as f64 / self.n as f64
        }
    }

    pub fn p50(&self) -> u64 {
        percentile(&self.latency_us, 50)
    }

    pub fn p95(&self) -> u64 {
        percentile(&self.latency_us, 95)
    }
}

pub fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let mut v = sorted.to_vec();
    v.sort_unstable();
    let idx = ((p as f64 / 100.0) * (v.len() as f64 - 1.0)).round() as usize;
    v[idx.min(v.len() - 1)]
}

pub fn aggregate(records: &[TrialRecord]) -> BTreeMap<(String, String), Aggregate> {
    let mut out: BTreeMap<(String, String), Aggregate> = BTreeMap::new();
    for r in records {
        let e = out
            .entry((r.scenario.clone(), r.baseline.clone()))
            .or_default();
        e.n += 1;
        if r.unsafe_successor() {
            e.unsafe_successors += 1;
        }
        match r.successor.as_str() {
            "ok" => e.successor_ok += 1,
            "error" => e.successor_error += 1,
            "timeout" => e.successor_timeout += 1,
            _ => {}
        }
        if r.recovered {
            e.recovered += 1;
        }
        e.reconnects += r.reconnects;
        e.latency_us.push(r.latency_us);
    }
    out
}

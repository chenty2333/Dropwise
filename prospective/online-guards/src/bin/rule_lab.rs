//! Action-labelled state study.
//!
//! For each injected state we execute every candidate recovery action on a
//! fresh reproduction of that state against the real server, then label the
//! state with the cheapest action that kept the successor correctly
//! associated.  Two offline sets are compared: a sparse one a developer might
//! write (fast cuts only) and a full one that includes delayed and lost
//! replies.  The learned tables are evaluated on held-out states, including
//! states with a different reply delay.

use online_guards::lab::actions::{Action, ActionResult, Injection, apply, prepare, successor};
use online_guards::lab::proxy::ProxyMode;
use online_guards::proto::sky::CutPoint;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

const RECONNECT_PENALTY_US: u64 = 150_000;

#[derive(Clone)]
struct StateSpec {
    name: &'static str,
    cut: Option<CutPoint>,
    write_chunk: Option<usize>,
    natural: Option<(ProxyMode, Duration)>,
    contract: &'static str,
    cut_op: char,
    write: &'static str,
    read: &'static str,
    op: &'static str,
    holdout: bool,
}

fn states() -> Vec<StateSpec> {
    let cut_before_read = CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(0),
    };
    let cut_mid_reply = CutPoint {
        after_write_bytes: None,
        after_read_bytes: Some(1),
    };
    let cut_zero = CutPoint {
        after_write_bytes: Some(0),
        after_read_bytes: None,
    };
    vec![
        StateSpec {
            name: "S1_complete_before_read",
            cut: Some(cut_before_read),
            write_chunk: None,
            natural: None,
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "none",
            op: "select",
            holdout: false,
        },
        StateSpec {
            name: "S2_complete_mid_reply",
            cut: Some(cut_mid_reply),
            write_chunk: None,
            natural: None,
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "partial",
            op: "select",
            holdout: true,
        },
        StateSpec {
            name: "S3_partial_insert",
            cut: Some(CutPoint {
                after_write_bytes: Some(5),
                after_read_bytes: None,
            }),
            write_chunk: Some(1),
            natural: None,
            contract: "may",
            cut_op: 'i',
            write: "partial",
            read: "none",
            op: "insert",
            holdout: false,
        },
        StateSpec {
            name: "S4_insert_before_read",
            cut: Some(cut_before_read),
            write_chunk: None,
            natural: None,
            contract: "may",
            cut_op: 'i',
            write: "complete",
            read: "none",
            op: "insert",
            holdout: true,
        },
        StateSpec {
            name: "S5_nothing_written",
            cut: Some(cut_zero),
            write_chunk: None,
            natural: None,
            contract: "must",
            cut_op: 's',
            write: "none",
            read: "none",
            op: "select",
            holdout: false,
        },
        StateSpec {
            name: "S6_delay_20ms",
            cut: None,
            write_chunk: None,
            natural: Some((
                ProxyMode::Delay(Duration::from_millis(20)),
                Duration::from_millis(2),
            )),
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "none",
            op: "select",
            holdout: false,
        },
        StateSpec {
            name: "S7_delay_300ms",
            cut: None,
            write_chunk: None,
            natural: Some((
                ProxyMode::Delay(Duration::from_millis(300)),
                Duration::from_millis(2),
            )),
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "none",
            op: "select",
            holdout: false,
        },
        StateSpec {
            name: "S8_reply_lost",
            cut: None,
            write_chunk: None,
            natural: Some((ProxyMode::LoseReply, Duration::from_millis(2))),
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "none",
            op: "select",
            holdout: false,
        },
        StateSpec {
            name: "S9_delay_100ms",
            cut: None,
            write_chunk: None,
            natural: Some((
                ProxyMode::Delay(Duration::from_millis(100)),
                Duration::from_millis(2),
            )),
            contract: "must",
            cut_op: 's',
            write: "complete",
            read: "none",
            op: "select",
            holdout: true,
        },
    ]
}

// Only facts present at the decision boundary. Fault configuration (including
// whether a reply is lost) is intentionally not a feature.
fn observed_features(s: &StateSpec, io: (usize, usize, usize)) -> BTreeMap<String, String> {
    let (written, total, read) = io;
    let write = if written == 0 {
        "none"
    } else if written == total {
        "complete"
    } else {
        "partial"
    };
    let read = if read == 0 { "none" } else { "partial" };
    BTreeMap::from([
        ("write".into(), write.into()),
        ("read".into(), read.into()),
        ("contract".into(), s.contract.into()),
        ("op".into(), s.op.into()),
    ])
}

fn authorised(action: Action, f: &BTreeMap<String, String>) -> bool {
    action != Action::FinishDrain
        || f.get("write").map(String::as_str) == Some("complete")
        || f.get("contract").map(String::as_str) == Some("may")
}

async fn run_action(
    s: &StateSpec,
    action: Action,
    trial: usize,
    space: &str,
    server: SocketAddr,
    pass: &str,
) -> ActionResult {
    let (cut_sql, cut_params) = match s.cut_op {
        's' => (
            format!("select * from {space}.kv where id = ?"),
            online_guards::proto::sky::encode_param_str(&format!("{space}_b{trial}")),
        ),
        _ => {
            let mut p = online_guards::proto::sky::encode_param_str(&format!("{space}_c{trial}"));
            p.extend(online_guards::proto::sky::encode_param_uint(333));
            (format!("insert into {space}.kv(?, ?)"), p)
        }
    };
    let mut run = match prepare(
        trial,
        space,
        server,
        pass,
        Injection {
            natural: s.natural,
            cut: s.cut,
            write_chunk: s.write_chunk,
        },
        &cut_sql,
        &cut_params,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return ActionResult {
                action,
                observed: None,
                safe: false,
                latency_us: 0,
                reconnects: 0,
                side_effect: None,
                note: e,
            };
        }
    };
    let observed = observed_features(s, run.raw.io_state());
    // A timer firing is not proof of the intended byte boundary. Reject the
    // experiment instead of assigning a successful action to an unrealized cut.
    assert_eq!(
        observed["write"], s.write,
        "unrealized write state: {}",
        s.name
    );
    assert_eq!(
        observed["read"], s.read,
        "unrealized read state: {}",
        s.name
    );
    if !authorised(action, &observed) {
        return ActionResult {
            action,
            observed: Some(observed),
            safe: false,
            latency_us: 0,
            reconnects: 0,
            side_effect: None,
            note: "unauthorised action; not executed".into(),
        };
    }
    let timeout = if action == Action::DrainSlow {
        Duration::from_millis(1200)
    } else {
        Duration::from_millis(120)
    };
    let started = std::time::Instant::now();
    let note = apply(action, &mut run.raw, timeout).await;
    let (kind, _) = successor(&mut run.raw, space, trial, Duration::from_millis(900)).await;
    let latency_us = started.elapsed().as_micros() as u64;
    let side_effect = if action == Action::FinishDrain {
        let (s, p) = (
            format!("select * from {space}.kv where id = ?"),
            online_guards::proto::sky::encode_param_str(&format!("{space}_c{trial}")),
        );
        match tokio::time::timeout(Duration::from_millis(600), run.raw.query(&s, &p)).await {
            Ok(Ok(online_guards::proto::sky::Reply::Row(vals))) => {
                vals.get(1).and_then(|v| v.as_u64())
            }
            _ => None,
        }
    } else {
        None
    };
    ActionResult {
        action,
        observed: Some(observed),
        safe: kind == "ok",
        latency_us,
        reconnects: run.raw.reconnects as i64,
        side_effect,
        note: format!("{note}; successor={kind}"),
    }
}

#[derive(Clone, Debug)]
struct Sample {
    features: BTreeMap<String, String>,
    label: Action,
}

#[derive(Clone, Debug)]
enum Tree {
    Leaf(Action),
    Split {
        feat: String,
        children: BTreeMap<String, Tree>,
    },
}

impl Tree {
    fn decide(&self, f: &BTreeMap<String, String>) -> Action {
        match self {
            Tree::Leaf(a) => *a,
            Tree::Split { feat, children } => match f.get(feat).and_then(|v| children.get(v)) {
                Some(t) => t.decide(f),
                None => Action::Retire,
            },
        }
    }

    fn rules(&self) -> Vec<(Vec<(String, String)>, Action)> {
        let mut out = Vec::new();
        fn walk(
            t: &Tree,
            path: &mut Vec<(String, String)>,
            out: &mut Vec<(Vec<(String, String)>, Action)>,
        ) {
            match t {
                Tree::Leaf(a) => out.push((path.clone(), *a)),
                Tree::Split { feat, children } => {
                    for (v, c) in children {
                        path.push((feat.clone(), v.clone()));
                        walk(c, path, out);
                        path.pop();
                    }
                }
            }
        }
        walk(self, &mut Vec::new(), &mut out);
        out
    }

    fn render(&self) -> String {
        let mut s = String::new();
        for (cond, a) in self.rules() {
            let c: Vec<String> = cond.iter().map(|(k, v)| format!("{k}={v}")).collect();
            s.push_str(&format!("if {} -> {}\n", c.join(" & "), a.name()));
        }
        s
    }
}

fn entropy(samples: &[Sample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut counts: BTreeMap<Action, usize> = BTreeMap::new();
    for s in samples {
        *counts.entry(s.label).or_default() += 1;
    }
    let n = samples.len() as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn learn(samples: &[Sample], feats: &[&str], depth: usize) -> Tree {
    if samples.is_empty() {
        return Tree::Leaf(Action::Retire);
    }
    let mut counts: BTreeMap<Action, usize> = BTreeMap::new();
    for s in samples {
        *counts.entry(s.label).or_default() += 1;
    }
    if counts.len() == 1 || depth >= 5 {
        let best = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(a, _)| *a)
            .unwrap();
        return Tree::Leaf(best);
    }
    let base = entropy(samples);
    let mut best: Option<(f64, &str)> = None;
    for f in feats {
        let mut groups: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
        for s in samples {
            groups
                .entry(s.features.get(*f).cloned().unwrap_or_default())
                .or_default()
                .push(s.clone());
        }
        let mut e = 0.0;
        for g in groups.values() {
            e += g.len() as f64 / samples.len() as f64 * entropy(g);
        }
        let gain = base - e;
        if gain > 1e-9 && best.map(|(b, _)| gain > b).unwrap_or(true) {
            best = Some((gain, f));
        }
    }
    match best {
        None => {
            let best = counts
                .iter()
                .max_by_key(|(_, n)| **n)
                .map(|(a, _)| *a)
                .unwrap();
            Tree::Leaf(best)
        }
        Some((_, f)) => {
            let mut groups: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
            for s in samples {
                groups
                    .entry(s.features.get(f).cloned().unwrap_or_default())
                    .or_default()
                    .push(s.clone());
            }
            let rest: Vec<&str> = feats.iter().filter(|x| **x != f).copied().collect();
            let mut children = BTreeMap::new();
            for (v, g) in groups {
                children.insert(v, learn(&g, &rest, depth + 1));
            }
            Tree::Split {
                feat: f.to_string(),
                children,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Cell {
    observed: BTreeMap<String, String>,
    safe_runs: usize,
    runs: usize,
    median_latency: u64,
    median_reconnects: i64,
    side_effect: Option<u64>,
    note: String,
}

fn cell(results: &[ActionResult]) -> Cell {
    let observed = results
        .first()
        .and_then(|r| r.observed.clone())
        .expect("missing observation: preparation failed, no valid experiment");
    for r in results {
        assert_eq!(
            r.observed.as_ref(),
            Some(&observed),
            "unstable/missing decision state; cannot label an action cell"
        );
    }
    let mut lat: Vec<u64> = results.iter().map(|r| r.latency_us).collect();
    lat.sort_unstable();
    let mut recs: Vec<i64> = results.iter().map(|r| r.reconnects).collect();
    recs.sort_unstable();
    Cell {
        observed,
        safe_runs: results.iter().filter(|r| r.safe).count(),
        runs: results.len(),
        median_latency: lat.get(lat.len() / 2).copied().unwrap_or(0),
        median_reconnects: recs.get(recs.len() / 2).copied().unwrap_or(0),
        side_effect: results.iter().find_map(|r| r.side_effect),
        note: results.first().map(|r| r.note.clone()).unwrap_or_default(),
    }
}

impl Cell {
    fn safe(&self) -> bool {
        self.runs > 0 && self.safe_runs == self.runs
    }
    fn cost(&self, penalty: u64) -> u64 {
        self.median_latency + penalty * (self.median_reconnects.max(0) as u64)
    }
}

struct PolicyResult {
    name: &'static str,
    rules: usize,
    unsafe_choices: usize,
    false_evictions: usize,
    total_cost: u64,
    details: Vec<String>,
}

fn evaluate(
    name: &'static str,
    rules: usize,
    choose: impl Fn(&BTreeMap<String, String>) -> Action,
    table: &BTreeMap<(String, Action), Cell>,
    states: &[StateSpec],
    penalty: u64,
) -> PolicyResult {
    let mut unsafe_choices = 0;
    let mut false_evictions = 0;
    let mut total_cost = 0;
    let mut details = Vec::new();
    for s in states {
        let f = &table[&(s.name.to_string(), Action::Reuse)].observed;
        let a = choose(f);
        let c = table.get(&(s.name.to_string(), a)).expect("action cell");
        if !c.safe() || !authorised(a, f) {
            unsafe_choices += 1;
        }
        total_cost += c.cost(penalty);
        let zero_reconnect_safe = Action::ALL.iter().any(|x| {
            table
                .get(&(s.name.to_string(), *x))
                .map(|c| c.safe() && c.median_reconnects == 0)
                .unwrap_or(false)
        });
        if c.median_reconnects > 0 && zero_reconnect_safe {
            false_evictions += 1;
        }
        details.push(format!(
            "{}: {} safe={} lat={} rec={}",
            s.name,
            a.name(),
            c.safe(),
            c.median_latency,
            c.median_reconnects
        ));
    }
    PolicyResult {
        name,
        rules,
        unsafe_choices,
        false_evictions,
        total_cost,
        details,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SKY_PORT").ok())
        .expect("usage: rule_lab <port>")
        .parse()
        .expect("port");
    let pass = std::env::var("SKY_PASS").unwrap_or_else(|_| "online-guards-pass".into());
    let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let space = format!("rule{}", std::process::id());
    {
        let cfg = online_guards::proto::sky::SkyConfig::loopback(port, "root", pass.clone());
        let mut c = online_guards::proto::sky::SkyConn::connect(&cfg)
            .await
            .expect("connect");
        c.query(&format!("create space {space}"))
            .await
            .expect("space");
        c.query(&format!("create model {space}.kv(id: string, v: uint64)"))
            .await
            .expect("model");
    }
    let states = states();
    let mut table: BTreeMap<(String, Action), Cell> = BTreeMap::new();
    for (si, s) in states.iter().enumerate() {
        if let Ok(only) = std::env::var("RULE_ONLY")
            && !s.name.contains(&only)
        {
            continue;
        }
        for a in Action::ALL {
            if s.cut_op == 'i' && a == Action::FinishDrain && s.write != "partial" {
                // Nothing to finish; equivalent to drain.  Still run it so the
                // table is complete.
            }
            let mut results = Vec::new();
            for trial in 0..3u64 {
                // Distinct keys per (state, action, trial) so side effects do
                // not collide.
                let t = si * 10_000
                    + (trial as usize) * 100
                    + Action::ALL.iter().position(|x| *x == a).unwrap();
                results.push(run_action(s, a, t, &space, server, &pass).await);
            }
            let c = cell(&results);
            println!(
                "{:30} {:14} safe={}/{} lat={}us rec={} side={:?} | {}",
                s.name,
                a.name(),
                c.safe_runs,
                c.runs,
                c.median_latency,
                c.median_reconnects,
                c.side_effect,
                c.note
            );
            if let Some(first) = table.get(&(s.name.to_string(), Action::Reuse)) {
                assert_eq!(
                    first.observed, c.observed,
                    "actions reached different states"
                );
            }
            println!("    observed={:?}", c.observed);
            table.insert((s.name.to_string(), a), c);
        }
    }

    if std::env::var_os("RULE_ONLY").is_some() {
        return; // Partial diagnostic runs are not complete policy evaluations.
    }
    let feats = ["write", "read", "contract", "op"];
    let holdout: Vec<StateSpec> = states.iter().filter(|s| s.holdout).cloned().collect();
    let probe: Vec<StateSpec> = states
        .iter()
        .filter(|s| s.name == "S5_nothing_written")
        .cloned()
        .collect();
    for penalty in [0u64, RECONNECT_PENALTY_US] {
        println!("\n### penalty={penalty}us");
        for (setname, names) in [
            (
                "sparse",
                vec!["S1_complete_before_read", "S3_partial_insert"],
            ),
            (
                "mid",
                vec![
                    "S1_complete_before_read",
                    "S3_partial_insert",
                    "S5_nothing_written",
                ],
            ),
            (
                "full",
                vec![
                    "S1_complete_before_read",
                    "S3_partial_insert",
                    "S5_nothing_written",
                    "S6_delay_20ms",
                    "S7_delay_300ms",
                    "S8_reply_lost",
                ],
            ),
        ] {
            let samples: Vec<Sample> = states
                .iter()
                .filter(|s| names.contains(&s.name))
                .filter_map(|s| {
                    let mut best: Option<(Action, u64)> = None;
                    for a in Action::ALL {
                        let c = table.get(&(s.name.to_string(), a))?;
                        if !c.safe() {
                            continue;
                        }
                        let cost = c.cost(penalty);
                        if best.map(|(_, b)| cost < b).unwrap_or(true) {
                            best = Some((a, cost));
                        }
                    }
                    best.map(|(a, _)| Sample {
                        features: table[&(s.name.to_string(), Action::Reuse)].observed.clone(),
                        label: a,
                    })
                })
                .collect();
            let tree = learn(&samples, &feats, 0);
            println!("--- training set {setname} ({} samples)", samples.len());
            print!("{}", tree.render());
            let tree_rules = tree.rules().len();
            let tree_for_eval = tree.clone();
            for (name, rules, f) in [
                (
                    "stock_reuse",
                    1,
                    Box::new(|_: &BTreeMap<String, String>| Action::Reuse)
                        as Box<dyn Fn(&BTreeMap<String, String>) -> Action>,
                ),
                (
                    "retire_all",
                    1,
                    Box::new(|_: &BTreeMap<String, String>| Action::Retire),
                ),
                (
                    "drain_only",
                    1,
                    Box::new(|_: &BTreeMap<String, String>| Action::Drain),
                ),
                (
                    "handwritten",
                    4,
                    Box::new(|f: &BTreeMap<String, String>| {
                        match (f["write"].as_str(), f["contract"].as_str()) {
                            ("none", _) => Action::Reuse,
                            ("partial", "may") => Action::FinishDrain,
                            ("partial", _) => Action::Retire,
                            _ => Action::DrainSlow,
                        }
                    }),
                ),
                (
                    "learned",
                    tree_rules,
                    Box::new(move |f: &BTreeMap<String, String>| tree_for_eval.decide(f)),
                ),
            ] {
                let r = evaluate(name, rules, f, &table, &holdout, penalty);
                println!(
                    "{:14} rules={} unsafe={} false_evictions={} cost={}us",
                    r.name, r.rules, r.unsafe_choices, r.false_evictions, r.total_cost
                );
                for d in &r.details {
                    println!("    {d}");
                }
                if name == "learned" {
                    let p = evaluate(
                        "probe_S5",
                        tree_rules,
                        |f: &BTreeMap<String, String>| tree.decide(f),
                        &table,
                        &probe,
                        penalty,
                    );
                    println!(
                        "    probe S5_nothing_written: {} unsafe={} false_evictions={} cost={}",
                        p.details.first().cloned().unwrap_or_default(),
                        p.unsafe_choices,
                        p.false_evictions,
                        p.total_cost
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_lost_and_unseen_delays_are_indistinguishable_before_recovery() {
        let all = states();
        let f = observed_features(&all[5], (32, 32, 0));
        for i in [6, 7, 8] {
            assert_eq!(f, observed_features(&all[i], (32, 32, 0)));
        }
        assert!(!f.contains_key("delay"));
        assert!(!f.contains_key("name"));
    }

    #[test]
    fn observation_is_not_the_planned_cut() {
        let s = &states()[0]; // expects complete/none
        let f = observed_features(s, (3, 32, 1));
        assert_eq!(f["write"], "partial");
        assert_eq!(f["read"], "partial");
        assert!(!authorised(Action::FinishDrain, &f));
        assert!(authorised(Action::Retire, &f));
    }

    #[test]
    #[should_panic(expected = "missing observation")]
    fn preparation_failure_is_not_an_unsafe_training_sample() {
        cell(&[ActionResult {
            action: Action::Reuse,
            observed: None,
            safe: false,
            latency_us: 0,
            reconnects: 0,
            side_effect: None,
            note: "setup failed".into(),
        }]);
    }
}

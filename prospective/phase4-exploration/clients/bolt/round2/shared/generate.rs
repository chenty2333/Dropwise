// Generated cancellation-target x successor cases for the pooled BOLT client.
//
// Registered targets:
// * `checkout` — pool checkout/validation, no business object key;
// * `write(primary, 1)` — a business write on the pre-created node.
//
// Registered successors:
// * `write(node, value)` — one business write;
// * `write+read(node, value)` — write, read back through the same pool, then
//   read the independently observed value and node tag;
// * `read-tag(node)` — identity read only, no comparable payload.
//
// The shared layer expands same/different object and same/different payload
// where both sides have a comparable slot. The keyless checkout target reports
// those dimensions as not applicable instead of inventing object combinations.
// Each case rebuilds its proxy, observer, two nodes, pool and warm-up; the
// original/fixed projects include this same file and differ only in the bb8
// source patch.

use crate::bolt::*;
use crate::combine::{
    Adapter, Case, CaseFuture, ObjectRelation, PayloadRelation, Shape, Slot, plan_cases,
    replay_case, run_case, run_cases,
};

use std::fmt;

use dropwise::{Config, Ctx, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeRole {
    Primary,
    Alternate,
}

impl NodeRole {
    fn as_str(self) -> &'static str {
        match self {
            NodeRole::Primary => "primary",
            NodeRole::Alternate => "alternate",
        }
    }

    fn other(self) -> Self {
        match self {
            NodeRole::Primary => NodeRole::Alternate,
            NodeRole::Alternate => NodeRole::Primary,
        }
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetOp {
    Checkout,
    Write { node: NodeRole, value: i64 },
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Checkout => f.write_str("checkout"),
            TargetOp::Write { node, value } => write!(f, "write({node},{value})"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Write { node: NodeRole, value: i64 },
    WriteRead { node: NodeRole, value: i64 },
    ReadTag { node: NodeRole },
}

impl SuccessorOp {
    fn node(&self) -> NodeRole {
        match self {
            SuccessorOp::Write { node, .. }
            | SuccessorOp::WriteRead { node, .. }
            | SuccessorOp::ReadTag { node } => *node,
        }
    }
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Write { node, value } => write!(f, "write({node},{value})"),
            SuccessorOp::WriteRead { node, value } => write!(f, "write+read({node},{value})"),
            SuccessorOp::ReadTag { node } => write!(f, "read-tag({node})"),
        }
    }
}

/// Concrete successor returns and independent effects. The cancelled target's
/// completed/cancelled control result is not included. Tags are normalized by
/// the adapter's explicit rule, which preserves primary/alternate identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    own_write: Option<Result<i64, String>>,
    own_read: Option<Result<i64, String>>,
    tag: Option<Result<String, String>>,
    visible_value: Option<Result<i64, String>>,
    visible_tag: Option<Result<String, String>>,
}

fn target_shape(target: &TargetOp) -> Shape {
    match target {
        TargetOp::Checkout => Shape::default(),
        TargetOp::Write { node, value } => Shape {
            object: Some(Slot::new("bolt/node", node.as_str())),
            payload: Some(Slot::new("bolt/write-value", value.to_string())),
        },
    }
}

fn successor_shape(successor: &SuccessorOp) -> Shape {
    match successor {
        SuccessorOp::Write { node, value } | SuccessorOp::WriteRead { node, value } => Shape {
            object: Some(Slot::new("bolt/node", node.as_str())),
            payload: Some(Slot::new("bolt/write-value", value.to_string())),
        },
        SuccessorOp::ReadTag { node } => Shape {
            object: Some(Slot::new("bolt/node", node.as_str())),
            payload: None,
        },
    }
}

fn target_node(target: &TargetOp) -> Option<NodeRole> {
    match target {
        TargetOp::Checkout => None,
        TargetOp::Write { node, .. } => Some(*node),
    }
}

fn target_value(target: &TargetOp) -> Option<i64> {
    match target {
        TargetOp::Checkout => None,
        TargetOp::Write { value, .. } => Some(*value),
    }
}

struct BoltAdapter {
    direct_url: String,
}

impl BoltAdapter {
    fn from_env() -> Result<Self, String> {
        Ok(Self { direct_url: url()? })
    }
}

impl Adapter for BoltAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![
            TargetOp::Write { node: NodeRole::Primary, value: TARGET_V },
            TargetOp::Checkout,
        ]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Write { node: NodeRole::Primary, value: SURVIVOR_V },
            SuccessorOp::WriteRead { node: NodeRole::Primary, value: SURVIVOR_V },
            SuccessorOp::ReadTag { node: NodeRole::Primary },
        ]
    }

    fn target_shape(&self, target: &Self::Target) -> Shape {
        target_shape(target)
    }

    fn successor_shape(&self, successor: &Self::Successor) -> Shape {
        successor_shape(successor)
    }

    fn realize(
        &self,
        target: &Self::Target,
        successor: &Self::Successor,
        object: Option<ObjectRelation>,
        payload: Option<PayloadRelation>,
    ) -> Result<Self::Successor, String> {
        let node_for = |base: NodeRole| -> Result<NodeRole, String> {
            match object {
                Some(ObjectRelation::Same) => target_node(target)
                    .ok_or_else(|| "target has no business object to reuse".to_string()),
                Some(ObjectRelation::Different) => target_node(target)
                    .map(NodeRole::other)
                    .ok_or_else(|| "target has no business object to differ from".to_string()),
                None => Ok(base),
            }
        };
        let value_for = |base: i64| -> Result<i64, String> {
            match payload {
                Some(PayloadRelation::Same) => target_value(target)
                    .ok_or_else(|| "target has no payload to reuse".to_string()),
                Some(PayloadRelation::Different) => Ok(base),
                None => Ok(base),
            }
        };
        match successor {
            SuccessorOp::Write { node, value } => {
                Ok(SuccessorOp::Write { node: node_for(*node)?, value: value_for(*value)? })
            }
            SuccessorOp::WriteRead { node, value } => Ok(SuccessorOp::WriteRead {
                node: node_for(*node)?,
                value: value_for(*value)?,
            }),
            SuccessorOp::ReadTag { node } => {
                if payload.is_some() {
                    return Err("read-tag has no comparable payload".into());
                }
                Ok(SuccessorOp::ReadTag { node: node_for(*node)? })
            }
        }
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        // A cancelled checkout or write may legitimately look not-started or
        // completed; the successor's own observation must equal one of those.
        Expect::Atomic
    }

    fn scenario(&self, ctx: Ctx, case: Case<Self::Target, Self::Successor>) -> CaseFuture<Self::Obs> {
        Box::pin(async move {
            let mut env = setup_nodes("gen", &["primary", "alternate"]).await?;
            let target_done: Option<Result<(), String>> = match &case.target {
                TargetOp::Checkout => ctx.target(env.checkout()).await,
                TargetOp::Write { node, value } => {
                    let node = env
                        .node(node.as_str())
                        .ok_or_else(|| format!("setup did not create node {node}"))?
                        .clone();
                    ctx.target(env.write_node(&node, *value)).await.map(|result| result.map(|_| ()))
                }
            };
            if let Some(Err(err)) = target_done {
                let _ = bounded("cleanup", env.cleanup()).await;
                return Err(format!("completed target failed: {err}"));
            }
            let obs = successor_observation(&mut env, &case.successor).await;
            let _ = bounded("cleanup", env.cleanup()).await;
            Ok(obs)
        })
    }
}

fn node_of(env: &Env, role: NodeRole) -> Node {
    env.node(role.as_str())
        .unwrap_or_else(|| panic!("setup did not create node {role}"))
        .clone()
}

fn normalized(prefix: &str, value: Result<String, String>) -> Result<String, String> {
    value.map(|value| normalize(prefix, &value))
}

async fn successor_observation(env: &mut Env, successor: &SuccessorOp) -> Obs {
    let node = node_of(env, successor.node());
    match successor {
        SuccessorOp::Write { value, .. } => {
            let own_write = bounded("successor write", env.write_node(&node, *value)).await;
            let visible_value =
                bounded("observer value after successor", env.observer_value_node(&node)).await;
            let visible_tag =
                bounded("observer tag after successor", env.observer_tag_node(&node)).await;
            Obs {
                own_write: Some(own_write),
                own_read: None,
                tag: None,
                visible_value: Some(visible_value),
                visible_tag: Some(normalized(&env.tag, visible_tag)),
            }
        }
        SuccessorOp::WriteRead { value, .. } => {
            let own_write = bounded("successor write", env.write_node(&node, *value)).await;
            let own_read = bounded("successor read-back", env.read_node(&node)).await;
            let tag = bounded("successor tag read", env.read_node_tag(&node)).await;
            let visible_value =
                bounded("observer value after successor", env.observer_value_node(&node)).await;
            let visible_tag =
                bounded("observer tag after successor", env.observer_tag_node(&node)).await;
            Obs {
                own_write: Some(own_write),
                own_read: Some(own_read),
                tag: Some(normalized(&env.tag, tag)),
                visible_value: Some(visible_value),
                visible_tag: Some(normalized(&env.tag, visible_tag)),
            }
        }
        SuccessorOp::ReadTag { .. } => {
            let tag = bounded("successor tag read", env.read_node_tag(&node)).await;
            let visible_tag =
                bounded("observer tag after successor", env.observer_tag_node(&node)).await;
            Obs {
                own_write: None,
                own_read: None,
                tag: Some(normalized(&env.tag, tag)),
                visible_value: None,
                visible_tag: Some(normalized(&env.tag, visible_tag)),
            }
        }
    }
}

fn gen_config() -> Config {
    Config { max_runs: 24, ..config() }
}

#[test]
fn generated_bolt_cases() {
    let adapter = BoltAdapter::from_env()
        .unwrap_or_else(|err| panic!("BOLT generated cases need the round-2 environment: {err}"));
    println!(
        "R2-GEN variant={VARIANT} reply_delay={REPLY_DELAY:?} url={}",
        adapter.direct_url
    );
    let runs = run_cases(&adapter, &gen_config(), 16);
    runs.print();

    assert_eq!(runs.plan.cases.len(), 13, "{}", runs.plan.notes.join(" | "));
    assert!(runs.plan.skipped.is_empty());
    assert!(runs.plan.unrun.is_empty());
    let count_object = |relation: ObjectRelation| {
        runs.plan
            .cases
            .iter()
            .filter(|case| case.object == Some(relation))
            .count()
    };
    let count_payload = |relation: PayloadRelation| {
        runs.plan
            .cases
            .iter()
            .filter(|case| case.payload == Some(relation))
            .count()
    };
    assert_eq!(count_object(ObjectRelation::Same), 5);
    assert_eq!(count_object(ObjectRelation::Different), 5);
    assert_eq!(count_payload(PayloadRelation::Same), 4);
    assert_eq!(count_payload(PayloadRelation::Different), 4);
    assert!(runs.plan.notes.iter().any(|note| {
        note.contains("target has no business object; successor kind bolt/node")
    }));
    for result in &runs.results {
        assert!(
            !result.report.trials.is_empty(),
            "no cancellation plan for {}: {}",
            result.label(),
            result.summary()
        );
    }
}

#[test]
fn replay_generated_bolt_checkout_case() {
    let adapter = BoltAdapter::from_env()
        .unwrap_or_else(|err| panic!("BOLT replay needs the round-2 environment: {err}"));
    let config = gen_config();
    let plan = plan_cases(&adapter, 16);
    let case = plan
        .cases
        .iter()
        .find(|case| case.target == TargetOp::Checkout && matches!(case.successor, SuccessorOp::Write { .. }))
        .expect("checkout -> write case was not generated")
        .clone();
    let run = run_case(&adapter, &config, &case);
    println!("R2-GEN-REPLAY source={}", run.summary());
    let source = run
        .report
        .violations()
        .next()
        .or_else(|| run.report.trials.first())
        .expect("generated checkout case produced no cancellation plan");
    let replay_plan = source.replay_plan();
    println!(
        "R2-GEN-REPLAY source_plan={} source_verdict={:?} source_detail={:?}",
        source.plan_label(),
        source.verdict,
        source.invariant
    );
    let replay = replay_case(&adapter, &config, &case, &replay_plan)
        .expect("replay rejected the generated case's plan");
    println!("R2-GEN-REPLAY {}", replay.summary());
    let trial = replay.report.trials.first().expect("replay produced no trial");
    println!(
        "R2-GEN-REPLAY plan={} realized={} verdict={:?} comparison={:?} invariant={:?}",
        trial.plan_label(),
        trial.is_realized(),
        trial.verdict,
        trial.comparison,
        trial.invariant
    );
    assert_eq!(replay.label(), run.label());
    assert_eq!(trial.plan, replay_plan.cuts);
}

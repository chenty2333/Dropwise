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
    Adapter, Case, CaseFuture, Diagnostics, IdentityBinding, IdentityMatch, ObjectRelation, Param,
    ParamSlots, PayloadRelation, SlotRole, SlotVisitor, plan_cases, replay_case, run_case,
    run_cases_with,
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

    /// The stable identity left after `env.tag` strips its per-run prefix.
    /// This must stay aligned with the adapter's explicit normalization rule.
    fn normalized_tag(self) -> &'static str {
        match self {
            NodeRole::Primary => "-primary",
            NodeRole::Alternate => "-alternate",
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
    Write {
        node: Param<NodeRole>,
        value: Param<i64>,
    },
}

impl TargetOp {
    fn node(&self) -> Option<NodeRole> {
        match self {
            TargetOp::Checkout => None,
            TargetOp::Write { node, .. } => Some(node.value),
        }
    }
}

impl fmt::Display for TargetOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetOp::Checkout => f.write_str("checkout"),
            TargetOp::Write { node, value } => {
                write!(f, "write({},{})", node.value, value.value)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SuccessorOp {
    Write {
        node: Param<NodeRole>,
        value: Param<i64>,
    },
    WriteRead {
        node: Param<NodeRole>,
        value: Param<i64>,
    },
    ReadTag {
        node: Param<NodeRole>,
    },
}

impl SuccessorOp {
    fn node(&self) -> NodeRole {
        match self {
            SuccessorOp::Write { node, .. }
            | SuccessorOp::WriteRead { node, .. }
            | SuccessorOp::ReadTag { node } => node.value,
        }
    }
}

impl fmt::Display for SuccessorOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuccessorOp::Write { node, value } => {
                write!(f, "write({},{})", node.value, value.value)
            }
            SuccessorOp::WriteRead { node, value } => {
                write!(f, "write+read({},{})", node.value, value.value)
            }
            SuccessorOp::ReadTag { node } => write!(f, "read-tag({})", node.value),
        }
    }
}

impl ParamSlots for TargetOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            TargetOp::Checkout => Ok(()),
            TargetOp::Write { node, value } => {
                visitor.visit(SlotRole::Object, node)?;
                visitor.visit(SlotRole::Payload, value)
            }
        }
    }
}

impl ParamSlots for SuccessorOp {
    fn visit_slots<V: SlotVisitor>(&mut self, visitor: &mut V) -> Result<(), String> {
        match self {
            SuccessorOp::Write { node, value } | SuccessorOp::WriteRead { node, value } => {
                visitor.visit(SlotRole::Object, node)?;
                visitor.visit(SlotRole::Payload, value)
            }
            SuccessorOp::ReadTag { node } => visitor.visit(SlotRole::Object, node),
        }
    }
}

/// Concrete successor returns and independent effects. The cancelled target's
/// completed/cancelled control result is not included. Tags are normalized by
/// the adapter's explicit rule, which preserves primary/alternate identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct Obs {
    own_write: Option<Result<i64, String>>,
    own_read: Option<Result<i64, String>>,
    tag: Option<Result<String, String>>,
    visible_value: Option<Result<i64, String>>,
    visible_tag: Option<Result<String, String>>,
}

fn node_param(role: NodeRole) -> Param<NodeRole> {
    Param::new("bolt/node", role)
}

fn successor_node_param(role: NodeRole) -> Param<NodeRole> {
    Param::new("bolt/node", role).with_alternatives([role.other()])
}

fn value_param(value: i64) -> Param<i64> {
    Param::new("bolt/write-value", value)
}

fn successor_value_param() -> Param<i64> {
    value_param(SURVIVOR_V).with_alternatives([TARGET_V])
}

struct BoltAdapter {
    direct_url: String,
}

impl BoltAdapter {
    fn from_env() -> Result<Self, String> {
        Ok(Self { direct_url: url()? })
    }

    fn identity_bindings(
        &self,
        case: &Case<TargetOp, SuccessorOp>,
        observed: &Obs,
    ) -> Vec<IdentityBinding> {
        let target = case.target.node().map(NodeRole::normalized_tag);
        let successor = Some(case.successor.node().normalized_tag());
        let mut bindings = Vec::new();
        for (field, value) in [
            ("tag", &observed.tag),
            ("visible_tag", &observed.visible_tag),
        ] {
            if let Some(Ok(value)) = value {
                bindings.push(IdentityBinding::new(field, value, target, successor));
            }
        }
        bindings
    }
}

impl Adapter for BoltAdapter {
    type Target = TargetOp;
    type Successor = SuccessorOp;
    type Obs = Obs;

    fn targets(&self) -> Vec<Self::Target> {
        vec![
            TargetOp::Write {
                node: node_param(NodeRole::Primary),
                value: value_param(TARGET_V),
            },
            TargetOp::Checkout,
        ]
    }

    fn successors(&self) -> Vec<Self::Successor> {
        vec![
            SuccessorOp::Write {
                node: successor_node_param(NodeRole::Primary),
                value: successor_value_param(),
            },
            SuccessorOp::WriteRead {
                node: successor_node_param(NodeRole::Primary),
                value: successor_value_param(),
            },
            SuccessorOp::ReadTag {
                node: successor_node_param(NodeRole::Primary),
            },
        ]
    }

    fn identities(
        &self,
        case: &Case<Self::Target, Self::Successor>,
        observed: &Self::Obs,
    ) -> Vec<IdentityBinding> {
        self.identity_bindings(case, observed)
    }

    fn expect(&self, _case: &Case<Self::Target, Self::Successor>) -> Expect {
        // A cancelled checkout or write may legitimately look not-started or
        // completed; the successor's own observation must equal one of those.
        Expect::Atomic
    }

    fn scenario(
        &self,
        ctx: Ctx,
        case: Case<Self::Target, Self::Successor>,
    ) -> CaseFuture<Self::Obs> {
        Box::pin(async move {
            let mut env = setup_nodes("gen", &["primary", "alternate"]).await?;
            let target_done: Option<Result<(), String>> = match &case.target {
                TargetOp::Checkout => ctx.target(env.checkout()).await,
                TargetOp::Write { node, value } => {
                    let node = env
                        .node(node.value.as_str())
                        .ok_or_else(|| format!("setup did not create node {}", node.value))?
                        .clone();
                    ctx.target(env.write_node(&node, value.value))
                        .await
                        .map(|result| result.map(|_| ()))
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
            let own_write = bounded("successor write", env.write_node(&node, value.value)).await;
            let visible_value = bounded(
                "observer value after successor",
                env.observer_value_node(&node),
            )
            .await;
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
            let own_write = bounded("successor write", env.write_node(&node, value.value)).await;
            let own_read = bounded("successor read-back", env.read_node(&node)).await;
            let tag = bounded("successor tag read", env.read_node_tag(&node)).await;
            let visible_value = bounded(
                "observer value after successor",
                env.observer_value_node(&node),
            )
            .await;
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
    Config {
        max_runs: 24,
        ..config()
    }
}

#[test]
fn bolt_plan_without_service() {
    let adapter = BoltAdapter {
        direct_url: String::new(),
    };
    let plan = plan_cases(&adapter, 16);
    let labels: Vec<String> = plan.cases.iter().map(Case::label).collect();
    assert_eq!(
        labels,
        vec![
            "target=write(primary,1) -> successor=write(primary,1) [object=same, payload=same]",
            "target=write(primary,1) -> successor=write(primary,7) [object=same, payload=different]",
            "target=write(primary,1) -> successor=write(alternate,1) [object=different, payload=same]",
            "target=write(primary,1) -> successor=write(alternate,7) [object=different, payload=different]",
            "target=write(primary,1) -> successor=write+read(primary,1) [object=same, payload=same]",
            "target=write(primary,1) -> successor=write+read(primary,7) [object=same, payload=different]",
            "target=write(primary,1) -> successor=write+read(alternate,1) [object=different, payload=same]",
            "target=write(primary,1) -> successor=write+read(alternate,7) [object=different, payload=different]",
            "target=write(primary,1) -> successor=read-tag(primary) [object=same, payload=n/a]",
            "target=write(primary,1) -> successor=read-tag(alternate) [object=different, payload=n/a]",
            "target=checkout -> successor=write(primary,7) [object=n/a, payload=n/a]",
            "target=checkout -> successor=write+read(primary,7) [object=n/a, payload=n/a]",
            "target=checkout -> successor=read-tag(primary) [object=n/a, payload=n/a]",
        ]
    );
    assert_eq!(plan.cases.len(), 13);
    assert!(plan.skipped.is_empty());
    assert!(plan.unrun.is_empty());
    let count_object = |relation: ObjectRelation| {
        plan.cases
            .iter()
            .filter(|case| case.object == Some(relation))
            .count()
    };
    let count_payload = |relation: PayloadRelation| {
        plan.cases
            .iter()
            .filter(|case| case.payload == Some(relation))
            .count()
    };
    assert_eq!(count_object(ObjectRelation::Same), 5);
    assert_eq!(count_object(ObjectRelation::Different), 5);
    assert_eq!(count_payload(PayloadRelation::Same), 4);
    assert_eq!(count_payload(PayloadRelation::Different), 4);
    assert!(
        plan.notes.iter().any(|note| {
            note.contains("target has no business object; successor kind bolt/node")
        })
    );
}

#[test]
fn same_object_identity_ambiguity_is_visible() {
    let adapter = BoltAdapter {
        direct_url: String::new(),
    };
    let plan = plan_cases(&adapter, 16);
    let same = plan
        .cases
        .iter()
        .find(|case| {
            case.target
                == TargetOp::Write {
                    node: node_param(NodeRole::Primary),
                    value: value_param(TARGET_V),
                }
                && case.successor
                    == SuccessorOp::Write {
                        node: successor_node_param(NodeRole::Primary),
                        value: successor_value_param(),
                    }
                && case.object == Some(ObjectRelation::Same)
        })
        .expect("same-object write case missing")
        .clone();
    let observed = Obs {
        own_write: Some(Ok(TARGET_V)),
        own_read: None,
        tag: None,
        visible_value: Some(Ok(TARGET_V)),
        visible_tag: Some(Ok("-primary".into())),
    };
    let bindings = adapter.identity_bindings(&same, &observed);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].field, "visible_tag");
    assert_eq!(bindings[0].observed, "-primary");
    assert_eq!(bindings[0].target.as_deref(), Some("-primary"));
    assert_eq!(bindings[0].successor.as_deref(), Some("-primary"));
    assert_eq!(bindings[0].matched, IdentityMatch::MatchesBoth);

    let different = plan
        .cases
        .iter()
        .find(|case| case.object == Some(ObjectRelation::Different))
        .expect("different-object case missing")
        .clone();
    let observed = Obs {
        own_write: Some(Ok(SURVIVOR_V)),
        own_read: None,
        tag: None,
        visible_value: Some(Ok(SURVIVOR_V)),
        visible_tag: Some(Ok("-alternate".into())),
    };
    let bindings = adapter.identity_bindings(&different, &observed);
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].matched, IdentityMatch::MatchesSuccessor);

    let observed = Obs {
        own_write: None,
        own_read: None,
        tag: None,
        visible_value: None,
        visible_tag: Some(Ok("ghost".into())),
    };
    let bindings = adapter.identity_bindings(&different, &observed);
    assert_eq!(bindings[0].matched, IdentityMatch::Unmatched);
}

#[test]
fn generated_bolt_cases() {
    let adapter = BoltAdapter::from_env()
        .unwrap_or_else(|err| panic!("BOLT generated cases need the round-2 environment: {err}"));
    println!(
        "R2-GEN variant={VARIANT} reply_delay={REPLY_DELAY:?} url={}",
        adapter.direct_url
    );
    let runs = run_cases_with(&adapter, &gen_config(), 16, Diagnostics::On);
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
    assert!(
        runs.plan.notes.iter().any(|note| {
            note.contains("target has no business object; successor kind bolt/node")
        })
    );
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
        .find(|case| {
            case.target == TargetOp::Checkout && matches!(case.successor, SuccessorOp::Write { .. })
        })
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
    let trial = replay
        .report
        .trials
        .first()
        .expect("replay produced no trial");
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

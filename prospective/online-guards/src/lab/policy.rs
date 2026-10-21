//! Policies under comparison.  The handwritten guarded policy is deliberately
//! small; the rule study learns a table with the same feature vocabulary so
//! the two can be compared without a representation advantage.

use crate::features::Features;
use crate::guard::Suspect;
use crate::policy::{Plan, RecoveryPolicy};

/// Byte-state policy: the plan is a function of what the protocol adapter
/// recorded, not of the wall-clock cause of the cancellation.
pub struct ByteStatePolicy {
    /// Whether completing a partially written request is authorised for this
    /// deployment.  This is an authority parameter, not a protocol fact.
    pub allow_finish_write: bool,
}

impl RecoveryPolicy for ByteStatePolicy {
    fn choose(&self, s: &Suspect) -> Plan {
        let write = s.features.name("write").unwrap_or("unknown");
        let read = s.features.name("read").unwrap_or("unknown");
        let contract = s.features.name("contract").unwrap_or("must_not_apply");
        match (write, read) {
            ("none", "none") => Plan::Run("discard_local"),
            ("complete", _) => {
                if s.attempts >= 2 {
                    Plan::Run("drain_slow")
                } else {
                    Plan::Run("drain")
                }
            }
            ("partial", _)
                if self.allow_finish_write && contract == "may_apply" =>
            {
                if s.attempts >= 2 {
                    Plan::Run("finish_write_drain_slow")
                } else {
                    Plan::Run("finish_write_drain")
                }
            }
            _ => Plan::Retire,
        }
    }

    fn describe(&self) -> String {
        format!("byte-state(finish={})", self.allow_finish_write)
    }
}

pub struct RetireAll;

impl RecoveryPolicy for RetireAll {
    fn choose(&self, _s: &Suspect) -> Plan {
        Plan::Retire
    }
    fn describe(&self) -> String {
        "retire-all".into()
    }
}

/// A table-driven policy used by the rule study: first matching rule wins.
pub struct LearnedPolicy {
    pub rules: Vec<LearnedRule>,
    pub fallback: Plan,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct LearnedRule {
    pub when: Vec<(&'static str, &'static str)>,
    pub plan: Plan,
}

impl LearnedPolicy {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for r in &self.rules {
            let cond: Vec<String> = r
                .when
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let plan = match r.plan {
                Plan::Run(p) => p,
                Plan::Retire => "retire",
            };
            out.push_str(&format!("if {} then {plan}\n", cond.join(" & ")));
        }
        out
    }
}

impl RecoveryPolicy for LearnedPolicy {
    fn choose(&self, s: &Suspect) -> Plan {
        for r in &self.rules {
            if r.when
                .iter()
                .all(|(k, v)| s.features.name(k) == Some(*v))
            {
                return r.plan;
            }
        }
        self.fallback
    }
    fn describe(&self) -> String {
        self.description.clone()
    }
}

pub fn feature(s: &Suspect, key: &str) -> String {
    s.features.name(key).unwrap_or("unknown").to_string()
}

pub fn features_of(s: &Suspect) -> Features {
    s.features.clone()
}

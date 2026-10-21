//! Policies that choose a recovery plan for a suspect object.
//!
//! A policy only chooses an action name.  It cannot mark an object reusable and
//! cannot bypass the ledger: the protocol adapter executes the plan and mints a
//! witness only after checking the wire state.

use crate::guard::Suspect;

/// One possible action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Run the named adapter plan.
    Run(&'static str),
    /// Do not try to repair; close and replace the object.
    Retire,
}

/// A policy maps the observation at the hand-off point to a plan.  The decision
/// must depend only on facts recorded in the report (plus its own static
/// knowledge of the protocol); consulting the wall clock or global state would
/// make a learned rule untestable.
pub trait RecoveryPolicy: Send + Sync {
    fn choose(&self, report: &Suspect) -> Plan;
    /// Stable description for logs and result tables.
    fn describe(&self) -> String;
}

/// The boolean-poison baseline: every abandoned operation retires the object.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConservativeRetire;

impl RecoveryPolicy for ConservativeRetire {
    fn choose(&self, _report: &Suspect) -> Plan {
        Plan::Retire
    }

    fn describe(&self) -> String {
        "retire-on-any-suspect".into()
    }
}

/// A hand-written plan table, used as the expert baseline before learned rules.
/// Rules are evaluated in order; the first matching rule wins.
pub struct RuleTable {
    pub name: String,
    pub rules: Vec<Rule>,
    pub fallback: Plan,
}

pub struct Rule {
    pub when: Box<dyn Fn(&Suspect) -> bool + Send + Sync>,
    pub plan: Plan,
    pub note: &'static str,
}

impl RuleTable {
    pub fn new(name: impl Into<String>, fallback: Plan) -> Self {
        Self {
            name: name.into(),
            rules: Vec::new(),
            fallback,
        }
    }

    pub fn when(
        mut self,
        note: &'static str,
        when: impl Fn(&Suspect) -> bool + Send + Sync + 'static,
        plan: Plan,
    ) -> Self {
        self.rules.push(Rule {
            when: Box::new(when),
            plan,
            note,
        });
        self
    }
}

impl RecoveryPolicy for RuleTable {
    fn choose(&self, report: &Suspect) -> Plan {
        for rule in &self.rules {
            if (rule.when)(report) {
                return rule.plan;
            }
        }
        self.fallback
    }

    fn describe(&self) -> String {
        let mut s = format!("{}[", self.name);
        for (i, r) in self.rules.iter().enumerate() {
            if i > 0 {
                s.push_str("; ");
            }
            s.push_str(r.note);
        }
        s.push_str("] fallback=");
        match self.fallback {
            Plan::Run(p) => s.push_str(p),
            Plan::Retire => s.push_str("retire"),
        }
        s
    }
}

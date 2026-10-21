//! Named observations taken at a hand-off decision point.
//!
//! The guard itself never interprets these; a protocol adapter records what it
//! knows, a [`crate::policy::RecoveryPolicy`] maps the record to a plan, and the
//! offline rule learner uses the same keys.  Keeping one representation for all
//! three is deliberate: an observation the policy cannot see must not appear in
//! a learned rule.

use std::collections::BTreeMap;
use std::fmt;

/// A small, comparable value.  `Name` is an open-ended symbolic tag chosen by
/// the adapter (for example `"partial"` or `"intrans"`); `Int` is used for byte
/// counts and attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Name(&'static str),
}

impl Value {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn as_name(self) -> Option<&'static str> {
        match self {
            Value::Name(n) => Some(n),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Name(n) => f.write_str(n),
        }
    }
}

/// Ordered feature map.  Ordered so logs, test assertions and learned-rule
/// output are stable; the size is tiny (adapters record fewer than a dozen
/// keys).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features(BTreeMap<&'static str, Value>);

impl Features {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &'static str, value: impl Into<Value>) {
        self.0.insert(key, value.into());
    }

    /// Set a symbolic tag.  Adapters use this for enumerable states.
    pub fn tag(&mut self, key: &'static str, name: &'static str) {
        self.0.insert(key, Value::Name(name));
    }

    pub fn with(mut self, key: &'static str, value: impl Into<Value>) -> Self {
        self.set(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.0.get(key).copied()
    }

    pub fn name(&self, key: &str) -> Option<&'static str> {
        self.get(key).and_then(Value::as_name)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_int)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        self.0.iter().map(|(k, v)| (*k, *v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Stable `k=v,k=v` rendering for logs and datasets.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, (k, v)) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push('=');
            out.push_str(&v.to_string());
        }
        out
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Value::Int(v as i64)
    }
}

impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Value::Int(v as i64)
    }
}

impl From<&'static str> for Value {
    fn from(v: &'static str) -> Self {
        Value::Name(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_ordered_and_stable() {
        let mut f = Features::new();
        f.tag("a", "x");
        f.set("b", 3u64);
        assert_eq!(f.render(), "a=x,b=3");
        assert_eq!(f.name("a"), Some("x"));
        assert_eq!(f.int("b"), Some(3));
    }
}

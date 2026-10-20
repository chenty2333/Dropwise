//! A toy request/response server and a one-connection pool, shared by the
//! direct-handle and pooled examples.
//!
//! This is a teaching model, not a real client, connection pool or protocol.
//! A `get` writes its request to the server-side log, queues the reply, returns
//! `Pending` once, and only then consumes the next queued reply. Cancelling
//! after that `Pending` therefore leaves a reply behind. That is enough to show
//! the shape Dropwise is meant to check; it says nothing about a real driver.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// What the scenario judges: the survivor's value, the effect it wrote, and a
/// server-side log read independently of the handle or pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obs {
    pub successor: String,
    pub derived: Option<String>,
    pub requests: Vec<String>,
}

/// The server-side state. The scenario holds this `Arc` separately from the
/// handle/pool, which is what makes the observation independent.
pub struct Server {
    values: BTreeMap<String, String>,
    requests: Vec<String>,
}

impl Server {
    /// A fresh server with `a -> "a"` and `b -> "b"` for every scenario run.
    pub fn new() -> Arc<Mutex<Self>> {
        let values = BTreeMap::from([("a".to_string(), "a".to_string()), ("b".into(), "b".into())]);
        Arc::new(Mutex::new(Self {
            values,
            requests: Vec::new(),
        }))
    }

    fn get(&mut self, key: &str) -> String {
        self.requests.push(format!("get {key}"));
        self.values.get(key).cloned().unwrap_or_default()
    }

    fn put(&mut self, key: &str, value: String) {
        self.requests.push(format!("put {key}"));
        self.values.insert(key.to_string(), value);
    }
}

/// Read the independent server-side observation after the survivor has run.
pub fn observe(server: &Arc<Mutex<Server>>, successor: String) -> Obs {
    let server = server.lock().unwrap();
    Obs {
        successor,
        derived: server.values.get("derived").cloned(),
        requests: server.requests.clone(),
    }
}

/// A direct handle: an ordered request/response channel with no implicit reset
/// boundary between callers.
pub struct Session {
    pending: VecDeque<String>,
    server: Arc<Mutex<Server>>,
}

impl Session {
    pub fn new(server: Arc<Mutex<Server>>) -> Self {
        Self {
            pending: VecDeque::new(),
            server,
        }
    }

    /// Send a request, yield once (the `Pending` boundary), then take the next
    /// reply. A cancelled call leaves its reply queued.
    pub async fn get(&mut self, key: &str) -> String {
        let reply = self.server.lock().unwrap().get(key);
        self.pending.push_back(reply);
        tokio::task::yield_now().await;
        self.pending
            .pop_front()
            .expect("the request queued its own reply")
    }

    /// A write without a reply, followed by one yield so control flow is
    /// explicit. It does not produce or consume a reply.
    pub async fn put(&mut self, key: &str, value: String) {
        self.server.lock().unwrap().put(key, value);
        tokio::task::yield_now().await;
    }

    /// Explicit recovery for a direct handle: discard one reply that belongs to
    /// an operation the caller cancelled. Returns whether something was
    /// discarded.
    pub fn discard_stale_reply(&mut self) -> bool {
        self.pending.pop_front().is_some()
    }
}

struct PoolInner {
    sessions: Mutex<Vec<Session>>,
    clean_on_return: bool,
}

/// A one-connection pool. Checking out removes the only session; dropping the
/// lease returns it. This is the smallest shape that demonstrates hand-off.
pub struct Pool {
    inner: Arc<PoolInner>,
}

impl Pool {
    /// `clean_on_return` selects whether the lease drains a stale reply before
    /// the next user can check the connection out.
    pub fn new(server: Arc<Mutex<Server>>, clean_on_return: bool) -> Self {
        let session = Session::new(server);
        Self {
            inner: Arc::new(PoolInner {
                sessions: Mutex::new(vec![session]),
                clean_on_return,
            }),
        }
    }

    pub async fn get(&self, key: &str) -> String {
        let mut lease = self.checkout();
        lease.get(key).await
    }

    pub async fn put(&self, key: &str, value: String) {
        let mut lease = self.checkout();
        lease.put(key, value).await;
    }

    fn checkout(&self) -> Lease {
        let session = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .pop()
            .expect("the one-connection pool is exhausted");
        Lease {
            session: Some(session),
            inner: self.inner.clone(),
        }
    }
}

struct Lease {
    session: Option<Session>,
    inner: Arc<PoolInner>,
}

impl Lease {
    async fn get(&mut self, key: &str) -> String {
        self.session
            .as_mut()
            .expect("lease holds a session")
            .get(key)
            .await
    }

    async fn put(&mut self, key: &str, value: String) {
        self.session
            .as_mut()
            .expect("lease holds a session")
            .put(key, value)
            .await;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            if self.inner.clean_on_return {
                session.discard_stale_reply();
            }
            self.inner.sessions.lock().unwrap().push(session);
        }
    }
}

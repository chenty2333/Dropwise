use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A value that must be explicitly resolved ([`discharge`](Self::discharge) or
/// [`abandon`](Self::abandon)).
///
/// Typical use: wrap a message right after it is taken from a queue. Inside a
/// Dropwise run, every obligation is registered with the run's ledger, which
/// reports it if it is dropped unresolved (e.g. by a cancellation) or if it is
/// still unresolved when the run settles (forgotten, or held forever by some
/// task). Outside a run, tracking is off and an `Obligation` is a thin wrapper.
pub struct Obligation<T> {
    value: Option<T>,
    id: u64,
    label: &'static str,
    site: &'static Location<'static>,
    ledger: Option<Arc<Ledger>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeakKind {
    /// Dropped while still holding its value.
    DroppedUnresolved,
    /// Neither resolved nor dropped by the end of the settle phase.
    Outstanding,
}

/// An obligation that was not resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leak {
    /// Unique per process, so the same obligation can be followed across reports.
    pub id: u64,
    pub label: &'static str,
    pub site: &'static Location<'static>,
    pub kind: LeakKind,
}

impl fmt::Display for Leak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            LeakKind::DroppedUnresolved => "was dropped unresolved",
            LeakKind::Outstanding => "was still unresolved after settling (forgotten or held forever)",
        };
        write!(f, "obligation #{} `{}` created at {} {what}", self.id, self.label, self.site)
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

type Origin = (&'static str, &'static Location<'static>);

/// Obligations of one scenario run, shared by every thread of its runtime.
#[derive(Default)]
pub(crate) struct Ledger {
    leaks: Mutex<Vec<Leak>>,
    live: Mutex<BTreeMap<u64, Origin>>,
}

impl Ledger {
    /// Leaks so far plus every obligation still live, ordered by id.
    pub(crate) fn finish(&self) -> Vec<Leak> {
        let mut leaks = std::mem::take(&mut *self.leaks.lock().unwrap());
        let live = std::mem::take(&mut *self.live.lock().unwrap());
        leaks.extend(live.into_iter().map(|(id, (label, site))| Leak {
            id,
            label,
            site,
            kind: LeakKind::Outstanding,
        }));
        leaks.sort_by_key(|l| l.id);
        leaks
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Ledger>>> = const { RefCell::new(None) };
}

/// Make `ledger` the current thread's ledger (`None` detaches it), returning
/// the ledger the thread had before.
pub(crate) fn set_current(ledger: Option<Arc<Ledger>>) -> Option<Arc<Ledger>> {
    CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), ledger))
}

/// Tracks the ledger of the current thread for one run, restoring the previous
/// one even if the run panics: a stale ledger would keep collecting obligations
/// into an already-reported run.
pub(crate) struct CurrentLedger(Option<Arc<Ledger>>);

impl CurrentLedger {
    pub(crate) fn attach(ledger: Arc<Ledger>) -> Self {
        Self(set_current(Some(ledger)))
    }
}

impl Drop for CurrentLedger {
    fn drop(&mut self) {
        set_current(self.0.take());
    }
}

impl<T> Obligation<T> {
    #[track_caller]
    pub fn new(value: T, label: &'static str) -> Self {
        Self::at(value, label, Location::caller())
    }

    pub(crate) fn at(value: T, label: &'static str, site: &'static Location<'static>) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let ledger = CURRENT.with(|c| c.borrow().clone());
        if let Some(ledger) = &ledger {
            ledger.live.lock().unwrap().insert(id, (label, site));
        }
        Self { value: Some(value), id, label, site, ledger }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn get(&self) -> &T {
        self.value.as_ref().expect("obligation already resolved")
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.value.as_mut().expect("obligation already resolved")
    }

    /// Resolve the obligation; the caller takes over responsibility for the value.
    pub fn discharge(mut self) -> T {
        self.resolve();
        self.value.take().expect("obligation already resolved")
    }

    /// Resolve the obligation by explicitly giving the value up.
    pub fn abandon(mut self, _reason: &str) {
        self.resolve();
        self.value.take();
    }

    fn resolve(&self) {
        if let Some(ledger) = &self.ledger {
            ledger.live.lock().unwrap().remove(&self.id);
        }
    }
}

impl<T> Drop for Obligation<T> {
    fn drop(&mut self) {
        if let (Some(_), Some(ledger)) = (&self.value, &self.ledger) {
            ledger.live.lock().unwrap().remove(&self.id);
            if !std::thread::panicking() {
                let leak = Leak {
                    id: self.id,
                    label: self.label,
                    site: self.site,
                    kind: LeakKind::DroppedUnresolved,
                };
                ledger.leaks.lock().unwrap().push(leak);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Ctx, explore};

    #[test]
    fn run_restores_the_previous_ledger_on_every_exit_path() {
        let previous = Arc::new(Ledger::default());
        let binding = CurrentLedger::attach(previous.clone());
        let config = Config { max_runs: 1, ..Config::default() };
        // Constructor panic, future panic, check panic, and normal completion.
        for phase in 0..4 {
            let result = std::panic::catch_unwind(|| {
                explore(&config, |ctx: Ctx| {
                    assert!(phase != 0, "constructor panic");
                    async move {
                        assert!(phase != 1, "future panic");
                        if phase == 2 {
                            ctx.after_settle(|| panic!("check panic"));
                        }
                        Ok(())
                    }
                })
            });
            assert_eq!(result.is_err(), phase < 3);
            CURRENT.with(|current| {
                assert!(Arc::ptr_eq(current.borrow().as_ref().unwrap(), &previous));
            });
        }
        drop(binding);
        CURRENT.with(|current| assert!(current.borrow().is_none()));
    }
}

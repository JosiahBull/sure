//! Remembering *where* each tracked task was spawned, so an abandoned one can be named.
//!
//! A drain that times out saying "3 tasks still running" tells you that you have a
//! problem and nothing about where it is — and the tasks are gone by the time you could
//! look. Recording [`Location::caller`] at spawn time turns that into
//! `packages/server/src/lib.rs:158`, which is the actual answer.
//!
//! Debug builds only. The bookkeeping is a mutex and a `HashMap` insert per spawn, which
//! is nothing next to spawning a task but is not free either, and a release binary has no
//! one reading its warnings interactively. In release every operation here compiles to a
//! zero-sized no-op and [`CallSites::outstanding`] returns an empty list — hence the
//! count in [`DrainOutcome::TimedOut`](crate::DrainOutcome::TimedOut) staying alongside
//! the locations rather than being replaced by them.

// Only `CallSites` is named elsewhere; the guard is an opaque return value whose only job
// is to be held and then dropped.
#[cfg(debug_assertions)]
pub(crate) use debug::CallSites;
#[cfg(not(debug_assertions))]
pub(crate) use release::CallSites;

#[cfg(debug_assertions)]
mod debug {
    use std::collections::HashMap;
    use std::panic::Location;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// The live spawn sites, keyed by an id that exists only to let two tasks spawned
    /// from the same line be removed independently.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct CallSites {
        live: Arc<Mutex<HashMap<u64, &'static Location<'static>>>>,
        next_id: Arc<AtomicU64>,
    }

    impl CallSites {
        /// Register a spawn site. The returned guard removes it again when the task ends
        /// — by completing, or by being dropped.
        pub(crate) fn record(&self, location: &'static Location<'static>) -> CallSiteGuard {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            self.lock().insert(id, location);
            CallSiteGuard {
                sites: self.clone(),
                id,
            }
        }

        /// Where every still-running tracked task was spawned, sorted so a repeated run
        /// produces a comparable list.
        ///
        /// Snapshot this *before* shutting the runtime down: once the tasks are dropped
        /// their guards run and the table is empty again.
        pub(crate) fn outstanding(&self) -> Vec<String> {
            let mut sites: Vec<String> = self
                .lock()
                .values()
                .map(|location| location.to_string())
                .collect();
            sites.sort_unstable();
            sites
        }

        /// A poisoned lock here means some other thread panicked mid-insert. The map is
        /// still structurally fine and this is diagnostics, not correctness — taking the
        /// inner value beats poisoning shutdown itself.
        fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, &'static Location<'static>>> {
            self.live.lock().unwrap_or_else(|err| err.into_inner())
        }
    }

    /// Held by the spawned task for exactly as long as the task is alive.
    #[derive(Debug)]
    pub(crate) struct CallSiteGuard {
        sites: CallSites,
        id: u64,
    }

    impl Drop for CallSiteGuard {
        fn drop(&mut self) {
            self.sites.lock().remove(&self.id);
        }
    }
}

#[cfg(not(debug_assertions))]
mod release {
    use std::panic::Location;

    #[derive(Clone, Debug, Default)]
    pub(crate) struct CallSites;

    impl CallSites {
        pub(crate) fn record(&self, _location: &'static Location<'static>) -> CallSiteGuard {
            CallSiteGuard
        }

        pub(crate) fn outstanding(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[derive(Debug)]
    pub(crate) struct CallSiteGuard;
}

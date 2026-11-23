//! The shared lazy load worker pool and the per-open transient memory tracker.
//!
//! One process-wide pool of persistent worker slots serves every open's load
//! phase. A load scope ([`with_load_scope`]) fixes how many slots (`k`) that
//! open may use and which [`OpenTransientTracker`] its transient allocations
//! are charged to; [`run_load_tasks`] then fans tasks out onto slots `0..k`
//! only, so slots are spawned on demand and reused across opens. Opens that
//! share a slot never share a tracker. On wasm32 there are no threads and
//! [`run_load_tasks`] runs its tasks sequentially.

use std::cell::RefCell;
use std::env::VarError;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{Error, Result};

/// The environment variable that sets the load thread count when the
/// `load_threads` option is absent.
const LOAD_THREADS_ENV: &str = "RELATE_LOAD_THREADS";

/// How many load worker slots one open may use (always at least one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadThreads(NonZeroUsize);

impl LoadThreads {
    /// The thread count as a plain number (>= 1).
    pub fn get(self) -> usize {
        self.0.get()
    }
}

/// The one environment-value triage the variable-backed resolvers share:
/// unset, blank, and whitespace are `None`; a value that is not valid UTF-8
/// fails naming the variable `name` — an invalid value is never silently
/// ignored — the preview-feature contract. Pure in the raw `std::env::var`
/// result, so tests never touch the process environment.
pub fn present_env_value(
    name: &str,
    raw: std::result::Result<String, VarError>,
) -> Result<Option<String>> {
    match raw {
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(Error::semantic(format!(
            "invalid {name}: value is not valid UTF-8"
        ))),
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
    }
}

/// Resolve the load thread count: an explicit `load_threads` wins, then
/// `RELATE_LOAD_THREADS`, then the machine's available parallelism. Zero,
/// negative, non-integer and non-UTF-8 values are rejected; an unset or
/// blank environment variable counts as absent.
pub fn resolve_load_threads(load_threads: Option<usize>) -> Result<LoadThreads> {
    resolve(load_threads, std::env::var(LOAD_THREADS_ENV))
}

fn resolve(
    load_threads: Option<usize>,
    env: std::result::Result<String, VarError>,
) -> Result<LoadThreads> {
    let requested = match load_threads {
        Some(n) => NonZeroUsize::new(n)
            .ok_or_else(|| Error::semantic("invalid load_threads: must be positive"))?,
        None => match present_env_value(LOAD_THREADS_ENV, env)? {
            Some(text) => text.trim().parse::<NonZeroUsize>().map_err(|_| {
                Error::semantic(format!(
                    "invalid {LOAD_THREADS_ENV} '{}': must be a positive integer",
                    text.trim()
                ))
            })?,
            None => available(),
        },
    };
    Ok(LoadThreads(requested))
}

fn available() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

/// Peak transient memory of one open: bytes currently leased and the
/// high-water mark of that sum.
#[derive(Debug, Default)]
pub struct OpenTransientTracker {
    current: AtomicU64,
    peak: AtomicU64,
}

impl OpenTransientTracker {
    /// A tracker with nothing leased.
    pub fn new() -> Self {
        Self::default()
    }

    /// The most bytes that were ever leased at once.
    pub fn peak_bytes(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    fn lease(self: &Arc<Self>, bytes: u64) -> TransientLease {
        let now = self.current.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.peak.fetch_max(now, Ordering::Relaxed);
        TransientLease {
            tracker: Arc::clone(self),
            bytes,
        }
    }
}

/// Bytes charged to an open's tracker until this guard drops.
#[derive(Debug)]
#[must_use = "the lease is released as soon as it is dropped"]
pub struct TransientLease {
    tracker: Arc<OpenTransientTracker>,
    bytes: u64,
}

impl Drop for TransientLease {
    fn drop(&mut self) {
        self.tracker
            .current
            .fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Charge `bytes` of transient memory to the tracker of the load scope the
/// calling thread is in (load workers inherit their caller's scope).
/// Outside a load scope there is no tracker and no lease.
pub fn record_transient_lease(bytes: u64) -> Option<TransientLease> {
    let tracker = SCOPE.with(|scope| scope.borrow().as_ref().map(|s| Arc::clone(&s.tracker)))?;
    Some(tracker.lease(bytes))
}

#[derive(Clone)]
struct Scope {
    threads: LoadThreads,
    tracker: Arc<OpenTransientTracker>,
}

thread_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// Restores the previous scope of the thread on drop.
struct ScopeGuard(Option<Scope>);

fn enter(scope: Scope) -> ScopeGuard {
    ScopeGuard(SCOPE.with(|cell| cell.borrow_mut().replace(scope)))
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        SCOPE.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}

/// Run `f` as one open's load phase: tasks it fans out through
/// [`run_load_tasks`] use at most `threads` pool slots and charge their
/// transient leases to `tracker`.
pub fn with_load_scope<R>(
    threads: LoadThreads,
    tracker: Arc<OpenTransientTracker>,
    f: impl FnOnce() -> R,
) -> R {
    let _scope = enter(Scope { threads, tracker });
    f()
}

/// [`with_load_scope`] of `threads` load threads, for tests that name a
/// count instead of resolving one.
#[doc(hidden)]
pub fn with_test_load_scope<R>(
    threads: usize,
    tracker: &Arc<OpenTransientTracker>,
    f: impl FnOnce() -> R,
) -> R {
    let threads = resolve_load_threads(Some(threads)).expect("a positive thread count");
    with_load_scope(threads, Arc::clone(tracker), f)
}

/// The load thread count of the scope the calling thread is in; one outside
/// any scope.
pub fn current_load_threads() -> LoadThreads {
    SCOPE
        .with(|scope| scope.borrow().as_ref().map(|s| s.threads))
        .unwrap_or(LoadThreads(NonZeroUsize::MIN))
}

/// Run `tasks` and return their results in order. Inside a load scope with
/// more than one thread the tasks run on the pool's slots (task `i` on slot
/// `i % min(k, tasks)`); otherwise, and for calls made from a load worker,
/// they run inline on the calling thread. The tasks may borrow from the
/// caller's stack: this returns only once every task has finished. A task's
/// panic is resumed on the caller after the rest have finished.
pub fn run_load_tasks<T: Send, F: FnOnce() -> T + Send>(tasks: Vec<F>) -> Vec<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let scope = SCOPE.with(|scope| scope.borrow().clone());
        if let Some(scope) = scope.filter(|s| s.threads.get() > 1 && tasks.len() > 1) {
            if !native::on_worker() {
                return native::POOL.run(&scope, tasks);
            }
        }
    }
    tasks.into_iter().map(|task| task()).collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::cell::Cell;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::sync::{mpsc, Condvar};
    use std::thread;

    use super::*;

    type Job = Box<dyn FnOnce() + Send + 'static>;

    thread_local! {
        static ON_WORKER: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) static POOL: LoadPool = LoadPool {
        slots: Mutex::new(Vec::new()),
    };

    pub(super) fn on_worker() -> bool {
        ON_WORKER.get()
    }

    /// Persistent worker threads, one per slot, spawned on first use.
    pub(super) struct LoadPool {
        pub(super) slots: Mutex<Vec<mpsc::Sender<Job>>>,
    }

    /// The results of one `run`, and how many of its jobs are unfinished.
    struct Batch<T> {
        state: Mutex<BatchState<T>>,
        done: Condvar,
    }

    struct BatchState<T> {
        pending: usize,
        results: Vec<Option<thread::Result<T>>>,
    }

    impl<T> Batch<T> {
        fn finish(&self, index: usize, result: thread::Result<T>) {
            let mut state = lock(&self.state);
            state.results[index] = Some(result);
            state.pending -= 1;
            if state.pending == 0 {
                self.done.notify_all();
            }
        }
    }

    /// Blocks on drop until the batch has no unfinished job, including while
    /// `run` unwinds, so no job outlives the borrows its task captured.
    struct WaitOnDrop<'a, T>(&'a Batch<T>);

    impl<T> Drop for WaitOnDrop<'_, T> {
        fn drop(&mut self) {
            let mut state = lock(&self.0.state);
            while state.pending > 0 {
                state = self
                    .0
                    .done
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    impl LoadPool {
        fn sender(&self, slot: usize) -> mpsc::Sender<Job> {
            let mut slots = lock(&self.slots);
            while slots.len() <= slot {
                let (tx, rx) = mpsc::channel::<Job>();
                thread::Builder::new()
                    .name(format!("relate-load-{}", slots.len()))
                    .spawn(move || {
                        ON_WORKER.set(true);
                        rx.into_iter().for_each(|job| job());
                    })
                    .expect("spawn load worker");
                slots.push(tx);
            }
            slots[slot].clone()
        }

        pub(super) fn run<'a, T: Send + 'a, F: FnOnce() -> T + Send + 'a>(
            &self,
            scope: &Scope,
            tasks: Vec<F>,
        ) -> Vec<T> {
            let lanes = scope.threads.get().min(tasks.len());
            let batch = Arc::new(Batch {
                state: Mutex::new(BatchState {
                    pending: 0,
                    results: tasks.iter().map(|_| None).collect(),
                }),
                done: Condvar::new(),
            });
            let wait = WaitOnDrop(&batch);
            for (index, task) in tasks.into_iter().enumerate() {
                let sender = self.sender(index % lanes);
                let (done, scope) = (Arc::clone(&batch), scope.clone());
                let job: Box<dyn FnOnce() + Send + 'a> = Box::new(move || {
                    let _scope = enter(scope);
                    let result = catch_unwind(AssertUnwindSafe(task));
                    done.finish(index, result);
                });
                // SAFETY: `wait` blocks until every sent job has called
                // `finish`, which happens after the task (and everything it
                // borrowed) is consumed, so no job outlives `'a`.
                let job: Job = unsafe { std::mem::transmute(job) };
                lock(&batch.state).pending += 1;
                if sender.send(job).is_err() {
                    lock(&batch.state).pending -= 1;
                    panic!("load worker pool is unavailable");
                }
            }
            drop(wait);
            let results = std::mem::take(&mut lock(&batch.state).results);
            results
                .into_iter()
                .map(|result| match result.expect("every load task finished") {
                    Ok(value) => value,
                    Err(panic) => resume_unwind(panic),
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;
    use std::thread::{self, ThreadId};

    use super::*;

    fn thread_ids(count: usize) -> Vec<(ThreadId, String)> {
        run_load_tasks(
            (0..count)
                .map(|_| {
                    || {
                        (
                            thread::current().id(),
                            thread::current().name().unwrap().to_owned(),
                        )
                    }
                })
                .collect(),
        )
    }

    fn unset() -> std::result::Result<String, VarError> {
        Err(VarError::NotPresent)
    }

    #[test]
    fn load_pool_resolve_rejects_bad_values() {
        let semantic = |r: Result<LoadThreads>| match r.unwrap_err() {
            Error::Semantic(msg) => msg,
            other => panic!("not semantic: {other:?}"),
        };
        assert!(semantic(resolve(Some(0), unset())).contains("load_threads"));
        let mut bad = vec![OsString::from("0"), "-1".into(), "two".into(), "1.5".into()];
        #[cfg(unix)]
        bad.push(std::os::unix::ffi::OsStringExt::from_vec(vec![0x66, 0xff]));
        for raw in bad {
            let label = format!("{raw:?}");
            let raw = raw.into_string().map_err(VarError::NotUnicode);
            let msg = semantic(resolve(None, raw));
            assert!(msg.contains("RELATE_LOAD_THREADS"), "{label}: {msg}");
        }
    }

    #[test]
    fn load_pool_resolve_accepts_values_and_defaults() {
        let parallelism = std::thread::available_parallelism().unwrap().get();
        let set = |text: &str| Ok(text.to_owned());
        assert_eq!(resolve(Some(3), unset()).unwrap().get(), 3);
        assert_eq!(resolve(Some(3), set("9")).unwrap().get(), 3);
        assert_eq!(resolve(None, set(" 4 ")).unwrap().get(), 4);
        assert_eq!(resolve(None, unset()).unwrap().get(), parallelism);
        assert_eq!(resolve(None, set("  ")).unwrap().get(), parallelism);
    }

    #[test]
    fn load_pool_slots_are_reused_and_capped_by_scope() {
        let seen: Mutex<HashSet<(ThreadId, String)>> = Mutex::default();
        thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let tracker = Arc::default();
                    for _ in 0..5 {
                        let ids = with_test_load_scope(2, &tracker, || thread_ids(6));
                        lock(&seen).extend(ids);
                    }
                });
            }
        });
        let seen = lock(&seen);
        assert!(seen.len() <= 2, "{seen:?}");
        assert!(seen
            .iter()
            .all(|(_, name)| name == "relate-load-0" || name == "relate-load-1"));
        assert!(lock(&native::POOL.slots).len() <= 2);
    }

    #[test]
    fn load_pool_borrows_stack_and_resumes_panics_after_all_finish() {
        let finished = AtomicUsize::new(0);
        let data = [1_u64, 2, 3, 4];
        let tracker = Arc::default();
        let sums = with_test_load_scope(2, &tracker, || {
            run_load_tasks(data.iter().map(|n| || *n * 10).collect())
        });
        assert_eq!(sums, [10, 20, 30, 40]);

        let outcome = std::panic::catch_unwind(|| {
            with_test_load_scope(2, &tracker, || {
                run_load_tasks(
                    (0..4)
                        .map(|i| {
                            let finished = &finished;
                            move || {
                                if i == 0 {
                                    panic!("boom");
                                }
                                thread::sleep(std::time::Duration::from_millis(30));
                                finished.fetch_add(1, Ordering::SeqCst);
                            }
                        })
                        .collect(),
                )
            })
        });
        assert_eq!(*outcome.unwrap_err().downcast::<&str>().unwrap(), "boom");
        assert_eq!(finished.load(Ordering::SeqCst), 3);
        // The pool survives a panicking task.
        assert_eq!(with_test_load_scope(2, &tracker, || thread_ids(2)).len(), 2);
    }

    #[test]
    fn load_pool_nested_calls_run_inline_on_the_worker() {
        let tracker = Arc::default();
        let nested = with_test_load_scope(2, &tracker, || {
            run_load_tasks(
                (0..2)
                    .map(|_| {
                        || {
                            let outer = thread::current().id();
                            let inner = run_load_tasks(vec![|| thread::current().id(); 3]);
                            (outer, inner, current_load_threads().get())
                        }
                    })
                    .collect(),
            )
        });
        for (outer, inner, k) in nested {
            assert_eq!(inner, [outer; 3]);
            assert_eq!(k, 2);
        }
        assert_eq!(current_load_threads().get(), 1);
    }

    #[test]
    fn load_pool_tracker_peaks_are_isolated_per_scope() {
        let trackers: Vec<Arc<OpenTransientTracker>> = (0..2).map(|_| Arc::default()).collect();
        thread::scope(|s| {
            for (tracker, bytes) in trackers.iter().zip([100_u64, 7]) {
                s.spawn(move || {
                    let barrier = Barrier::new(2);
                    with_test_load_scope(2, tracker, || {
                        run_load_tasks(
                            (0..2)
                                .map(|_| {
                                    let barrier = &barrier;
                                    move || {
                                        let _lease = record_transient_lease(bytes);
                                        barrier.wait();
                                    }
                                })
                                .collect(),
                        )
                    });
                });
            }
        });
        assert_eq!(trackers[0].peak_bytes(), 200);
        assert_eq!(trackers[1].peak_bytes(), 14);
        assert_eq!(trackers[0].current.load(Ordering::Relaxed), 0);
        assert!(record_transient_lease(5).is_none()); // outside a scope: no lease
    }
}

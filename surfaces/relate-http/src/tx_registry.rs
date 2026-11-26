//! The registry core of the Query API v2 `tx` routes (ADR-0237, ADR-0435):
//! a tx id maps to its engine [`Connection`] holding an open
//! `Connection::begin` transaction (ADR-0028). The engine's Connection is
//! `Send` but not `Sync`, so the registry lives behind a `Mutex` — but the
//! mutex is only ever held for registry bookkeeping, never across an
//! engine statement: the route family in [`crate::tx`] leases its entry out
//! of the map and runs the statement with the lock released. The lease has
//! two exits — [`Lease::release`] puts the entry back (a run),
//! [`Lease::take`] hands it to its one-way close (commit, rollback) (the
//! engine is single-writer anyway, so statements serialize at the commit
//! loop, not here).
//!
//! The lifecycle is ADR-0435's: every entry records the monotonic instant
//! it last did something, every registry operation reclaims entries idle
//! past the configured deadline in the one critical section it acts in —
//! the lease takes its target first and sweeps the rest (there is no
//! background reaper) — a swept [`Connection`] is dropped after the
//! registry lock is released — which rolls the transaction back and
//! releases its snapshot pin — and the registry refuses a begin at its cap
//! before the engine is asked for a pin. A begin's cap slot is a
//! reservation counted from `admit` until its entry registers, so the cap
//! holds under a burst of concurrent begins rather than only at a point in
//! time.
//!
//! This module is the registry core only; the route family — the request
//! body, the statement run, and the four handlers — lives in [`crate::tx`]
//! and reaches this module through [`TxRegistry`], the one public surface
//! (`lib.rs` re-exports it). The registry never names a route or a reply
//! type: a lookup miss is the typed [`LookupMiss`], and the route family
//! renders it.
//!
//! The limits are the typed [`TxLimits`] model (ADR-0435 §4): an idle
//! timeout of `None` disables expiry and a cap of `None` is unlimited.
//! [`TxLimits::new`] converts the wire's `0` spelling at that one boundary,
//! so no consumer here re-derives a zero sentinel.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use relate::Connection;

use crate::tx_limits::TxLimits;

/// One open transaction: the connection holding it and the client's
/// declared access mode (writes are rejected on `READ`). Crate-private: the
/// tx routes' surface is [`TxRegistry`] plus begin/run/commit/rollback, and
/// the route family in [`crate::tx`] builds the entry through
/// [`TxEntry::new`], reads `conn`/`mode`, and re-stamps activity through
/// [`TxEntry::touch`].
pub(crate) struct TxEntry {
    /// The connection holding the open transaction.
    pub(crate) conn: Connection,
    /// The declared access mode (default [`AccessMode::Write`]).
    pub(crate) mode: AccessMode,
    /// Caller that opened the transaction. `None` means the transaction
    /// is not scoped to a caller (ADR-0428).
    pub(crate) owner: Option<String>,
    /// The `maxExecutionTime` the opening request carried (ADR-0692): the
    /// request value each statement of the transaction resolves its
    /// budget from, each with its own clock. `None` is the server default.
    pub(crate) max_execution: Option<std::time::Duration>,
    /// The monotonic instant this transaction last did something
    /// (ADR-0435 §1). Private: only the registry family's [`TxEntry::new`]
    /// and [`TxEntry::touch`] write it, so no route can re-derive the
    /// stamp beside the one policy. Refreshed AFTER a statement completes —
    /// a statement failure included, the client is alive — never at entry:
    /// a statement longer than the idle timeout must not expire a client
    /// that is demonstrably working. The sweep compares it with the
    /// configured idle timeout; the wall clock only renders the deadline.
    last_activity: Instant,
}

impl TxEntry {
    /// A freshly opened entry: the activity stamp starts at the open, so a
    /// begin that runs no statement is already on the deadline. The begin
    /// route touches it again after its body's statement (ADR-0435 §1).
    pub(crate) fn new(
        conn: Connection,
        mode: AccessMode,
        owner: Option<String>,
        max_execution: Option<std::time::Duration>,
    ) -> Self {
        Self {
            conn,
            mode,
            owner,
            max_execution,
            last_activity: Instant::now(),
        }
    }

    /// Re-stamp the activity clock — the statement completed (failed or
    /// not, the client is alive) — and hand the new stamp to the caller,
    /// which renders it as the reply's `expires`. The one write of
    /// `last_activity` outside the registry's own sweep/aging paths.
    pub(crate) fn touch(&mut self) -> Instant {
        self.last_activity = Instant::now();
        self.last_activity
    }
}

/// The transaction's access mode, typed at the body boundary: `READ` and
/// `WRITE` are the only values, and anything else fails parsing with the
/// 400 envelope — a misspelled `accessMode` never silently opens a writable
/// transaction. It lives beside the entry that records it
/// ([`TxEntry::mode`]); the route family in [`crate::tx`] imports it for the
/// body field and the write guard.
#[derive(Debug, Default, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum AccessMode {
    /// Read-only: write and schema statements are rejected.
    Read,
    /// Read-write (the default when `accessMode` is absent). An explicit
    /// `null` fails parsing — the wire values are the two variants.
    #[default]
    Write,
}

/// Why a registry lookup found nothing (ADR-0435 §3).
pub(crate) enum LookupMiss {
    /// This operation took the id and found it idle past the deadline — the
    /// one observer that answers the typed expiry. Carries the configured
    /// timeout the entry exceeded, so the reply names it without reading
    /// the limits again.
    Expired(Duration),
    /// Unknown id, another caller's transaction (ADR-0428), or an entry an
    /// earlier operation's sweep already reclaimed: the generic not-found.
    Missing,
}

/// The registry of open transactions for one server.
pub struct TxRegistry {
    next: AtomicU64,
    txs: Mutex<TxMap>,
    /// The managed-transaction limits (ADR-0435 §4): the idle deadline and
    /// the cap on open entries. Carried here, not in `HttpConfig`, so the
    /// registry that enforces them owns the one copy; the lifecycle
    /// enforcement reads it on every registry operation.
    limits: TxLimits,
}

/// The registry's one mutable state, under its one mutex: the open
/// transactions plus every reservation that is holding (or about to hold)
/// a snapshot pin without being in `entries`. The count sits beside the
/// map it belongs to so the cap reads both under the same lock: a leased
/// entry keeps counting against the cap — its `Connection` still holds its
/// pin exactly like a registered one — and so does a begin admitted but
/// not yet registered, which is the window the begin route's engine work
/// runs in. A burst of long statements or concurrent begins therefore
/// cannot raise a registry's pin footprint above the cap (ADR-0435 §2).
struct TxMap {
    entries: HashMap<String, TxEntry>,
    /// Entries leased out of the map (`Lease`: a run's statement or a
    /// commit/rollback's final statement — each holds its pin for the whole
    /// span) plus begins admitted but not yet registered (`Admission`).
    /// Each is returned to the count by its guard's completion path or its
    /// drop.
    in_flight: usize,
}

impl TxRegistry {
    /// An empty registry with the ADR-0435 defaults (60 s idle, 128 open).
    pub fn new() -> Self {
        Self::with_limits(TxLimits::default())
    }

    /// An empty registry with the given managed-transaction limits
    /// (ADR-0435 §4) — product `serve` constructs it from the values it
    /// resolved from `--tx-idle-timeout-secs` / `--max-open-transactions`
    /// (or their environment variables).
    pub fn with_limits(limits: TxLimits) -> Self {
        Self {
            next: AtomicU64::new(0),
            txs: Mutex::new(TxMap {
                entries: HashMap::new(),
                in_flight: 0,
            }),
            limits,
        }
    }

    /// The configured managed-transaction limits. Crate-private: the
    /// route family in [`crate::tx`] reads them to render `expires` and the
    /// over-cap refusal; an embedder supplies limits through
    /// [`TxRegistry::with_limits`] and has no reason to read them back.
    pub(crate) fn limits(&self) -> TxLimits {
        self.limits
    }

    /// The registry's one lock-recovery policy: a poisoned mutex is
    /// recovered (the map is still valid after a panic elsewhere) — spelled
    /// once, called by every op.
    fn lock(&self) -> std::sync::MutexGuard<'_, TxMap> {
        self.txs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The registry's one lock-and-drop envelope: hold the mutex for `f`,
    /// hand it the swept entries it reclaims, and drop that list only after
    /// the lock is released — dropping a [`Connection`] joins its worker,
    /// rolls the transaction back and releases its snapshot pin, and that
    /// work must not lengthen the critical section every other registry
    /// operation waits on. The closure owns the order of its own work
    /// against the sweep: [`TxRegistry::take_leased`] takes its target
    /// first so an expired target can still answer the typed expiry, while
    /// [`TxRegistry::with_map`] sweeps first.
    fn with_swept<R>(&self, f: impl FnOnce(&mut TxMap, &mut Vec<TxEntry>) -> R) -> R {
        let (result, swept) = {
            let mut map = self.lock();
            let mut swept = Vec::new();
            let result = f(&mut map, &mut swept);
            (result, swept)
        };
        // Dropped after the lock, per the registry's one drop policy.
        drop(swept);
        result
    }

    /// Run `f` on the registry map under the mutex, after the lazy sweep
    /// (ADR-0435 §1: a registry operation reclaims expired entries in its
    /// own critical section). The lease's acquire does not use this helper —
    /// it takes its target before sweeping — so no presence probe ever sits
    /// between the two. The lock-and-drop rule is [`TxRegistry::with_swept`]'s.
    fn with_map<R>(&self, f: impl FnOnce(&mut TxMap) -> R) -> R {
        self.with_swept(|map, swept| {
            *swept = self.sweep(map);
            f(map)
        })
    }

    /// The timeout `entry` exceeded when it is idle past the deadline
    /// (ADR-0435 §1) — the one expiry predicate, consulted by the sweep and
    /// by the lease's classification of the entry it took in hand. The
    /// exceeded timeout IS the answer: the sweep reads `is_some`, and the
    /// lease hands the `Duration` to the reply, so neither re-reads the
    /// limits. The disabled fact is the typed limits' own `None` arm: no
    /// second zero sentinel is re-derived beside the predicate.
    fn expired_after(&self, entry: &TxEntry) -> Option<Duration> {
        let idle_timeout = self.limits.idle_timeout()?;
        (entry.last_activity.elapsed() >= idle_timeout).then_some(idle_timeout)
    }

    /// Remove every entry idle past the deadline and hand its entry back.
    /// The result is the pin-holding part only: a caller that must answer a
    /// typed expiry decides it from the entry it took in hand, never from a
    /// presence probe around the sweep (see [`TxRegistry::take_leased`]).
    /// Expiry is disabled when the limits carry no idle timeout; the
    /// predicate's `None` arm already answers `None`, so the any-probe
    /// below is the whole story and nothing is drained. A leased entry is
    /// not in the map, so a statement in flight can never be swept out from
    /// under itself.
    fn sweep(&self, map: &mut TxMap) -> Vec<TxEntry> {
        if !map
            .entries
            .values()
            .any(|entry| self.expired_after(entry).is_some())
        {
            return Vec::new();
        }
        // The map is drained and refilled rather than scanned for ids to
        // clone: every entry this sweep keeps is moved back whole, and no
        // id String is duplicated just to find it again.
        let mut swept = Vec::new();
        for (id, entry) in std::mem::take(&mut map.entries) {
            if self.expired_after(&entry).is_some() {
                swept.push(entry);
            } else {
                map.entries.insert(id, entry);
            }
        }
        swept
    }

    /// The configured cap when the registry already holds its worth of open
    /// transactions (ADR-0435 §2) — registered entries and in-flight
    /// reservations alike, since each holds or is about to hold a snapshot
    /// pin. A reservation is any leased statement (a run, or a commit or
    /// rollback running its final statement) or an admitted,
    /// not-yet-registered begin. `None` is both the typed model's unlimited
    /// spelling and the under-cap answer; the refusal carries the cap it
    /// reached, so the begin route renders it without reading the limits.
    fn reached_cap(&self, map: &TxMap) -> Option<usize> {
        let max_open = self.limits.max_open()?;
        (map.entries.len() + map.in_flight >= max_open).then_some(max_open)
    }

    /// Sweep expired entries, then reserve one of the cap's slots for a
    /// begin and hand the reservation back as an [`Admission`]. Consulted
    /// BEFORE the connection is opened and the engine pin is taken
    /// (ADR-0435 §2), so an over-cap begin consumes no pin, and an entry
    /// the sweep reclaimed frees its slot for it. The reservation is
    /// counted under the same lock that reads the cap, so it holds its
    /// slot from this point until [`Admission::register`] converts it into
    /// a registered entry or the guard drops: concurrent begins cannot
    /// race past the cap. The `Err` carries the configured cap the refusal
    /// reached, so the route names it without reading the limits.
    pub(crate) fn admit(&self) -> Result<Admission<'_>, usize> {
        self.with_map(|map| {
            if let Some(max_open) = self.reached_cap(map) {
                return Err(max_open);
            }
            map.in_flight += 1;
            Ok(())
        })
        .map(|()| Admission {
            registry: Some(self),
        })
    }

    /// Take the transaction `id` out of the map and count it in flight, in
    /// the one critical section that also sweeps — the lease every
    /// statement route (run, commit, rollback) starts with. The miss names
    /// the reason when the id is unknown, already swept, or `owner` is not
    /// the recorded caller; an expired target yields the timeout it
    /// exceeded.
    fn take_leased(&self, id: &str, owner: Option<&str>) -> Result<TxEntry, LookupMiss> {
        self.with_swept(|map, swept| {
            // The target leaves the map first, then the rest is swept — one
            // critical section, no probe around the sweep. The classification
            // is made on the entry in hand (ADR-0435 §3): an expired target
            // IS this operation's own expiry, a live one leases out, and no
            // entry for this owner is the generic not-found. The expired
            // target joins the sweep's drop list, so its `Connection` is
            // reclaimed and dropped with the others, outside the lock.
            let taken = take_entry(map, id, owner);
            swept.extend(self.sweep(map));
            match taken {
                Some(entry) => match self.expired_after(&entry) {
                    Some(idle_timeout) => {
                        swept.push(entry);
                        Err(LookupMiss::Expired(idle_timeout))
                    }
                    None => {
                        // Counted under the very lock that removed it, so the
                        // cap never observes a gap between "in the map" and
                        // "in flight".
                        map.in_flight += 1;
                        Ok(entry)
                    }
                },
                None => Err(LookupMiss::Missing),
            }
        })
    }

    /// Lease the transaction `id` out of the registry for one engine
    /// statement, so no statement ever runs under the registry mutex: a
    /// slow one in one transaction cannot stall another
    /// transaction's begin, run, commit, or rollback. Every statement
    /// route leases — the run, and the optional final statement of a
    /// commit or rollback — so a transaction holding its snapshot pin while
    /// it is out of the map keeps counting against the cap (see [`TxMap`]).
    /// [`Lease::release`] puts the entry back (run), [`Lease::take`] hands
    /// it to its one-way close (commit, rollback), and a statement that
    /// unwinds drops it — the `Connection` drop rolls the transaction back
    /// and releases its pin — without leaking the cap slot.
    ///
    /// A leased entry is invisible to the map: any operation naming that
    /// id while the statement runs — a commit or rollback from its own
    /// owner included — finds no entry and answers the generic not-found
    /// (`LookupMiss::Missing`). That is the deliberate answer: the
    /// in-flight statement owns the connection, so the transaction cannot
    /// be closed under it; after the statement a run's entry is back and
    /// usable (the racing client retries against the still-open
    /// transaction), and a commit's or rollback's has left for good. The
    /// lease also makes an in-flight statement immune to
    /// expiry by sweep — the client is demonstrably active, which is
    /// ADR-0435 §1's "refresh after the statement" rule taken literally.
    pub(crate) fn lease(&self, id: &str, owner: Option<&str>) -> Result<Lease<'_>, LookupMiss> {
        let entry = self.take_leased(id, owner)?;
        Ok(Lease {
            registry: self,
            id: id.to_string(),
            entry: Some(entry),
        })
    }

    /// Close a run's lease: return its entry to the map and give the
    /// in-flight slot back, both in one critical section so the cap reads
    /// them together.
    fn return_lease(&self, id: &str, entry: TxEntry) {
        self.with_map(|map| {
            let previous = map.entries.insert(id.to_string(), entry);
            debug_assert!(
                previous.is_none(),
                "a leased transaction id is never re-registered"
            );
            map.in_flight -= 1;
        });
    }

    /// Close a lease without returning an entry — the commit/rollback
    /// one-way close and the unwind path, whose `Connection` was dropped
    /// (which is the rollback). Only the in-flight slot goes back.
    fn release_slot(&self) {
        self.with_map(|map| map.in_flight -= 1);
    }

    /// Test-only: the registry's occupancy under one lock — (registered
    /// entries, in-flight reservations). The cap-burst test reads it to
    /// prove a whole burst is admitted but unregistered before it reads
    /// the cap.
    #[cfg(test)]
    pub(crate) fn occupancy(&self) -> (usize, usize) {
        let map = self.lock();
        (map.entries.len(), map.in_flight)
    }

    /// Test-only: whether the map currently holds `id`. A leased entry is
    /// out of the map while its statement runs, so this is the
    /// observation a concurrency test handshakes on — under the
    /// pre-lease shape the entry stayed in the map (and this call blocked
    /// on the mutex) for the statement's whole duration.
    #[cfg(test)]
    pub(crate) fn holds(&self, id: &str) -> bool {
        self.lock().entries.contains_key(id)
    }

    /// Test-only: back-date every entry past the idle deadline, so a sweep
    /// test exercises the deadline without sleeping the configured
    /// timeout. A registry with expiry disabled has no deadline to
    /// back-date against, so the test-only call is a no-op there.
    #[cfg(test)]
    pub(crate) fn age_past_deadline(&self) {
        let Some(idle_timeout) = self.limits.idle_timeout() else {
            return;
        };
        let now = Instant::now();
        // Back-date by the deadline plus a margin, so the entry is expired
        // even with the time a later sweep spends reading the clock again.
        // `Instant` has no guaranteed origin: a process younger than the
        // margin cannot subtract it, so fall back to the deadline itself
        // (expiry is `elapsed() >= timeout`, and elapsed keeps growing).
        // When even that is not expressible the test's own setup is what
        // failed — the clock's uptime is shorter than the configured
        // timeout — so the panic names that assumption.
        let aged = now
            .checked_sub(idle_timeout + std::time::Duration::from_secs(1))
            .or_else(|| now.checked_sub(idle_timeout))
            .expect(
                "the test clock's uptime is shorter than the configured idle timeout: \
                 pick a smaller fixture timeout",
            );
        let mut map = self.lock();
        for entry in map.entries.values_mut() {
            entry.last_activity = aged;
        }
    }
}

/// The registry's one entry hand-off: removal of `id` for `owner`. The
/// lease every statement route takes is its only caller, so the lookup
/// policy and the owner scoping are spelled once; the sweep-expiry answer
/// is decided by [`TxRegistry::take_leased`] on the taken entry itself.
fn take_entry(map: &mut TxMap, id: &str, owner: Option<&str>) -> Option<TxEntry> {
    match map.entries.get(id) {
        Some(entry) if owner_matches(entry.owner.as_deref(), owner) => map.entries.remove(id),
        _ => None,
    }
}

/// One transaction leased out of the registry for a running statement.
/// The guard is the lease's whole accounting: dropping it without a
/// terminal operation means the statement unwound, so the entry
/// drops (its `Connection` rolls the transaction back and releases the
/// pin) and the cap slot goes back — a panic leaks neither. The two
/// terminal operations are the two exits a statement route has:
/// [`Lease::release`] keeps the transaction open (run), [`Lease::take`]
/// hands the entry to its one-way close (commit, rollback).
pub(crate) struct Lease<'a> {
    registry: &'a TxRegistry,
    id: String,
    /// `Some` until the lease is consumed by a terminal operation; a drop
    /// without one is the unwind path.
    entry: Option<TxEntry>,
}

impl Lease<'_> {
    /// The leased connection, for the statement about to run.
    pub(crate) fn entry(&mut self) -> &mut TxEntry {
        self.entry
            .as_mut()
            .expect("a live lease holds its transaction")
    }

    /// Refresh the transaction's activity stamp — the statement completed,
    /// failed or not, so the client is alive (ADR-0435 §1) — return the
    /// entry to the registry, and close the lease. The stamp is what the
    /// reply's `transaction` block renders. This is the run exit: the
    /// transaction stays open.
    pub(crate) fn release(mut self) -> Instant {
        let mut entry = self
            .entry
            .take()
            .expect("a live lease holds its transaction");
        let last_activity = entry.touch();
        self.registry.return_lease(&self.id, entry);
        last_activity
    }

    /// Consume the lease and hand the entry to its one-way close — the
    /// commit/rollback exit. The entry leaves the registry with it (no
    /// re-registration, no activity stamp: there is no deadline left to
    /// honor) and the cap slot goes back through the same in-flight
    /// decrement the drop path uses, so the pin the entry held while its
    /// final statement ran was counted for that whole span.
    pub(crate) fn take(mut self) -> TxEntry {
        let entry = self
            .entry
            .take()
            .expect("a live lease holds its transaction");
        self.registry.release_slot();
        entry
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(entry) = self.entry.take() {
            // The statement unwound: give the slot back first, then drop
            // the entry outside the lock — the `Connection` drop is the
            // rollback and releases the pin, and it must not lengthen the
            // critical section (the registry's one drop policy).
            self.registry.release_slot();
            drop(entry);
        }
    }
}

/// One begin's cap reservation (ADR-0435 §2): the slot counted by
/// [`TxRegistry::admit`] before the connection is opened, converted into a
/// registered entry by [`Admission::register`]. Dropped without it — any
/// `?` exit or a panic between admit and register — the guard gives the
/// slot back, so an abandoned begin leaks neither capacity nor a pin.
pub(crate) struct Admission<'a> {
    /// `Some` until [`Admission::register`] consumes the reservation; the
    /// drop is the reservation's only other exit, and a consumed admission
    /// (`None`) decrements nothing twice.
    registry: Option<&'a TxRegistry>,
}

impl Admission<'_> {
    /// Convert the reservation into a registered entry in one critical
    /// section: the entry joins the map and the in-flight count gives its
    /// slot back under the same lock, so the cap never sees both. Ids are
    /// process-local and never reused, and a statement in a begin body has
    /// already run, so a guessed id can never access a half-open
    /// transaction.
    pub(crate) fn register(mut self, entry: TxEntry) -> String {
        let registry = self.registry.take().expect("an admission is consumed once");
        let id = format!("tx-{}", registry.next.fetch_add(1, Ordering::Relaxed));
        registry.with_map(|map| {
            map.entries.insert(id.clone(), entry);
            map.in_flight -= 1;
        });
        id
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.with_map(|map| map.in_flight -= 1);
        }
    }
}

/// A recorded owner admits only that caller. An unscoped transaction
/// admits every caller.
fn owner_matches(recorded: Option<&str>, presented: Option<&str>) -> bool {
    recorded.is_none_or(|recorded| presented == Some(recorded))
}

impl Default for TxRegistry {
    fn default() -> Self {
        Self::new()
    }
}

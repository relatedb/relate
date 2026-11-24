//! The hashed-value memo over `datetime(<string>)` (ADR-0640, memo tier 3's
//! first consumer). `date(<string>)` is a measured no-go and stays unwired:
//! its parse costs about what the key hash does (64 ms before and after on
//! the pokec `memo_date_row` group), so a probe could not pay. `labels(n)` is a measured no-go too: building the
//! list costs ~41 ns, the key hash of even one label ~57 ns.
//!
//! **Purity contract.** The one-string-argument form is a parse of the text
//! alone: no row, no snapshot, no clock (the zero-argument and map forms
//! sample the clock and never reach this memo). So the answer is a function
//! of the argument text, and the key is the text's content hash. An
//! erroring parse is never remembered ([`Memo::answer`]).
//!
//! The break-even policy is microsecond-class ([`policy`]): a parse costs
//! hundreds of nanoseconds, the hash of a short string a fraction of that,
//! so the memo warms after a handful of inputs and reviews early.
//! A literal argument never gets here (the front end folds it at lowering);
//! the memo serves a per-row or parameter-fed string. Bytes are charged to
//! the ADR-0463 retained meter and the query's shared memo budget.

use std::cell::RefCell;

use relate_core::temporal::{self, ClockKind, Temporal};
use relate_core::{Result, Value};
use relate_model::wire::MemoKindWire;

use super::memo::{Memo, MemoMeter, MemoPolicy};
use super::value_hash::{hash_value, HashedValues, RetainedHeap};
use crate::metrics::StatementMemoRecord;
use crate::overlay::MemoLive;

type Table = Memo<HashedValues<Value>>;

/// Warm after 8 inputs, review the hit rate after 65 536 probes, keep the
/// table under half the shared memo budget (ADR-0657).
fn policy(live: &MemoLive) -> MemoPolicy {
    MemoPolicy {
        warm_up: 8,
        window: 1 << 16,
        min_hit_percent: 2,
        max_bytes: live.table_ceiling(),
    }
}

/// The temporal constructors' memos, behind their own interior mutability
/// so the read-only expression evaluator answers through a shared `&self`.
///
/// **Scope.** One value lives in an `ExecGraph`, and a view is built per
/// read or write call (`ExecGraph::read_at` / `write`), never reused across
/// statements. The answer is also a function of the argument text alone (no
/// snapshot, no overlay, no clock, no session zone: a zoneless `datetime`
/// reads in the fixed UTC default), so a write that changes visible data
/// cannot change a remembered answer.
///
/// **Decision.** [`Self::construct`] is the one home of the datetime-only
/// rule: it decides off the canonical constructor kind
/// ([`temporal::clock_kind`], so `zoned_datetime` shares the memo) and the
/// one-string-argument shape, and any other call costs no hash and no
/// probe.
///
/// **Retained-bytes invariant.** A parsed zoned datetime owns its zone id
/// (`Temporal::DateTime::zone`, a `String`; even `Z` is one), so the table
/// charges each stored answer's [`RetainedHeap`] beside the map's own
/// allocation: the byte ceiling, the ADR-0463 meter and the query's memo
/// budget see the whole retained size. A one-string-argument
/// `datetime` answers a temporal or an error (errors are never stored), so
/// the table holds temporals only and [`RetainedHeap`] charges nothing for
/// any other shape.
pub(crate) struct ConstructMemos {
    datetime: RefCell<Table>,
}

/// Only temporals reach the table, so the non-temporal arm is never charged.
impl RetainedHeap for Value {
    fn retained_heap(&self) -> usize {
        match self {
            Value::Temporal(Temporal::DateTime { zone, .. }) => {
                zone.as_ref().map_or(0, String::capacity)
            }
            _ => 0,
        }
    }
}

impl ConstructMemos {
    pub(crate) fn new(live: MemoLive) -> Self {
        Self::with_policy(policy(&live), live)
    }

    fn with_policy(policy: MemoPolicy, live: MemoLive) -> Self {
        Self {
            datetime: RefCell::new(Table::new(policy, live)),
        }
    }

    /// What the constructor memos did over the statement (ADR-0654 §3): the
    /// counters summed over the memos that were probed, and their kinds.
    /// Read once, at the statement's end behind PROFILE's sink.
    pub(crate) fn statement_record(&self) -> StatementMemoRecord {
        let datetime = self.datetime.borrow().stats();
        let mut record = StatementMemoRecord::default();
        if datetime.probes > 0 {
            record.counters += datetime.counters();
            record.kinds = record.kinds.with(MemoKindWire::Datetime);
        }
        record
    }

    /// `name(vals…)`: the shared construction (`temporal::construct_named`),
    /// memoised when it is a one-string-argument `datetime`.
    pub(crate) fn construct(
        &self,
        meter: &impl MemoMeter,
        name: &str,
        vals: &[Value],
    ) -> Result<Value> {
        let compute = || temporal::construct_named(name, vals);
        match (vals, temporal::clock_kind(name)) {
            ([arg @ Value::String(_)], Some(ClockKind::DateTime)) => self
                .datetime
                .borrow_mut()
                .answer_lazy(meter, || hash_value(arg), compute),
            _ => compute(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::exec::kernels::value_hash::HASH_CALLS;

    /// Counts what the memo charges to the retained meter.
    #[derive(Default)]
    struct Charged(Cell<u64>);

    impl MemoMeter for Charged {
        fn charge(&self, total: &mut u64, added: u64) -> Result<()> {
            *total += added;
            self.0.set(self.0.get() + added);
            Ok(())
        }
    }

    fn text(s: &str) -> Value {
        Value::String(s.into())
    }

    fn call(memos: &ConstructMemos, meter: &Charged, name: &str, s: &Value) -> Result<Value> {
        memos.construct(meter, name, std::slice::from_ref(s))
    }

    fn hashes() -> u64 {
        HASH_CALLS.with(Cell::get)
    }

    /// `rows` calls over two distinct strings: (table bytes, bytes charged).
    fn run(rows: usize) -> (u64, u64) {
        let memos = ConstructMemos::new(MemoLive::default());
        let meter = Charged::default();
        let (a, b) = (text("2024-05-05T10:00:00Z"), text("2023-01-01T09:00:00Z"));
        for i in 0..rows {
            let arg = if i.is_multiple_of(2) { &a } else { &b };
            let got = call(&memos, &meter, "datetime", arg).unwrap();
            assert_eq!(
                got,
                temporal::construct_named("datetime", std::slice::from_ref(arg)).unwrap()
            );
        }
        let bytes = memos.datetime.borrow().stats().bytes;
        (bytes, meter.0.get())
    }

    #[test]
    fn retained_bytes_follow_distinct_inputs_not_rows_and_are_charged() {
        let (small, small_charged) = run(100);
        let (large, large_charged) = run(100_000);
        assert!(small > 0, "the warm memo allocated a table");
        assert_eq!(small, large, "table bytes independent of row count");
        assert_eq!(small_charged, large_charged);
        assert_eq!(large_charged, large, "every table byte is charged");
    }

    #[test]
    fn errors_are_never_cached_and_other_kinds_are_unwired() {
        let memos = ConstructMemos::new(MemoLive::default());
        let meter = Charged::default();
        let bad = text("not a date");
        for _ in 0..50 {
            assert!(call(&memos, &meter, "datetime", &bad).is_err());
        }
        assert_eq!(memos.datetime.borrow().stats().inserts, 0);
        let s = text("2024-05-05T10:00:00Z");
        for _ in 0..20 {
            let dt = call(&memos, &meter, "datetime", &s).unwrap();
            assert_eq!(
                dt,
                temporal::construct_named("datetime", std::slice::from_ref(&s)).unwrap()
            );
        }
        assert!(memos.datetime.borrow().stats().hits > 0);
        let probes = memos.datetime.borrow().stats().probes;
        call(&memos, &meter, "date", &text("2024-05-05")).unwrap();
        assert_eq!(memos.datetime.borrow().stats().probes, probes);
    }

    #[test]
    fn spellings_of_datetime_share_the_memo_and_other_names_never_hash() {
        let memos = ConstructMemos::new(MemoLive::default());
        let meter = Charged::default();
        let before = hashes();
        for name in ["date", "time", "duration", "localdatetime", "zoned_time"] {
            let arg = text("2024-05-05");
            let got = call(&memos, &meter, name, &arg);
            let direct = temporal::construct_named(name, std::slice::from_ref(&arg));
            assert_eq!(got.is_ok(), direct.is_ok(), "{name}: same outcome");
            if let (Ok(got), Ok(direct)) = (got, direct) {
                assert_eq!(got, direct, "{name}: same value");
            }
        }
        assert_eq!(hashes(), before, "a non-datetime kind never hashes");
        let s = text("2024-05-05T10:00:00Z");
        for name in ["datetime", "DATETIME", "zoned_datetime"] {
            for _ in 0..10 {
                call(&memos, &meter, name, &s).unwrap();
            }
        }
        assert!(
            memos.datetime.borrow().stats().hits > 0,
            "every spelling reaches the one table"
        );
    }

    #[test]
    fn a_warming_or_disabled_memo_does_not_hash() {
        let policy = MemoPolicy {
            warm_up: 4,
            window: 16,
            min_hit_percent: 50,
            max_bytes: 1 << 20,
        };
        let memos = ConstructMemos::with_policy(policy, MemoLive::default());
        let meter = Charged::default();
        let before = hashes();
        for i in 0..4 {
            call(
                &memos,
                &meter,
                "datetime",
                &text(&format!("2024-05-{:02}T10:00:00Z", i + 1)),
            )
            .unwrap();
        }
        assert_eq!(hashes(), before, "warm-up costs one branch, no hash");
        // Distinct inputs never hit: the review disables the memo.
        for i in 0..40 {
            call(
                &memos,
                &meter,
                "datetime",
                &text(&format!("2023-01-01T10:00:{i:02}Z")),
            )
            .unwrap();
        }
        assert!(memos.datetime.borrow().stats().disabled);
        let settled = hashes();
        for _ in 0..10 {
            call(&memos, &meter, "datetime", &text("2024-05-05T10:00:00Z")).unwrap();
        }
        assert_eq!(hashes(), settled, "a bypassed memo hashes nothing");
    }

    #[test]
    fn zoned_answers_charge_their_zone_id_and_stop_at_the_byte_ceiling() {
        let policy = MemoPolicy {
            warm_up: 0,
            window: 1 << 16,
            min_hit_percent: 0,
            max_bytes: 1,
        };
        let live = MemoLive::default();
        let memos = ConstructMemos::with_policy(policy, live.clone());
        let meter = Charged::default();
        let zone = "America/Argentina/ComodRivadavia";
        let mut owned = 0;
        for i in 0..20 {
            let arg = text(&format!("2024-05-05T10:00:{i:02}[{zone}]"));
            let got = call(&memos, &meter, "datetime", &arg).unwrap();
            owned = got.retained_heap();
            assert!(owned >= zone.len(), "the answer owns its zone id");
        }
        let stats = memos.datetime.borrow().stats();
        assert_eq!(stats.inserts, 1, "the ceiling stops the second insert");
        assert!(stats.refused > 0);
        assert_eq!(meter.0.get(), stats.bytes, "every table byte is charged");
        assert!(stats.bytes >= owned as u64, "the zone id is in the bytes");
        assert_eq!(live.live_bytes(), stats.bytes);
    }

    #[test]
    fn a_zero_budget_never_hashes_or_allocates_a_table() {
        let live = MemoLive::new(0);
        let memos = ConstructMemos::new(live.clone());
        let meter = Charged::default();
        let arg = text("2024-05-05T10:00:00Z");
        let before = hashes();
        for _ in 0..100 {
            let got = call(&memos, &meter, "datetime", &arg).unwrap();
            assert_eq!(
                got,
                temporal::construct_named("datetime", std::slice::from_ref(&arg)).unwrap()
            );
        }
        assert_eq!(hashes(), before);
        let stats = memos.datetime.borrow().stats();
        assert_eq!((stats.bytes, stats.inserts, stats.probes), (0, 0, 0));
        assert_eq!((meter.0.get(), live.live_bytes()), (0, 0));
    }
}

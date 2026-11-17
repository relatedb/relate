//! A once-built lookup for `x IN <the same list>` over a long list (LABS-1478).
//!
//! `x IN $ids` used to scan the list with `cypher_eq` on every row. The list
//! is the one value a statement binds, so the bound `Arc` is the same
//! allocation on every row: this module remembers the last long list it saw
//! (holding a `Weak`, so the address cannot be reused while the entry lives
//! yet the client's list is freed with the statement) and, from the second
//! sighting on, answers an exact `Int`-in-all-`Int` or `String`-in-all-`String`
//! probe by binary search. A list past the caps below is never indexed, so
//! the copy a thread keeps after the statement stays small.
//!
//! Three-valued logic decides what is left to the scan. The index is built
//! only for a list whose every member is an `Int` (or every member a
//! `String`) — no null, no float, no mixed types — and answers only a probe
//! of the same type: the answer is then exactly `true`/`false`, never null.
//! A null probe, a float probe (`1.0 IN [1]` is true, NaN never), a probe of
//! another type, a short list and a list with any null or mixed member all
//! take the unchanged linear scan, so the result is identical by
//! construction.

use std::cell::RefCell;
use std::sync::{Arc, Weak};

use relate_core::Value;

use super::fold::membership;

/// Lists shorter than this are scanned: the scan beats the bookkeeping.
const MIN_INDEXED_LEN: usize = 9;
/// Lists longer than this are scanned: the index copy a thread keeps after
/// the statement is bounded by this and the string cap below.
const MAX_INDEXED_LEN: usize = 65_536;
/// Total string bytes past which a list is scanned, not indexed.
const MAX_INDEXED_STRING_BYTES: usize = 1 << 20;

enum Index {
    Ints(Vec<i64>),
    /// The list's own string bytes, shared: each entry is a clone of the
    /// `Arc<str>` the statement's list already holds, so the table pins
    /// no new bytes.
    Strings(Vec<Arc<str>>),
    /// A list the index cannot answer exactly (null, float, mixed, other).
    Unindexable,
}

struct Slot {
    list: Weak<Vec<Value>>,
    /// Built on the second sighting of the same allocation, so a fresh
    /// literal list per row never pays for the build.
    index: Option<Index>,
}

thread_local! {
    static LAST: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

/// The one collect-and-reject, sort, dedup pipeline both index arms share:
/// the ordered, deduped table, or `None` when any member breaks the arm's
/// type — so the two arms cannot drift apart in what a "table" is.
fn table<T: Ord>(items: impl Iterator<Item = Option<T>>) -> Option<Vec<T>> {
    let mut table: Vec<T> = items.collect::<Option<_>>()?;
    table.sort_unstable();
    table.dedup();
    Some(table)
}

fn build(list: &[Value]) -> Index {
    // The constant-time length gate first: a list past it never pays the
    // classification pass.
    if list.len() > MAX_INDEXED_LEN {
        return Index::Unindexable;
    }
    // The kind is classified once, from the first member; the single typed
    // collect in each arm rejects a list whose later members disagree.
    match list.first() {
        Some(Value::Int(_)) => table(list.iter().map(|v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        }))
        .map(Index::Ints)
        .unwrap_or(Index::Unindexable),
        Some(Value::String(_)) => {
            // One typed pass owns both the table and its byte budget: the
            // budget sums the strings the pass proved are there, never a
            // fabricated zero for a member it would reject.
            let Some(strings) = table(list.iter().map(|v| match v {
                Value::String(s) => Some(Arc::clone(s)),
                _ => None,
            })) else {
                return Index::Unindexable;
            };
            let string_bytes: usize = strings.iter().map(|s| s.len()).sum();
            if string_bytes > MAX_INDEXED_STRING_BYTES {
                return Index::Unindexable;
            }
            Index::Strings(strings)
        }
        // An empty list, or a first member that is neither an `Int` nor a
        // `String`: there is no one-typed table to binary-search.
        _ => Index::Unindexable,
    }
}

/// The one production membership entry over a list's shared allocation,
/// published at the `eval` tier (the bare [`membership`] reference scan
/// stays private to this module pair — production never calls it
/// directly): the index's
/// exact answer when it has one, the linear scan otherwise. `IN`
/// (`expr::in_op`) and the `coll.has*Of` family (`collection.rs`) both route
/// through here, so the index-or-scan decision lives beside the index it
/// decides on — never re-spelled at an operator site.
pub(crate) fn member_of(x: &Value, list: &Arc<Vec<Value>>) -> Value {
    indexed_membership(x, list).map_or_else(|| membership(x, list), Value::Bool)
}

/// The exact answer for a same-typed probe, or `None` when the scan decides.
fn indexed_membership(x: &Value, list: &Arc<Vec<Value>>) -> Option<bool> {
    if list.len() < MIN_INDEXED_LEN || !matches!(x, Value::Int(_) | Value::String(_)) {
        return None;
    }
    LAST.with(|cell| {
        let mut slot = cell.borrow_mut();
        let seen = match slot.as_mut() {
            Some(s) if s.list.as_ptr() == Arc::as_ptr(list) => s,
            _ => {
                *slot = Some(Slot {
                    list: Arc::downgrade(list),
                    index: None,
                });
                return None;
            }
        };
        match (x, seen.index.get_or_insert_with(|| build(list))) {
            (Value::Int(i), Index::Ints(ints)) => Some(ints.binary_search(i).is_ok()),
            (Value::String(s), Index::Strings(strings)) => {
                Some(strings.binary_search_by(|m| (**m).cmp(&**s)).is_ok())
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::fold::membership;
    use super::*;

    fn ints(xs: &[i64]) -> Arc<Vec<Value>> {
        Arc::new(xs.iter().map(|i| Value::Int(*i)).collect())
    }

    /// The differential: the one production entry (called repeatedly so
    /// the index builds) against the linear `membership` scan, null
    /// outcomes included.
    fn agrees(probe: &Value, list: &Arc<Vec<Value>>) {
        let scan = membership(probe, list);
        for _ in 0..3 {
            assert_eq!(
                member_of(probe, list),
                scan,
                "probe {probe:?} over {list:?}"
            );
        }
    }

    fn probes() -> Vec<Value> {
        vec![
            Value::Null,
            Value::Int(0),
            Value::Int(5),
            Value::Int(-3),
            Value::Int(i64::MAX),
            Value::Float(5.0),
            Value::Float(5.5),
            Value::Float(f64::NAN),
            Value::Bool(true),
            Value::String("a".into()),
            Value::String("".into()),
            Value::String("zz".into()),
            Value::List(Arc::new(vec![Value::Int(1)])),
        ]
    }

    #[test]
    fn matches_the_scan_over_adversarial_lists() {
        let strings = |xs: &[&str]| {
            Arc::new(
                xs.iter()
                    .map(|s| Value::String((*s).into()))
                    .collect::<Vec<_>>(),
            )
        };
        let mut lists = vec![
            ints(&[]),
            ints(&[1, 2, 3]),
            ints(&[9, 5, 5, 1, 0, -3, 7, 7, 2, 4, 6, i64::MAX]),
            strings(&["a", "b", "a", "c", "d", "e", "f", "g", "h", "i", ""]),
        ];
        // null, float, NaN and mixed members force the scan.
        let mut with_null = (0..12).map(Value::Int).collect::<Vec<_>>();
        with_null.push(Value::Null);
        lists.push(Arc::new(with_null));
        let mut with_float = (0..12).map(Value::Int).collect::<Vec<_>>();
        with_float.push(Value::Float(5.0));
        lists.push(Arc::new(with_float));
        let mut with_nan = (0..12).map(Value::Int).collect::<Vec<_>>();
        with_nan.push(Value::Float(f64::NAN));
        lists.push(Arc::new(with_nan));
        let mut mixed = (0..6).map(Value::Int).collect::<Vec<_>>();
        mixed.extend((0..6).map(|i| Value::String(format!("s{i}").into())));
        lists.push(Arc::new(mixed));
        for list in &lists {
            for probe in probes() {
                agrees(&probe, list);
            }
        }
    }

    #[test]
    fn builds_only_on_the_second_sighting_of_one_allocation() {
        let list = ints(&(0..20).collect::<Vec<_>>());
        assert_eq!(indexed_membership(&Value::Int(3), &list), None);
        assert_eq!(indexed_membership(&Value::Int(3), &list), Some(true));
        assert_eq!(indexed_membership(&Value::Int(99), &list), Some(false));
        // An equal-content list in a fresh allocation is a new sighting.
        let other = ints(&(0..20).collect::<Vec<_>>());
        assert_eq!(indexed_membership(&Value::Int(3), &other), None);
        // Two sightings index `other`; a null probe still never touches
        // the slot (the probe-type gate returns before the sighting is
        // recorded, so the scan decides it), while a same-typed probe
        // answers from the index.
        assert_eq!(indexed_membership(&Value::Int(3), &other), Some(true));
        assert_eq!(indexed_membership(&Value::Null, &other), None);
        assert_eq!(indexed_membership(&Value::Int(99), &other), Some(false));
    }

    #[test]
    fn short_lists_are_never_indexed() {
        let list = ints(&[1, 2, 3]);
        for _ in 0..3 {
            assert_eq!(indexed_membership(&Value::Int(1), &list), None);
        }
    }

    #[test]
    fn a_list_past_the_length_cap_is_never_indexed() {
        // Both ceilings bound retention, not the answer, so the
        // differential stays green whichever way they are set — pin each
        // directly: one element past the cap never builds.
        let list = ints(&(0..=(MAX_INDEXED_LEN as i64)).collect::<Vec<_>>());
        for _ in 0..3 {
            assert_eq!(indexed_membership(&Value::Int(1), &list), None);
        }
        agrees(&Value::Int(1), &list);
    }

    #[test]
    fn a_string_table_past_the_byte_cap_is_never_indexed() {
        // 16 distinct members, each a little past a sixteenth of the byte
        // budget: the deduped table passes the cap while the list stays
        // under the length cap.
        let member = "x".repeat(MAX_INDEXED_STRING_BYTES / 8 + 1);
        let list: Arc<Vec<Value>> = Arc::new(
            (0..16)
                .map(|i| Value::String(format!("{i}{member}").into()))
                .collect(),
        );
        let probe = Value::String(format!("0{member}").into());
        for _ in 0..3 {
            assert_eq!(indexed_membership(&probe, &list), None);
        }
        agrees(&probe, &list);
    }
}

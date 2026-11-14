//! A once-built lookup for `x IN <the same list>` over a long list (LABS-1478).
//!
//! `x IN $ids` used to scan the list with `cypher_eq` on every row. The list
//! is the one value a statement binds, so the bound `Arc` is the same
//! allocation on every row: this module remembers the last long list it saw
//! (holding a clone of the `Arc`, so the address cannot be reused while the
//! entry lives) and, from the second sighting on, answers an exact
//! `Int`-in-all-`Int` or `String`-in-all-`String` probe by binary search.
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
use std::sync::Arc;

use relate_core::Value;

/// Lists at most this long are scanned: the scan beats the bookkeeping.
const MIN_INDEXED_LEN: usize = 9;

enum Index {
    Ints(Vec<i64>),
    Strings(Vec<Box<str>>),
    /// A list the index cannot answer exactly (null, float, mixed, other).
    Unindexable,
}

struct Slot {
    list: Arc<Vec<Value>>,
    /// Built on the second sighting of the same allocation, so a fresh
    /// literal list per row never pays for the build.
    index: Option<Index>,
}

thread_local! {
    static LAST: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

fn build(list: &[Value]) -> Index {
    let ints: Option<Vec<i64>> = list
        .iter()
        .map(|v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        })
        .collect();
    if let Some(mut ints) = ints {
        ints.sort_unstable();
        ints.dedup();
        return Index::Ints(ints);
    }
    let strings: Option<Vec<Box<str>>> = list
        .iter()
        .map(|v| match v {
            Value::String(s) => Some(Box::<str>::from(&**s)),
            _ => None,
        })
        .collect();
    match strings {
        Some(mut strings) => {
            strings.sort_unstable();
            strings.dedup();
            Index::Strings(strings)
        }
        None => Index::Unindexable,
    }
}

/// The exact answer for a same-typed probe, or `None` when the scan decides.
pub(super) fn indexed_membership(x: &Value, list: &Arc<Vec<Value>>) -> Option<Value> {
    if list.len() < MIN_INDEXED_LEN || !matches!(x, Value::Int(_) | Value::String(_)) {
        return None;
    }
    LAST.with(|cell| {
        let mut slot = cell.borrow_mut();
        match slot.as_mut() {
            Some(s) if Arc::ptr_eq(&s.list, list) => {
                if s.index.is_none() {
                    s.index = Some(build(list));
                }
            }
            _ => {
                *slot = Some(Slot {
                    list: Arc::clone(list),
                    index: None,
                });
                return None;
            }
        }
        match (x, slot.as_ref()?.index.as_ref()?) {
            (Value::Int(i), Index::Ints(ints)) => Some(Value::Bool(ints.binary_search(i).is_ok())),
            (Value::String(s), Index::Strings(strings)) => Some(Value::Bool(
                strings.binary_search_by(|m| (**m).cmp(&**s)).is_ok(),
            )),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::membership;
    use super::*;

    fn ints(xs: &[i64]) -> Arc<Vec<Value>> {
        Arc::new(xs.iter().map(|i| Value::Int(*i)).collect())
    }

    /// The differential: the indexed path (called repeatedly so the index
    /// builds) against the linear `membership` scan, null outcomes included.
    fn agrees(probe: &Value, list: &Arc<Vec<Value>>) {
        let scan = membership(probe, list);
        for _ in 0..3 {
            let got = indexed_membership(probe, list).unwrap_or_else(|| membership(probe, list));
            assert_eq!(got, scan, "probe {probe:?} over {list:?}");
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
        assert_eq!(
            indexed_membership(&Value::Int(3), &list),
            Some(Value::Bool(true))
        );
        assert_eq!(
            indexed_membership(&Value::Int(99), &list),
            Some(Value::Bool(false))
        );
        // An equal-content list in a fresh allocation is a new sighting.
        let other = ints(&(0..20).collect::<Vec<_>>());
        assert_eq!(indexed_membership(&Value::Int(3), &other), None);
        // A null probe over an indexed list is the scan's null.
        assert_eq!(indexed_membership(&Value::Null, &other), None);
    }

    #[test]
    fn short_lists_are_never_indexed() {
        let list = ints(&[1, 2, 3]);
        for _ in 0..3 {
            assert_eq!(indexed_membership(&Value::Int(1), &list), None);
        }
    }
}

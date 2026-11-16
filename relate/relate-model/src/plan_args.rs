//! The plan wire args' scalar readers (ADR-0345): the optional-int /
//! bool / fraction extraction policies and the all-or-none, non-negative
//! group read every metric family shares. Both wire accessors read here
//! — the node accessor (`plan_wire`) and the fused-child parser
//! (`plan_fused`) — and the accessor also reads the parser for its fused
//! fact, so the dependency chain is `plan_wire` → `plan_fused` → this
//! module, never the other way.

use crate::CypherValue;

/// The one absent/present/malformed match every optional scalar arg
/// shares: the caller names its variant's extractor (which carries its
/// own guard) and its contract text; a malformed value fails loudly
/// naming the key.
fn optional_arg<T>(
    args: &crate::IndexMap<String, CypherValue>,
    key: &str,
    extract: impl Fn(&CypherValue) -> Option<T>,
    contract: &str,
) -> Option<T> {
    args.get(key).map(|value| {
        extract(value)
            .unwrap_or_else(|| panic!("plan wire {key} is {contract} when present, got {value:?}"))
    })
}

/// The optional-integer arg — one extraction policy for every optional
/// metric key: absent or a non-negative integer, a malformed value fails
/// loudly. No plan-wire key carries a negative integer, so non-negativity
/// is part of this one policy rather than re-checked at each use site.
pub(super) fn optional_int(args: &crate::IndexMap<String, CypherValue>, key: &str) -> Option<i64> {
    optional_arg(
        args,
        key,
        |value| match value {
            CypherValue::Int(n) if *n >= 0 => Some(*n),
            _ => None,
        },
        "a non-negative integer (ADR-0345)",
    )
}

/// The optional-boolean arg — the same policy as [`optional_int`] for the
/// one boolean fact (LABS-369's walk direction).
pub(super) fn optional_bool(
    args: &crate::IndexMap<String, CypherValue>,
    key: &str,
) -> Option<bool> {
    optional_arg(
        args,
        key,
        |value| match value {
            CypherValue::Bool(flag) => Some(*flag),
            _ => None,
        },
        "a boolean (LABS-369)",
    )
}

/// The optional-fraction arg — [`optional_int`]'s policy for the one
/// float metric (the frontier budget fraction, ADR-0516): absent or a
/// non-negative float, a malformed value fails loudly.
pub(super) fn optional_fraction(
    args: &crate::IndexMap<String, CypherValue>,
    key: &str,
) -> Option<f64> {
    optional_arg(
        args,
        key,
        |value| match value {
            CypherValue::Float(fraction) if *fraction >= 0.0 => Some(*fraction),
            _ => None,
        },
        "a non-negative float (ADR-0516)",
    )
}

/// The rides-the-metric-set group policy (one loud spelling, never a
/// hand-rolled copy per field family): the keys' values are an
/// all-or-none group of non-negative integers, present only when the
/// metric set is — a partial group, a group without metrics, or a
/// negative value is a wire-contract break kept loud. `None` when the
/// whole group is absent (an EXPLAIN node).
pub(super) fn metric_set_ints<const N: usize>(
    args: &crate::IndexMap<String, CypherValue>,
    keys: [&str; N],
    has_metrics: bool,
) -> Option<[i64; N]> {
    match (has_metrics, metric_group_ints(args, keys)) {
        (false, None) => None,
        (true, Some(values)) => Some(values),
        (false, Some(_)) => panic!(
            "plan wire {keys:?} are present without the metric fields \
             (all-or-none group policy)"
        ),
        (true, None) => panic!(
            "plan wire {keys:?} are absent from the metric fields \
             (all-or-none group policy)"
        ),
    }
}

/// The all-or-none group read — `metric_set_ints` minus its metric-set
/// gate, so the scalar metric block, the riding families (read kinds,
/// phase times) and the fused seek child's measured pair share one
/// extraction policy. The fixed arity lives in the return type (a caller
/// destructures `[a, b, …]`), so a wider or reordered group is a compile
/// error. A partial group is a wire-contract break kept loud; the keys'
/// non-negativity is [`optional_int`]'s policy; `None` when every key is
/// absent.
pub(super) fn metric_group_ints<const N: usize>(
    args: &crate::IndexMap<String, CypherValue>,
    keys: [&str; N],
) -> Option<[i64; N]> {
    let values = keys.map(|key| optional_int(args, key));
    let present = values.iter().filter(|value| value.is_some()).count();
    if present == 0 {
        return None;
    }
    assert!(
        present == N,
        "plan wire {keys:?} are an all-or-none group, got a partial group: values={values:?}"
    );
    Some(values.map(|value| value.expect("the group is all-present")))
}

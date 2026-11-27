//! ADR-0462 (LABS-606) depth-cap tests: the iterative checker, the
//! construction boundary, and the four renderers' agreement — split from
//! `value/tests.rs` for the file-size rule (the `exec/integer_arg_tests.rs`
//! convention).
//!
//! Over-cap fixtures (values deeper than [`MAX_VALUE_DEPTH`]) are leaked with
//! `std::mem::forget`: their derived recursive `Drop` is only depth-safe
//! behind the construction cap, and these fixtures are deliberately past it.

use super::depth::DEPTH_LIMIT_MARKER;
use super::*;
use std::sync::Arc;

/// `n` nested lists containing a null leaf: a value of container depth `n`.
fn nested(n: u32) -> Value {
    let mut v = Value::List(Vec::new().into());
    for _ in 1..n {
        v = Value::List(vec![v].into());
    }
    v
}

/// ADR-0462: `depth_within` counts container levels — a scalar is 0, `[]`
/// is 1, `[[]]` is 2 — and the boundary is exactly [`MAX_VALUE_DEPTH`].
#[test]
fn depth_within_counts_container_levels() {
    assert!(Value::Int(1).depth_within(0));
    assert!(nested(1).depth_within(1));
    assert!(!nested(2).depth_within(1));
    assert!(nested(MAX_VALUE_DEPTH).depth_within(MAX_VALUE_DEPTH));
    assert!(!nested(MAX_VALUE_DEPTH + 1).depth_within(MAX_VALUE_DEPTH));
    // The map arm nests too.
    let mut m = BTreeMap::new();
    m.insert("a".to_string(), nested(MAX_VALUE_DEPTH));
    let map = Value::Map(m.into());
    assert!(!map.depth_within(MAX_VALUE_DEPTH));
    assert!(map.depth_within(MAX_VALUE_DEPTH + 1));
}

/// The checker is iterative: a value far deeper than any stack could hold
/// is walked (and rejected) without recursing.
#[test]
fn depth_within_is_iterative_on_a_deep_narrow_value() {
    let v = nested(5000);
    assert!(v.depth_within(5000));
    assert!(!v.depth_within(4999));
    // A byte budget would not early-exit here — the value is tiny — so the
    // depth half of the walk is what stops it.
    assert_eq!(logical_bytes_within([&v], u64::MAX), None);
    // Leak: see the module's over-cap fixture policy.
    std::mem::forget(v);
}

/// The checked constructors accept an at-cap container and throw the typed
/// `ValueDepthExceeded` execution error one level deeper.
#[test]
fn checked_constructors_enforce_the_depth_cap() {
    let mut at_cap = Value::List(Vec::new().into());
    for _ in 1..MAX_VALUE_DEPTH {
        at_cap = Value::checked_list(vec![at_cap]).expect("at-cap construction");
    }
    assert!(at_cap.depth_within(MAX_VALUE_DEPTH));

    let err = Value::checked_list(vec![at_cap.clone()]).expect_err("501 levels must reject");
    assert!(
        err.to_string().starts_with("ValueDepthExceeded:"),
        "typed payload: {err}"
    );
    let mut m = BTreeMap::new();
    m.insert("a".to_string(), at_cap.clone());
    let err = Value::checked_map(m).expect_err("501 levels must reject");
    assert!(err.to_string().starts_with("ValueDepthExceeded:"));
    drop(at_cap);
}

/// ADR-0462 boundary: a path's embedded node/relationship entities are not
/// an extra container level. All four walkers — `depth_within`,
/// `to_cypher_literal`, `to_json_plain` and `value_to_cypher` — must agree
/// that a path whose node property holds `MAX_VALUE_DEPTH - 1` nested lists
/// sits exactly at the cap and renders fully; one more level trips the
/// guard.
#[test]
fn path_walkers_agree_on_the_depth_boundary() {
    fn path_with_property_list(n: u32) -> Value {
        let prop = nested(n);
        let mut props = BTreeMap::new();
        props.insert("p".to_string(), prop);
        let node = Node {
            id: 1,
            labels: Arc::new(vec!["N".to_string()]),
            properties: PropertyMap::share(props),
        };
        Value::from_path(Path {
            nodes: vec![node],
            rels: Vec::new(),
        })
    }
    fn json_array_depth(v: &serde_json::Value) -> usize {
        match v {
            serde_json::Value::Array(xs) => 1 + xs.iter().map(json_array_depth).max().unwrap_or(0),
            _ => 0,
        }
    }
    fn cypher_list_depth(v: &relate_model::CypherValue) -> usize {
        match v {
            relate_model::CypherValue::List(xs) => {
                1 + xs.iter().map(cypher_list_depth).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    let property_levels = MAX_VALUE_DEPTH - 1;
    let at_cap = path_with_property_list(property_levels);
    // Path level 1 + `property_levels` list levels = exactly the cap.
    assert!(at_cap.depth_within(MAX_VALUE_DEPTH));
    assert!(!at_cap.depth_within(MAX_VALUE_DEPTH - 1));

    let literal = at_cap.to_cypher_literal();
    assert!(
        !literal.contains(DEPTH_LIMIT_MARKER),
        "at-cap path must render fully, got: {literal}"
    );

    let json = at_cap.to_json_plain();
    assert_eq!(
        json_array_depth(&json["nodes"][0]["properties"]["p"]),
        property_levels as usize
    );

    let cypher = crate::convert::value_to_cypher(&at_cap);
    let relate_model::CypherValue::Path { nodes, .. } = &cypher else {
        panic!("value_to_cypher must keep the path shape");
    };
    let relate_model::CypherValue::Node { properties, .. } = &nodes[0] else {
        panic!("value_to_cypher must keep the node shape");
    };
    assert_eq!(
        cypher_list_depth(&properties["p"]),
        property_levels as usize
    );

    // One level past the cap trips every guard: `depth_within` says no and
    // the literal walker saturates instead of recursing.
    let over = path_with_property_list(MAX_VALUE_DEPTH);
    assert!(!over.depth_within(MAX_VALUE_DEPTH));
    assert!(over.to_cypher_literal().contains(DEPTH_LIMIT_MARKER));

    // Leak: see the module's over-cap fixture policy.
    std::mem::forget(over);
}

/// REL8-217 regression: the path display form carries no depth guard — it
/// renders no properties, so it cannot recurse — while the literal form
/// (which does recurse into properties) saturates past the cap. The
/// distinguishing input is a *shallow* path nested inside
/// `MAX_VALUE_DEPTH` raw lists, so the path itself renders at positional
/// depth `MAX_VALUE_DEPTH` — the exact level a path-arm guard would
/// charge. (A deep-*property* path does not distinguish: display never
/// reads properties, and a path at positional depth below the cap admits
/// its guard, so both shapes render the same.)
#[test]
fn path_display_renders_past_the_depth_cap_but_the_literal_saturates() {
    let path = Value::from_path(Path {
        nodes: vec![Node {
            id: 1,
            labels: Arc::new(vec!["N".to_string()]),
            properties: PropertyMap::share(BTreeMap::new()),
        }],
        rels: Vec::new(),
    });
    let mut nested_path = path;
    for _ in 0..MAX_VALUE_DEPTH {
        nested_path = Value::List(vec![nested_path].into());
    }
    let display = nested_path.to_display_string();
    assert!(
        !display.contains(DEPTH_LIMIT_MARKER),
        "the display walk renders the path's labels, never its properties, so no path-level depth guard applies, got: {display}"
    );
    assert!(display.contains("(:N)"));
    assert!(nested_path.to_cypher_literal().contains(DEPTH_LIMIT_MARKER));
    // Leak: see the module's over-cap fixture policy.
    std::mem::forget(nested_path);
}

/// ADR-0462: `cypher_eq` threads the same depth guard as its sibling
/// comparison walkers. At the cap it compares fully; one level past, the
/// container arms saturate to `false` instead of recursing (unreachable
/// behind the checked constructors).
#[test]
fn cypher_eq_saturates_past_the_value_depth_cap() {
    let at_cap = nested(MAX_VALUE_DEPTH);
    assert_eq!(at_cap.cypher_eq(&at_cap), Value::Bool(true));
    let over = nested(MAX_VALUE_DEPTH + 1);
    assert_eq!(over.cypher_eq(&over), Value::Bool(false));
    // Leak: see the module's over-cap fixture policy.
    std::mem::forget(over);
}

/// ADR-0462 security regression (PR #547 round-5): the depth walk memoizes
/// container payloads by identity, so a fan-out DAG costs work linear in the
/// number of distinct payloads — not the exponential expanded node count.
/// Forty doublings would be 2^40 expanded visits on the old walk; the
/// memoized walk answers instantly. A legitimately shared subtree still
/// contributes its own max depth, not a doubled count.
#[test]
fn shared_fan_out_depth_is_linear_in_distinct_payloads() {
    let mut value = nested(1);
    for _ in 0..40 {
        value = Value::List(vec![value.clone(), value].into());
    }
    assert_eq!(value.depth(), 41);
    assert!(!value.depth_within(40));
    assert!(value.depth_within(41));

    let leaf = nested(3);
    let shared = Value::List(vec![leaf.clone(), leaf.clone()].into());
    assert_eq!(shared.depth(), 4);
}

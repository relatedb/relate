//! LABS-1606: a filtered list comprehension's WHERE may be a type
//! predicate — the `|` after the type yields to the comprehension's map
//! separator instead of demanding another union component.

mod common;

use common::{connection, run, scalar_one, strings};

/// The compat scenario [9] contract (live-verified against Neo4j
/// Enterprise 2026.10.1): filtering a node's keys by their value's type
/// and mapping to the key keeps only the string-valued property name.
#[test]
fn a_filtered_comprehension_filters_keys_by_value_type() {
    let mut c = connection();
    run(&mut c, "CREATE (:N {name: 'a', num: 1})");
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (n) RETURN [k IN keys(n) WHERE n[k] IS :: STRING | k] AS x"
        ),
        strings(&["name"])
    );
    // The negated spelling filters the other way, through the same
    // pipe-yielding type parse.
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (n) RETURN [k IN keys(n) WHERE n[k] IS NOT :: STRING | k] AS x"
        ),
        strings(&["num"])
    );
}

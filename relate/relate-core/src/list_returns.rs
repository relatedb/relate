//! The statically-list-returning builtin functions — the type-discipline
//! fact for boolean-position coercion (ADR-0715): a direct call to one of
//! these names in a position that requires a boolean is a compile-time
//! `TypeError` (`invalid coercion. Cannot coerce LIST to BOOLEAN`), while
//! a dynamically-typed value (a parameter, a property read) keeps the
//! deprecated runtime list-to-boolean conversion.
//!
//! The names mirror the builtin signature registry's non-aggregating
//! `LIST<…>`-returning rows — one table per fact, the two homes pinned
//! together by the executor registry's drift test, the same contract
//! [`crate::arity`]'s shared arity table carries. Aggregations are absent
//! on purpose: their own position rules reject them before any boolean
//! coercion question arises.

/// The statically-list-returning builtin names, lower-case: the reference
/// compile-time/runtime split is exactly "the return type is knowable at
/// compile time", and these are the names whose catalog-declared return
/// type is a list.
const LIST_RETURNING: &[&str] = &[
    "allshortestpaths",
    "coll.distinct",
    "coll.flatten",
    "coll.insert",
    "coll.remove",
    "coll.sort",
    "coll.zip",
    "graph.names",
    "keys",
    "labels",
    "nodes",
    "range",
    "relationships",
    "split",
    "tail",
    "tobooleanlist",
    "tofloatlist",
    "tointegerlist",
    "tostringlist",
];

/// Whether a builtin call's declared return type is a list: the front's
/// static boolean-position rule reads this one fact (names arrive
/// lower-cased — the same convention [`crate::arity::builtin_arity`]'s
/// callers apply).
pub fn builtin_returns_list(name: &str) -> bool {
    LIST_RETURNING.contains(&name)
}

/// The table itself, for the executor registry's drift test — the one
/// reader that pins this table to the signature registry's
/// `LIST<…>`-returning rows (ADR-0715's shared-fact contract).
pub fn list_returning_names() -> &'static [&'static str] {
    LIST_RETURNING
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is the drift-test's asserted set — pin its size so a
    /// silent partial build or a bad edit fails here first.
    #[test]
    fn the_table_carries_every_registry_list_row() {
        assert_eq!(LIST_RETURNING.len(), 19, "{:?}", LIST_RETURNING);
        assert!(builtin_returns_list("tail"));
        assert!(!builtin_returns_list("tostring"));
    }
}

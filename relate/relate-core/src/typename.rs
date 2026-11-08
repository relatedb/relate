//! Cypher value type names (`valueType()`) and their CIP-100 sort order.
//!
//! The one `Value` → Cypher-type-name spelling and the one type-union order
//! (CIP-100, mirroring the parser's `type_normalize::rank`; a pinning test in
//! each crate fails when its table moves). Two renderings come from the one
//! table: [`value_type_name`] for the executor's `valueType()` surface (its
//! `exec`/`eval` call sites import `relate_core::value_type_name`) and
//! [`bare_type_name`] for the 22N27/22NB1 dynamic-operand detail text, which
//! drops the nullability suffix. The suffix spelling is [`NOT_NULL_SUFFIX`],
//! so neither consumer restates it.

use std::collections::HashSet;

use crate::temporal;
use crate::Value;

/// The nullability suffix the `valueType()` surface renders on every
/// non-null type occurrence, nested occurrences included: a list of integers
/// is `LIST<INTEGER NOT NULL> NOT NULL`.
const NOT_NULL_SUFFIX: &str = " NOT NULL";

/// `name` as the `valueType()` surface renders a non-null type.
fn suffixed(name: &str) -> String {
    format!("{name}{NOT_NULL_SUFFIX}")
}

/// The Cypher type name of `value`, including the ` NOT NULL` suffix the
/// `valueType()` surface renders.
pub fn value_type_name(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Bool(_) => suffixed("BOOLEAN"),
        Value::Int(_) => suffixed("INTEGER"),
        Value::Float(_) => suffixed("FLOAT"),
        Value::String(_) => suffixed("STRING"),
        Value::Map(_) => suffixed("MAP"),
        Value::Node(_) => suffixed("NODE"),
        Value::Relationship(_) => suffixed("RELATIONSHIP"),
        Value::Path(_) => suffixed("PATH"),
        Value::Point(_) => suffixed("POINT"),
        Value::Uuid(_) => suffixed("UUID"),
        // Cypher has no byte-array type: Neo4j types a byte array as a
        // list of integers (ADR-0327), and an empty one as an empty list.
        Value::Bytes(b) if b.is_empty() => format!("LIST<NOTHING>{NOT_NULL_SUFFIX}"),
        Value::Bytes(_) => format!("LIST<INTEGER{NOT_NULL_SUFFIX}>{NOT_NULL_SUFFIX}"),
        Value::Temporal(t) => suffixed(match t {
            temporal::Temporal::Date(_) | temporal::Temporal::CivilDate { .. } => "DATE",
            temporal::Temporal::LocalTime(_) => "LOCAL TIME",
            temporal::Temporal::Time { .. } => "ZONED TIME",
            temporal::Temporal::LocalDateTime(_) | temporal::Temporal::CivilDateTime { .. } => {
                "LOCAL DATETIME"
            }
            temporal::Temporal::DateTime { .. } => "ZONED DATETIME",
            temporal::Temporal::Duration { .. } => "DURATION",
        }),
        Value::Vector { coord, values } => format!(
            "VECTOR<{}{NOT_NULL_SUFFIX}>({}){NOT_NULL_SUFFIX}",
            coord.canonical_name(),
            values.len()
        ),
        Value::List(values) => list_type_name(values),
    }
}

/// [`value_type_name`] without the nullability suffix — the type spelling the
/// 22N27/22NB1 dynamic-operand detail text renders (`DATE`,
/// `LIST<STRING | INTEGER>`).
///
/// Composed from the one table above: the removed markers are exactly
/// [`NOT_NULL_SUFFIX`], so the type vocabulary and its nullability spelling
/// stay in this module.
pub fn bare_type_name(value: &Value) -> String {
    value_type_name(value).replace(NOT_NULL_SUFFIX, "")
}

/// The value-bearing form of [`crate::expected_type_detail`] (22N01): the offending
/// value renders as its Cypher literal, except the null literal, which Neo4j
/// spells `NULL` in this sentence, and the actual type is
/// [`bare_type_name`]'s. The one spelling for a write's bound node position
/// and the read-side entity checks.
pub fn expected_value_type_detail(expected: &str, value: &Value) -> String {
    let rendered = match value {
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    };
    format!(
        "Expected the value {rendered} to be of type {expected}, but was of type {}",
        bare_type_name(value)
    )
}

/// The CIP-100 union type name of a list's element types: an empty (or
/// all-null) list renders `LIST<NOTHING>`; otherwise the element types are
/// sorted, deduplicated, subtype-subsumed and joined in CIP-100 order.
fn list_type_name(values: &[Value]) -> String {
    if values.is_empty() {
        return format!("LIST<NOTHING>{NOT_NULL_SUFFIX}");
    }
    let has_null = values.iter().any(Value::is_null);
    let mut types = values
        .iter()
        .filter(|v| !v.is_null())
        .map(value_type_name)
        .map(|s| {
            if has_null {
                s.trim_end_matches(NOT_NULL_SUFFIX).to_string()
            } else {
                s
            }
        })
        .collect::<Vec<_>>();
    types.sort_by(|a, b| {
        let ak = value_type_sort_key(a);
        let bk = value_type_sort_key(b);
        if a.starts_with("LIST<")
            && b.starts_with("LIST<")
            && ak.1 == bk.1
            && !a["LIST<".len()..].contains("LIST<")
            && !b["LIST<".len()..].contains("LIST<")
        {
            b.cmp(a)
        } else {
            ak.cmp(&bk)
        }
    });
    types.dedup();
    let nested_sets = types
        .iter()
        .map(|ty| nested_list_members(ty))
        .collect::<Vec<_>>();
    let mut keep = vec![true; types.len()];
    for i in 0..types.len() {
        let Some(left) = &nested_sets[i] else {
            continue;
        };
        for (j, right) in nested_sets.iter().enumerate() {
            if i != j
                && right
                    .as_ref()
                    .is_some_and(|right| left.len() < right.len() && left.is_subset(right))
            {
                keep[i] = false;
                break;
            }
        }
    }
    types = types
        .into_iter()
        .zip(keep)
        .filter_map(|(ty, keep)| keep.then_some(ty))
        .collect();
    if types.len() > 1 {
        types.retain(|ty| ty.strip_suffix(NOT_NULL_SUFFIX) != Some("LIST<NOTHING>"));
    }
    let element = if types.is_empty() {
        "NULL".into()
    } else {
        types.join(" | ")
    };
    format!("LIST<{element}>{NOT_NULL_SUFFIX}")
}

/// The member set of a non-nested `LIST<…>` type name, for the CIP-100
/// subtype-subsumption step; `None` for any other name.
fn nested_list_members(name: &str) -> Option<HashSet<String>> {
    let suffix = format!(">{NOT_NULL_SUFFIX}");
    let inner = name.strip_prefix("LIST<")?.strip_suffix(&suffix)?;
    if inner.contains("LIST<") {
        return None;
    }
    Some(inner.split(" | ").map(str::to_string).collect())
}

/// CIP-100 "General order of types" rank for a rendered type name, with the
/// member count as tiebreaker so a union sorts after its members.
fn value_type_sort_key(name: &str) -> (u8, usize, &str) {
    // CIP-100 order (matching type_normalize::rank so a dynamic union
    // normalizes identically wherever it is sorted): UUID sits between
    // STRING and the numerics per the CIP-246 amendment.
    let rank = if name.starts_with("BOOLEAN") {
        0
    } else if name.starts_with("STRING") {
        1
    } else if name.starts_with("UUID") {
        2
    } else if name.starts_with("INTEGER") {
        3
    } else if name.starts_with("FLOAT") {
        4
    } else if name.starts_with("DATE ") || name == "DATE" {
        5
    } else if name.starts_with("LOCAL TIME") {
        6
    } else if name.starts_with("ZONED TIME") {
        7
    } else if name.starts_with("LOCAL DATETIME") {
        8
    } else if name.starts_with("ZONED DATETIME") {
        9
    } else if name.starts_with("DURATION") {
        10
    } else if name.starts_with("POINT") {
        11
    } else if name.starts_with("NODE") {
        12
    } else if name.starts_with("RELATIONSHIP") {
        13
    } else if name.starts_with("MAP") {
        14
    } else if name.starts_with("VECTOR") {
        15
    } else if name.starts_with("LIST") {
        16
    } else if name.starts_with("PATH") {
        17
    } else {
        18
    };
    // Union members: "A | B" counts its separators. Non-union names count
    // zero separators and yield 1 -- no rank gate needed (the old
    // rank-based gate silently widened with every renumbering).
    let members = name.matches(" | ").count() + 1;
    (rank, members, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::Temporal;

    /// The CIP-100 "General order of types" sequence the shared vocabulary
    /// must render in. This table pins [`value_type_sort_key`]; the parser's
    /// `type_normalize::rank` has a twin pinning test in its own crate (the
    /// two sort tables share this order but `type_normalize` is not a
    /// dependency of this crate).
    const CIP100_ORDER: &[&str] = &[
        "BOOLEAN",
        "STRING",
        "UUID",
        "INTEGER",
        "FLOAT",
        "DATE",
        "LOCAL TIME",
        "ZONED TIME",
        "LOCAL DATETIME",
        "ZONED DATETIME",
        "DURATION",
        "POINT",
        "NODE",
        "RELATIONSHIP",
        "MAP",
        "VECTOR<FLOAT64>(3)",
        "LIST<INTEGER>",
        "PATH",
    ];

    #[test]
    fn value_type_sort_key_follows_the_documented_cip100_order() {
        // Start reversed so a sort that silently does nothing cannot pass.
        let mut names = CIP100_ORDER.to_vec();
        names.reverse();
        names.sort_by_key(|name| value_type_sort_key(name));
        assert_eq!(names, CIP100_ORDER);
    }

    /// The suffix-free rendering is `value_type_name` minus exactly the
    /// nullability markers, so the 22N27/22NB1 detail text and the
    /// `valueType()` surface can never diverge on the type vocabulary.
    #[test]
    fn bare_type_name_renders_the_suffix_free_spelling() {
        assert_eq!(bare_type_name(&Value::Null), "NULL");
        assert_eq!(value_type_name(&Value::Null), "NULL");
        assert_eq!(bare_type_name(&Value::Int(1)), "INTEGER");
        assert_eq!(value_type_name(&Value::Int(1)), "INTEGER NOT NULL");
        assert_eq!(bare_type_name(&Value::Bool(true)), "BOOLEAN");

        let date = Value::Temporal(Temporal::CivilDate {
            year: 2020,
            month: 1,
            day: 1,
        });
        assert_eq!(bare_type_name(&date), "DATE");
        assert_eq!(value_type_name(&date), "DATE NOT NULL");

        let empty_bytes = Value::Bytes(Vec::<u8>::new().into());
        assert_eq!(bare_type_name(&empty_bytes), "LIST<NOTHING>");
        assert_eq!(value_type_name(&empty_bytes), "LIST<NOTHING> NOT NULL");

        // Nested suffix occurrences are dropped too, matching the union
        // order the detail text renders.
        let mixed = Value::List(vec![Value::Int(1), Value::String("x".into())].into());
        assert_eq!(bare_type_name(&mixed), "LIST<STRING | INTEGER>");
        assert_eq!(
            value_type_name(&mixed),
            "LIST<STRING NOT NULL | INTEGER NOT NULL> NOT NULL"
        );
    }

    /// 22N01's value-bearing sentence: the null literal is Neo4j's `NULL`,
    /// every other value its Cypher literal, and the actual type is the
    /// suffix-free spelling.
    #[test]
    fn expected_value_type_detail_renders_null_uppercase() {
        assert_eq!(
            expected_value_type_detail("NODE", &Value::Null),
            "Expected the value NULL to be of type NODE, but was of type NULL"
        );
        assert_eq!(
            expected_value_type_detail("NODE", &Value::Int(7)),
            "Expected the value 7 to be of type NODE, but was of type INTEGER"
        );
        assert_eq!(
            expected_value_type_detail("NODE", &Value::Map(Default::default())),
            "Expected the value {} to be of type NODE, but was of type MAP"
        );
    }
}

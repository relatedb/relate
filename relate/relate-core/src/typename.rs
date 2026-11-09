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
        Value::Temporal(t) => suffixed(match t.kind() {
            temporal::TemporalKind::Date => "DATE",
            temporal::TemporalKind::LocalTime => "LOCAL TIME",
            temporal::TemporalKind::Time => "ZONED TIME",
            temporal::TemporalKind::LocalDateTime => "LOCAL DATETIME",
            temporal::TemporalKind::DateTime => "ZONED DATETIME",
            temporal::TemporalKind::Duration => "DURATION",
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

/// Strip exactly one trailing ` NOT NULL` refinement — the outer-only
/// policy the union merge reads (a null sibling elides the element's own
/// trailing suffix; inner suffixes survive). The all-occurrence spelling
/// is [`bare_type_name`] — the two policies stay distinguishable by name.
fn strip_outer_not_null(name: &str) -> &str {
    name.strip_suffix(NOT_NULL_SUFFIX).unwrap_or(name)
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
/// all-null) list renders `LIST<NOTHING>`; otherwise the element types
/// are built once ([`ElementType::of`]), sorted ([`sort_union_members`]),
/// deduplicated, subtype-subsumed ([`retain_not_subsumed`]) and joined —
/// a null sibling's outer suffix strip applied once, here, where the
/// parent's null fact lives.
fn list_type_name(values: &[Value]) -> String {
    if values.is_empty() {
        return format!("LIST<NOTHING>{NOT_NULL_SUFFIX}");
    }
    let has_null = values.iter().any(Value::is_null);
    let mut types: Vec<ElementType> = values
        .iter()
        .filter(|v| !v.is_null())
        .map(ElementType::of)
        .collect();
    sort_union_members(&mut types);
    types.dedup_by(|a, b| a.name == b.name);
    let mut types = retain_not_subsumed(types);
    if types.len() > 1 {
        types.retain(|ty| !ty.is_empty_sequence());
    }
    let element = if types.is_empty() {
        "NULL".into()
    } else {
        types
            .iter()
            .map(|ty| match has_null {
                true => strip_outer_not_null(&ty.name),
                false => ty.name.as_str(),
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    format!("LIST<{element}>{NOT_NULL_SUFFIX}")
}

/// The union's CIP-100 order — one total order: the type sort key's rank
/// and member count, then the element's flatness (a flat `LIST<…>` name
/// sorts after a nested one), then the name — descending between two
/// flat lists (live Neo4j's rendering order), ascending otherwise. Every
/// level is a per-element key, so no comparison can mix directions and
/// the order can never cycle.
fn sort_union_members(types: &mut [ElementType]) {
    types.sort_by(|a, b| {
        let ak = value_type_sort_key(&a.name);
        let bk = value_type_sort_key(&b.name);
        (ak.0, ak.1, a.flat())
            .cmp(&(bk.0, bk.1, b.flat()))
            .then_with(|| match (a.flat(), b.flat()) {
                (true, true) => b.name.cmp(&a.name),
                _ => a.name.cmp(&b.name),
            })
    });
}

/// Drop every element whose flat-list member set another element
/// subsumes (CIP-100's subtype-subsumption step): `LIST<INTEGER | FLOAT>`
/// absorbs `LIST<INTEGER>`. Both sides read [`ElementType::member_rows`],
/// so elements without rows — nested lists, non-lists, the
/// empty-sequence marker — skip the step in both directions; the union's
/// elision owns the empty sequences.
fn retain_not_subsumed(types: Vec<ElementType>) -> Vec<ElementType> {
    let mut keep = vec![true; types.len()];
    for i in 0..types.len() {
        let Some(left) = types[i].member_rows() else {
            continue;
        };
        for (j, right) in types.iter().enumerate() {
            if i != j
                && right
                    .member_rows()
                    .is_some_and(|right| subsumes(right, left))
            {
                keep[i] = false;
                break;
            }
        }
    }
    types
        .into_iter()
        .zip(keep)
        .filter_map(|(ty, keep)| keep.then_some(ty))
        .collect()
}

/// One element's type as the CIP-100 union merge reads it — the rendered
/// name and, for a flat list union, the member rows the subsumption
/// merge consumes, classified once from the value (never re-parsed from
/// a rendered name).
struct ElementType {
    name: String,
    members: Option<HashSet<(String, bool)>>,
}

impl ElementType {
    /// One element's typed representation, classified once from the
    /// value in this pure constructor: the rendered name (the union's
    /// join applies the outer null-sibling strip, where the parent's
    /// null fact lives) plus the member rows a flat list union
    /// contributes to the subsumption merge. The empty row set marks an
    /// empty sequence (empty bytes or an empty list); non-empty bytes
    /// are a flat INTEGER list; an all-null list keeps its null
    /// member's NULL row. A byte-array element's own suffix sits inside
    /// the brackets and never strips (ADR-0327). `None` member rows — a
    /// nested list (a member that is itself a list or byte array
    /// renders an inner `LIST<`) or a non-list element — skip the
    /// subsumption step.
    fn of(v: &Value) -> ElementType {
        let name = value_type_name(v);
        let members = match v {
            // An empty sequence has no members at all: the empty row set
            // is the marker `is_empty_sequence` reads.
            Value::Bytes(b) if b.is_empty() => Some(HashSet::new()),
            Value::Bytes(_) => Some([("INTEGER".to_string(), true)].into()),
            Value::List(items) => {
                // The non-null members, collected once: their count is
                // the element's own not-null flag. An empty collection
                // is an empty list (no members) or an all-null list
                // (whose one member is the null literal — its row
                // renders NULL and survives the union: live 2026.10.1
                // types valueType([[null],[1]]) as
                // LIST<LIST<NULL> NOT NULL | LIST<INTEGER NOT NULL>>).
                let non_null: Vec<&Value> = items.iter().filter(|it| !it.is_null()).collect();
                let not_null = non_null.len() == items.len();
                if non_null.is_empty() {
                    return match items.is_empty() {
                        true => ElementType {
                            name,
                            members: Some(HashSet::new()),
                        },
                        false => ElementType {
                            name,
                            members: Some([("NULL".to_string(), false)].into()),
                        },
                    };
                }
                // One walk builds the rows and answers the nested-list
                // question: a list/byte-array member yields `None` and
                // `collect` short-circuits on the first one.
                non_null
                    .into_iter()
                    .map(|it| match it {
                        Value::List(_) | Value::Bytes(_) => None,
                        _ => Some((
                            strip_outer_not_null(&value_type_name(it)).to_string(),
                            not_null,
                        )),
                    })
                    .collect()
            }
            _ => None,
        };
        ElementType { name, members }
    }

    /// The non-empty member rows a subsumption candidate carries —
    /// `None` for a nested list, a non-list element, or the
    /// empty-sequence marker (which participates in neither side of the
    /// subsumption step; the union's elision owns it). The one read
    /// path for the member classification.
    fn member_rows(&self) -> Option<&HashSet<(String, bool)>> {
        self.members.as_ref().filter(|rows| !rows.is_empty())
    }

    /// A flat list union element — one that carries member rows; a
    /// nested list or non-list element does not. Derived from the
    /// classified shape, so the sort key can never drift from the
    /// classifier and names stay render-only.
    fn flat(&self) -> bool {
        self.members.is_some()
    }

    /// An empty sequence element — empty bytes or an empty list — the
    /// row the multi-element union elides (live 2026.10.1:
    /// valueType([[],[1]]) keeps only LIST<INTEGER NOT NULL>). The
    /// empty member-row set is the marker; an all-null list is not
    /// empty — its null member renders NULL and survives.
    fn is_empty_sequence(&self) -> bool {
        self.members.as_ref().is_some_and(|rows| rows.is_empty())
    }
}

/// Whether list member set `wide` subsumes `narrow`: every member of
/// `narrow` is in `wide` once the ` NOT NULL` refinement is ignored, and
/// `wide` is strictly wider (a proper superset, or the same names with
/// fewer ` NOT NULL` markers). A list of nullable elements subsumes the
/// same list of non-null elements — live Neo4j 2026.10.1 types
/// `[[1, null], [2]]` as `LIST<LIST<INTEGER> NOT NULL> NOT NULL` and
/// `[[1, 2], [1.0, 1, null]]` as `LIST<LIST<INTEGER | FLOAT>> NOT NULL`.
fn subsumes(wide: &HashSet<(String, bool)>, narrow: &HashSet<(String, bool)>) -> bool {
    wide != narrow
        && narrow.iter().all(|(name, narrow_not_null)| {
            wide.iter().any(|(wide_name, wide_not_null)| {
                wide_name == name && (!wide_not_null || *narrow_not_null)
            })
        })
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

    /// The one list-construction helper the union tests share.
    fn list(values: Vec<Value>) -> Value {
        Value::List(values.into())
    }

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

    /// Nested list types merge by subtype subsumption on the bare member
    /// names, so a wider element union (or a nullable-element list) absorbs
    /// the narrower one. Every expectation is live Neo4j 2026.10.1.
    #[test]
    fn nested_list_types_merge_by_subsumption() {
        let ints = || list(vec![Value::Int(1), Value::Int(2)]);

        // [[1, 2], null, [1.0, 1, null]]: the FLOAT-bearing list absorbs
        // the INTEGER one.
        assert_eq!(
            value_type_name(&list(vec![
                ints(),
                Value::Null,
                list(vec![Value::Float(1.0), Value::Int(1), Value::Null]),
            ])),
            "LIST<LIST<INTEGER | FLOAT>> NOT NULL"
        );
        // [[1, null], [2]]: a nullable-element list subsumes the non-null
        // list of the same member.
        assert_eq!(
            value_type_name(&list(vec![
                list(vec![Value::Int(1), Value::Null]),
                list(vec![Value::Int(2)]),
            ])),
            "LIST<LIST<INTEGER> NOT NULL> NOT NULL"
        );
        // [[1], [2.0], null]: disjoint member sets stay separate.
        assert_eq!(
            value_type_name(&list(vec![
                list(vec![Value::Int(1)]),
                list(vec![Value::Float(2.0)]),
                Value::Null,
            ])),
            "LIST<LIST<INTEGER NOT NULL> | LIST<FLOAT NOT NULL>> NOT NULL"
        );
        // [[], [1], null]: the NOTHING member is dropped beside a sibling.
        assert_eq!(
            value_type_name(&list(vec![
                list(Vec::new()),
                list(vec![Value::Int(1)]),
                Value::Null,
            ])),
            "LIST<LIST<INTEGER NOT NULL>> NOT NULL"
        );
    }

    /// An empty sequence element is elided from a multi-element union;
    /// an all-null element is not — its null member renders NULL and
    /// survives. Every expectation is live Neo4j 2026.10.1 (the
    /// empty-row-set marker vs the surviving NULL row).
    #[test]
    fn unions_elide_empty_sequences_but_keep_all_null_elements() {
        // [[], [1]] and [[null], []]: the empty element drops, the
        // all-null one stays.
        assert_eq!(
            value_type_name(&list(vec![list(Vec::new()), list(vec![Value::Int(1)]),])),
            "LIST<LIST<INTEGER NOT NULL> NOT NULL> NOT NULL"
        );
        assert_eq!(
            value_type_name(&list(vec![list(vec![Value::Null]), list(Vec::new()),])),
            "LIST<LIST<NULL> NOT NULL> NOT NULL"
        );
        // [[null], [1]]: both survive, the flat-list descending order.
        assert_eq!(
            value_type_name(&list(vec![
                list(vec![Value::Null]),
                list(vec![Value::Int(1)]),
            ])),
            "LIST<LIST<NULL> NOT NULL | LIST<INTEGER NOT NULL> NOT NULL> NOT NULL"
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

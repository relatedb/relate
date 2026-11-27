//! The single-stage SEARCH filter subsystem (the inner `WHERE` of a
//! `<source specification clause>`): the RHS value-type gates, the
//! per-search preparation of the lowered [`SearchFilterPred`]s against
//! the input row (`prepare_search_filters`), and the per-candidate gate
//! (`search_filter_passes`) both scans — the vector and the fulltext —
//! drive.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, PropertyMap, Result, Value};
use relate_ir::{SearchFilterPred, SearchOp};
use relate_model::EntityKind;

use crate::eval::{binary, eval, in_op, in_rhs, is_membership_scalar};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// A member that a search filter admits but that never matches: null
/// (absent ids read as null, and a null member leaves membership
/// unknown, exactly like `IN`'s own fold) and the membership scalars
/// ([`is_membership_scalar`]'s one kind-set). Two policies read this
/// one rule: the entity-set member skip, and the comparison filter's
/// right-operand value-type gate.
fn search_filter_rhs_allowed(value: &Value) -> bool {
    value.is_null() || is_membership_scalar(value)
}

/// The filter forms' one member-set accessor: the operand
/// classification's member set, or the typed rejection for a scalar or
/// container right operand — stated once beside
/// [`invalid_filter_rhs`], so the "no member set at all ⇒ the one
/// rejection" policy lives with the rejection it produces.
fn filter_members(value: &Value) -> Result<&[Value]> {
    match in_rhs(value).filter_members() {
        Some(members) => Ok(members),
        None => Err(invalid_filter_rhs()),
    }
}

/// A property-in-set filter's RHS: the operand classification's filter
/// policy — a null operand filters everything out, a list's members
/// must each satisfy the member rule ([`search_filter_rhs_allowed`]:
/// null admitted — a null member leaves membership unknown, exactly
/// like `IN`'s own fold — or a membership scalar), and no member set at
/// all is the rejection.
fn search_in_set_rhs_allowed(value: &Value) -> bool {
    filter_members(value).is_ok_and(|members| members.iter().all(search_filter_rhs_allowed))
}

/// The one rejection of an ill-typed filter RHS: every filter form (a
/// comparison, a property-in-set, an entity-set) reports it with this
/// wording, so a non-list `IN` operand reads the same on both `IN` forms.
fn invalid_filter_rhs() -> Error {
    Error::ty("InvalidValueType: invalid vector search filter RHS type")
}

/// The one missing-property-reads-as-null policy for the value-comparing
/// filter arms; the existence arms read the entry's presence instead.
fn property_or_null(properties: &PropertyMap, property: &str) -> Value {
    properties.get(property).cloned().unwrap_or(Value::Null)
}

/// One CIP-240 filter with its RHS evaluated against the current input
/// row. [`SearchFilterPred`] is the lowered grammar shape; this is the
/// per-search, per-row executable shape.
pub(crate) enum PreparedSearchFilter<'a> {
    Comparison {
        property: &'a str,
        op: SearchOp,
        rhs: Value,
    },
    Exists {
        property: &'a str,
    },
    NotExists {
        property: &'a str,
    },
    InSet {
        property: &'a str,
        rhs: Value,
    },
    /// The candidate's id must be a member of the evaluated id set — the
    /// one prepared form of both `IN` grammar shapes (the entity-set
    /// match filter and its id-spelled twin): the extraction differs
    /// ([`entity_set_ids`] vs [`id_set_ids`]) at preparation, and once
    /// prepared the two forms evaluate identically, so the runtime
    /// carries one variant.
    Ids {
        ids: HashSet<i64>,
    },
}

pub(crate) fn search_filter_passes(
    graph: &ExecGraph<'_>,
    id: i64,
    properties: &PropertyMap,
    filters: &[PreparedSearchFilter<'_>],
) -> Result<bool> {
    for filter in filters {
        let passes = match filter {
            PreparedSearchFilter::Comparison { property, op, rhs } => {
                // The canonical binary comparison (stellar round 1, std-4):
                // no ad-hoc Expr tree for two already-evaluated values. It
                // owns the value-type rules, including durations (no total
                // order, so a range predicate yields Null and is excluded by
                // the truthy gate).
                let left = property_or_null(properties, property);
                binary(graph, (*op).into(), left, rhs.clone())?.truthy() == Some(true)
            }
            PreparedSearchFilter::Exists { property } => properties
                .get(property)
                .is_some_and(|value| !value.is_null()),
            PreparedSearchFilter::NotExists { property } => {
                properties.get(property).is_none_or(Value::is_null)
            }
            PreparedSearchFilter::InSet { property, rhs } => {
                let left = property_or_null(properties, property);
                in_op(&left, rhs)?.truthy() == Some(true)
            }
            PreparedSearchFilter::Ids { ids } => ids.contains(&id),
        };
        if !passes {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The one membership walk both `IN`-form extractions share: the operand
/// classification's filter policy (a null operand matches nothing, a
/// non-list is no member set at all — the caller's typed rejection),
/// the per-member loop, the allowed-scalar skip, and the container
/// rejection — the forms differ only in which members contribute an id,
/// the `id_of` policy.
fn set_ids(value: &Value, id_of: impl Fn(&Value) -> Result<Option<i64>>) -> Result<HashSet<i64>> {
    let members = filter_members(value)?;
    let mut ids = HashSet::new();
    for item in members.iter() {
        if let Some(id) = id_of(item)? {
            ids.insert(id);
        }
    }
    Ok(ids)
}

/// The entity-member policy both `IN`-form extractions share: an entity
/// member contributes its id only when it is the index's own kind (node
/// and relationship ids share one numeric space, so a cross-kind member
/// is a false positive, not a harmless never-match); an allowed scalar
/// member never matches; a container member is the one typed rejection.
fn entity_member_id(item: &Value, kind: EntityKind) -> Result<Option<i64>> {
    match (item, kind) {
        (Value::Node(node), EntityKind::Node) => Ok(Some(node.id)),
        (Value::Relationship(rel), EntityKind::Relationship) => Ok(Some(rel.id)),
        (Value::Node(_) | Value::Relationship(_), _) => Ok(None),
        (item, _) if search_filter_rhs_allowed(item) => Ok(None),
        _ => Err(invalid_filter_rhs()),
    }
}

/// The entity-set members of an `EntityInSet` filter, as candidate ids of
/// the index's own entity kind: the shared entity-member policy, with no
/// id rule of its own. `in_rhs` supplies `IN`'s one operand policy — a
/// null operand matches nothing (`x IN null` is null, so an empty set
/// carries the same outcome).
fn entity_set_ids(value: &Value, kind: EntityKind) -> Result<HashSet<i64>> {
    set_ids(value, |item| entity_member_id(item, kind))
}

/// The id-set members of an `IdInSet` filter: the shared entity-member
/// policy plus the one id rule of its own — an integer member is an id
/// (the predicate compares ids, not entities, so an explicit integer is
/// kind-agnostic).
fn id_set_ids(value: &Value, kind: EntityKind) -> Result<HashSet<i64>> {
    set_ids(value, |item| match item {
        Value::Int(id) => Ok(Some(*id)),
        item => entity_member_id(item, kind),
    })
}

/// The single-stage filter preparation: the index property membership
/// check (the one catalog-dependent check — a VECTOR source's additional
/// `WITH [...]` set is the single-stage filter's declared properties, so
/// a property outside it is the wrong-property rejection; a FULLTEXT
/// source's DDL declares no additional set, so its inner WHERE reads the
/// entity's own properties, whatever they are), the RHS value-type
/// gate, and the per-search RHS evaluation. The binding-variable and
/// RHS-shape checks fired at lowering, the per-property combination
/// limits and the comparison-operator ban at the grammar walk.
pub(crate) fn prepare_search_filters<'a>(
    filters: &'a [SearchFilterPred],
    detail: &crate::redo::IndexDetailSpec,
    graph: &ExecGraph<'_>,
    input: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<(EntityKind, Vec<PreparedSearchFilter<'a>>)> {
    // The entity kind is the detail's own fact, derived once here and
    // returned with the prepared predicates: every caller materializes
    // with the returned kind, no site re-derives it beside the same read.
    let kind = detail.entity_kind();
    let additional: HashSet<_> = detail.with_properties.iter().map(String::as_str).collect();
    let vector_source = detail.kind == crate::redo::IndexKindSpec::Vector;
    let mut evaluated = Vec::with_capacity(filters.len());
    for filter in filters {
        // The catalog-dependent property check runs first: it keeps its
        // error precedence over RHS evaluation, and only a VECTOR source
        // pays it — the additional-properties set is the vector index's
        // own declaration, one stated guard.
        if vector_source
            && filter
                .property_name()
                .is_some_and(|property| !additional.contains(property))
        {
            return Err(Error::execution("WrongPropertyForVectorSearchFilter"));
        }
        // Each arm destructures its own variant's rhs and evaluates it
        // there (stellar round 2, F-2-27): the variant's shape is the
        // one decode — no pre-evaluated Option, no transpose, no
        // per-arm expect re-asserting what the match already proved.
        let prepared = match filter {
            SearchFilterPred::Comparison { property, op, rhs } => {
                let value = eval(graph, input, rhs, params)?;
                if !search_filter_rhs_allowed(&value) {
                    return Err(invalid_filter_rhs());
                }
                PreparedSearchFilter::Comparison {
                    property,
                    op: *op,
                    rhs: value,
                }
            }
            SearchFilterPred::Exists { property } => PreparedSearchFilter::Exists { property },
            SearchFilterPred::NotExists { property } => {
                PreparedSearchFilter::NotExists { property }
            }
            SearchFilterPred::InSet { property, rhs } => {
                let value = eval(graph, input, rhs, params)?;
                if !search_in_set_rhs_allowed(&value) {
                    return Err(invalid_filter_rhs());
                }
                PreparedSearchFilter::InSet {
                    property,
                    rhs: value,
                }
            }
            SearchFilterPred::EntityInSet { rhs } => PreparedSearchFilter::Ids {
                ids: entity_set_ids(&eval(graph, input, rhs, params)?, kind)?,
            },
            SearchFilterPred::IdInSet { rhs, .. } => PreparedSearchFilter::Ids {
                ids: id_set_ids(&eval(graph, input, rhs, params)?, kind)?,
            },
        };
        evaluated.push(prepared);
    }
    Ok((kind, evaluated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duration has no total order, so a range filter over one excludes
    /// the candidate while an equality still compares (CIP-240 value-type
    /// rules) — pinning the guard search_filter_passes carries.
    #[test]
    fn duration_range_filter_excludes_the_candidate() {
        use relate_core::Temporal;
        let duration = || {
            Value::Temporal(Temporal::Duration {
                months: 0,
                days: 1,
                nanos: 0,
            })
        };
        let mut properties = PropertyMap::new();
        properties.insert("ttl".into(), duration());
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let range = vec![PreparedSearchFilter::Comparison {
            property: "ttl",
            op: SearchOp::Gt,
            rhs: duration(),
        }];
        assert!(
            !search_filter_passes(&graph, 1, &properties, &range).unwrap(),
            "a range over a duration filters the candidate out"
        );
        let equality = vec![PreparedSearchFilter::Comparison {
            property: "ttl",
            op: SearchOp::Eq,
            rhs: duration(),
        }];
        assert!(search_filter_passes(&graph, 1, &properties, &equality).unwrap());
    }

    /// The four non-comparison filter shapes: existence checks the
    /// additional property, membership rides `IN`'s own null-aware fold,
    /// and the entity-set filter matches the candidate id (null and scalar
    /// members never do).
    #[test]
    fn existence_membership_and_entity_set_filters() {
        let mut properties = PropertyMap::new();
        properties.insert("rating".into(), Value::Float(7.5));
        properties.insert("nulls".into(), Value::Null);
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let passes = |filter: PreparedSearchFilter<'_>| {
            search_filter_passes(&graph, 42, &properties, &[filter]).unwrap()
        };

        assert!(passes(PreparedSearchFilter::Exists { property: "rating" }));
        assert!(!passes(PreparedSearchFilter::Exists {
            property: "missing"
        }));
        assert!(!passes(PreparedSearchFilter::Exists { property: "nulls" }));
        assert!(passes(PreparedSearchFilter::NotExists {
            property: "missing"
        }));
        assert!(passes(PreparedSearchFilter::NotExists {
            property: "nulls"
        }));
        assert!(!passes(PreparedSearchFilter::NotExists {
            property: "rating"
        }));

        let in_set = |members: Vec<Value>| PreparedSearchFilter::InSet {
            property: "rating",
            rhs: Value::List(members.into()),
        };
        assert!(passes(in_set(vec![Value::Float(7.5), Value::Null])));
        assert!(!passes(in_set(vec![Value::Float(7.6)])));

        assert!(passes(PreparedSearchFilter::Ids {
            ids: HashSet::from([1, 42])
        }));
        assert!(!passes(PreparedSearchFilter::Ids {
            ids: HashSet::from([1, 2])
        }));
    }

    /// Entity-set extraction keeps only the index kind's own entities;
    /// nulls and scalars are members that can never match an entity.
    #[test]
    fn entity_set_ids_filter_by_candidate_kind() {
        let node = |id| {
            Value::Node(std::sync::Arc::new(relate_core::Node {
                id,
                labels: std::sync::Arc::new(Vec::new()),
                properties: std::sync::Arc::new(PropertyMap::new()),
            }))
        };
        let value = Value::List(vec![node(1), Value::Null, Value::Int(3)].into());
        assert_eq!(
            entity_set_ids(&value, EntityKind::Node).unwrap(),
            HashSet::from([1])
        );
        assert!(entity_set_ids(&value, EntityKind::Relationship)
            .unwrap()
            .is_empty());
        // `x IN null` matches nothing, not an error.
        assert!(entity_set_ids(&Value::Null, EntityKind::Node)
            .unwrap()
            .is_empty());
    }

    /// A member that is neither an entity nor a scalar is the same typed
    /// rejection on both `IN` forms.
    #[test]
    fn container_in_member_is_rejected_on_both_forms() {
        let nested =
            || Value::List(vec![Value::Int(1), Value::List(vec![Value::Int(2)].into())].into());
        assert!(!search_in_set_rhs_allowed(&nested()));
        let err = entity_set_ids(&nested(), EntityKind::Node).unwrap_err();
        assert_eq!(err.to_string(), invalid_filter_rhs().to_string());
    }

    /// A non-list `IN` operand is one typed rejection on both `IN` forms
    /// (the property-in-set gate and the entity-set extraction), not two
    /// different wordings.
    #[test]
    fn non_list_in_rhs_has_one_wording_on_both_forms() {
        let expected = invalid_filter_rhs().to_string();
        let err = entity_set_ids(&Value::Int(3), EntityKind::Node).unwrap_err();
        assert_eq!(err.to_string(), expected);

        let detail = crate::redo::IndexDetailSpec {
            kind: crate::redo::IndexKindSpec::Vector,
            relationship: false,
            labels: vec!["N".into()],
            properties: vec!["v".into()],
            with_properties: vec!["p".into()],
        };
        let g = crate::graph::Graph::default();
        let graph = ExecGraph::read(&g);
        let row = Row::default();
        let params = BTreeMap::new();
        let filters = [SearchFilterPred::InSet {
            property: "p".into(),
            rhs: relate_ir::Expr::Int(3),
        }];
        let err = prepare_search_filters(&filters, &detail, &graph, &row, &params)
            .err()
            .expect("a non-list property IN operand is rejected");
        assert_eq!(err.to_string(), expected);
    }
}

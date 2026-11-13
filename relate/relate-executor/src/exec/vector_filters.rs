//! The CIP-240 vector-search filter subsystem: the RHS value-type gates,
//! the per-search preparation of the lowered [`SearchFilterPred`]s against
//! the input row (`prepare_vector_filters`), and the per-candidate gate
//! (`vector_filter_passes`) the scan in `super::vector` drives.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, PropertyMap, Result, Value};
use relate_ir::{SearchFilterPred, SearchOp};
use relate_model::EntityKind;

use crate::eval::{binary, eval, in_op, in_rhs};
use crate::overlay::ExecGraph;
use crate::row::Row;

fn vector_filter_rhs_allowed(value: &Value) -> bool {
    matches!(
        value,
        Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::String(_)
            | Value::Temporal(_)
    )
}

/// A property-in-set filter's RHS: a null operand is accepted (it filters
/// everything out through `IN`'s null), a list's members must each be a
/// scalar (a null member leaves membership unknown, exactly like `IN`'s
/// own fold). A non-list or nested container is rejected, matching Neo4j's
/// list/element type rule.
fn vector_in_set_rhs_allowed(value: &Value) -> bool {
    match in_rhs(value) {
        Ok(None) => true,
        Ok(Some(items)) => items.iter().all(vector_filter_rhs_allowed),
        Err(_) => false,
    }
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
pub(super) enum PreparedVectorFilter<'a> {
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
    /// The candidate entity's id must be a member of the evaluated set.
    /// Non-entity members can never equal the candidate and null members
    /// leave membership unknown, so neither contributes an id.
    EntityInSet {
        ids: HashSet<i64>,
    },
}

pub(super) fn vector_filter_passes(
    graph: &ExecGraph<'_>,
    id: i64,
    properties: &PropertyMap,
    filters: &[PreparedVectorFilter<'_>],
) -> Result<bool> {
    for filter in filters {
        let passes = match filter {
            PreparedVectorFilter::Comparison { property, op, rhs } => {
                // The canonical binary comparison (stellar round 1, std-4):
                // no ad-hoc Expr tree for two already-evaluated values. It
                // owns the value-type rules, including durations (no total
                // order, so a range predicate yields Null and is excluded by
                // the truthy gate).
                let left = property_or_null(properties, property);
                binary(graph, (*op).into(), left, rhs.clone())?.truthy() == Some(true)
            }
            PreparedVectorFilter::Exists { property } => properties
                .get(property)
                .is_some_and(|value| !value.is_null()),
            PreparedVectorFilter::NotExists { property } => {
                properties.get(property).is_none_or(Value::is_null)
            }
            PreparedVectorFilter::InSet { property, rhs } => {
                let left = property_or_null(properties, property);
                in_op(left, rhs.clone())?.truthy() == Some(true)
            }
            PreparedVectorFilter::EntityInSet { ids } => ids.contains(&id),
        };
        if !passes {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The entity-set members of an `EntityInSet` filter, as candidate ids of
/// the index's own entity kind. `in_rhs` supplies `IN`'s one operand
/// policy: a null operand matches nothing (`x IN null` is null, so an empty
/// set carries the same outcome), and members of the other entity kind can
/// never equal a candidate.
fn entity_set_ids(value: &Value, kind: EntityKind) -> Result<HashSet<i64>> {
    let Some(members) = in_rhs(value).map_err(|_| invalid_filter_rhs())? else {
        return Ok(HashSet::new());
    };
    let mut ids = HashSet::new();
    for item in members {
        match (item, kind) {
            (Value::Node(node), EntityKind::Node) => {
                ids.insert(node.id);
            }
            (Value::Relationship(rel), EntityKind::Relationship) => {
                ids.insert(rel.id);
            }
            _ => {}
        }
    }
    Ok(ids)
}

/// The vector filter preparation: the index property membership check
/// (the one catalog-dependent check), the RHS value-type gate, and the
/// per-search RHS evaluation. The binding-variable and RHS-shape checks
/// fired at lowering, the per-property combination limits and the
/// comparison-operator ban at the grammar walk.
pub(super) fn prepare_vector_filters<'a>(
    filters: &'a [SearchFilterPred],
    detail: &crate::redo::IndexDetailSpec,
    kind: EntityKind,
    graph: &ExecGraph<'_>,
    input: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<PreparedVectorFilter<'a>>> {
    let additional: HashSet<_> = detail.with_properties.iter().map(String::as_str).collect();
    let mut evaluated = Vec::with_capacity(filters.len());
    for filter in filters {
        // The catalog-dependent property check runs first: it keeps its
        // error precedence over RHS evaluation.
        if let Some(property) = filter.property_name() {
            if !additional.contains(property) {
                return Err(Error::execution("WrongPropertyForVectorSearchFilter"));
            }
        }
        // Each arm destructures its own variant's rhs and evaluates it
        // there (stellar round 2, F-2-27): the variant's shape is the
        // one decode — no pre-evaluated Option, no transpose, no
        // per-arm expect re-asserting what the match already proved.
        let prepared = match filter {
            SearchFilterPred::Comparison { property, op, rhs } => {
                let value = eval(graph, input, rhs, params)?;
                if !vector_filter_rhs_allowed(&value) {
                    return Err(invalid_filter_rhs());
                }
                PreparedVectorFilter::Comparison {
                    property,
                    op: *op,
                    rhs: value,
                }
            }
            SearchFilterPred::Exists { property } => PreparedVectorFilter::Exists { property },
            SearchFilterPred::NotExists { property } => {
                PreparedVectorFilter::NotExists { property }
            }
            SearchFilterPred::InSet { property, rhs } => {
                let value = eval(graph, input, rhs, params)?;
                if !vector_in_set_rhs_allowed(&value) {
                    return Err(invalid_filter_rhs());
                }
                PreparedVectorFilter::InSet {
                    property,
                    rhs: value,
                }
            }
            SearchFilterPred::EntityInSet { rhs } => PreparedVectorFilter::EntityInSet {
                ids: entity_set_ids(&eval(graph, input, rhs, params)?, kind)?,
            },
        };
        evaluated.push(prepared);
    }
    Ok(evaluated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duration has no total order, so a range filter over one excludes
    /// the candidate while an equality still compares (CIP-240 value-type
    /// rules) — pinning the guard vector_filter_passes carries.
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
        let range = vec![PreparedVectorFilter::Comparison {
            property: "ttl",
            op: SearchOp::Gt,
            rhs: duration(),
        }];
        assert!(
            !vector_filter_passes(&graph, 1, &properties, &range).unwrap(),
            "a range over a duration filters the candidate out"
        );
        let equality = vec![PreparedVectorFilter::Comparison {
            property: "ttl",
            op: SearchOp::Eq,
            rhs: duration(),
        }];
        assert!(vector_filter_passes(&graph, 1, &properties, &equality).unwrap());
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
        let passes = |filter: PreparedVectorFilter<'_>| {
            vector_filter_passes(&graph, 42, &properties, &[filter]).unwrap()
        };

        assert!(passes(PreparedVectorFilter::Exists { property: "rating" }));
        assert!(!passes(PreparedVectorFilter::Exists {
            property: "missing"
        }));
        assert!(!passes(PreparedVectorFilter::Exists { property: "nulls" }));
        assert!(passes(PreparedVectorFilter::NotExists {
            property: "missing"
        }));
        assert!(passes(PreparedVectorFilter::NotExists {
            property: "nulls"
        }));
        assert!(!passes(PreparedVectorFilter::NotExists {
            property: "rating"
        }));

        let in_set = |members: Vec<Value>| PreparedVectorFilter::InSet {
            property: "rating",
            rhs: Value::List(members.into()),
        };
        assert!(passes(in_set(vec![Value::Float(7.5), Value::Null])));
        assert!(!passes(in_set(vec![Value::Float(7.6)])));

        assert!(passes(PreparedVectorFilter::EntityInSet {
            ids: HashSet::from([1, 42])
        }));
        assert!(!passes(PreparedVectorFilter::EntityInSet {
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
        let err =
            prepare_vector_filters(&filters, &detail, EntityKind::Node, &graph, &row, &params)
                .err()
                .expect("a non-list property IN operand is rejected");
        assert_eq!(err.to_string(), expected);
    }
}

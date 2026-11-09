//! Vector-index scan orchestration: the operand and CIP-240 filter
//! preparation, the stored-config read (`stored_config`, the create side's
//! mirror in `super::ddl_options`), and the per-candidate scoring the
//! SEARCH clause machinery (`super::search`) layers over the crate-root
//! similarity kernels ([`crate::vector_similarity`]) and the shared scan
//! contract — the index population, ranked ordering and bounded top-k
//! window — in [`crate::exec::candidates`].

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, PropertyMap, Result, Value};
use relate_ir::{SearchFilterPred, SearchOp};
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::eval::{binary, eval, in_op, in_rhs};
use crate::exec::candidates::{
    index_population, materialize_candidate, resolve_search_index, TopWindow,
};
use crate::exec::ddl_options::index_config;
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::vector_config::{parse_config, StorableMetric, StoredVectorConfig};
use crate::vector_similarity::{similarity_values, value_coord, value_coords, PreparedQuery};

pub(crate) fn vector_operand(value: &Value) -> Result<Option<Vec<f64>>> {
    let values = match value {
        Value::Null => return Ok(None),
        Value::Vector { values, .. } | Value::List(values) => values,
        _ => {
            return Err(Error::ty(
                "InvalidValueType: vector search requires a numeric list or VECTOR",
            ))
        }
    };
    if values.is_empty() {
        return Err(Error::ty(
            "InvalidValueType: vector search requires a non-empty vector",
        ));
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values.iter() {
        out.push(value_coord(value).ok_or_else(|| {
            Error::ty("InvalidValueType: vector coordinates must be finite numbers")
        })?);
    }
    Ok(Some(out))
}

pub(crate) fn vector_filter_rhs_allowed(value: &Value) -> bool {
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

/// The one missing-property-reads-as-null policy for the value-comparing
/// filter arms; the existence arms read the entry's presence instead.
fn property_or_null(properties: &PropertyMap, property: &str) -> Value {
    properties.get(property).cloned().unwrap_or(Value::Null)
}

/// One CIP-240 filter with its RHS evaluated against the current input
/// row. [`SearchFilterPred`] is the lowered grammar shape; this is the
/// per-search, per-row executable shape.
enum PreparedVectorFilter<'a> {
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

fn vector_filter_passes(
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
    let Some(members) = in_rhs(value)? else {
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

/// One drift contract for every invalid stored index option: the
/// create-time validation (`super::ddl_options::create_config`)
/// rejects these shapes on input, so a stored entry that still carries
/// one is catalog drift, never a silently unconfigured index.
fn invalid_index_option(index_name: &str) -> Error {
    Error::execution(format!(
        "InvalidIndexDefinition: vector index {index_name} has an invalid indexConfig option"
    ))
}

/// The stored read of a vector index's options: navigate the outer
/// `indexConfig` (the one [`index_config`] boundary, shared with the
/// create side) and reuse the root schema's one parse
/// ([`crate::vector_config::parse_config`]), mapping any failure onto
/// the drift contract — a stored entry a CREATE would have rejected is
/// catalog drift.
fn stored_config(
    options: &BTreeMap<String, Value>,
    index_name: &str,
) -> Result<StoredVectorConfig> {
    // Absent is the parse over the empty config (the documented
    // defaults), and every failure folds onto the drift contract.
    let empty = BTreeMap::new();
    let config = index_config(options)
        .map_err(|_| invalid_index_option(index_name))?
        .unwrap_or(&empty);
    parse_config(config).map_err(|_| invalid_index_option(index_name))
}

/// The catalog-descriptor phase: fetch the index detail and options,
/// validate the kind and `indexConfig` leaves, and parse the metric and
/// the one indexed property — everything the scan needs before a
/// candidate is ever scored.
fn resolve_vector_index(
    graph: &ExecGraph<'_>,
    index_name: &str,
    query: &[f64],
) -> Result<(crate::redo::IndexDetailSpec, String, StorableMetric)> {
    // The shared SEARCH index resolution (stellar round 3, std-6): the
    // schema read, the missing-index policy, and the wrong-kind
    // rejection are the one spelling in `exec::candidates`, shared with
    // the fulltext path.
    let (detail, options) =
        resolve_search_index(graph, index_name, crate::redo::IndexKindSpec::Vector)?;
    let config = stored_config(&options, index_name)?;
    if config
        .dimension
        .is_some_and(|dimension| dimension != query.len())
    {
        return Err(Error::ty("VectorIndexDimensionalityMismatch"));
    }
    let metric = config.metric;
    // A vector index always indexes exactly one property (catalog DDL
    // invariant) — no fabricated empty property name, and a drifted
    // multi-property detail fails loudly instead of first() quietly
    // picking a winner (stellar round 1, std-5).
    if detail.properties.len() != 1 {
        return Err(Error::execution(format!(
            "InvalidIndexDefinition: vector index {index_name} must index exactly one property"
        )));
    }
    let property = detail.properties[0].clone();
    Ok((detail, property, metric))
}

/// The CIP-240 filter preparation: Rule 2 (index property membership --
/// the one catalog-dependent rule), the RHS value-type gate, and the
/// per-search RHS evaluation. Rules 1/6 fired at lowering, Rules 3/7 at
/// the grammar walk.
fn prepare_vector_filters<'a>(
    filters: &'a [SearchFilterPred],
    detail: &crate::redo::IndexDetailSpec,
    kind: EntityKind,
    graph: &ExecGraph<'_>,
    input: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Vec<PreparedVectorFilter<'a>>> {
    let additional: HashSet<_> = detail.with_properties.iter().map(String::as_str).collect();
    let invalid_rhs = || Error::ty("InvalidValueType: invalid vector search filter RHS type");
    let mut evaluated = Vec::with_capacity(filters.len());
    for filter in filters {
        // Rule 2 first: the catalog-dependent property check keeps its
        // error precedence over RHS evaluation.
        if let Some(property) = filter.property_name() {
            if !additional.contains(property) {
                return Err(Error::execution("WrongPropertyForVectorSearchFilter"));
            }
        }
        let value = filter
            .rhs()
            .map(|rhs| eval(graph, input, rhs, params, cypher_version))
            .transpose()?;
        let prepared = match filter {
            SearchFilterPred::Comparison { property, op, .. } => {
                let value = value.expect("a comparison predicate carries a value RHS");
                if !vector_filter_rhs_allowed(&value) {
                    return Err(invalid_rhs());
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
            SearchFilterPred::InSet { property, .. } => {
                let value = value.expect("an in-set predicate carries a value RHS");
                if !vector_in_set_rhs_allowed(&value) {
                    return Err(invalid_rhs());
                }
                PreparedVectorFilter::InSet {
                    property,
                    rhs: value,
                }
            }
            SearchFilterPred::EntityInSet { .. } => PreparedVectorFilter::EntityInSet {
                ids: entity_set_ids(
                    &value.expect("an entity-set predicate carries a value RHS"),
                    kind,
                )?,
            },
        };
        evaluated.push(prepared);
    }
    Ok(evaluated)
}

/// Bundled CIP-240 vector-search inputs: the decomposed filter
/// predicates (attached at lowering, Rules 1/6 already enforced there)
/// and the row their RHS expressions are evaluated against. (Bundled to
/// avoid `clippy::too_many_arguments`.)
pub(crate) struct VectorSearchFilter<'a> {
    pub(crate) filters: &'a [SearchFilterPred],
    pub(crate) input: &'a Row,
}

pub(crate) fn vector_index_candidates(
    ctx: ExecCtx<'_>,
    index_name: &str,
    query: &[f64],
    offset: usize,
    limit: usize,
    filter: VectorSearchFilter<'_>,
) -> Result<(EntityKind, Vec<(Value, f64)>)> {
    let ExecCtx {
        graph,
        params,
        cypher_version,
        ..
    } = ctx;
    let VectorSearchFilter { filters, input } = filter;
    let (detail, property, metric) = resolve_vector_index(graph, index_name, query)?;
    // The per-search query facts (ADR-0256 hoist, stellar round 2 bundle):
    // norm and zero-guard are shared by every candidate's cosine.
    let prepared = PreparedQuery::prepare(query);
    let kind = EntityKind::from_relationship_flag(detail.relationship);
    let evaluated_filters =
        prepare_vector_filters(filters, &detail, kind, graph, input, params, cypher_version)?;
    // ADR-0256: the unfiltered path never materializes a `Value` per scored
    // candidate — only the ≤ offset+limit survivors become entities below.
    // Filtered (CIP-235 `WITH`) searches still evaluate properties per
    // scored candidate and take the same flat path.
    // The one per-candidate scan body: indexed-property read, coordinate
    // parse, score, filter gate. The entity-kind arms below gate on
    // labels themselves (the rel arm via rel_type, the node arm beside
    // its computation) and drive this once.
    let candidate_score = |id: i64, properties: &PropertyMap| -> Result<Option<f64>> {
        let Some(value) = properties.get(&property) else {
            return Ok(None);
        };
        let Some(coords) = value_coords(value) else {
            return Ok(None);
        };
        let Some(score) = similarity_values(metric, &prepared, coords) else {
            return Ok(None);
        };
        if !vector_filter_passes(graph, id, properties, &evaluated_filters)? {
            return Ok(None);
        }
        Ok(Some(score))
    };
    let mut top = TopWindow::new(offset, limit);
    // The one fetch-gate-push tail both enumeration arms drive.
    let mut keep = |id: i64, properties: &PropertyMap| -> Result<()> {
        if let Some(score) = candidate_score(id, properties)? {
            top.push(score, id);
        }
        Ok(())
    };
    // The shared index population (exec::candidates): the node arm rides
    // the exact per-label enumeration, the relationship arm the type-first
    // sweep — both already enumerated, so a materialization miss there is
    // that contract's invariant.
    for (id, properties) in index_population(graph, &detail) {
        keep(id, &properties)?;
    }
    let rows = top.into_sorted_rows();
    let mut candidates = Vec::with_capacity(rows.len());
    for (id, score) in rows {
        candidates.push((materialize_candidate(graph, kind, id), score));
    }
    Ok((kind, candidates))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::candidates::ranked_better_cmp;

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

    /// Deterministic LCG: fixed corpus across runs and platforms.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *state >> 11
    }

    /// Brute-force reference for the CIP-225 window: full sort by
    /// `ranked_better_cmp`, drop `offset`, keep `limit`.
    fn reference_window(rows: &[(i64, f64)], offset: usize, limit: usize) -> Vec<(i64, f64)> {
        let mut sorted = rows.to_vec();
        sorted.sort_by(|(left_id, left_score), (right_id, right_score)| {
            ranked_better_cmp(*right_score, *right_id, *left_score, *left_id)
        });
        sorted.drain(..offset.min(sorted.len()));
        sorted.truncate(limit);
        sorted
    }

    /// ADR-0256: the bounded `TopWindow` collector produces exactly the rows
    /// the full-collect `reference_window` oracle produces — score
    /// descending, element id ascending on ties — across window sizes,
    /// offsets, ties, and NaN-scored windows.
    #[test]
    fn top_window_matches_full_sort_reference() {
        // One window-sweep spelling for every corpus below (stellar
        // round 3, std-2): push all rows, then assert the bounded
        // collector equals the full-sort oracle across the six window
        // shapes. NaN != NaN under `==`, so the assertion compares
        // score bit patterns — identical to `==` for the finite corpus.
        let sweep = |rows: &[(i64, f64)], label: &str| {
            let as_bits = |window: Vec<(i64, f64)>| -> Vec<(i64, u64)> {
                window
                    .into_iter()
                    .map(|(id, score)| (id, score.to_bits()))
                    .collect()
            };
            for &(offset, limit) in &[(0usize, 10usize), (0, 1), (5, 7), (3, 0), (0, 500), (17, 3)]
            {
                let mut top = TopWindow::new(offset, limit);
                for &(id, score) in rows {
                    top.push(score, id);
                }
                assert_eq!(
                    as_bits(top.into_sorted_rows()),
                    as_bits(reference_window(rows, offset, limit)),
                    "{label} n={} offset={offset} limit={limit}",
                    rows.len()
                );
            }
        };
        let mut state = 0x5eed_cafe_f00d_bee5u64;
        for trial in 0..50 {
            let n = 1 + (lcg(&mut state) % 200) as usize;
            // Repeated scores force the element-id tiebreak. Non-finite
            // scores are reachable in a real scan (stellar round 3,
            // std-5): a nonzero query whose coordinate squares underflow
            // (e.g. `[1e-320, 0.0]`) hoists a zero norm and the cosine
            // divides by it to NaN — the all-zero guard up front does
            // not cover it. NaN ties every finite score in
            // `ranked_better_cmp`, so a mixed finite/NaN corpus has no
            // transitive order (no consistent full-sort oracle); the
            // all-NaN corpus below pins the tie path where the
            // comparator is the pure element-id order.
            let rows: Vec<(i64, f64)> = (0..n as i64)
                .map(|id| {
                    let score = match lcg(&mut state) % 10 {
                        1 => 0.5,
                        2 => 1.0,
                        _ => (lcg(&mut state) % 1000) as f64 / 997.0,
                    };
                    (id, score)
                })
                .collect();
            sweep(&rows, &format!("trial={trial}"));
            // The NaN-scored corpus: every row NaN, so the score tie
            // (`partial_cmp` → `Equal`) leaves the element-id tiebreak
            // as the whole order — one total order the heap and the
            // full-sort oracle share.
            let nan_rows: Vec<(i64, f64)> = (0..n as i64).map(|id| (id, f64::NAN)).collect();
            sweep(&nan_rows, "all-NaN");
        }
    }
}

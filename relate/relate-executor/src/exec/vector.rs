//! Vector-index scan orchestration: the operand, the stored-config read (`stored_config`, the create side's
//! mirror in `super::ddl_options`), and the per-candidate scoring the
//! SEARCH clause machinery (`super::search`) layers over the crate-root
//! similarity kernels ([`crate::vector_similarity`]) and the shared scan
//! contract — the index population, ranked ordering and bounded top-k
//! window — in [`crate::exec::candidates`]. The CIP-240 filter subsystem
//! lives in [`super::vector_filters`].

use std::collections::BTreeMap;

use relate_core::{Error, PropertyMap, Result, Value};
use relate_ir::SearchFilterPred;
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::exec::candidates::{
    index_population, materialize_candidate, resolve_search_index, TopWindow,
};
use crate::exec::ddl_options::index_config;
use crate::exec::vector_filters::{prepare_vector_filters, vector_filter_passes};
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

/// Bundled CIP-240 vector-search inputs: the decomposed filter
/// predicates (attached at lowering, the binding-variable and RHS-shape checks already enforced there)
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
    let ExecCtx { graph, params, .. } = ctx;
    let VectorSearchFilter { filters, input } = filter;
    let (detail, property, metric) = resolve_vector_index(graph, index_name, query)?;
    // The per-search query facts (ADR-0256 hoist, stellar round 2 bundle):
    // norm and zero-guard are shared by every candidate's cosine.
    let prepared = PreparedQuery::prepare(query);
    let kind = EntityKind::from_relationship_flag(detail.relationship);
    let evaluated_filters = prepare_vector_filters(filters, &detail, kind, graph, input, params)?;
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

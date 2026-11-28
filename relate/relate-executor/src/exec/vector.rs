//! Vector-index scan orchestration: the operand, the stored-config read (`stored_config`, the create side's
//! mirror in `super::ddl_options`), and the per-candidate scoring the
//! SEARCH clause machinery (`super::search`) layers over the crate-root
//! similarity kernels ([`crate::vector_similarity`]) and the shared scan
//! contract — the index population, ranked ordering and bounded top-k
//! window — in [`crate::exec::candidates`]. The CIP-240 filter subsystem
//! lives in [`super::search_filters`].

use std::collections::BTreeMap;

use relate_core::{Error, PropertyMap, Result, Value};
use relate_ir::SearchIndexSpec;
use relate_model::EntityKind;

use crate::ctx::ExecCtx;
use crate::exec::candidates::{
    expect_enumerated_candidate, index_population, ranked_candidates, resolve_search_index,
    TopWindow,
};
use crate::exec::ddl_options::index_config;
use crate::exec::search_filters::{
    prepare_search_filters, search_filter_passes, PreparedSearchFilter,
};
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::vector_config::{StorableMetric, StoredVectorConfig};
use crate::vector_similarity::{similarity_row, value_coords, PreparedQuery};

pub(crate) fn vector_operand(value: &Value) -> Result<Option<Vec<f64>>> {
    // The query side converts once per search (ADR-0256) through the one
    // candidate reader (ADR-0706): a packed VECTOR widens out of its
    // family, a list keeps the per-element read — both with the scan's
    // own skip contract for non-numbers.
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let Some(coords) = value_coords(value) else {
        return Err(Error::ty(
            "InvalidValueType: vector search requires a numeric list or VECTOR",
        ));
    };
    let values = coords
        .to_f64_vec()
        .ok_or_else(|| Error::ty("InvalidValueType: vector coordinates must be finite numbers"))?;
    if values.is_empty() {
        return Err(Error::ty(
            "InvalidValueType: vector search requires a non-empty vector",
        ));
    }
    Ok(Some(values))
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
/// create side) and reuse the root schema's stored read
/// ([`crate::vector_config::parse_stored_config`], the one parse plus
/// its legacy fold), mapping any failure onto the drift contract — a
/// stored entry a CREATE would have rejected is catalog drift.
fn stored_config(
    options: &BTreeMap<String, Value>,
    index_name: &str,
) -> Result<StoredVectorConfig> {
    // Absent is the parse over the empty config (the documented
    // defaults), and every failure folds onto the drift contract —
    // through the stored read's parse, whose one legacy fold keeps
    // indexes older builds created queryable.
    let empty = BTreeMap::new();
    let config = index_config(options)
        .map_err(|_| invalid_index_option(index_name))?
        .unwrap_or(&empty);
    crate::vector_config::parse_stored_config(config).map_err(|_| invalid_index_option(index_name))
}

/// The catalog-descriptor phase: fetch the index detail and options,
/// validate the kind and `indexConfig` leaves, and parse the metric and
/// the one indexed property — everything the scan needs before a
/// candidate is ever scored.
pub(crate) fn resolve_vector_index(
    graph: &ExecGraph<'_>,
    index_name: &str,
    query: &[f64],
) -> Result<VectorScan> {
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
    Ok(VectorScan {
        detail,
        property,
        metric,
    })
}

/// The resolved facts one vector scan needs, from the one canonical
/// resolver ([`resolve_vector_index`]): the catalog detail (whose
/// relationship flag is the entity kind's fact — one derivation,
/// [`crate::redo::IndexDetailSpec::entity_kind`] — and whose labels gate
/// the population), the single indexed property, and the stored
/// similarity metric.
pub(crate) struct VectorScan {
    pub(crate) detail: crate::redo::IndexDetailSpec,
    pub(crate) property: String,
    pub(crate) metric: StorableMetric,
}

/// The SEARCH clause's vector candidate generation — the fulltext
/// orchestration's twin ([`super::fulltext::index_candidates`]): index
/// resolution, the CIP-240 decomposed predicates prepared against the
/// input row, the similarity scan, survivor materialization. The
/// preparation returns the entity kind with the prepared predicates —
/// the one derivation — and the returned candidates materialize with
/// that kind.
pub(crate) fn index_candidates(
    ctx: ExecCtx<'_>,
    index: &SearchIndexSpec,
    input: &Row,
    query: &[f64],
    window: usize,
) -> Result<(EntityKind, Vec<(Value, f64)>)> {
    let ExecCtx { graph, params, .. } = ctx;
    let scan = resolve_vector_index(graph, &index.index_name, query)?;
    let (kind, prepared) =
        prepare_search_filters(&index.filters, &scan.detail, graph, input, params)?;
    let candidates = scan_candidates(ctx, &scan, query, window, kind, &prepared)?;
    Ok((kind, candidates))
}

/// The bare similarity scan over resolved facts: every live entity the
/// index's population covers, scored per candidate and gated by the
/// prepared filters, the best `limit` survivors materialized through the
/// one ranked composition. The kind is the caller's — the SEARCH
/// orchestration passes the preparation's returned value, a procedure
/// caller the detail's own.
pub(crate) fn scan_candidates(
    ctx: ExecCtx<'_>,
    scan: &VectorScan,
    query: &[f64],
    limit: usize,
    kind: EntityKind,
    filters: &[PreparedSearchFilter<'_>],
) -> Result<Vec<(Value, f64)>> {
    let ExecCtx { graph, .. } = ctx;
    // The per-search query facts (ADR-0256 hoist, stellar round 2 bundle):
    // norm and zero-guard are shared by every candidate's cosine.
    let prepared = PreparedQuery::prepare(query);
    // ADR-0705: the packed matrix answers when it can serve this view
    // exactly; otherwise the `Value` sweep runs unchanged.
    let top = match matrix_window(graph, scan, &prepared, limit, filters).transpose()? {
        Some(top) => top,
        None => value_window(graph, scan, &prepared, limit, filters)?,
    };
    // The survivors materialize through the one ranked composition
    // (exec::candidates), with the kind the caller passed.
    Ok(ranked_candidates(graph, kind, top.into_sorted_rows()))
}

/// The `Value` sweep (ADR-0256): the top-k window filled from the shared
/// index population — every live entity the index covers, already gated
/// by the population's own per-kind label/type policies — scored per
/// candidate and filter-gated, [`matrix_window`]'s twin over `Value`
/// coordinate lists. The unfiltered path never materializes a `Value`
/// per scored candidate — only the ≤ limit survivors become entities in
/// `scan_candidates`'s tail. Filtered (CIP-235 `WITH`) searches still
/// evaluate properties per scored candidate and take the same flat path.
fn value_window(
    graph: &ExecGraph<'_>,
    scan: &VectorScan,
    prepared: &PreparedQuery<'_>,
    limit: usize,
    filters: &[PreparedSearchFilter<'_>],
) -> Result<TopWindow> {
    let VectorScan {
        detail,
        property,
        metric,
    } = scan;
    // The one per-candidate scan body: indexed-property read, coordinate
    // parse, score, filter gate.
    let candidate_score = |id: i64, properties: &PropertyMap| -> Result<Option<f64>> {
        let Some(value) = properties.get(property) else {
            return Ok(None);
        };
        let Some(coords) = value_coords(value) else {
            return Ok(None);
        };
        let Some(score) = coords.score(*metric, prepared) else {
            return Ok(None);
        };
        if !search_filter_passes(graph, id, properties, filters)? {
            return Ok(None);
        }
        Ok(Some(score))
    };
    // The scan ranks from the top: a VECTOR source admits no SKIP (the
    // front's 42N14), so its window is the LIMIT alone — the caller's
    // growth loop pages the survivors (ADR-0678).
    let mut top = TopWindow::new(0, limit);
    // The one fetch-gate-push sweep.
    for (id, properties) in index_population(graph, detail) {
        if let Some(score) = candidate_score(id, &properties)? {
            top.push(score, id);
        }
    }
    Ok(top)
}

/// The packed-matrix scan (ADR-0705): the same top-k window the `Value`
/// scan fills, from the node index's packed `f32` rows. `None` when the
/// matrix cannot serve this search exactly — a relationship or
/// multi-label index, or a view with uncommitted writes of its own — or
/// declines it ([`crate::graph::Graph::with_vector_matrix`]); the caller
/// then scans `Value`s.
///
/// Filters keep their pre-ranking semantics: a row is scored first, and
/// only a row the window would retain has its properties read and its
/// filters checked, so a row that fails never enters the window and the
/// next-best passing rows fill it — the `Value` scan's answer, without
/// reading the properties of rows that could not place.
fn matrix_window(
    graph: &ExecGraph<'_>,
    scan: &VectorScan,
    prepared: &PreparedQuery<'_>,
    limit: usize,
    filters: &[PreparedSearchFilter<'_>],
) -> Option<Result<TopWindow>> {
    let VectorScan {
        detail,
        property,
        metric,
    } = scan;
    let [label] = detail.labels.as_slice() else {
        return None;
    };
    if detail.relationship || !graph.has_no_pending_writes() {
        return None;
    }
    let dim = prepared.dimension();
    graph
        .base()
        .with_vector_matrix(label, property, graph.snapshot_ts(), dim, |matrix| {
            let mut top = TopWindow::new(0, limit);
            for (id, row) in matrix.rows() {
                let Some(score) = similarity_row(*metric, prepared, row) else {
                    continue;
                };
                if !filters.is_empty() {
                    if !top.admits(score, id) {
                        continue;
                    }
                    // The matrix serves this snapshot, so the row's node is
                    // visible in it.
                    let node = expect_enumerated_candidate(graph.node(id), id);
                    if !search_filter_passes(graph, id, &node.properties, filters)? {
                        continue;
                    }
                }
                top.push(score, id);
            }
            Ok(top)
        })
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

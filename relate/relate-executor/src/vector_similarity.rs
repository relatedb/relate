//! CIP-121/CIP-214 vector similarity, matching Neo4j float32 rounding.
//! The stored vector-index option schema (the metric narrowing, the key
//! table, the one parse) lives in [`crate::vector_config`]; this module
//! owns the scoring kernels only.

use relate_core::Value;

use crate::vector_config::StorableMetric;

fn round_f32(x: f64) -> f64 {
    x as f32 as f64
}
/// Eight independent dot-product accumulators. The split into lanes is the
/// only reordering versus a single-accumulator loop; it breaks the
/// serial dependency chain so the compiler can vectorize the loop. The
/// `round_f32` results are contract-identical to the sequential loop
/// (see the equivalence tests below, ADR-0256).
fn dot_unrolled(a: &[f64], b: &[f64]) -> f64 {
    let mut lanes = [0.0f64; 8];
    let mut i = 0;
    while i + 8 <= a.len() {
        lanes[0] += a[i] * b[i];
        lanes[1] += a[i + 1] * b[i + 1];
        lanes[2] += a[i + 2] * b[i + 2];
        lanes[3] += a[i + 3] * b[i + 3];
        lanes[4] += a[i + 4] * b[i + 4];
        lanes[5] += a[i + 5] * b[i + 5];
        lanes[6] += a[i + 6] * b[i + 6];
        lanes[7] += a[i + 7] * b[i + 7];
        i += 8;
    }
    let mut dot = lanes.iter().sum();
    while i < a.len() {
        dot += a[i] * b[i];
        i += 1;
    }
    dot
}

/// The scan score for the stored metric — the `Value`-coordinate entry
/// point of [`similarity`], the one kernel below both scan paths.
pub(crate) fn similarity_values(
    metric: StorableMetric,
    query: &PreparedQuery<'_>,
    candidate: &[Value],
) -> Option<f64> {
    similarity(metric, query, candidate.len(), |i| {
        value_coord(&candidate[i])
    })
}

/// The scan score of one packed `f32` row (the vector matrix, ADR-0705):
/// [`similarity_values`]' twin over a row packed from an f32-exact
/// `VECTOR` — the same kernel, the row's coordinates widened to the
/// `f64` the `Value` walk reads, so the score is bit-identical. The row
/// was finiteness-checked when it was packed; the skip contracts left
/// here are the query's guards and the all-zero cosine candidate.
pub(crate) fn similarity_row(
    metric: StorableMetric,
    query: &PreparedQuery<'_>,
    row: &[f32],
) -> Option<f64> {
    similarity(metric, query, row.len(), |i| Some(f64::from(row[i])))
}

/// The one scan score (ADR-0705): the query's length and zero guards,
/// the exhaustive [`StorableMetric`] dispatch and the CIP-121/CIP-214
/// rounding chains, over a reader-parameterized coordinate source — the
/// `Value` walk's fallible [`value_coord`] or the packed row's
/// infallible widen. Monomorphized per entry point, so the two
/// coordinate sources keep their own codegen and stay bit-identical by
/// construction, not by maintenance. Every skip contract lives here: an
/// empty or length-mismatched query, a non-finite coordinate, and the
/// all-zero cosine candidate all skip (None). The zero-query guard is
/// cosine's only (a zero query has no direction; euclidean distance
/// stays defined).
fn similarity(
    metric: StorableMetric,
    query: &PreparedQuery<'_>,
    len: usize,
    reader: impl Fn(usize) -> Option<f64>,
) -> Option<f64> {
    if query.coords.is_empty() || query.coords.len() != len {
        return None;
    }
    match metric {
        StorableMetric::Cosine if query.zero => None,
        StorableMetric::Cosine => Some(cosine_score(cosine_lanes(
            query.coords,
            query.norm,
            len,
            reader,
        )?)),
        StorableMetric::Euclidean => Some(euclidean_score(euclidean_squared_lanes(
            query.coords,
            len,
            reader,
        )?)),
    }
}

/// The one eight-lane cosine pass (ADR-0705): dot lanes and
/// candidate-norm lanes over `len` coordinates, with `reader(i)` reading
/// the coordinate at `i` — the `Value` walk's fallible [`value_coord`]
/// or the packed row's infallible widen. Monomorphized per caller, so
/// each twin keeps its own codegen and the two spellings stay
/// bit-identical by construction, not by maintenance. The caller
/// guarantees `len == query.len()` ([`similarity`]'s length guard).
fn cosine_lanes(
    query: &[f64],
    query_norm: f64,
    len: usize,
    reader: impl Fn(usize) -> Option<f64>,
) -> Option<f64> {
    let mut dot = [0.0f64; 8];
    let mut norm = [0.0f64; 8];
    let mut any_nonzero = false;
    let mut i = 0;
    while i + 8 <= len {
        for lane in 0..8 {
            let c = reader(i + lane)?;
            dot[lane] += query[i + lane] * c;
            norm[lane] += c * c;
            any_nonzero |= c != 0.0;
        }
        i += 8;
    }
    let mut dot_sum: f64 = dot.iter().sum();
    let mut norm_sum: f64 = norm.iter().sum();
    while i < len {
        let c = reader(i)?;
        dot_sum += query[i] * c;
        norm_sum += c * c;
        any_nonzero |= c != 0.0;
        i += 1;
    }
    if !any_nonzero {
        return None;
    }
    Some(dot_sum / (query_norm * norm_sum.sqrt()))
}

/// The one eight-lane squared-euclidean pass (ADR-0705), with the same
/// reader parameterization as [`cosine_lanes`].
fn euclidean_squared_lanes(
    query: &[f64],
    len: usize,
    reader: impl Fn(usize) -> Option<f64>,
) -> Option<f64> {
    let mut dist = [0.0f64; 8];
    let mut i = 0;
    while i + 8 <= len {
        for lane in 0..8 {
            let c = reader(i + lane)?;
            let d = query[i + lane] - c;
            dist[lane] += d * d;
        }
        i += 8;
    }
    let mut dist_sum: f64 = dist.iter().sum();
    while i < len {
        let c = reader(i)?;
        let d = query[i] - c;
        dist_sum += d * d;
        i += 1;
    }
    Some(dist_sum)
}

/// The CIP-121 cosine score chain (ADR-0256): the kernel's raw cosine,
/// rounded to the float32 contract, mapped to `[0, 1]`, rounded again —
/// one spelling shared by the `Value` and packed-row dispatches, so the
/// user-visible score contract cannot drift between them (ADR-0705).
fn cosine_score(raw: f64) -> f64 {
    round_f32((round_f32(raw) + 1.0) / 2.0)
}

/// The CIP-214 euclidean score chain (ADR-0256): `1 / (1 + d²)` at the
/// same float32 rounding contract, one spelling for both dispatches.
fn euclidean_score(squared_distance: f64) -> f64 {
    round_f32(1.0 / (1.0 + squared_distance))
}

/// One `Value` list coordinate. `None` when the entry is not a finite
/// number — the candidate-skip half of `vector_operand`'s contract
/// (ADR-0256: scan candidates are never converted to a fresh `Vec<f64>`).
pub(crate) fn value_coord(value: &Value) -> Option<f64> {
    match value {
        Value::Int(x) => Some(*x as f64),
        Value::Float(x) if x.is_finite() => Some(*x),
        _ => None,
    }
}

/// The `Value` coordinate list of a candidate property: a VECTOR or a list.
/// `None` skips the candidate, exactly as `vector_operand` does.
pub(crate) fn value_coords(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Vector { values, .. } | Value::List(values) => Some(values),
        _ => None,
    }
}

/// The query's L2 norm — a per-search constant hoisted out of the scan
/// (ADR-0256). Computed by the same unrolled kernel the per-candidate
/// cosine uses, so the norm is bit-consistent within one search.
fn query_norm(query: &[f64]) -> f64 {
    dot_unrolled(query, query).sqrt()
}

/// The per-search query facts, built once (stellar round 2, std-3): the
/// coordinates, their norm, and the all-zero guard travel together as
/// one type, so no caller can pass a bool that contradicts the slice.
/// (The zero guard is the exact `all(|v| *v == 0.0)` — denormal
/// underflow can zero the norm of a nonzero query, so the norm must not
/// derive the guard.)
pub(crate) struct PreparedQuery<'a> {
    coords: &'a [f64],
    norm: f64,
    zero: bool,
}

impl PreparedQuery<'_> {
    pub(crate) fn prepare(query: &[f64]) -> PreparedQuery<'_> {
        PreparedQuery {
            coords: query,
            norm: query_norm(query),
            zero: query.iter().all(|value| *value == 0.0),
        }
    }

    /// The query's dimension: the one spelling of the length every scan's
    /// length guard and the packed rows' dimension derive from (ADR-0705).
    pub(crate) fn dimension(&self) -> usize {
        self.coords.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Sequential single-accumulator reference, the pre-ADR-0256 kernel.
    fn cosine_raw_sequential(a: &[f64], b: &[f64]) -> f64 {
        let mut dot = 0.0;
        let mut na = 0.0;
        let mut nb = 0.0;
        for i in 0..a.len() {
            dot += a[i] * b[i];
            na += a[i] * a[i];
            nb += b[i] * b[i];
        }
        dot / (na.sqrt() * nb.sqrt())
    }

    fn euclidean_squared_sequential(a: &[f64], b: &[f64]) -> f64 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    /// Deterministic LCG so the equivalence corpus is fixed across runs and
    /// platforms.
    fn lcg(state: &mut u64) -> f64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*state >> 11) as f64 / (1u64 << 53) as f64) - 0.5
    }

    fn value_list(coords: &[f64]) -> Arc<Vec<Value>> {
        Arc::new(coords.iter().map(|&x| Value::Float(x)).collect())
    }

    /// ADR-0256: the unrolled kernels reorder only the f64 partial sums, so
    /// every float32-rounded score — the CIP-121/CIP-214 user-visible
    /// contract — matches the sequential reference bit-for-bit over a fixed
    /// corpus of random, aligned, tail-carrying, and scale-varied inputs.
    /// The `Value`-list kernels (the scan path) and the `Vec<f64>` kernels
    /// (the reference path) round identically too.
    #[test]
    fn unrolled_kernels_round_identically_to_sequential() {
        let dims = [1, 2, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128, 1536];
        let mut state = 0x5eed_cafe_f00d_bee5u64;
        for &d in &dims {
            for trial in 0..200 {
                let scale = [1e-8, 1e-3, 1.0, 1e3][trial % 4];
                let a: Vec<f64> = (0..d).map(|_| lcg(&mut state) * scale).collect();
                let b: Vec<f64> = (0..d).map(|_| lcg(&mut state) * scale).collect();
                // The scan path: candidate as a Value coordinate list, query
                // facts hoisted per search, compared against the sequential
                // single-accumulator reference with the same rounding chain.
                let b_values = Value::List(value_list(&b));
                let coords = value_coords(&b_values).expect("list coordinates");
                let prepared = PreparedQuery::prepare(&a);
                assert_eq!(
                    similarity_values(StorableMetric::Cosine, &prepared, coords),
                    Some(round_f32(
                        (round_f32(cosine_raw_sequential(&a, &b)) + 1.0) / 2.0
                    )),
                    "cosine d={d} trial={trial}"
                );
                assert_eq!(
                    similarity_values(StorableMetric::Euclidean, &prepared, coords),
                    Some(round_f32(
                        1.0 / (1.0 + euclidean_squared_sequential(&a, &b))
                    )),
                    "euclidean d={d} trial={trial}"
                );
            }
        }
    }

    /// ADR-0705: a packed f32 row scores bit-identically to the same
    /// coordinates walked as `Value`s, for both metrics, across aligned and
    /// tail-carrying lengths, scales, and an all-zero row.
    #[test]
    fn packed_row_kernels_match_value_kernels_bit_for_bit() {
        let dims = [1, 2, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128, 384, 1536];
        let mut state = 0x0dd_ba11_cafe_f00du64;
        for metric in [StorableMetric::Cosine, StorableMetric::Euclidean] {
            for &d in &dims {
                for trial in 0..100 {
                    let scale = [1e-8, 1e-3, 1.0, 1e3][trial % 4];
                    let query: Vec<f64> = (0..d).map(|_| lcg(&mut state) * scale).collect();
                    // Candidates are f32-exact, as a FLOAT32 VECTOR stores them.
                    let row: Vec<f32> = if trial == 0 {
                        vec![0.0; d]
                    } else {
                        (0..d).map(|_| (lcg(&mut state) * scale) as f32).collect()
                    };
                    let widened: Vec<f64> = row.iter().map(|&c| f64::from(c)).collect();
                    let values = Value::List(value_list(&widened));
                    let coords = value_coords(&values).expect("list coordinates");
                    let prepared = PreparedQuery::prepare(&query);
                    let from_values = similarity_values(metric, &prepared, coords);
                    let from_row = similarity_row(metric, &prepared, &row);
                    assert_eq!(
                        from_values.map(f64::to_bits),
                        from_row.map(f64::to_bits),
                        "{metric:?} d={d} trial={trial}"
                    );
                }
            }
        }
        let prepared = PreparedQuery::prepare(&[1.0, 2.0]);
        assert!(
            similarity_row(StorableMetric::Cosine, &prepared, &[1.0]).is_none(),
            "length mismatch skips"
        );
        let zero_query = PreparedQuery::prepare(&[0.0, 0.0]);
        assert!(
            similarity_row(StorableMetric::Cosine, &zero_query, &[1.0, 1.0]).is_none(),
            "zero query skips cosine"
        );
    }

    /// The kernels keep `vector_operand`'s skip contracts: a non-number or
    /// non-finite coordinate, a non-list property, and an all-zero cosine
    /// candidate all skip the candidate (None); euclidean has no zero
    /// guard.
    #[test]
    fn value_kernels_keep_the_skip_contracts() {
        let query = [1.0, 0.0];
        let qn = query_norm(&query);
        let values = |coords: &[Value]| Value::List(Arc::new(coords.to_vec()));
        fn value_reader(coords: &[Value; 2]) -> impl Fn(usize) -> Option<f64> + '_ {
            move |i| value_coord(&coords[i])
        }

        // (The query/candidate length check lives in `similarity`.)
        let non_number = [Value::Int(1), Value::Null];
        assert!(
            cosine_lanes(&query, qn, 2, value_reader(&non_number)).is_none(),
            "non-number coordinate"
        );
        let non_finite = [Value::Float(f64::NAN), Value::Float(0.0)];
        assert!(
            cosine_lanes(&query, qn, 2, value_reader(&non_finite)).is_none(),
            "non-finite coordinate"
        );
        let zero = [Value::Float(0.0), Value::Float(0.0)];
        assert!(
            cosine_lanes(&query, qn, 2, value_reader(&zero)).is_none(),
            "zero-vector candidate"
        );
        assert!(
            euclidean_squared_lanes(&query, 2, value_reader(&zero)).is_some(),
            "euclidean has no zero guard"
        );
        assert!(value_coords(&Value::Null).is_none(), "null property");
        assert!(value_coords(&Value::Int(3)).is_none(), "scalar property");
        assert_eq!(
            value_coords(&values(&[Value::Float(1.0)])).map(|c| c.len()),
            Some(1)
        );
    }
}

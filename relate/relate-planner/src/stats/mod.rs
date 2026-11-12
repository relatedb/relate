//! Cardinality hints for MATCH pattern ordering.
//!
//! The planner does not own the graph. Callers that can see label sizes,
//! index hit counts, and relationship-type counts pass them in. A missing
//! count (`None`) makes ordering fall back to the fixed rank of that
//! endpoint. The [`PatternStats`] trait and its stub live here; the
//! per-statement consult memo (ADR-0508) wrapping every implementor lives
//! in [`memo`].

use relate_ir::{BinaryOp, Expr};
use relate_names::{NamedToken, SmallName};

/// Provenance of a plan-time `CONTAINS` candidate count (LABS-643): the
/// TEXT index stays the one exact measurement, and without one the count
/// comes from a bounded sample scan of the label's nodes — exact when the
/// label fits inside the sampler's bound, an extrapolation beyond it. The
/// variant, never a bare number, says which: no consumer can mistake the
/// extrapolation for the measurement, and the sampled form carries the
/// whole-label scan rows the unseekable filter still pays — the floor the
/// compound start estimate (ADR-0474) maxes with the
/// sampled output.
///
/// The sampler lives in the executor; the type lives here because the
/// [`PatternStats::contains_rows`] contract is the one place both the
/// planner's pattern ordering and the executor's start selection consume
/// it, and the executor re-exports it so its own construction sites and
/// tests keep one canonical path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainsCount {
    /// Exact hit count a TEXT index on `(label, property)` answered.
    TextIndex(u64),
    /// A bounded sample scan's estimate: `rows` is the output estimate —
    /// the exact count when the label's index bucket holds at most the
    /// sample bound, otherwise `matches * bucket_rows / scanned`, floored
    /// at a divisor of the bucket — and `scan` is the bucket's whole row
    /// count, the label scan the unindexed filter pays before any match.
    SampleScan { rows: u64, scan: u64 },
}

impl ContainsCount {
    /// The row count both provenances carry on the planner's one scale.
    pub fn rows(self) -> u64 {
        match self {
            ContainsCount::TextIndex(rows) | ContainsCount::SampleScan { rows, .. } => rows,
        }
    }
}

/// The first two moments of an out-degree distribution — what a cost gate
/// needs to price a chain of hops: the mean fanout is `sum / nodes` and
/// the size-biased (excess) degree a trail meets after its first hop is
/// `sum_sq / sum - 1`. Estimates on the histogram's log2-bucket scale
/// (each bucket read at the uniform mean of its degree range), summed
/// over the selected labels and rel types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DegreeMoments {
    /// Estimated sum of out-degrees (the relationship count the selected
    /// nodes start).
    pub sum: u128,
    /// Estimated sum of squared out-degrees.
    pub sum_sq: u128,
}

/// Live or stubbed counts used to estimate how many rows a pattern endpoint
/// produces. Every method returns `None` when that signal is unavailable,
/// which is not the same as zero rows. Labels and relationship types
/// arrive as a name beside its planning-time [`relate_names::Token`]
/// (ADR-0541): an implementor reads the id when planning resolved one and
/// falls back to the name only for an unresolved token.
pub trait PatternStats {
    /// Nodes carrying `label`.
    fn label_rows(&self, label: &NamedToken) -> Option<u64>;

    /// Hits of an equality seek on a RANGE index for `(label, property)`.
    /// `value` is the raw predicate expression. Implementors resolve
    /// literals and parameters with [`Expr::seek_value`] and return `None`
    /// for anything else. `None` also means no RANGE index, not zero hits.
    fn equality_rows(&self, label: &NamedToken, property: &NamedToken, value: &Expr)
        -> Option<u64>;

    /// Hits of a comparison predicate (`<`, `<=`, `>`, `>=`) on a RANGE
    /// index for `(label, property)` — the range window (ADR-0486). The
    /// four comparison ops only: `=` stays [`PatternStats::equality_rows`]'s
    /// domain, and `IN` and `STARTS WITH` are deferred. `bound` is the raw
    /// predicate expression; implementors resolve literals and parameters
    /// with [`Expr::seek_value`] and return `None` for anything else.
    /// `None` also means no RANGE index on the pair, no order key on the
    /// bound, or unbounded temporal postings — the executor seek's own
    /// decline conditions, spelled once by the index layer — never zero
    /// hits.
    fn range_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        let _ = (label, property, op, bound);
        None
    }

    /// Candidates of a `CONTAINS` filter on `(label, property)`, with the
    /// provenance that says how the count was measured (LABS-643): the
    /// exact hits a TEXT index answered, or — without one — the sampled
    /// estimate carrying the whole-label scan rows the unseekable filter
    /// still pays. `needle` is the raw predicate expression. Implementors
    /// resolve literals and parameters with [`Expr::seek_value`] and
    /// return `None` for anything else. `None` means the count is
    /// unmeasurable (a non-string needle), not zero candidates.
    fn contains_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        needle: &Expr,
    ) -> Option<ContainsCount>;

    /// Relationships of `rel_type`.
    fn rel_type_rows(&self, rel_type: &NamedToken) -> Option<u64>;

    /// LABS-351: the tail of a (label, rel-type) out-degree distribution —
    /// the conservative floor of the highest observed degree (a log2-bucket
    /// histogram's top bucket). `None` when the stats keep no histogram.
    /// Consumed by `apply_degree` (LABS-426, the mean-to-tail flip): an
    /// outgoing hop's fanout rides the tail when the histogram has one,
    /// the mean otherwise — the plan-choice consequence of the flip is
    /// the measured part of that change.
    fn degree_tail(&self, label: &NamedToken, rel_type: &NamedToken) -> Option<u64> {
        let _ = (label, rel_type);
        None
    }

    /// LABS-991: the out-degree tail over every labeled node — the
    /// log2-bucket floor of the highest observed degree of `rel_type`
    /// (for `None`, of any relationship: the per-type tails summed, the
    /// same floor scale, not a strict upper bound). The estimate a hop leaving
    /// an unlabeled node, or crossing an untyped rel, rides when
    /// [`PatternStats::degree_tail`] has no `(label, type)` pair to name.
    /// Nodes carrying no label sit in no histogram and are not covered.
    /// `None` when the stats keep no histogram.
    fn any_degree_tail(&self, rel_type: Option<&SmallName>) -> Option<u64> {
        let _ = rel_type;
        None
    }

    /// The out-degree moments ([`DegreeMoments`]) of nodes carrying the
    /// labels in `labels` (empty: every labeled node; several: a bound,
    /// the smallest per-label moments) over the rel types
    /// `rel_types` (empty: every type). A name the store never interned
    /// selects nothing, so its moments are zero. `None` when the stats
    /// keep no histogram.
    fn degree_moments(
        &self,
        labels: &[NamedToken],
        rel_types: &[NamedToken],
    ) -> Option<DegreeMoments> {
        let _ = (labels, rel_types);
        None
    }
}

/// No counts. Ordering uses the bound / equality / label / bare ranks.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPatternStats;

impl PatternStats for NoPatternStats {
    fn label_rows(&self, _label: &NamedToken) -> Option<u64> {
        None
    }

    fn equality_rows(
        &self,
        _label: &NamedToken,
        _property: &NamedToken,
        _value: &Expr,
    ) -> Option<u64> {
        None
    }

    fn contains_rows(
        &self,
        _label: &NamedToken,
        _property: &NamedToken,
        _needle: &Expr,
    ) -> Option<ContainsCount> {
        None
    }

    fn rel_type_rows(&self, _rel_type: &NamedToken) -> Option<u64> {
        None
    }
}

/// ADR-0508's per-statement memo over the value-keyed consults — one
/// [`PatternStats`] wrapper, built once by [`crate::plan_with_stats`] for a
/// whole plan call, answering repeats from a table of the consults already
/// made.
mod memo;

pub(crate) use memo::StatsMemo;

#[cfg(test)]
mod tests;

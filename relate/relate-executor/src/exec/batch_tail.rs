//! ADR-0641: the generator-seeded batch-major tail — the one classification
//! that lets a `LOAD CSV` head run its tail per batch instead of holding the
//! whole inter-clause container.
//!
//! The shape is `LOAD CSV … AS row MATCH … CREATE …` (the bulk-load
//! statement): the generator streams its records (#568), but the boundary
//! between the generator and the tail carries each composed row's payload,
//! so a 656 k-record load holds 656 k rows × the CSV-record width — the
//! container ADR-0463 charges and the one that peaks at 13.1 GiB. Running
//! the tail once per bounded batch of records keeps the *driving* rows at
//! one batch, inside the statement's one transaction (no implicit per-batch
//! commit): the redo is the transaction's own state, and the explicit
//! bounded form stays `CALL { … } IN TRANSACTIONS`. The tail's own
//! inter-clause containers (a `MATCH` fan-out — one row per driver × match)
//! are not bounded by the batch; they stay charged by ADR-0463.
//!
//! The classifier here is the whole eligibility decision; the driver that
//! pulls batches and runs the tail lives beside the clause loop
//! ([`super::dispatch`]). It is deliberately conservative — any deviation
//! yields `None` and the statement runs unbatched, which is always correct.

use std::collections::HashSet;

use relate_ir::{Clause, Expr, PathPattern};
use relate_names::SmallName;

use super::create_admission::{
    compact_pattern, create_endpoint_is_fresh, create_patterns_eligible,
};
use super::label_expr_ok;
use super::shortcut_common::plain_match_patterns;

/// Driving rows per batch (ADR-0555's batch bound). The tail runs once per
/// batch, so this bounds the *driving* rows — not the tail's own containers,
/// which a `MATCH` fan-out grows past the batch and which stay charged by
/// ADR-0463.
pub(super) const BATCH_ROWS: usize = 1024;

/// One footprint axis: the names a read pattern requires, or `Any` when some
/// read pattern is unlabeled and so matches any node (rel). The two states
/// are one value — an unlabeled read pattern supersedes the names, so no
/// dead set survives beside a flag.
enum Axis {
    Any,
    Names(HashSet<String>),
}

impl Default for Axis {
    fn default() -> Self {
        // An empty footprint is not `Any`: nothing read means nothing matches.
        Axis::Names(HashSet::new())
    }
}

impl Axis {
    /// Absorb one pattern element's names; an unlabeled element makes the
    /// axis `Any`.
    fn absorb(&mut self, names: Vec<String>) {
        match self {
            Axis::Any => {}
            Axis::Names(set) => {
                if names.is_empty() {
                    *self = Axis::Any;
                } else {
                    set.extend(names);
                }
            }
        }
    }

    /// Whether an entity carrying `names` could be matched by a read pattern
    /// on this axis — `Any`, or a name in common.
    fn collides(&self, names: &[String]) -> bool {
        match self {
            Axis::Any => true,
            Axis::Names(set) => names.iter().any(|n| set.contains(n)),
        }
    }
}

/// The label/type footprint of the read patterns: one [`Axis`] per element
/// kind. Absorbed once per read pattern and asked once per created entity —
/// the two phases of the classifier.
#[derive(Default)]
struct Footprint {
    nodes: Axis,
    rels: Axis,
}

impl Footprint {
    /// Absorb one plain read pattern's labels/types and variables; `false`
    /// when it carries a dynamic label expression or a var-length/quantified
    /// relationship, which the read side refuses.
    fn absorb_read(&mut self, pattern: &PathPattern, bound: &mut HashSet<String>) -> bool {
        for node in &pattern.nodes {
            if !label_expr_ok(&node.label_expr, &node.labels) {
                return false;
            }
            if let Some(var) = node.variable.as_ref() {
                bound.insert(var.to_string());
            }
            self.nodes
                .absorb(node.label_names().map(|l| l.to_string()).collect());
        }
        for rel in &pattern.rels {
            // `quantified` is `needs_gpm`'s (the caller gates on it); the
            // length bound is not.
            if !label_expr_ok(&rel.type_expr, &rel.types) || rel.var_length {
                return false;
            }
            if let Some(var) = rel.variable.as_ref() {
                bound.insert(var.to_string());
            }
            self.rels
                .absorb(rel.type_names().map(|t| t.to_string()).collect());
        }
        true
    }
}

/// A generator seeding a per-row-independent tail: the `LOAD CSV` head,
/// already destructured from the clause the classifier matched, and the
/// tail it drives. Carrying the head's fields means the driver never
/// re-matches `clauses[0]` — the admitted-head fact lives here, once.
pub(super) struct BatchTail<'a> {
    pub(super) with_headers: bool,
    pub(super) url: &'a Expr,
    pub(super) variable: &'a SmallName,
    pub(super) field_terminator: Option<&'a str>,
    pub(super) tail: &'a [Clause],
}

/// Whether `clauses` is a generator seeding a per-row-independent tail, and
/// the classified head plus tail when it is.
///
/// The admitted shape, all required:
/// - the first clause is `LOAD CSV` (the one generator whose source is
///   external and unbounded);
/// - the tail is zero or more plain `MATCH` clauses — the shared plain-MATCH
///   gate ([`plain_match_patterns`]) plus no GPM/path-mode pattern —
///   followed by exactly one terminal `CREATE`, and nothing after it;
/// - the `CREATE` is admitted by the canonical per-row-independence
///   predicate ([`create_patterns_eligible`]: matched vars as bare
///   endpoints, static labels, no graph-sensitive property expression);
/// - the created entities cannot be matched by any read pattern: every
///   created node carries a label, every created rel a type, no read
///   pattern is unlabeled, and no created label/type appears in the read
///   patterns. A read in a later batch must not see an earlier batch's
///   writes (they share one scratch overlay), and `CREATE` only adds
///   entities — so label/type disjointness is the whole condition.
///
/// Every other write kind (`SET`/`REMOVE`/`DELETE`/`MERGE`) and every
/// global operator (aggregation, `DISTINCT`, `ORDER BY`, paging, a terminal
/// `RETURN`) is out of scope: the first is per-row but needs its own
/// disjointness analysis, the second would answer per batch instead of over
/// the statement. Both fall to the ordinary path, which is correct.
pub(super) fn generator_seeded_tail(clauses: &[Clause]) -> Option<BatchTail<'_>> {
    let [Clause::LoadCsv {
        with_headers,
        url,
        variable,
        field_terminator,
    }, tail @ ..] = clauses
    else {
        return None;
    };
    let (last, reads) = tail.split_last()?;
    let Clause::Create { patterns: creates } = last else {
        return None;
    };

    // Phase 1: the read footprint — every label/type the read patterns
    // require, and whether any read pattern is unlabeled (then it matches
    // any node/rel).
    let mut bound: HashSet<String> = HashSet::new();
    let mut read = Footprint::default();
    for clause in reads {
        for pattern in plain_match_patterns(clause)? {
            if !compact_pattern(pattern) {
                return None;
            }
            if !read.absorb_read(pattern, &mut bound) {
                return None;
            }
        }
    }

    // Phase 2: the canonical CREATE admission (the predicate the ADR-0209
    // bulk analyzer reads): after it, a node naming a matched variable is a
    // bare endpoint, and every other node is created (or relabeled) here.
    // Each created entity must be unmatchable by the read footprint.
    if !create_patterns_eligible(creates, &bound) {
        return None;
    }
    for pattern in creates {
        for node in &pattern.nodes {
            if !create_endpoint_is_fresh(node, &bound) {
                continue;
            }
            let labels: Vec<String> = node.label_names().map(|l| l.to_string()).collect();
            // A created entity must carry a label/type of its own, and share
            // none with the read footprint.
            if labels.is_empty() || read.nodes.collides(&labels) {
                return None;
            }
        }
        for rel in &pattern.rels {
            let types: Vec<String> = rel.type_names().map(|t| t.to_string()).collect();
            if types.is_empty() || read.rels.collides(&types) {
                return None;
            }
        }
    }
    Some(BatchTail {
        with_headers: *with_headers,
        url,
        variable,
        field_terminator: field_terminator.as_deref(),
        tail,
    })
}

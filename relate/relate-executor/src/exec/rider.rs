//! The rider vocabulary and the sink vocabulary — what a one-path match
//! carries beside its reduce tail (`MatchTail`, pattern_policies): a
//! MATCH cap (the LIMIT pull-in) or an EXISTS-style probe (the early
//! exit); and what the classic walk offers complete bindings to
//! ([`BindingSink`], [`CollectSink`], [`PathSink`]).
//! Engine-neutral: `match_one_path`'s one fork routes a rider into
//! either engine — the GPM walk mounts it (`gpm::emit`'s
//! `mount_rider`/`GpmEmit`), the classic walk sinks it ([`PathSink`]) —
//! so both vocabularies live here, at the exec level above both
//! engines, not in either's home (the classic walk, `exec/expand.rs`,
//! imports the sink contract from here). The rider never rides in the
//! tail and the tail never rides in the rider: two parallel
//! vocabularies, threaded as two arguments (`match_one_path`,
//! `match_gpm_path`).

use crate::UsedSet;
use std::cell::Cell;
use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::PathPattern;

use crate::eval::inline_predicates_ok;
use crate::overlay::ExecGraph;
use crate::pattern_walk::UsedRels;
use crate::row::Row;

use super::match_cap::MatchCap;
use super::pattern_policies::{MatchTail, Matched};
use super::pushdown::{residual_ok, Residual};

/// One delivery path for a complete binding: collect it, or probe and
/// stop. The halt fact has ONE channel — the [`BindingSink::halted`]
/// cell query — never a second one carried on the offer's return: the
/// offer delivers the row, the walk consults `halted()` afterwards
/// (exactly the way it consults its own halt cells), so a sink can
/// never report a halt its cell does not know.
pub(crate) trait BindingSink {
    fn offer(&mut self, graph: &ExecGraph<'_>, row: Row, used: &UsedRels) -> Result<()>;
    fn halted(&self) -> bool;
}

/// Collect bindings, optionally stopping once a [`MatchCap`] is full.
pub(crate) struct CollectSink<'out, 'cap, 'data, 'tail> {
    out: &'out mut Matched,
    cap: Option<&'cap mut MatchCap<'data>>,
    /// The reduce-tail state model (`MatchTail`, pattern_policies): the
    /// last pattern's finish — the reduce running when the following
    /// clause is projected here — or an intermediate pattern's (or plain
    /// side's) carry. The one tail shape, shared with the GPM chain —
    /// the classic walk and the GPM walk read the same state model,
    /// never two.
    tail: MatchTail<'tail>,
    /// Workspace of `out`, released when the collector drops.
    charge: relate_core::tokens::WorkspaceCharge,
    /// Running total of `out`. Updated only while a ledger is installed.
    meter: crate::workspace::BindingMeter,
}

impl BindingSink for CollectSink<'_, '_, '_, '_> {
    fn offer(&mut self, graph: &ExecGraph<'_>, row: Row, used: &UsedRels) -> Result<()> {
        // The fused tail step, shared with the GPM walk's Cap arm:
        // `MatchTail::finish_row` (pattern_policies) runs the reduce's
        // checks and projection and the cap's count on the pre-projection
        // row the checks accepted — the one spelling of the ordering
        // policy, its rationale living beside the tail it belongs to.
        let Some(stored) = self.tail.finish_row(graph, row, self.cap.as_deref_mut())? else {
            return Ok(());
        };
        // Meter before the store: the row is in hand (`stored`), so there
        // is no readback of `out`'s last entry, no invariant panic path,
        // and an error leaves `out` untouched — the row never entered it.
        let used_len = used.len();
        if relate_core::tokens::workspace_accounting() {
            let mark = self.meter.push(&stored, used_len);
            if let Err(err) = self.charge.resize(self.meter.bytes) {
                self.meter.unpush(mark);
                return Err(err);
            }
        }
        // the shared back half of both cap arms — the row stored with
        // its carried used-set (lazily: the drop arm never materializes
        // one, and a rejected row, returned above, never builds a set
        // at all). Both arms convert through the carrier's one
        // spelling (LABS-822, `UsedRels::to_set`; the fixed-hasher
        // `UsedSet::default` since LABS-1116): no offered row pays a
        // fresh `RandomState` — the thread-local RNG seed read a
        // per-row set construction once paid.
        self.tail.store_row(stored, || used.to_set(), self.out);
        Ok(())
    }

    fn halted(&self) -> bool {
        self.cap.as_ref().is_some_and(|cap| cap.full())
    }
}

/// An EXISTS-style probe riding a one-path match: the predicate receives
/// each complete row (with its used-rel set, the uniqueness carrier the
/// next pattern of an EXISTS body needs) and the first `true` answer
/// halts the match. The caller reads its halt cell for the verdict. The
/// probe owns its fixed state — the one pattern it answers for, fixed at
/// its single construction site, `exists_probe::seek`'s probe dispatch
/// (built there, it rides `match_one_path`'s one engine fork into either
/// engine) — while the MATCH-level residual it answers through stays the
/// fork's single `where_clause` fact, threaded to every answer site. The
/// predicate takes the row and used-set by value: its consumer recurses
/// with them, so a reference-taking contract would force a clone per
/// offered row.
pub(crate) struct PathProbe<'a> {
    pred: &'a dyn Fn(Row, UsedSet) -> Result<bool>,
    /// The verdict cell — outward by design: the mounting arm hands the
    /// walk this same cell (the one reader spelling of the halt), and
    /// the caller reads it for the verdict. The probe's other state is
    /// its own.
    pub(crate) halt: &'a Cell<bool>,
    /// The one pattern the probe answers for — outward like the halt
    /// cell: the mounting arm's completion pipeline reads it (the
    /// deferred element predicates run against this pattern).
    pub(crate) pattern: &'a PathPattern,
}

impl<'a> PathProbe<'a> {
    /// The one construction entry — the probe's fixed state (its
    /// predicate, its verdict cell, and the one pattern it answers for)
    /// is set here, at its single construction site
    /// (`exists_probe::seek`'s probe dispatch), and nowhere else.
    pub(crate) fn new(
        pred: &'a dyn Fn(Row, UsedSet) -> Result<bool>,
        halt: &'a Cell<bool>,
        pattern: &'a PathPattern,
    ) -> Self {
        PathProbe {
            pred,
            halt,
            pattern,
        }
    }

    /// The probe's answer core — the one spelling of the
    /// predicate-and-halt step both engine shapes read: the caller's
    /// predicate on the offered row, the halt cell set on the first
    /// accept. The GPM answer policy (`accept_row` below) wraps it with
    /// the deferred element predicates and the residual pass; the
    /// classic sink shape (the [`BindingSink`] impl on the shared
    /// reference) is the core itself, the walk's own spelling of the
    /// same verdict.
    fn accept_core(&self, row: Row, used: UsedSet) -> Result<bool> {
        let accepted = (self.pred)(row, used)?;
        if accepted {
            self.halt.set(true);
        }
        Ok(accepted)
    }

    /// The GPM engine's probe-answer policy (the rider's offer on the
    /// walk and the materialized rows' answer): the deferred element
    /// predicates (the row-completion sweep the materializing path's
    /// callers apply to its candidates), then the residual pass (the
    /// body's WHERE on the completed row — the walk applied its ready
    /// conjuncts at the binding points, this pass covers the rest),
    /// then the answer core; the first accept sets the halt cell, the
    /// caller's verdict.
    pub(crate) fn accept_row(
        &self,
        graph: &ExecGraph<'_>,
        row: Row,
        used: UsedSet,
        residual: &Residual<'_>,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<bool> {
        if !inline_predicates_ok(
            graph,
            std::slice::from_ref(self.pattern),
            &row,
            params,
            cypher_version,
        )? {
            return Ok(false);
        }
        let accepted = residual_ok(graph, &row, params, cypher_version, residual)?
            && self.accept_core(row, used)?;
        Ok(accepted)
    }
}

/// The probe as a classic-walk sink — the classic engine's half of the
/// rider vocabulary: a probe riding `match_one_path`'s classic arm IS
/// the sink, its offer exactly the probe-answer's classic shape — the
/// predicate on the offered row, the halt cell on the first accept, no
/// rows collected (the verdict is the cell). On the shared reference,
/// because that is what a rider carries (`Rider::Probe(&…)`) and the
/// sink contract wants `&mut Self`: the cell's mutation is interior, so
/// a shared probe mutates nothing it does not own.
impl BindingSink for &PathProbe<'_> {
    fn offer(&mut self, _graph: &ExecGraph<'_>, row: Row, used: &UsedRels) -> Result<()> {
        // The collector's twin conversion (LABS-822): the offered set
        // seeds from the carrier's one process-wide hasher, not a fresh
        // `RandomState` per offered row.
        self.accept_core(row, used.to_set())?;
        Ok(())
    }

    fn halted(&self) -> bool {
        self.halt.get()
    }
}

/// The rider a one-path match carries beside its tail — one enum, one
/// arm per rider kind: a MATCH cap (the clause's LIMIT, materialized
/// through the counting pull-in) or an EXISTS probe (the caller's
/// verdict cell). One argument, not two independent nullable modes —
/// the invalid both-present combination is unrepresentable, and the
/// halt cell derives from the rider.
pub(crate) enum Rider<'a, 'cap> {
    Cap(&'cap mut MatchCap<'a>),
    Probe(&'a PathProbe<'a>),
}

impl<'a> Rider<'a, '_> {
    /// The probe this rider carries, if it is one — the vocabulary's one
    /// probe-or-not split, for the consumers that treat every non-probe
    /// rider alike (the shortest route, which no cap rides). Exhaustive,
    /// so a new rider kind must decide here.
    pub(crate) fn probe(&self) -> Option<&'a PathProbe<'a>> {
        match self {
            Rider::Probe(probe) => Some(probe),
            Rider::Cap(_) => None,
        }
    }
}

/// The classic walk's sink for whatever rides the one-path match — the
/// rider→sink decision in one exhaustive match (`for_rider`), so the
/// classic arm of `match_one_path` spells neither an early return nor a
/// second rider match: a probe rides the classic walk exactly as it
/// rides the GPM one (the probe IS the sink — the [`BindingSink`] impl
/// on the shared probe reference below); a cap (or no rider at all)
/// collects rows through the
/// tail. A probe ride collects no rows — the collector's `out` stays
/// empty and the verdict is the probe's halt cell.
pub(crate) enum PathSink<'out, 'cap, 'data, 'tail> {
    Probe(&'data PathProbe<'data>),
    Collect(CollectSink<'out, 'cap, 'data, 'tail>),
}

impl<'out, 'cap, 'data, 'tail> PathSink<'out, 'cap, 'data, 'tail> {
    /// The one rider→sink decision — a single exhaustive match over the
    /// rider vocabulary, the only place the classic walk consults it.
    pub(crate) fn for_rider(
        rider: Option<&'cap mut Rider<'data, '_>>,
        out: &'out mut Matched,
        tail: MatchTail<'tail>,
    ) -> Self {
        match rider {
            Some(Rider::Probe(probe)) => PathSink::Probe(probe),
            Some(Rider::Cap(cap)) => PathSink::Collect(collect_sink(out, Some(cap), tail)),
            None => PathSink::Collect(collect_sink(out, None, tail)),
        }
    }
}

/// The collector arm's one construction — the cap a cap-rider holds (or
/// none), the tail, and the workspace-accounting fields.
fn collect_sink<'out, 'cap, 'data, 'tail>(
    out: &'out mut Matched,
    cap: Option<&'cap mut MatchCap<'data>>,
    tail: MatchTail<'tail>,
) -> CollectSink<'out, 'cap, 'data, 'tail> {
    CollectSink {
        out,
        cap,
        tail,
        charge: relate_core::tokens::WorkspaceCharge::empty(),
        meter: crate::workspace::BindingMeter::default(),
    }
}

impl BindingSink for PathSink<'_, '_, '_, '_> {
    fn offer(&mut self, graph: &ExecGraph<'_>, row: Row, used: &UsedRels) -> Result<()> {
        match self {
            PathSink::Probe(probe) => probe.offer(graph, row, used),
            PathSink::Collect(sink) => sink.offer(graph, row, used),
        }
    }

    fn halted(&self) -> bool {
        match self {
            PathSink::Probe(probe) => probe.halted(),
            PathSink::Collect(sink) => sink.halted(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_core::RelId;

    /// A batch of offers through one collector — the shape a walk's
    /// completed rows arrive in: many offers, one sink.
    fn offered_sets(tail: MatchTail<'_>, batches: &[&[RelId]]) -> Vec<UsedSet> {
        let base = crate::graph::Graph::default();
        let graph = ExecGraph::read(&base);
        let mut out: Matched = Vec::new();
        let mut sink = collect_sink(&mut out, None, tail);
        for ids in batches {
            let mut used = UsedRels::new();
            for &id in *ids {
                used.insert(id);
            }
            sink.offer(&graph, Row::new(), &used).unwrap();
        }
        out.into_iter().map(|(_, used)| used).collect()
    }

    #[test]
    fn carried_offers_store_each_rows_own_rels() {
        // LABS-822: every carried offer materializes its own set — the
        // conversion may not leak one row's rels into another row's
        // stored set, and an empty walk stores an empty set.
        let sets = offered_sets(MatchTail::Carry, &[&[1, 2], &[3], &[]]);
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[0], UsedSet::from_iter([1, 2]));
        assert_eq!(sets[1], UsedSet::from_iter([3]));
        assert_eq!(sets[2], UsedSet::default());
    }

    #[test]
    fn finished_offers_drop_the_carried_rels() {
        // LABS-822: a finish tail's offers arrive with their used-sets
        // dropped — the offered rels are not carried. (The
        // shared-empty representation those drops read is the
        // implementation's own cost story, in `UsedSet::default`.)
        let sets = offered_sets(MatchTail::Finish { reduce: None }, &[&[1, 2], &[3]]);
        assert_eq!(sets.len(), 2);
        assert!(sets.iter().all(|used| used.is_empty()));
    }
}

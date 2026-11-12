//! The one live-graph [`relate_planner::PatternStats`] implementation (LABS-351): the
//! executor's compound start-element estimate consumes it directly, and
//! the top-level crate's plan-time stats adapter delegates to it, so
//! one implementation answers both consumers.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::{BinaryOp, Expr, NodePattern, RelPattern};
use relate_names::{NamedToken, SmallName, Token, TokenResolver};
use relate_planner::{
    endpoint_estimate, seed_on_rel_start, walk_cost, Bound, ContainsCount, DegreeMoments,
};

use crate::graph::Graph;

/// Construction goes through [`GraphStats::new`] (the ADR-0218 one-
/// construction idiom the sibling plan-time stats impls use), so an
/// inline build cannot drift from it.
pub struct GraphStats<'a> {
    graph: &'a Graph,
    params: &'a BTreeMap<String, Value>,
}

impl<'a> GraphStats<'a> {
    /// The one construction: the connection's graph and the statement's
    /// parameters.
    pub fn new(graph: &'a Graph, params: &'a BTreeMap<String, Value>) -> GraphStats<'a> {
        GraphStats { graph, params }
    }

    /// One element's compound start estimate (LABS-351): the planner's
    /// canonical endpoint ladder over this graph's stats, mapped onto
    /// the one u64 scale the executor compares. The rank sentinels
    /// share the measured scale (`direction_cost` compares them
    /// directly); the bare rank — an unlabeled element, an all-node
    /// scan — stays the whole-graph sentinel. A sampled `CONTAINS` seed
    /// compounds (ADR-0474): its first hop is the degree-scaled output
    /// floored at the whole-label scan the unseekable filter pays, the
    /// same arithmetic the planner's `direction_cost` runs, so the
    /// per-row start selection and the plan-time orientation agree on
    /// the anchor (GBL SF0.1 q8: the Forum side's 13,750-row scan floor
    /// ties Person's degree-scaled walk and the tie-break's seed
    /// comparison — 1,528 Person rows against the floored 13,750 —
    /// flips the anchor to Person).
    pub(crate) fn estimate<'c>(
        &self,
        np: &'c NodePattern,
        bound: Bound<'_>,
        where_conjuncts: impl Iterator<Item = &'c Expr> + Clone,
        first_hop: Option<&RelPattern>,
        reversed: bool,
    ) -> u64 {
        let estimate = endpoint_estimate(np, bound, where_conjuncts, self);
        // Two local facts: the orientation flag is the caller's derived
        // one (the forward walker passes false, the backward walker's
        // k > 0 pivots pass true — LABS-426), and the walk prices this
        // start's first hop only. The model itself — which estimate kind
        // gets which walk, and the seed-side derivation — is
        // [`walk_cost`]'s and [`seed_on_rel_start`]'s to state.
        let rows = walk_cost(
            estimate,
            first_hop
                .into_iter()
                .map(|rel| (np, rel, seed_on_rel_start(rel, reversed))),
            self,
        );
        rows.min(u64::MAX as u128) as u64
    }
}

impl GraphStats<'_> {
    /// The store id of a name beside its planning token (ADR-0541): `Id`
    /// answers it, `Absent` answers `None` without a store read, and only
    /// `Unresolved` interns by name. `None` means no stored entity
    /// carries the name.
    fn id_of(&self, name: &SmallName, token: Token) -> Option<u32> {
        token.id_or_else(|| name.with_str(|text| self.graph.resolve(text).id()))
    }
}

impl relate_planner::PatternStats for GraphStats<'_> {
    fn label_rows(&self, (name, token): &NamedToken) -> Option<u64> {
        Some(
            self.id_of(name, *token)
                .map_or(0, |id| self.graph.label_count_by_id(id)),
        )
    }

    fn equality_rows(
        &self,
        (label, label_token): &NamedToken,
        property: &NamedToken,
        value: &Expr,
    ) -> Option<u64> {
        let value = value.seek_value(self.params)?;
        let lid = self.id_of(label, *label_token)?;
        let kid = self.id_of(&property.0, property.1)?;
        self.graph.pattern_equality_rows_by_id(lid, kid, &value)
    }

    /// ADR-0486: the range window's posting-length count, delegated to
    /// [`Graph::pattern_range_rows`] — the same `range_seek_cardinality`
    /// the executor's own seek cardinality reads, so the ladder's price
    /// and the runtime funnel decline together. The bound resolves the
    /// `equality_rows` way: literals and parameters only, `None` for
    /// anything else (a temporal literal's function call included — the
    /// funnel's `eval`-resolved bound is the runtime's, not the
    /// plan-time estimate's).
    fn range_rows(
        &self,
        (label, label_token): &NamedToken,
        property: &NamedToken,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        let bound = bound.seek_value(self.params)?;
        // The one by-name fallback: the range seek's owned forms are
        // names, so a resolved label still reads through the name-keyed
        // store path; an `Absent` label declines before any store read.
        if *label_token == Token::Absent {
            return None;
        }
        label.with_str(|label_text| {
            property.0.with_str(|property_text| {
                self.graph
                    .pattern_range_rows(label_text, property_text, op, &bound)
            })
        })
    }

    /// LABS-643: the count is always measurable — the TEXT index's exact
    /// answer, or without one the sampled estimate
    /// ([`Graph::pattern_contains_rows`]) — so only a non-string needle
    /// stays unmeasured (`None`, the ladder's fallback). The provenance
    /// stays typed in [`ContainsCount`]; the planner compounds the
    /// sampled form's scan floor on its one scale (ADR-0474). The sample
    /// rides the name-keyed store path like `range_rows`; an `Absent`
    /// label is the measurable zero the by-name path answers for a label
    /// never interned.
    fn contains_rows(
        &self,
        (label, label_token): &NamedToken,
        property: &NamedToken,
        needle: &Expr,
    ) -> Option<ContainsCount> {
        let needle = needle.seek_value(self.params)?;
        let Value::String(needle) = needle else {
            return None;
        };
        if *label_token == Token::Absent {
            return Some(ContainsCount::SampleScan { rows: 0, scan: 0 });
        }
        label.with_str(|label_text| {
            property.0.with_str(|property_text| {
                Some(
                    self.graph
                        .pattern_contains_rows(label_text, property_text, &needle),
                )
            })
        })
    }

    fn rel_type_rows(&self, (name, token): &NamedToken) -> Option<u64> {
        Some(
            self.id_of(name, *token)
                .map_or(0, |id| self.graph.pattern_rel_type_rows_by_id(id)),
        )
    }

    /// LABS-351: the live graph's histogram tail (the executor keeps the
    /// real one; the null stats default None).
    fn degree_tail(&self, label: &NamedToken, rel_type: &NamedToken) -> Option<u64> {
        let label_id = self.id_of(&label.0, label.1)?;
        let type_id = self.id_of(&rel_type.0, rel_type.1)?;
        self.graph.pattern_degree_tail_by_id(label_id, type_id)
    }

    /// LABS-991: the live histograms' any-label tail.
    fn any_degree_tail(&self, rel_type: Option<&SmallName>) -> Option<u64> {
        match rel_type {
            Some(rel_type) => {
                rel_type.with_str(|text| self.graph.pattern_any_degree_tail(Some(text)))
            }
            None => self.graph.pattern_any_degree_tail(None),
        }
    }

    /// The live histograms' out-degree moments. Several labels name the
    /// nodes carrying all of them, bounded by each label's own moments:
    /// the smaller sum is the tighter bound (the `degree_tail` min
    /// discipline). A name the store never interned selects nothing.
    fn degree_moments(
        &self,
        labels: &[NamedToken],
        rel_types: &[NamedToken],
    ) -> Option<DegreeMoments> {
        let type_ids: Vec<u32> = rel_types
            .iter()
            .filter_map(|(name, token)| self.id_of(name, *token))
            .collect();
        let types = if rel_types.is_empty() {
            None
        } else {
            Some(type_ids.as_slice())
        };
        if labels.is_empty() {
            return Some(self.graph.pattern_degree_moments(None, types));
        }
        labels
            .iter()
            .map(|(name, token)| {
                self.id_of(name, *token)
                    .map_or(DegreeMoments::default(), |id| {
                        self.graph.pattern_degree_moments(Some(id), types)
                    })
            })
            .min_by_key(|moments| moments.sum)
    }
}

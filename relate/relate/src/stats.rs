//! Planner cardinality hints read from the connection's graph.

use std::collections::BTreeMap;

use relate_executor::Graph;
use relate_ir::{BinaryOp, Expr};
use relate_names::{NamedToken, SmallName};
use relate_planner::PatternStats;

use crate::Value;

/// One node `(label, property)` pair a RANGE index serves — the typed fact
/// the `MissingIndex` notification reads (ADR-0363). The label and the bare
/// property are separate fields, so no caller re-parses an `index_text()`
/// string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RangeIndexedProperty {
    pub(crate) label: String,
    pub(crate) property: String,
}

/// The one contract every plan-tree stats read goes through (ADR-0218
/// estimates + typed node warnings): the planner's `PatternStats`
/// estimates plus the store's property-index facts — implemented by
/// [`GraphPatternStats`] (the one construction every plan read site
/// builds, ADR-0218) and by the test fakes.
pub(crate) trait PlanStats: PatternStats {
    /// The property indexes the store carries, as `:Label(property)` texts
    /// — the suggestions the plan tree's `AllNodesScan` warning names.
    fn property_index_texts(&self) -> Vec<String>;

    /// Every node `(label, property)` pair a RANGE index serves an equality
    /// seek on — the fact the `MissingIndex` notification tests against,
    /// read through the executor's typed catalog details (never a re-parse of
    /// `IndexDef.property`). A composite range index contributes every
    /// component (the executor materializes and seeks each one); a
    /// relationship index contributes no node pair.
    fn range_indexed_properties(&self) -> Vec<RangeIndexedProperty>;

    /// Whether the store carries a RANGE index serving an equality seek
    /// on one `(label, property)` pair — the one spelling of the
    /// predicate both the keyed-lookup narration (LABS-1083's
    /// `NodeByLabelSeek`) and the `MissingIndex` notification test
    /// against, defaulted over [`Self::range_indexed_properties`] so
    /// every implementor answers it without a second table.
    fn range_index_serves(&self, label: &str, property: &str) -> bool {
        self.range_indexed_properties()
            .iter()
            .any(|pair| pair.label == label && pair.property == property)
    }

    /// The store's active pattern-engine rung (LABS-1117), stamped on
    /// every `MATCH` plan node so EXPLAIN carries the one static fact
    /// about which rung governs the clause's gates. The live graph
    /// answers; test fakes decline (`None`), so their trees carry no rung.
    fn pattern_engine(&self) -> Option<relate_executor::PatternEngine> {
        None
    }
}

/// Label, index, and relationship-type counts from the connection's graph.
/// Parameter predicates resolve against the statement's parameter map.
/// Fields are private: construction goes through
/// [`GraphPatternStats::new`] (the one construction every plan read site
/// builds, ADR-0218), so an inline build cannot drift from it.
pub(crate) struct GraphPatternStats<'a> {
    graph: &'a Graph,
    params: &'a BTreeMap<String, Value>,
}

impl<'a> GraphPatternStats<'a> {
    /// The one construction every plan read site builds (ADR-0218): the
    /// connection's graph and the statement's parameters, read under the
    /// caller's read guard.
    pub(crate) fn new(
        graph: &'a relate_executor::Graph,
        params: &'a std::collections::BTreeMap<String, Value>,
    ) -> GraphPatternStats<'a> {
        GraphPatternStats { graph, params }
    }
}

impl PlanStats for GraphPatternStats<'_> {
    /// The property indexes the store carries, as `:Label(property)` texts —
    /// the suggestions the plan tree's `AllNodesScan` warning names. The
    /// catalog's own listing order, never a re-sorted copy.
    fn property_index_texts(&self) -> Vec<String> {
        self.graph
            .named_indexes()
            .into_iter()
            .map(|index| index.index_text())
            .collect()
    }

    /// The store's range-indexed node pairs, read through the executor's
    /// typed catalog details — the fact `MissingIndex` tests, never a
    /// re-parse of the rendered `:Label(property)` text. Every component of
    /// a composite range index is covered, a relationship index serves no
    /// node pair, and a comma-bearing property name stays one name.
    fn range_indexed_properties(&self) -> Vec<RangeIndexedProperty> {
        self.graph
            .range_indexed_node_properties()
            .into_iter()
            .map(|(label, property)| RangeIndexedProperty { label, property })
            .collect()
    }

    /// The store's static pattern-engine rung (LABS-1117): the
    /// `RELATE_PATTERN_ENGINE` value read once at `Database::open`.
    fn pattern_engine(&self) -> Option<relate_executor::PatternEngine> {
        Some(self.graph.pattern_engine())
    }
}

impl PatternStats for GraphPatternStats<'_> {
    // One canonical live-graph implementation (LABS-351): every
    // PatternStats method delegates to the executor's GraphStats, so
    // plan-time ordering and exec-time start selection read the same
    // numbers. Only property_index_texts and range_indexed_properties
    // (this crate's own plan-warning/notification facts) stay local.
    fn label_rows(&self, label: &NamedToken) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params).label_rows(label)
    }

    fn equality_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        value: &Expr,
    ) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params)
            .equality_rows(label, property, value)
    }

    /// ADR-0486: the range window rides the same delegation — without
    /// this forward the plan path answered the trait's default `None`
    /// and every live plan kept the label-count pricing (the
    /// `degree_tail` precedent, LABS-426).
    fn range_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params)
            .range_rows(label, property, op, bound)
    }

    fn contains_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        needle: &Expr,
    ) -> Option<relate_planner::ContainsCount> {
        relate_executor::GraphStats::new(self.graph, self.params)
            .contains_rows(label, property, needle)
    }

    fn rel_type_rows(&self, rel_type: &NamedToken) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params).rel_type_rows(rel_type)
    }

    /// LABS-426 (the mean-to-tail flip): the histogram tail rides the same
    /// delegation — without this forward the plan path answered the
    /// trait's default `None` and every live plan kept the mean.
    fn degree_tail(&self, label: &NamedToken, rel_type: &NamedToken) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params).degree_tail(label, rel_type)
    }

    /// LABS-991: the any-label tail rides the same delegation.
    fn any_degree_tail(&self, rel_type: Option<&SmallName>) -> Option<u64> {
        relate_executor::GraphStats::new(self.graph, self.params).any_degree_tail(rel_type)
    }

    /// The degree moments ride the same delegation.
    fn degree_moments(
        &self,
        labels: &[NamedToken],
        rel_types: &[NamedToken],
    ) -> Option<relate_planner::DegreeMoments> {
        relate_executor::GraphStats::new(self.graph, self.params).degree_moments(labels, rel_types)
    }

    /// The in-degree sum rides the same delegation.
    fn in_degree_sum(&self, labels: &[NamedToken], rel_types: &[NamedToken]) -> Option<u128> {
        relate_executor::GraphStats::new(self.graph, self.params).in_degree_sum(labels, rel_types)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use relate_ir::Expr;

    use crate::Value;

    #[test]
    fn seek_value_resolves_literals_and_parameters() {
        let mut params = BTreeMap::new();
        params.insert("city".into(), Value::String(Arc::from("Manila")));
        assert_eq!(
            Expr::Parameter("city".into()).seek_value(&params),
            Some(Value::String(Arc::from("Manila")))
        );
        assert_eq!(
            Expr::String("Toronto".into()).seek_value(&params),
            Some(Value::String(Arc::from("Toronto")))
        );
        assert_eq!(Expr::Parameter("missing".into()).seek_value(&params), None);
    }

    /// The plan-time adapter forwards the in-degree sum to the executor's
    /// live pair counts (LABS-1661): a sink label's arriving rels.
    #[test]
    fn in_degree_sum_delegates_to_the_live_pair_counts() {
        use relate_names::{SmallName, Token};
        use relate_planner::PatternStats;

        let graph = relate_executor::Graph::default();
        graph.begin_write().unwrap();
        let from = graph
            .create_node(vec!["Src".into()], BTreeMap::new())
            .unwrap();
        let to = graph
            .create_node(vec!["Sink".into()], BTreeMap::new())
            .unwrap();
        graph
            .create_rel(from, to, "R".into(), BTreeMap::new())
            .unwrap();
        graph.commit_write();
        let params = BTreeMap::new();
        let stats = super::GraphPatternStats::new(&graph, &params);
        let named = |name: &str| (SmallName::from_name(name), Token::Unresolved);
        assert_eq!(
            stats.in_degree_sum(&[named("Sink")], &[named("R")]),
            Some(1)
        );
        assert_eq!(stats.in_degree_sum(&[named("Src")], &[]), Some(0));
    }
}

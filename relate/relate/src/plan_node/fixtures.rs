//! Shared plan-node test fixtures: the pattern builders the plan_node
//! submodules' tests build their subjects from — one spelling of the
//! bare `NodePattern` (and its companions), never a per-module copy —
//! plus the seek-capable stats stub ADR-0486's derivation tests read.

use std::collections::BTreeMap;

use relate_ir::{BinaryOp, Direction, Expr, NodePattern, PathPattern, RelPattern};
use relate_names::{NamedToken, SmallName};
use relate_planner::PatternStats;

use crate::stats::PlanStats;

/// The fold-shape builders shared with the executor's candidate-funnel
/// suite — one spelling, re-exported for the plan-node submodules.
pub(super) use relate_test_support::fold_shapes::{comparison, or_node};

/// A bare node pattern with the given variable and labels.
pub(super) fn node(variable: Option<&str>, labels: &[&str]) -> NodePattern {
    NodePattern {
        variable: variable.map(SmallName::from_name),
        labels: labels
            .iter()
            .map(|s| (SmallName::from_name(s), Default::default()))
            .collect(),
        label_expr: None,
        properties: None,
        prop_keys: Vec::new(),
        where_clause: None,
        force_scan: false,
    }
}

/// A node pattern with a folded property map — ADR-0024's fold shape,
/// the anchored-equality payload the point read narrates (ADR-0486).
pub(super) fn predicated(
    variable: Option<&str>,
    label: &str,
    prop: &str,
    value: Expr,
) -> NodePattern {
    let mut pattern = node(variable, &[label]);
    pattern.properties = Some(Expr::Map(vec![(prop.to_string(), value)]));
    pattern
}

/// One rel pattern with the given types and quantifier bounds.
pub(super) fn rel(types: &[&str], min: u32, max: Option<u32>) -> RelPattern {
    RelPattern {
        variable: None,
        types: types
            .iter()
            .map(|s| (SmallName::from_name(s), Default::default()))
            .collect(),
        type_expr: None,
        direction: Direction::Outgoing,
        properties: None,
        prop_keys: Vec::new(),
        var_length: max.is_some() || min > 1,
        min_hops: min,
        max_hops: max,
        where_clause: None,
        quantified: false,
    }
}

/// A compact alternating pattern: nodes then rels between them.
pub(super) fn compact(nodes: Vec<NodePattern>, rels: Vec<RelPattern>) -> PathPattern {
    PathPattern {
        variable: None,
        subpath_variable: None,
        elements: Vec::new(),
        nodes,
        rels,
        where_clause: None,
        mode: None,
        selector: None,
        is_shortest_path_function: false,
    }
}

/// A seek-capable stats stub (ADR-0486's derivation tests): the label
/// counts, the equality hit counts, the range window counts, and the
/// range-indexed (label, property) pairs (LABS-1083's keyed-lookup
/// narration), answered from tables. The indexed lookups model the live
/// stats' bound resolution — `seek_value` through the stub's parameters —
/// so a bound that does not resolve stays uncounted exactly the way the
/// real index layer declines it.
#[derive(Clone)]
pub(super) struct Seeking {
    labels: Vec<(&'static str, u64)>,
    equalities: Vec<(&'static str, &'static str, u64)>,
    ranges: Vec<(&'static str, &'static str, u64)>,
    indexed: Vec<(&'static str, &'static str)>,
    params: BTreeMap<String, crate::Value>,
}

/// The one construction of the [`Seeking`] stub.
pub(super) fn seeking(
    labels: &[(&'static str, u64)],
    equalities: &[(&'static str, &'static str, u64)],
    ranges: &[(&'static str, &'static str, u64)],
    params: BTreeMap<String, crate::Value>,
) -> Seeking {
    Seeking {
        labels: labels.to_vec(),
        equalities: equalities.to_vec(),
        ranges: ranges.to_vec(),
        indexed: Vec::new(),
        params,
    }
}

impl Seeking {
    /// The range-indexed (label, property) pairs the keyed-lookup
    /// narration reads (LABS-1083) — the store's typed index facts.
    pub(super) fn indexed(mut self, pairs: &[(&'static str, &'static str)]) -> Self {
        self.indexed = pairs.to_vec();
        self
    }
}

impl PlanStats for Seeking {
    fn property_index_texts(&self) -> Vec<String> {
        Vec::new()
    }
    fn range_indexed_properties(&self) -> Vec<crate::stats::RangeIndexedProperty> {
        self.indexed
            .iter()
            .map(|(label, property)| crate::stats::RangeIndexedProperty {
                label: (*label).to_string(),
                property: (*property).to_string(),
            })
            .collect()
    }
}

/// The one `(label, property)` → rows table lookup the equality and
/// range stubs share — never a second spelling of the lookup policy.
fn pair_rows(
    table: &[(&'static str, &'static str, u64)],
    label: &str,
    property: &str,
) -> Option<u64> {
    table
        .iter()
        .find(|(l, p, _)| *l == label && *p == property)
        .map(|(_, _, rows)| *rows)
}

/// The one `name` → rows lookup the label-count stubs share.
pub(super) fn rows_of(table: &[(&'static str, u64)], key: &SmallName) -> Option<u64> {
    table
        .iter()
        .find(|(name, _)| key == *name)
        .map(|(_, rows)| *rows)
}

impl PatternStats for Seeking {
    // The fixture tables are (&str, &str, u64) literals; the name-domain
    // parameters probe them through the text bridge.
    fn label_rows(&self, (label, _): &NamedToken) -> Option<u64> {
        rows_of(&self.labels, label)
    }
    fn equality_rows(
        &self,
        (label, _): &NamedToken,
        property: &NamedToken,
        value: &Expr,
    ) -> Option<u64> {
        value.seek_value(&self.params)?;
        label.with_str(|l| property.0.with_str(|p| pair_rows(&self.equalities, l, p)))
    }
    fn range_rows(
        &self,
        (label, _): &NamedToken,
        property: &NamedToken,
        _: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        bound.seek_value(&self.params)?;
        label.with_str(|l| property.0.with_str(|p| pair_rows(&self.ranges, l, p)))
    }
    fn contains_rows(
        &self,
        _: &NamedToken,
        _: &NamedToken,
        _: &Expr,
    ) -> Option<relate_planner::ContainsCount> {
        None
    }
    fn rel_type_rows(&self, _: &NamedToken) -> Option<u64> {
        None
    }
}

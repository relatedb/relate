//! The path-pattern payload walk (ADR-0363 decision 13): one post-rewrite
//! enumeration of a pattern's node, relationship and quantified-group
//! payloads, shared by the clause walk and the notification detectors.
//!
//! Split from [`super`] so the clause-tree walker and this pattern walker
//! stay separate concerns; [`super`] re-exports both items so
//! `relate_ir::{walk_pattern_payloads, PatternPayload}` remains the one
//! public path.

use relate_names::SmallName;

use crate::walk::each_label_expr_child;
use crate::{Expr, NodePattern, PathPattern, PatternElement, QuantifiedPathPattern, RelPattern};

/// One node, relationship or quantified-group payload of a path pattern, as
/// [`walk_pattern_payloads`] visits it.
#[derive(Clone, Copy, Debug)]
pub enum PatternPayload<'a> {
    Node(&'a NodePattern),
    Relationship(&'a RelPattern),
    Quantified(&'a QuantifiedPathPattern),
}

impl<'a> PatternPayload<'a> {
    /// The payload one element carries — the element view's entry to the
    /// same payload vocabulary the path walk visits, so a consumer that
    /// already holds `elements` need not re-spell the three-variant match.
    pub fn of(element: &'a PatternElement) -> Self {
        match element {
            PatternElement::Node(node) => PatternPayload::Node(node),
            PatternElement::Relationship(rel) => PatternPayload::Relationship(rel),
            PatternElement::Quantified(qpp) => PatternPayload::Quantified(qpp),
        }
    }

    /// The variable this payload binds: a node's or relationship's own
    /// name, a quantified group's subpath name — the one three-variant
    /// variable read every payload consumer shares.
    pub fn variable(&self) -> Option<&'a SmallName> {
        match self {
            PatternPayload::Node(node) => node.variable.as_ref(),
            PatternPayload::Relationship(rel) => rel.variable.as_ref(),
            PatternPayload::Quantified(qpp) => qpp.subpath_variable.as_ref(),
        }
    }
}

/// Visit every node and relationship payload of `pattern` exactly once,
/// quantified groups descended (a group is seen before the payloads nested
/// in it).
///
/// One post-rewrite precedence rules the two representations lowering
/// carries: the compact `nodes`/`rels` view is authoritative whenever it is
/// populated and the pattern does not need the GPM form, because
/// `fold_where` moves `WHERE` equalities and label predicates into that
/// view only; the lossless `elements` sequence is authoritative for
/// quantified/GPM shapes ([`PathPattern::needs_gpm`]) and for legacy IR
/// built with only that view. Walking both would double-visit every payload
/// and would read the pre-fold twins.
pub fn walk_pattern_payloads<'a>(
    pattern: &'a PathPattern,
    on_payload: &mut impl FnMut(PatternPayload<'a>),
) {
    let compact = !pattern.nodes.is_empty() || !pattern.rels.is_empty();
    if compact && (pattern.elements.is_empty() || !pattern.needs_gpm()) {
        for node in &pattern.nodes {
            on_payload(PatternPayload::Node(node));
        }
        for rel in &pattern.rels {
            on_payload(PatternPayload::Relationship(rel));
        }
    } else {
        walk_pattern_elements(&pattern.elements, on_payload);
    }
}

/// One payload's field enumeration — its binding, the dynamic operands of
/// its label/type expression, its property map and its inline predicate.
/// The one payload field list: the clause-tree walk and the read-set
/// collector both build on it, so a new payload field is handled here once.
pub(crate) fn for_each_payload_field<'a>(
    payload: PatternPayload<'a>,
    on_expr: &mut impl FnMut(&'a Expr),
    on_binding: &mut impl FnMut(&'a SmallName),
) {
    if let Some(variable) = payload.variable() {
        on_binding(variable);
    }
    match payload {
        PatternPayload::Node(node) => {
            if let Some(labels) = &node.label_expr {
                each_label_expr_child(labels, on_expr);
            }
            if let Some(properties) = &node.properties {
                on_expr(properties);
            }
            if let Some(where_clause) = &node.where_clause {
                on_expr(where_clause);
            }
        }
        PatternPayload::Relationship(rel) => {
            if let Some(types) = &rel.type_expr {
                each_label_expr_child(types, on_expr);
            }
            if let Some(properties) = &rel.properties {
                on_expr(properties);
            }
            if let Some(where_clause) = &rel.where_clause {
                on_expr(where_clause);
            }
        }
        PatternPayload::Quantified(quantified) => {
            if let Some(where_clause) = &quantified.where_clause {
                on_expr(where_clause);
            }
        }
    }
}

/// [`walk_pattern_payloads`]' one-shot boolean twin: whether any payload
/// of `pattern` satisfies `pred`. The walk owns the accumulator, so a
/// payload predicate stays one call instead of a hand-rolled
/// `let mut found = false; … found |= …` fold — the same shape the
/// plan-side walk helpers exist to delete.
pub fn any_pattern_payload<'a>(
    pattern: &'a PathPattern,
    pred: &mut impl FnMut(PatternPayload<'a>) -> bool,
) -> bool {
    let mut found = false;
    walk_pattern_payloads(pattern, &mut |payload| found |= pred(payload));
    found
}

/// The [`PatternElement`] recursion behind [`walk_pattern_payloads`], and
/// the one element-granular walk for a consumer that already holds an
/// `elements` list: the element→payload mapping lives in
/// [`PatternPayload::of`]; only the quantified descent is element-specific.
pub fn walk_pattern_elements<'a>(
    elements: &'a [PatternElement],
    on_payload: &mut impl FnMut(PatternPayload<'a>),
) {
    for element in elements {
        on_payload(PatternPayload::of(element));
        if let PatternElement::Quantified(quantified) = element {
            walk_pattern_elements(&quantified.elements, on_payload);
        }
    }
}

/// A pattern's node anchor variables, enumerated through the one payload
/// visitor — the post-rewrite view precedence and quantified-group
/// recursion live there, not here. The canonical enumeration for every
/// consumer of "which node variables does this pattern bind" (the
/// facade's CartesianProduct warning; the planner's ADR-0504 fast-path
/// classifier) — hoisted so no consumer re-walks the compact views.
pub fn pattern_node_anchor_variables(pattern: &PathPattern) -> Vec<SmallName> {
    let mut variables = Vec::new();
    walk_pattern_payloads(pattern, &mut |payload| {
        if let PatternPayload::Node(node) = payload {
            if let Some(variable) = node.variable.as_ref() {
                variables.push(variable.clone());
            }
        }
    });
    variables
}

/// Every variable one pattern binds — its path names, node and
/// relationship variables, and quantified subpath variables — through
/// the same one payload visitor as [`pattern_node_anchor_variables`],
/// in a single traversal. Declaration order is promised: the path names
/// first, then the payload variables in their spelled order, a
/// quantified subpath variable before its group's members. A name may
/// repeat (the path-names prefix and the payload walk can both yield
/// it); every consumer dedups or membership-tests, so no dedup is
/// promised here.
pub fn pattern_anchor_variables(pattern: &PathPattern) -> Vec<SmallName> {
    let mut variables: Vec<SmallName> = pattern.path_names().cloned().collect();
    walk_pattern_payloads(pattern, &mut |payload| {
        if let Some(name) = payload.variable() {
            variables.push(name.clone());
        }
    });
    variables
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QuantifiedPathPattern;

    fn node(variable: &str, label: &str) -> NodePattern {
        NodePattern {
            variable: Some(variable.into()),
            labels: vec![(label.into(), Default::default())],
            label_expr: None,
            properties: None,
            prop_keys: Vec::new(),
            where_clause: None,
            force_scan: false,
        }
    }

    /// The payload visitor owns the post-rewrite precedence: a non-GPM
    /// pattern reads the compact view (where `fold_where` wrote), while a
    /// quantified/GPM shape reads the lossless element sequence and
    /// descends into each group.
    #[test]
    fn walk_pattern_payloads_owns_the_post_rewrite_view_precedence() {
        let element_only = node("element", "Elements");
        let mut folded = PathPattern {
            variable: None,
            subpath_variable: None,
            nodes: vec![node("compact", "Compact")],
            rels: Vec::new(),
            elements: vec![PatternElement::Node(element_only.clone())],
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        };
        let mut seen = Vec::new();
        walk_pattern_payloads(&folded, &mut |payload| {
            if let PatternPayload::Node(node) = payload {
                seen.push(node.labels[0].0.clone());
            }
        });
        assert_eq!(
            seen,
            ["Compact"],
            "the compact view wins for a folded shape"
        );

        // A quantified element sequence forces the elements view even though
        // a compact twin is present.
        folded.elements = vec![PatternElement::Quantified(QuantifiedPathPattern {
            subpath_variable: Some("q".into()),
            elements: vec![PatternElement::Node(element_only)],
            where_clause: None,
            min: 1,
            max: None,
        })];
        let mut seen = Vec::new();
        let mut groups = 0;
        walk_pattern_payloads(&folded, &mut |payload| match payload {
            PatternPayload::Node(node) => seen.push(node.labels[0].0.clone()),
            PatternPayload::Relationship(_) => {}
            PatternPayload::Quantified(_) => groups += 1,
        });
        assert_eq!(seen, ["Elements"], "a quantified shape reads elements");
        assert_eq!(groups, 1, "the group is visited before its payloads");
    }
}

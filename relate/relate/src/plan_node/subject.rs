//! The MATCH subject (ADR-0344 phase 3): the operator's own object —
//! the pattern text shown in the `Details` column, and the cardinality
//! estimate ADR-0218 `PatternStats` can give it honestly.
//!
//! The pattern text is the static subject (variables, labels, rel types,
//! quantifier bounds); property and `WHERE` predicates are filters, not
//! the subject, so they stay out of the text (Neo4j renders them as
//! separate operators too). Label and type expressions beyond the
//! static conjunction/disjunction are not statically printable; the
//! retained static `labels`/`types` lists are what render.
//!
//! An index-backed start (ADR-0486) replaces the fused narration: the
//! operator becomes the seek's name (`NodeIndexSeek`,
//! `NodeIndexSeekByRange`) and the seek line leads the details — the
//! clause-granularity rename ADR-0344 phase 3 prescribes, never new
//! child nodes. The derivation lives in [`super::seek`].

use std::borrow::Cow;

use relate_ir::{Direction, LabelExpr, NodePattern, PathPattern, PatternElement, RelPattern};
use relate_names::SmallName;

use super::seek::IndexSeek;
use crate::stats::PlanStats;

/// The MATCH subject: the operator a pattern set lowers to and its
/// details text. A lone node with no relationships is a scan (it names
/// its labels, or scans all nodes when it has none); anything with a
/// relationship is an `Expand` over the rendered pattern texts. The
/// subject form names the operator; the `Optional(…)` wrap is derived
/// once here from that one fact — never re-spelled inside a form. An
/// index-backed start (ADR-0486, `seek`) replaces the fused narration:
/// the operator becomes the seek's name and the seek line leads the
/// details.
pub(super) fn match_subject(
    patterns: &[PathPattern],
    optional: bool,
    seek: Option<&IndexSeek>,
) -> (Cow<'static, str>, String) {
    let (operator, details) = if let Some(seek) = seek {
        seek_subject(seek, patterns)
    } else {
        fused_subject(patterns)
    };
    // The `Optional(…)` wrap, derived from the one operator fact —
    // whichever subject form named it.
    let operator = if optional {
        Cow::Owned(format!("Optional({operator})"))
    } else {
        operator
    };
    (operator, details)
}

/// The fused narration every unseekable MATCH keeps (ADR-0486's
/// fallbacks): a scan naming its labels, an `Expand` over the pattern
/// texts, the base operator the caller wraps.
fn fused_subject(patterns: &[PathPattern]) -> (Cow<'static, str>, String) {
    let operator = match lone_node_of(patterns) {
        Some(node) if is_all_nodes_scan(node) => "AllNodesScan",
        Some(_) => "NodeByLabelScan",
        None => "Expand",
    };
    (Cow::Borrowed(operator), pattern_texts(patterns))
}

/// The seek's replacement of the fused narration (ADR-0486): the
/// operator becomes the seek's name and the details lead with the seek
/// line. A multi-element pattern keeps its pattern text after the line
/// (the hops the fused operator still walks); a lone node's text the
/// line replaces.
fn seek_subject(seek: &IndexSeek, patterns: &[PathPattern]) -> (Cow<'static, str>, String) {
    let mut details = seek.line.clone();
    if lone_node_of(patterns).is_none() {
        details += ", ";
        details.push_str(&pattern_texts(patterns));
    }
    (Cow::Borrowed(seek.operator), details)
}

/// Whether a lone node pattern scans every node — no labels, however they
/// were spelled (`labels` retains the static conjunction; `label_expr` the
/// non-static spelling). The one derivation the operator naming and the
/// `AllNodesScan` warning detection share.
pub(super) fn is_all_nodes_scan(node: &NodePattern) -> bool {
    node.labels.is_empty() && node.label_expr.is_none()
}

/// A pattern set's lone node, when the set is a single pattern that is a
/// single node with no relationships — the scan shape. The one MATCH-shape
/// decomposition every read of the shape shares (the operator naming, the
/// estimate, the scan warning), never a second `[single]` + `lone_node`
/// spelling at a caller.
pub(super) fn lone_node_of(patterns: &[PathPattern]) -> Option<&NodePattern> {
    let [single] = patterns else {
        return None;
    };
    lone_node(single)
}

/// The node's one static label, when it has exactly one and no dynamic label
/// expression beyond a static conjunction — the only shape a single
/// `(label, property)` index can serve. The one derivation the estimate and
/// the `MissingIndex` notification share.
pub(super) fn single_static_label(node: &NodePattern) -> Option<&SmallName> {
    let [(label, _)] = node.labels.as_slice() else {
        return None;
    };
    if node
        .label_expr
        .as_ref()
        .is_some_and(|expr| !expr.is_static_conjunction())
    {
        return None;
    }
    Some(label)
}

/// Every pattern's text, statement order, comma-joined — the subject of a
/// multi-pattern MATCH or a CREATE.
pub(super) fn pattern_texts(patterns: &[PathPattern]) -> String {
    patterns
        .iter()
        .map(pattern_text)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The cardinality estimate for a MATCH's subject from ADR-0218
/// `PatternStats` — only what the stats offer honestly: an index-backed
/// start's own count (`seek_rows`, ADR-0486 — the narration and the
/// estimate gate together, so a narrated seek is never priced at
/// anything but the seek's count), a single-label scan's label row count
/// — the scan-then-filter reality a fused operator without a serving
/// index pays — and a single typed relationship's type row count when
/// both endpoints are unfiltered. `None` everywhere else (a multi-pattern
/// cartesian, a multi-label node, a quantified path — no signal covers
/// those).
pub(super) fn match_estimate(
    patterns: &[PathPattern],
    stats: &dyn PlanStats,
    seek_rows: Option<u64>,
) -> Option<u64> {
    // The index-backed start's own count (ADR-0486): the same `Option`
    // that narrates the seek prices it.
    if let Some(rows) = seek_rows {
        return Some(rows);
    }
    // One decomposition per function: the single pattern destructured
    // once at the top, the scan shape read off it through `lone_node`
    // (the shape half of `lone_node_of`, whose other callers hold no
    // other need for the single pattern).
    let [single] = patterns else {
        return None;
    };
    if let Some(node) = lone_node(single) {
        // One static label (the `labels` list is its retained conjunction,
        // however the label expression spelled it): the stats count it.
        // A conjunction of several labels has no count of its own.
        if single_static_label(node).is_some() {
            // The indexed point read and the range window narrate as the
            // seek above, whose own rows are this node's estimate.
            // Without one, the fused scan-then-filter operator really
            // scans the whole label before filtering — the label count IS
            // the honest estimate, and the est-vs-actual ⚠ it fires on a
            // point lookup is the missing-index diagnosis (the LABS-328
            // class), pairing with the scan warnings.
            return stats.label_rows(&node.labels[0]);
        }
        return None;
    }
    // A single relationship between two unfiltered endpoints: the
    // relationship type's row count is the pattern's row count.
    if single.where_clause.is_some() {
        return None;
    }
    let [start, end] = &single.nodes[..] else {
        return None;
    };
    let [rel] = &single.rels[..] else {
        return None;
    };
    if !node_is_unfiltered(start)
        || !node_is_unfiltered(end)
        || rel.var_length
        || rel.quantified
        || rel.properties.is_some()
        || rel.where_clause.is_some()
        || rel.types.len() != 1
        || !rel
            .type_expr
            .as_ref()
            .is_none_or(LabelExpr::is_static_disjunction)
    {
        return None;
    }
    stats.rel_type_rows(&rel.types[0])
}

/// A pattern's lone node, when the pattern is a single node with no
/// relationships — the scan shape.
pub(super) fn lone_node(pattern: &PathPattern) -> Option<&NodePattern> {
    if pattern.rels.is_empty() && pattern.nodes.len() == 1 {
        Some(&pattern.nodes[0])
    } else {
        None
    }
}

/// A node endpoint the stats say nothing about (no labels, no
/// properties, no predicates) — its count cannot narrow the estimate.
fn node_is_unfiltered(node: &NodePattern) -> bool {
    is_all_nodes_scan(node) && node.properties.is_none() && node.where_clause.is_none()
}

/// One pattern as text: the named-path prefix, then the elements (the
/// lossless form) or the compact node/rel interleave.
pub(super) fn pattern_text(pattern: &PathPattern) -> String {
    let mut text = String::new();
    if let Some(variable) = &pattern.variable {
        push_name(&mut text, variable);
        text.push_str(" = ");
    }
    if !pattern.elements.is_empty() {
        for element in &pattern.elements {
            push_element(&mut text, element);
        }
    } else {
        for (index, node) in pattern.nodes.iter().enumerate() {
            push_node(&mut text, node);
            if let Some(rel) = pattern.rels.get(index) {
                push_rel(
                    &mut text,
                    rel,
                    (rel.var_length || rel.quantified).then_some((rel.min_hops, rel.max_hops)),
                );
            }
        }
    }
    text
}

/// Append one pattern element — a node, a directed relationship, or a
/// quantified group with its hop bounds. A quantified element that IS a
/// var-length relationship (every `-[r:T*m..n]-` spelling lowers to it,
/// LABS-661) renders in the compact spelling: the plan text is the same
/// for every spelling of the pattern, one engine.
fn push_element(text: &mut String, element: &PatternElement) {
    match element {
        PatternElement::Node(node) => push_node(text, node),
        PatternElement::Relationship(rel) => push_rel(
            text,
            rel,
            (rel.var_length || rel.quantified).then_some((rel.min_hops, rel.max_hops)),
        ),
        PatternElement::Quantified(quantified) => {
            if let Some(rel) = quantified.implicit_var_length_rel() {
                push_rel(text, rel, Some((quantified.min, quantified.max)));
                return;
            }
            text.push('(');
            for element in &quantified.elements {
                push_element(text, element);
            }
            text.push(')');
            push_quantifier(text, quantified.min, quantified.max);
        }
    }
}

/// Push one name's text (the narration renders names through their text
/// arms — byte-identical to the query's spelling).
fn push_name(text: &mut String, name: &SmallName) {
    name.with_str(|name_text| text.push_str(name_text));
}

/// Append one node: `(variable:Label1:Label2)`.
fn push_node(text: &mut String, node: &NodePattern) {
    text.push('(');
    if let Some(variable) = &node.variable {
        push_name(text, variable);
    }
    for label in node.label_names() {
        text.push(':');
        push_name(text, label);
    }
    text.push(')');
}

/// Append one relationship with its direction: `-[v:T1|T2*1..2]->`. The
/// quantifier bounds are the caller's — the rel's own for a plain
/// var-length element, the quantifier's for a quantified element rendered
/// in the compact spelling.
fn push_rel(text: &mut String, rel: &RelPattern, quantifier: Option<(u32, Option<u32>)>) {
    if rel.direction == Direction::Incoming {
        text.push_str("<-");
    } else {
        text.push('-');
    }
    text.push('[');
    if let Some(variable) = &rel.variable {
        push_name(text, variable);
    }
    for (index, rel_type) in rel.type_names().enumerate() {
        if index > 0 {
            text.push('|');
        }
        text.push(':');
        push_name(text, rel_type);
    }
    if let Some((min, max)) = quantifier {
        push_quantifier(text, min, max);
    }
    text.push(']');
    if rel.direction == Direction::Outgoing {
        text.push_str("->");
    } else {
        text.push('-');
    }
}

/// Append a hop-bounds quantifier: `*n` for a bounded exact count, else
/// `*min..max` (`max` empty when the bound is open).
fn push_quantifier(text: &mut String, min: u32, max: Option<u32>) {
    text.push('*');
    match max {
        Some(max) if max == min => text.push_str(&min.to_string()),
        Some(max) => {
            text.push_str(&min.to_string());
            text.push_str("..");
            text.push_str(&max.to_string());
        }
        None => {
            text.push_str(&min.to_string());
            text.push_str("..");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate_names::NamedToken;
    use relate_planner::PatternStats;

    use super::super::fixtures::{compact, comparison, node, rel, rows_of, seeking};
    use super::super::seek::index_seek;
    use super::*;

    #[test]
    fn scan_subjects_name_their_labels() {
        let labeled = compact(vec![node(Some("n"), &["Person"])], Vec::new());
        assert_eq!(
            match_subject(&[labeled.clone()], false, None),
            ("NodeByLabelScan".into(), "(n:Person)".to_string())
        );
        let bare = compact(vec![node(None, &[])], Vec::new());
        assert_eq!(
            match_subject(&[bare], false, None),
            ("AllNodesScan".into(), "()".to_string())
        );
    }

    #[test]
    fn expand_subjects_carry_the_pattern_text_with_bounds() {
        let quantified = compact(
            vec![node(None, &["Person"]), node(None, &["Person"])],
            vec![rel(&["KNOWS"], 1, Some(2))],
        );
        assert_eq!(
            match_subject(&[quantified.clone()], false, None),
            (
                "Expand".into(),
                "(:Person)-[:KNOWS*1..2]->(:Person)".to_string()
            )
        );
        let fixed = compact(
            vec![node(Some("a"), &[]), node(Some("b"), &[])],
            vec![rel(&["KNOWS"], 1, None)],
        );
        assert_eq!(
            match_subject(&[fixed], false, None),
            ("Expand".into(), "(a)-[:KNOWS]->(b)".to_string())
        );
        // An open-bounded var-length: `*1..`.
        let mut open = rel(&["KNOWS"], 1, None);
        open.var_length = true;
        let open = compact(vec![node(Some("a"), &[]), node(Some("b"), &[])], vec![open]);
        assert_eq!(
            match_subject(&[open], false, None),
            ("Expand".into(), "(a)-[:KNOWS*1..]->(b)".to_string())
        );
    }

    #[test]
    fn optional_matches_wrap_their_operator_prefix() {
        let bare = compact(vec![node(None, &[])], Vec::new());
        assert_eq!(
            match_subject(&[bare], true, None),
            ("Optional(AllNodesScan)".into(), "()".to_string())
        );
        let labeled = compact(vec![node(Some("n"), &["Person"])], Vec::new());
        assert_eq!(
            match_subject(&[labeled], true, None),
            ("Optional(NodeByLabelScan)".into(), "(n:Person)".to_string())
        );
        let expand = compact(
            vec![node(Some("a"), &[]), node(Some("b"), &[])],
            vec![rel(&["KNOWS"], 1, None)],
        );
        assert_eq!(
            match_subject(&[expand], true, None),
            ("Optional(Expand)".into(), "(a)-[:KNOWS]->(b)".to_string())
        );
    }

    #[test]
    fn multi_pattern_matches_join_their_texts() {
        let first = compact(vec![node(Some("a"), &["A"])], Vec::new());
        let second = compact(vec![node(Some("b"), &["B"])], Vec::new());
        assert_eq!(
            match_subject(&[first, second], false, None),
            ("Expand".into(), "(a:A), (b:B)".to_string())
        );
    }

    #[test]
    fn named_paths_and_quantified_groups_render() {
        let mut named = compact(
            vec![node(Some("a"), &[]), node(Some("b"), &[])],
            vec![rel(&["R"], 1, None)],
        );
        named.variable = Some(SmallName::from_name("p"));
        assert_eq!(pattern_text(&named), "p = (a)-[:R]->(b)");
        let mut group = compact(
            vec![node(None, &[]), node(None, &[])],
            vec![rel(&["R"], 1, None)],
        );
        group.elements = vec![
            PatternElement::Node(node(None, &[])),
            PatternElement::Relationship(rel(&["R"], 1, None)),
            PatternElement::Node(node(None, &[])),
        ];
        assert_eq!(pattern_text(&group), "()-[:R]->()");
        // A quantified group renders its bounds: `((a)-[:R]->(b))*1..2`.
        let quantified = PathPattern {
            variable: None,
            subpath_variable: None,
            elements: vec![PatternElement::Quantified(
                relate_ir::QuantifiedPathPattern {
                    subpath_variable: None,
                    elements: vec![
                        PatternElement::Node(node(Some("a"), &[])),
                        PatternElement::Relationship(rel(&["R"], 1, None)),
                        PatternElement::Node(node(Some("b"), &[])),
                    ],
                    where_clause: None,
                    min: 1,
                    max: Some(2),
                },
            )],
            nodes: Vec::new(),
            rels: Vec::new(),
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        };
        assert_eq!(pattern_text(&quantified), "((a)-[:R]->(b))*1..2");
    }

    /// LABS-661: a quantified element that IS a var-length relationship
    /// (implicit boundary nodes — what every `-[r:T*m..n]-` spelling
    /// lowers to) renders in the compact spelling, so the plan text is
    /// identical for every spelling of the pattern.
    #[test]
    fn quantified_var_length_renders_in_the_compact_spelling() {
        let quantified = PathPattern {
            variable: Some(SmallName::from_name("p")),
            subpath_variable: None,
            elements: vec![
                PatternElement::Node(node(Some("a"), &["A"])),
                PatternElement::Quantified(relate_ir::QuantifiedPathPattern {
                    subpath_variable: None,
                    elements: vec![
                        PatternElement::Node(node(None, &[])),
                        // The plain one-hop inner rel (`var_length` false):
                        // the hop bounds live on the quantifier.
                        PatternElement::Relationship(rel(&["R"], 1, None)),
                        PatternElement::Node(node(None, &[])),
                    ],
                    where_clause: None,
                    min: 1,
                    max: Some(2),
                }),
                PatternElement::Node(node(None, &["B"])),
            ],
            nodes: Vec::new(),
            rels: Vec::new(),
            where_clause: None,
            mode: None,
            selector: None,
            is_shortest_path_function: false,
        };
        assert_eq!(pattern_text(&quantified), "p = (a:A)-[:R*1..2]->(:B)");
    }

    /// Stats offering exactly one label count and one rel-type count.
    struct Counting {
        labels: Vec<(&'static str, u64)>,
        rels: Vec<(&'static str, u64)>,
    }

    impl PlanStats for Counting {
        fn property_index_texts(&self) -> Vec<String> {
            Vec::new()
        }
        fn range_indexed_properties(&self) -> Vec<crate::stats::RangeIndexedProperty> {
            Vec::new()
        }
    }

    impl PatternStats for Counting {
        // The fixture tables are String literals; the name-domain
        // parameters compare by text.
        fn label_rows(&self, (label, _): &NamedToken) -> Option<u64> {
            rows_of(&self.labels, label)
        }
        fn equality_rows(
            &self,
            _: &NamedToken,
            _: &NamedToken,
            _: &relate_ir::Expr,
        ) -> Option<u64> {
            None
        }
        fn contains_rows(
            &self,
            _: &NamedToken,
            _: &NamedToken,
            _: &relate_ir::Expr,
        ) -> Option<relate_planner::ContainsCount> {
            None
        }
        fn rel_type_rows(&self, (rel_type, _): &NamedToken) -> Option<u64> {
            self.rels
                .iter()
                .find(|(t, _)| rel_type == *t)
                .map(|(_, rows)| *rows)
        }
    }

    #[test]
    fn estimates_come_from_the_stats_honestly() {
        let stats = Counting {
            labels: vec![("Person", 2)],
            rels: vec![("KNOWS", 3)],
        };
        // A single-label scan: the label's row count.
        let scan = compact(vec![node(Some("n"), &["Person"])], Vec::new());
        assert_eq!(match_estimate(&[scan], &stats, None), Some(2));
        // A single typed relationship between bare endpoints.
        let expand = compact(
            vec![node(None, &[]), node(None, &[])],
            vec![rel(&["KNOWS"], 1, None)],
        );
        assert_eq!(match_estimate(&[expand], &stats, None), Some(3));
        // A labeled endpoint narrows past the type count: no signal.
        let narrowed = compact(
            vec![node(None, &["Person"]), node(None, &[])],
            vec![rel(&["KNOWS"], 1, None)],
        );
        assert_eq!(match_estimate(&[narrowed], &stats, None), None);
        // A quantified path's row count is not the rel count: None.
        let quantified = compact(
            vec![node(None, &[]), node(None, &[])],
            vec![rel(&["KNOWS"], 1, Some(2))],
        );
        assert_eq!(match_estimate(&[quantified], &stats, None), None);
        // Two patterns: a cartesian — None.
        let pair = vec![
            compact(vec![node(Some("a"), &["Person"])], Vec::new()),
            compact(vec![node(Some("b"), &["Person"])], Vec::new()),
        ];
        assert_eq!(match_estimate(&pair, &stats, None), None);
        // A label the stats do not count: None, never zero.
        let unknown = compact(vec![node(None, &["City"])], Vec::new());
        assert_eq!(match_estimate(&[unknown], &stats, None), None);
    }

    /// A multi-element pattern keeps its pattern text after the seek
    /// line (ADR-0486) — the hops the fused operator still walks — and
    /// the estimate is the seek's own count, where the fused `Expand`
    /// offered none.
    #[test]
    fn a_multi_element_pattern_keeps_its_text_after_the_seek_line() {
        let stats = seeking(&[("User", 10)], &[], &[("User", "age", 5)], BTreeMap::new());
        let patterns = [compact(
            vec![node(Some("n"), &["User"]), node(Some("m"), &["User"])],
            vec![rel(&["LINKS"], 1, None)],
        )];
        let where_age = comparison("n", "age", relate_ir::BinaryOp::Gt, relate_ir::Expr::Int(5));
        let seek = index_seek(&patterns, Some(&where_age), &stats)
            .expect("the start element's window serves");
        assert_eq!(
            match_subject(&patterns, false, Some(&seek)),
            (
                "NodeIndexSeekByRange".into(),
                "RANGE INDEX n:User(age) WHERE age > 5, (n:User)-[:LINKS]->(m:User)".to_string()
            )
        );
        assert_eq!(match_estimate(&patterns, &stats, Some(seek.rows)), Some(5));
        // An OPTIONAL MATCH wraps the seek's name the fused form's way.
        assert_eq!(
            match_subject(&patterns, true, Some(&seek)).0,
            "Optional(NodeIndexSeekByRange)"
        );
    }
}

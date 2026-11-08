use super::expr::*;
use super::*;
use relate_names::SmallName;

pub(super) fn lower_paths(paths: &[cl::PatternPath]) -> Result<Vec<mdb::PathPattern>> {
    paths.iter().map(lower_path).collect()
}

/// Mark the nodes named by a MATCH clause's `USING SCAN <var>:<Label>`
/// hints. Both views of a lowered path carry their own `NodePattern` clones
/// (`elements` and the compact `nodes` projection), so both are marked; a
/// quantified path part is walked recursively so an inner variable is not
/// missed. An unknown variable (no such node in the pattern) is a no-op,
/// like the other CIP-79 hints.
pub(super) fn apply_scan_hints(
    patterns: &mut [mdb::PathPattern],
    scan_hints: &[relate_parser::ScanHint],
) {
    if scan_hints.is_empty() {
        return;
    }
    fn mark_node(node: &mut mdb::NodePattern, scan_hints: &[relate_parser::ScanHint]) {
        if node
            .variable
            .as_ref()
            .is_some_and(|v| scan_hints.iter().any(|hint| hint.variable == *v))
        {
            node.force_scan = true;
        }
    }
    fn mark_element(element: &mut mdb::PatternElement, scan_hints: &[relate_parser::ScanHint]) {
        match element {
            mdb::PatternElement::Node(node) => mark_node(node, scan_hints),
            mdb::PatternElement::Quantified(qpp) => {
                for inner in &mut qpp.elements {
                    mark_element(inner, scan_hints);
                }
            }
            mdb::PatternElement::Relationship(_) => {}
        }
    }
    for path in patterns {
        for element in &mut path.elements {
            mark_element(element, scan_hints);
        }
        for node in &mut path.nodes {
            mark_node(node, scan_hints);
        }
    }
}

pub(super) fn lower_path(p: &cl::PatternPath) -> Result<mdb::PathPattern> {
    let raw = lower_pattern_elements(&p.elements)?;
    // The compact `nodes`/`rels` split is taken from the RAW alternating
    // element sequence, before var-length relationships are quantified
    // (below): the eval-pattern engine (pattern predicates, pattern
    // comprehensions, `shortestPath()` expressions) and the classic
    // analyses walk that view, and its rel still carries the `*m..n`
    // bounds they read. A non-compact path (a quantified path part or a
    // broken alternation) lowers to empty projections: the planner
    // matches it through `elements` instead.
    let (nodes, rels) = compact_projections(&raw).unwrap_or_default();
    let elements = quantify_var_length(raw);
    Ok(mdb::PathPattern {
        variable: p.variable.clone(),
        subpath_variable: p.subpath_variable.clone(),
        elements,
        nodes,
        rels,
        where_clause: lower_boxed(p.where_expr.as_ref(), lower_expr)?,
        mode: p.mode.map(|mode| match mode {
            cl::PathMode::Walk => mdb::PathMode::Walk,
            cl::PathMode::Trail => mdb::PathMode::Trail,
            cl::PathMode::Acyclic => mdb::PathMode::Acyclic,
            cl::PathMode::Simple => mdb::PathMode::Simple,
        }),
        selector: p.selector.as_ref().map(lower_path_selector),
        is_shortest_path_function: p.is_shortest_path_function,
    })
}

/// The `nodes`/`rels` split of a compact path — a plain node/relationship
/// alternation (nodes at even positions, relationships at odd ones) with
/// no quantified path part. `None` for a non-compact path.
fn compact_projections(
    elements: &[mdb::PatternElement],
) -> Option<(Vec<mdb::NodePattern>, Vec<mdb::RelPattern>)> {
    let mut nodes = Vec::new();
    let mut rels = Vec::new();
    for (i, el) in elements.iter().enumerate() {
        match el {
            mdb::PatternElement::Node(n) if i % 2 == 0 => nodes.push(n.clone()),
            mdb::PatternElement::Relationship(r) if i % 2 == 1 => rels.push(r.clone()),
            _ => return None,
        }
    }
    Some((nodes, rels))
}

/// Quantify every top-level var-length relationship element: `-[r:T*m..n]-`
/// lowers to the same `Quantified` element the parenthesized group
/// `(()-[r:T]-()){m,n}` produces — implicit anonymous boundary nodes inside
/// the group, the rel clamped to a plain one-hop, the `*m..n` bounds moved
/// onto the quantifier. One engine path downstream: every var-length
/// spelling (legacy `*m..n`, postfix `->{m,n}`, parenthesized group, any
/// path-mode prefix) walks the GPM quantified traversal with identical
/// semantics and plan shape.
///
/// Position-preserving: the rel's neighboring node elements stay in place —
/// the group's target — so a chain `(a)-[:R*1..2]->(b:B)-[:S*1..2]->(c)`
/// quantifies to `(a)((_)-[:R]->(_)){1,2}(b:B)((_)-[:S]->(_)){1,2}(c)`: the
/// interior node is the first group's target and the second group's start,
/// matched once, exactly the legacy semantics. A rel's inline `WHERE`
/// (postfix-quantifier spelling) stays on the inner rel, where the
/// traversal applies it per traversed edge, rather than becoming the
/// group's per-iteration `where_clause`.
///
/// Top level only: a var-length rel nested inside a parenthesized group
/// keeps its `Relationship` element (the group's own traversal expands it).
fn quantify_var_length(elements: Vec<mdb::PatternElement>) -> Vec<mdb::PatternElement> {
    elements
        .into_iter()
        .map(|element| match element {
            mdb::PatternElement::Relationship(rel) if rel.var_length => {
                let min = rel.min_hops;
                let max = rel.max_hops;
                let mut inner = rel;
                inner.var_length = false;
                inner.min_hops = 1;
                inner.max_hops = Some(1);
                mdb::PatternElement::Quantified(mdb::QuantifiedPathPattern {
                    subpath_variable: None,
                    elements: vec![
                        mdb::PatternElement::Node(mdb::NodePattern::anonymous()),
                        mdb::PatternElement::Relationship(inner),
                        mdb::PatternElement::Node(mdb::NodePattern::anonymous()),
                    ],
                    where_clause: None,
                    min,
                    max,
                })
            }
            other => other,
        })
        .collect()
}

pub(super) fn lower_pattern_elements(
    items: &[cl::PatternElement],
) -> Result<Vec<mdb::PatternElement>> {
    items
        .iter()
        .map(|item| {
            Ok(match item {
                cl::PatternElement::Node(node) => mdb::PatternElement::Node(lower_node(node)?),
                cl::PatternElement::Relationship(rel) => {
                    mdb::PatternElement::Relationship(lower_rel(rel)?)
                }
                cl::PatternElement::Quantified(qpp) => {
                    mdb::PatternElement::Quantified(mdb::QuantifiedPathPattern {
                        subpath_variable: qpp.subpath_variable.clone(),
                        elements: lower_pattern_elements(&qpp.elements)?,
                        where_clause: qpp.where_expr.as_ref().map(lower_expr).transpose()?,
                        min: qpp.quantifier.min,
                        max: qpp.quantifier.max,
                    })
                }
            })
        })
        .collect()
}

pub(super) fn lower_path_count(count: &cl::PathCount) -> mdb::PathCount {
    match count {
        cl::PathCount::Literal(value) => mdb::PathCount::Literal(*value),
        cl::PathCount::Parameter(name) => mdb::PathCount::Parameter(name.clone()),
    }
}

pub(super) fn lower_path_selector(selector: &cl::PathSelector) -> mdb::PathSelector {
    match selector {
        cl::PathSelector::All => mdb::PathSelector::All,
        cl::PathSelector::Any(count) => {
            mdb::PathSelector::Any(count.as_ref().map(lower_path_count))
        }
        cl::PathSelector::AnyShortest => mdb::PathSelector::AnyShortest,
        cl::PathSelector::AllShortest => mdb::PathSelector::AllShortest,
        cl::PathSelector::Shortest { count, group } => mdb::PathSelector::Shortest {
            count: count.as_ref().map(lower_path_count),
            group: *group,
        },
    }
}

/// The written-property fact of a node or relationship element, lowered one
/// way for both: a `$param` is the whole map, a written `{...}` (even an
/// empty `{}`) is a map, and nothing written is `None` (unconstrained).
fn lower_element_properties(
    parameter: Option<&cl::Expr>,
    has_properties: bool,
    properties: &[(SmallName, cl::Expr)],
) -> Result<Option<mdb::Expr>> {
    if let Some(parameter) = parameter {
        return Ok(Some(lower_expr(parameter)?));
    }
    if !has_properties {
        return Ok(None);
    }
    let pairs = properties
        .iter()
        .map(|(k, v)| {
            // Map-bridge feed: IR map keys are value-domain keys
            // (runtime `Value::Map`), rendered once here.
            Ok((k.to_string(), lower_expr(v)?))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(mdb::Expr::Map(pairs)))
}

pub(super) fn lower_node(n: &cl::NodePattern) -> Result<mdb::NodePattern> {
    let labels = match &n.label_expr {
        None => Vec::new(),
        Some(le) => le.as_label_list().unwrap_or_default(),
    };
    let properties =
        lower_element_properties(n.properties_param.as_ref(), n.has_properties, &n.properties)?;
    Ok(mdb::NodePattern {
        variable: n.variable.clone(),
        labels,
        label_expr: n.label_expr.as_ref().map(lower_label_expr).transpose()?,
        properties,
        where_clause: n.where_expr.as_ref().map(lower_expr).transpose()?,
        force_scan: false,
    })
}

pub(super) fn lower_rel(r: &cl::RelPattern) -> Result<mdb::RelPattern> {
    let types = r
        .type_expr
        .as_ref()
        .and_then(cl::LabelExpr::as_ddl_list)
        .unwrap_or_default();
    let properties =
        lower_element_properties(r.properties_param.as_ref(), r.has_properties, &r.properties)?;
    let var_length = r.min_hops.is_some() || r.max_hops.is_some();
    let max_hops = if var_length { r.max_hops } else { Some(1) };
    Ok(mdb::RelPattern {
        variable: r.variable.clone(),
        types,
        type_expr: r.type_expr.as_ref().map(lower_label_expr).transpose()?,
        direction: match r.direction {
            cl::Direction::Right => mdb::Direction::Outgoing,
            cl::Direction::Left => mdb::Direction::Incoming,
            cl::Direction::Both => mdb::Direction::Both,
        },
        properties,
        var_length,
        min_hops: r.min_hops.unwrap_or(1),
        max_hops,
        where_clause: r.where_expr.as_ref().map(lower_expr).transpose()?,
        quantified: r.quantified,
    })
}

/// Lowering evidence for the 42 PathDB timeout classes: every timeout
/// query is parsed through the real front end and its MATCH patterns'
/// lowered `PatternElement` shape is asserted exactly.
#[cfg(test)]
mod pathdb_tests;

#[cfg(test)]
mod element_property_tests {
    use super::*;

    #[test]
    fn written_empty_map_lowers_the_same_for_nodes_and_relationships() {
        let node = lower_element_properties(None, true, &[]).unwrap();
        assert!(matches!(&node, Some(mdb::Expr::Map(m)) if m.is_empty()));
        assert!(lower_element_properties(None, false, &[])
            .unwrap()
            .is_none());
    }
}

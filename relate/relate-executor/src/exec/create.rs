//! `CREATE`/`MERGE` pattern materialization and property sanitization.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, ErrorKind, NodeId, Path, RelId, Result, Value};
use relate_ir::{
    Direction, DynamicLabelQuantifier, Expr, LabelExpr, NodePattern, PathPattern, PatternElement,
    RelPattern,
};
use relate_names::{NamedToken, SmallName, Token};

use crate::eval::{dynamic_label_names, eval};
use crate::overlay::ExecGraph;
use crate::pattern_walk::PatternMapRole;
use crate::row::Row;

use super::pushdown::anon_name;
use crate::pattern_walk::pattern_props;

pub(crate) fn create_patterns(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    patterns: &[PathPattern],
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    for p in patterns {
        // A standalone node pattern (`CREATE (n)`, no relationships) is a
        // declaration, never a reference: a name it reuses from an enclosing
        // scope is *shadowed* — the new node takes the name — which is what
        // the `FOREACH` body's fresh scope licenses (Neo4j `ForeachAcceptance`
        // "Shadow outer variable in CREATE/MERGE"). A node that appears in a
        // relationship pattern is instead a reference to the existing binding
        // (the same suite's `(n)-[:R]->(n)` rows), so only the standalone form
        // is forced fresh.
        let fresh = p.rels.is_empty();
        let mut node_ids: Vec<NodeId> = Vec::new();
        for np in &p.nodes {
            let id = create_or_bind_node(graph, row, np, fresh, params)?;
            node_ids.push(id);
        }
        let mut rel_ids = Vec::new();
        for (i, rp) in p.rels.iter().enumerate() {
            let start = node_ids[i];
            let end = node_ids[i + 1];
            rel_ids.push(create_rel(graph, row, rp, start, end, params)?);
        }
        if let Some(name) = &p.variable {
            let nodes: Vec<_> = node_ids
                .iter()
                .map(|id| graph.node(*id).expect("node just created by this CREATE"))
                .collect();
            let rels: Vec<_> = rel_ids
                .iter()
                .map(|id| graph.rel(*id).expect("rel just created by this CREATE"))
                .collect();
            row.insert(name.to_string(), Value::from_path(Path { nodes, rels }));
        }
    }
    Ok(())
}

pub(crate) fn resolve_create_patterns(
    graph: &ExecGraph<'_>,
    row: &Row,
    patterns: &[PathPattern],
    params: &BTreeMap<String, Value>,
) -> Result<Vec<PathPattern>> {
    let mut patterns = patterns.to_vec();
    for path in &mut patterns {
        for node in &mut path.nodes {
            if let Some(properties) = &node.properties {
                node.properties = Some(Expr::RuntimeValue(eval(graph, row, properties, params)?));
            }
            if let Some((labels, label_expr)) =
                resolve_element_names(graph, row, params, node.label_expr.as_ref())?
            {
                node.labels = labels;
                node.label_expr = Some(label_expr);
            }
        }
        for rel in &mut path.rels {
            if let Some(properties) = &rel.properties {
                rel.properties = Some(Expr::RuntimeValue(eval(graph, row, properties, params)?));
            }
            if let Some((types, type_expr)) =
                resolve_element_names(graph, row, params, rel.type_expr.as_ref())?
            {
                rel.types = types;
                rel.type_expr = Some(type_expr);
            }
        }
    }
    Ok(patterns)
}

/// The CREATE element's one name-vocabulary resolve-and-rebuild (F-1-9):
/// resolve a node's label (or a rel's type) expression once into the
/// interner-facing names, returning the classified name list plus the
/// rebuilt static conjunction — the block the node and rel loops of
/// [`resolve_create_patterns`] each spelled inline. `None` leaves an
/// element without an expression untouched.
fn resolve_element_names(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    expr: Option<&LabelExpr>,
) -> Result<Option<(Vec<NamedToken>, LabelExpr)>> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let names = resolve_creation_labels(graph, row, params, Some(expr))?;
    // The resolved (String, interner-facing) label list enters the IR
    // pattern's name fields here, classified once.
    let names: Vec<_> = names
        .iter()
        .map(|name| SmallName::from_name(name.as_str()))
        .collect();
    let rebuilt = LabelExpr::And(names.iter().cloned().map(LabelExpr::Label).collect());
    let names = names
        .into_iter()
        .map(|name| (name, Token::Unresolved))
        .collect();
    Ok(Some((names, rebuilt)))
}

pub(crate) fn patterns_have_graph_sensitive_expr(patterns: &[PathPattern]) -> bool {
    patterns.iter().any(|path| {
        path.nodes
            .iter()
            .any(|node| node.properties.as_ref().is_some_and(expr_graph_sensitive))
            || path
                .rels
                .iter()
                .any(|rel| rel.properties.as_ref().is_some_and(expr_graph_sensitive))
    })
}

pub(crate) fn expr_graph_sensitive(expr: &Expr) -> bool {
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        if matches!(
            expr,
            Expr::ExistsSubquery { .. }
                | Expr::CountSubquery { .. }
                | Expr::CollectSubquery { .. }
                | Expr::PatternComp { .. }
                | Expr::PatternPred { .. }
                | Expr::ShortestPath { .. }
        ) {
            return true;
        }
        stack.extend(expr.children());
    }
    false
}

pub(crate) fn resolve_creation_labels(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    expr: Option<&LabelExpr>,
) -> Result<Vec<String>> {
    fn append(
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        expr: &LabelExpr,
        out: &mut Vec<String>,
    ) -> Result<()> {
        match expr {
            LabelExpr::Label(name) => out.push(name.to_string()),
            LabelExpr::And(items) => {
                for item in items {
                    append(graph, row, params, item, out)?;
                }
            }
            LabelExpr::Dynamic { expr, quantifier } => {
                if *quantifier == DynamicLabelQuantifier::Any {
                    return Err(Error::semantic(
                        "InvalidLabelExpression: $any() is not allowed in CREATE/MERGE",
                    ));
                }
                out.extend(dynamic_label_names(graph, row, params, expr)?);
            }
            _ => {
                return Err(Error::semantic(
                    "InvalidLabelExpression: CREATE/MERGE only supports conjunctions",
                ))
            }
        }
        Ok(())
    }
    let mut labels = Vec::new();
    if let Some(expr) = expr {
        append(graph, row, params, expr, &mut labels)?;
    }
    let mut seen = HashSet::new();
    labels.retain(|label| seen.insert(label.clone()));
    Ok(labels)
}

/// The pattern MERGE matches (and null-checks) against: a property-map
/// entry that is a pure self-reference to the element's own variable
/// (`MERGE (a {prop: a.prop})`, the deprecated form `MergeAcceptance` [6]
/// pins) is a tautology for every candidate — the candidate's own value on
/// both sides — so it is dropped from the match constraint. The original
/// pattern still drives the create path, where the same self-reference has
/// no entity to read and fails closed with `UndefinedVariable`.
///
/// Both pattern views are reduced in lockstep: `nodes`/`rels` and the
/// lossless `elements` sequence (MERGE rejects quantified elements, so the
/// elements are the same alternating node/relationship shapes).
pub(crate) fn merge_match_pattern(pattern: &PathPattern) -> PathPattern {
    let mut reduced = pattern.clone();
    for np in &mut reduced.nodes {
        np.properties = without_self_tautologies(&np.properties, np.variable.as_ref());
    }
    for rp in &mut reduced.rels {
        rp.properties = without_self_tautologies(&rp.properties, rp.variable.as_ref());
    }
    for element in &mut reduced.elements {
        match element {
            PatternElement::Node(np) => {
                np.properties = without_self_tautologies(&np.properties, np.variable.as_ref());
            }
            PatternElement::Relationship(rp) => {
                rp.properties = without_self_tautologies(&rp.properties, rp.variable.as_ref());
            }
            PatternElement::Quantified(_) => {}
        }
    }
    reduced
}

/// One element's property map with its self-referencing tautologies
/// (`v.key` where `key` is the map key and `v` is the element's own
/// variable) removed. Any other self-reference is kept, so it still fails
/// with `UndefinedVariable` rather than silently dropping a real
/// constraint.
fn without_self_tautologies(
    properties: &Option<Expr>,
    variable: Option<&SmallName>,
) -> Option<Expr> {
    let (Some(Expr::Map(pairs)), Some(variable)) = (properties, variable) else {
        return properties.clone();
    };
    let kept: Vec<(String, Expr)> = pairs
        .iter()
        .filter(|(key, value)| {
            !matches!(
                value,
                Expr::Property { expr, key: read, .. }
                    if read == key.as_str()
                        && matches!(expr.as_ref(), Expr::Variable(v) if v == variable)
            )
        })
        .cloned()
        .collect();
    if kept.len() == pairs.len() {
        return properties.clone();
    }
    (!kept.is_empty()).then_some(Expr::Map(kept))
}

pub(crate) fn reject_null_merge_props(
    graph: &ExecGraph<'_>,
    row: &Row,
    pattern: &PathPattern,
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    let maps = pattern
        .nodes
        .iter()
        .filter_map(|n| n.properties.as_ref())
        .chain(pattern.rels.iter().filter_map(|r| r.properties.as_ref()));
    for expr in maps {
        match eval(graph, row, expr, params)? {
            Value::Map(m) if m.values().any(Value::is_null) => {
                return Err(Error::execution(
                    "MergeReadOwnWrites: cannot MERGE with null properties",
                ));
            }
            // NaN never equals itself, so a MERGE onto it could never match
            // (live 2026.10: SemanticError "Cannot merge … because of NaN
            // property value for '<key>'").
            Value::Map(m) if m.values().any(is_nan_float) => {
                return Err(Error::execution(
                    "MergeReadOwnWrites: cannot MERGE with NaN properties",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `value` is the NaN float — the one predicate both MERGE property
/// prechecks read (a NaN property value can never match itself).
fn is_nan_float(value: &Value) -> bool {
    matches!(value, Value::Float(f) if f.is_nan())
}

pub(crate) fn create_or_bind_node(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    np: &NodePattern,
    fresh: bool,
    params: &BTreeMap<String, Value>,
) -> Result<NodeId> {
    let labels = resolve_creation_labels(graph, row, params, np.label_expr.as_ref())?;
    let props = pattern_props(
        graph,
        row,
        np.properties.as_ref(),
        PatternMapRole::Create,
        params,
    )?;
    if let Some((variable, value)) = reusable_binding(np, row, fresh) {
        // A relationship pattern's node position reuses the row's binding, so
        // it must *be* a node: a null left by an `OPTIONAL MATCH` is Neo4j
        // 2026.10.1's 22G03 ArgumentError ("node `a` is missing"), while any
        // other runtime non-node is its 22G03 TypeError. Neither spelling
        // may silently create a fresh node. Live-verified on 2026.10.1 under
        // Cypher 25 for both the CREATE and the MERGE spelling.
        let Value::Node(node) = value else {
            let detail = relate_core::expected_value_type_detail("NODE", value);
            return Err(if matches!(value, Value::Null) {
                Error::argument_value_kind(ErrorKind::InvalidArgumentValue, detail)
            } else {
                Error::ty(detail)
            });
        };
        let id = node.id;
        graph.add_labels(id, &labels);
        graph.merge_properties_node(id, &props);
        row.insert(variable.to_string(), graph.node_value(id));
        return Ok(id);
    }
    let id = graph.create_node(labels, props)?;
    let name = np
        .variable
        .clone()
        .unwrap_or_else(|| SmallName::from_name(&anon_name()));
    row.insert(name.to_string(), graph.node_value(id));
    Ok(id)
}

/// The row binding a non-fresh CREATE/MERGE node position reuses, when it has
/// one: `(variable, value)`. The three nested guards are one lookup.
fn reusable_binding<'a>(
    np: &'a NodePattern,
    row: &'a Row,
    fresh: bool,
) -> Option<(&'a SmallName, &'a Value)> {
    if fresh {
        return None;
    }
    let variable = np.variable.as_ref()?;
    Some((variable, row.get_name(variable)?))
}

pub(crate) fn create_rel(
    graph: &ExecGraph<'_>,
    row: &mut Row,
    rp: &RelPattern,
    start: NodeId,
    end: NodeId,
    params: &BTreeMap<String, Value>,
) -> Result<RelId> {
    let (s, e) = match rp.direction {
        Direction::Outgoing | Direction::Both => (start, end),
        Direction::Incoming => (end, start),
    };
    let types = resolve_creation_labels(graph, row, params, rp.type_expr.as_ref())?;
    if types.len() != 1 {
        return Err(Error::syntax("CREATE relationship requires a type"));
    }
    let rel_type = types[0].clone();
    let props = pattern_props(
        graph,
        row,
        rp.properties.as_ref(),
        PatternMapRole::Create,
        params,
    )?;
    let id = graph.create_rel(s, e, rel_type, props)?;
    if let Some(v) = &rp.variable {
        row.insert(v.to_string(), graph.rel_value(id));
    }
    Ok(id)
}

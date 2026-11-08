//! `USING INDEX` hint fulfillability: 42N62 for an undefined hint variable,
//! 42N76 for a hint the MATCH cannot serve.
//!
//! A hint `USING INDEX <var>:<Label>(<prop>)` is fulfillable exactly when
//! the MATCH's graph pattern binds `<var>` as a *node* carrying `<Label>`
//! (in the pattern or a label predicate) and the WHERE clause contains a
//! compatible predicate on `<var>.<prop>` — an equality/range comparison,
//! an `IN`, or a string predicate. `id(v)`/`elementId(v)` as the compared
//! value is not a constant an index can serve, and `IS NULL`/`<>` are not
//! index predicates either. Every rule here is live-verified against
//! neo4j:2026.09-enterprise; Relate's planner is not cost-based, so the
//! hint steers nothing — only the compile-time rejection is reproduced.
//!
//! Label and property identity is exact (labels are case-sensitive
//! everywhere else in the engine; only function names are matched
//! case-insensitively).

use crate::ident_hash::IdentHashMap;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, IndexHint, LabelExpr, MatchClause, PatternElement};

/// The hint battery, one pass per `MATCH` clause.
pub fn validate_index_hints(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        if let Clause::Match(mc) = clause {
            reject_unfulfillable_index_hints(mc)?;
        }
    }
    Ok(())
}

fn reject_unfulfillable_index_hints(mc: &MatchClause) -> Result<(), EngineError> {
    if mc.using_index.is_empty() {
        return Ok(());
    }
    let pattern = pattern_bindings(&mc.pattern);
    for hint in &mc.using_index {
        let Some(binding) = pattern.get(&hint.variable) else {
            return Err(hint.variable.with_str(EngineError::undefined_variable));
        };
        let PatternBinding::Node { labels } = binding else {
            // A relationship binding can never serve a node index.
            return Err(unfulfillable_hint(hint));
        };
        let labelled = labels.contains(&hint.label)
            || where_has_label(mc.where_expr.as_ref(), &hint.variable, &hint.label);
        if !labelled {
            return Err(unfulfillable_hint(hint));
        }
        if !where_has_index_predicate(mc.where_expr.as_ref(), hint) {
            return Err(unfulfillable_hint(hint));
        }
    }
    Ok(())
}

fn unfulfillable_hint(hint: &IndexHint) -> EngineError {
    EngineError::semantic(
        ErrorKind::InvalidPatternCharacter,
        format!(
            "unfulfillable hints: the hint(s) USING INDEX {}:{}({}) cannot be fulfilled",
            hint.variable,
            hint.label,
            hint.properties_text()
        ),
    )
}

/// How one pattern variable is bound.
enum PatternBinding {
    /// A node binding, with the labels its pattern asserts.
    Node { labels: Vec<SmallName> },
    /// A relationship binding.
    Relationship,
}

/// The graph pattern's variable bindings, keyed by name. A name bound as a
/// node anywhere in the graph pattern is a node binding; only
/// relationship-only names are relationships.
fn pattern_bindings(
    patterns: &[relate_parser::PatternPath],
) -> IdentHashMap<SmallName, PatternBinding> {
    let mut bindings = IdentHashMap::default();
    for path in patterns {
        for element in &path.elements {
            bind_pattern_element(element, &mut bindings);
        }
    }
    bindings
}

/// Bind one pattern element's variables, recursing into a quantified group
/// (the one element walk both the flat and the nested view use).
fn bind_pattern_element(
    element: &PatternElement,
    bindings: &mut IdentHashMap<SmallName, PatternBinding>,
) {
    match element {
        PatternElement::Node(node) => {
            let Some(name) = &node.variable else { return };
            let binding = bindings
                .entry(name.clone())
                .or_insert_with(|| PatternBinding::Node { labels: Vec::new() });
            // A node binding wins over a relationship binding of the same
            // name elsewhere in the pattern (the type-conflict rule reports
            // that case; the hint only needs the label set).
            if matches!(binding, PatternBinding::Relationship) {
                *binding = PatternBinding::Node { labels: Vec::new() };
            }
            if let PatternBinding::Node { labels } = binding {
                if let Some(label_expr) = &node.label_expr {
                    collect_asserted_labels(label_expr, labels);
                }
            }
        }
        PatternElement::Relationship(rel) => {
            if let Some(name) = &rel.variable {
                bindings
                    .entry(name.clone())
                    .or_insert(PatternBinding::Relationship);
            }
        }
        PatternElement::Quantified(qpp) => {
            for inner in &qpp.elements {
                bind_pattern_element(inner, bindings);
            }
        }
    }
}

/// The label names a label expression *asserts*: a conjunction asserts each
/// conjunct, a disjunction asserts none of them (any one may hold), and a
/// negated or dynamic label asserts nothing usable for a hint.
fn collect_asserted_labels(expr: &LabelExpr, out: &mut Vec<SmallName>) {
    match expr {
        LabelExpr::Label(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        LabelExpr::And(parts) => {
            for part in parts {
                collect_asserted_labels(part, out);
            }
        }
        LabelExpr::Or(_) | LabelExpr::Not(_) | LabelExpr::Wildcard | LabelExpr::Dynamic(_) => {}
    }
}

/// Whether the WHERE clause asserts `<var>:<label>` (Neo4j accepts a hint
/// whose label is supplied by a label predicate rather than the pattern).
fn where_has_label(expr: Option<&Expr>, variable: &SmallName, label: &SmallName) -> bool {
    let Some(expr) = expr else { return false };
    crate::ast::tree::expr_contains(
        expr,
        |_, _| true,
        |node| match node {
            Expr::HasLabel(has_label) => {
                if has_label.negated
                    || !matches!(&has_label.expr, Expr::Variable(name) if name == variable)
                {
                    return false;
                }
                let mut labels = Vec::new();
                collect_asserted_labels(&has_label.label_expr, &mut labels);
                labels.iter().any(|asserted| asserted == label)
            }
            _ => false,
        },
    )
}

/// Whether the WHERE clause contains a compatible index predicate on the
/// hint's `<variable>.<property>`.
fn where_has_index_predicate(expr: Option<&Expr>, hint: &IndexHint) -> bool {
    let Some(expr) = expr else { return false };
    let target = |e: &Expr| match e {
        Expr::Property(prop) => match &prop.expr {
            Expr::Variable(name) if name == &hint.variable => {
                hint.properties.contains(&prop.property)
            }
            _ => false,
        },
        _ => false,
    };
    let usable_value = |e: &Expr| match e {
        Expr::FunctionCall(fc) => {
            let name = fc.name.with_str(str::to_ascii_lowercase);
            name != "id" && name != "elementid"
        }
        _ => true,
    };
    // Subquery bodies stay opaque (`children()` does not enter them): an
    // index predicate lives in this clause's own WHERE.
    crate::ast::tree::expr_contains(
        expr,
        |_, _| true,
        |node| match node {
            Expr::Comparison(cmp) => {
                let op = cmp.op;
                matches!(op, "=" | "<" | ">" | "<=" | ">=")
                    && ((target(&cmp.left) && usable_value(&cmp.right))
                        || (target(&cmp.right) && usable_value(&cmp.left)))
            }
            Expr::In(in_expr) => target(&in_expr.expr),
            Expr::StringMatch(m) => target(&m.left),
            _ => false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(query: &str) -> Result<(), String> {
        crate::validate::parse_and_validate(query, validate_index_hints)
    }

    /// 42N62/42N76: an index hint must name a pattern variable and be
    /// fulfillable by the pattern's label and the WHERE predicate (all live
    /// Neo4j 2026.09).
    #[test]
    fn unfulfillable_index_hints_are_rejected() {
        for query in [
            // Undefined hint variable.
            "MATCH (n:Person)-->() USING INDEX m:Person(name) WHERE n.name = 'kabam' RETURN n",
            // The pattern's node carries no hinted label.
            "MATCH (n) USING INDEX n:Person(name) WHERE n.name = 'Johan' RETURN n",
            "MATCH (n) USING INDEX n:N(prop) WHERE n:B AND n.prop = 'hello' RETURN n",
            // The label matches only case-insensitively.
            "MATCH (n:Person) USING INDEX n:person(name) WHERE n.name = 'Johan' RETURN n",
            // The predicate uses a different property.
            "MATCH (n:Person) USING INDEX n:Person(name) WHERE n.lastname = 'Teleman' RETURN n",
            // The hinted variable is a relationship.
            "MATCH ()-[n]-() USING INDEX n:Person(name) WHERE n.name = 'Johan' RETURN n",
            // `id()`/`elementId()` is not an index-usable value.
            "MATCH (n:Person) USING INDEX n:Person(id) WHERE n.id = id(n) RETURN n",
            "MATCH (n:Person) USING INDEX n:Person(id) WHERE n.id = elementId(n) RETURN n",
            // No compatible predicate at all.
            "MATCH (n:Person) USING INDEX n:Person(age) RETURN n",
            // A disjunctive label predicate asserts no single label.
            "MATCH (n) USING INDEX n:A(p) WHERE n:A|B AND n.p = 1 RETURN n",
        ] {
            let err = validate(query).unwrap_err();
            assert!(
                err.contains("cannot be fulfilled") || err.contains("not defined"),
                "{query}: {err}"
            );
        }
    }

    /// The fulfillable shapes stay accepted: label on the pattern or
    /// asserted by a conjunction in WHERE, and an equality/range/IN/string
    /// predicate on the hinted property.
    #[test]
    fn fulfillable_index_hints_are_accepted() {
        for query in [
            "MATCH (n:Person) USING INDEX n:Person(age) WHERE n.age = 30 RETURN n",
            "MATCH (n) USING INDEX n:Person(name) WHERE n:Person AND n.name = 'Johan' RETURN n",
            "MATCH (n:Person) USING INDEX n:Person(name) WHERE n.name STARTS WITH 'J' RETURN n",
            "MATCH (n:Person) USING INDEX n:Person(name) WHERE n.name IN ['a', 'b'] RETURN n",
            "MATCH (n:Person) USING INDEX n:Person(age) WHERE n.age > 30 RETURN n",
            // A conjunctive label expression asserts both labels.
            "MATCH (n:A&B) USING INDEX n:A(p) WHERE n.p = 1 RETURN n",
        ] {
            assert!(validate(query).is_ok(), "{query}: {:?}", validate(query));
        }
    }
}

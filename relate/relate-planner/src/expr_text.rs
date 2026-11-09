//! Best-effort source spelling of a planner-IR expression, for diagnostics
//! that must name the offending expression (the 42I28 importing-WITH
//! message).
//!
//! Every form whose operands the IR retains is rendered from those
//! operands — including the carried modifiers (`IS NOT :: …`, `IS NOT
//! NORMALIZED`, `IS NOT BEFORE …`) — so the quoted text is Cypher the query
//! could have written. Two families are structural summaries instead of
//! source text, because the IR keeps their tree and not their spelling:
//! pattern forms (`(pattern)`) and subquery forms (`EXISTS { … }`); a
//! literal the IR holds as an evaluated value renders as `<value>`. Callers
//! prefer [`Projection::source_text`](relate_ir::Projection::source_text)
//! when it exists, so these summaries are the synthetic-projection fallback.

use relate_ir::{ChronoOp, Expr, LabelExpr, MapProjectionItem, QuantifierKind, TypeExpr};
use relate_names::SmallName;

/// Render `expr` the way the query spelled it, as far as the IR retains the
/// spelling. Idempotent and allocation-light; recursion follows the arms
/// below, which cover every variant either by rendering its operands or by
/// naming its structural form.
pub(crate) fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Null => "null".to_string(),
        Expr::Bool(value) => value.to_string(),
        Expr::Int(value) => value.to_string(),
        Expr::Float(value) => value.to_string(),
        Expr::String(value) => format!("'{value}'"),
        Expr::Parameter(name) => format!("${name}"),
        Expr::Variable(name) => name.to_string(),
        Expr::Star => "*".to_string(),
        Expr::CountStar => "count(*)".to_string(),
        Expr::Property { expr, key, .. } => format!("{}.{key}", expr_text(expr)),
        Expr::DynamicProperty { expr, key } => format!("{}[{}]", expr_text(expr), expr_text(key)),
        Expr::Function {
            name,
            distinct,
            args,
        } => {
            let args = args.iter().map(expr_text).collect::<Vec<_>>().join(", ");
            let distinct = if *distinct { "DISTINCT " } else { "" };
            format!("{name}({distinct}{args})")
        }
        Expr::Unary { op, expr } => {
            let inner = expr_text(expr);
            match op {
                // `IS [NOT] NULL` is a postfix operator, not a prefix one.
                relate_ir::UnaryOp::IsNull => format!("{inner} IS NULL"),
                relate_ir::UnaryOp::IsNotNull => format!("{inner} IS NOT NULL"),
                relate_ir::UnaryOp::Not => format!("NOT {inner}"),
                relate_ir::UnaryOp::Minus => format!("-{inner}"),
                relate_ir::UnaryOp::Plus => format!("+{inner}"),
            }
        }
        Expr::Binary { op, left, right } => {
            format!("{} {} {}", expr_text(left), op.as_str(), expr_text(right))
        }
        Expr::List(items) => format!(
            "[{}]",
            items.iter().map(expr_text).collect::<Vec<_>>().join(", ")
        ),
        Expr::Map(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(key, value)| format!("{key}: {}", expr_text(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Subscript { expr, index } => format!("{}[{}]", expr_text(expr), expr_text(index)),
        Expr::Slice { expr, start, end } => format!(
            "{}[{}..{}]",
            expr_text(expr),
            start.as_deref().map(expr_text).unwrap_or_default(),
            end.as_deref().map(expr_text).unwrap_or_default()
        ),
        Expr::IsType {
            expr,
            type_expr,
            negated,
        } => format!(
            "{} IS {}:: {}",
            expr_text(expr),
            not_prefix(*negated),
            type_expr_text(type_expr)
        ),
        Expr::LabelPredicate {
            expr, label_expr, ..
        } => format!("{}:{}", expr_text(expr), label_expr_text(label_expr)),
        Expr::IsNormalized {
            expr,
            form,
            negated,
        } => format!(
            "{} IS {}{}NORMALIZED",
            expr_text(expr),
            not_prefix(*negated),
            form.as_deref()
                .map(|form| format!("{form} "))
                .unwrap_or_default()
        ),
        Expr::ChronoCompare {
            left,
            op,
            right,
            negated,
        } => format!(
            "{} IS {}{} {}",
            expr_text(left),
            not_prefix(*negated),
            chrono_op_text(*op),
            expr_text(right)
        ),
        Expr::Case {
            when_then,
            else_expr,
        } => {
            let arms = when_then
                .iter()
                .map(|(when, then)| format!("WHEN {} THEN {}", expr_text(when), expr_text(then)))
                .chain(
                    else_expr
                        .as_deref()
                        .map(|otherwise| format!("ELSE {}", expr_text(otherwise))),
                )
                .collect::<Vec<_>>()
                .join(" ");
            format!("CASE {arms} END")
        }
        Expr::ListComp {
            ident,
            list,
            predicate,
            projection,
        } => format!(
            "[{ident} IN {}{} | {}]",
            expr_text(list),
            predicate
                .as_deref()
                .map(|predicate| format!(" WHERE {}", expr_text(predicate)))
                .unwrap_or_default(),
            expr_text(projection)
        ),
        Expr::MapComp {
            key_ident,
            value_ident,
            source,
            predicate,
            key_expr,
            value_expr,
        } => format!(
            "map {{{}{value_ident} IN {}{} | {}: {}}}",
            key_ident
                .as_ref()
                .map(|key_ident| format!("{key_ident}: "))
                .unwrap_or_default(),
            expr_text(source),
            predicate
                .as_deref()
                .map(|predicate| format!(" WHERE {}", expr_text(predicate)))
                .unwrap_or_default(),
            expr_text(key_expr),
            expr_text(value_expr)
        ),
        Expr::Quantifier {
            kind,
            ident,
            list,
            predicate,
        } => format!(
            "{}({ident} IN {} WHERE {})",
            quantifier_text(*kind),
            expr_text(list),
            expr_text(predicate)
        ),
        Expr::Reduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
        } => format!(
            "reduce({accumulator} = {}, {ident} IN {} | {})",
            expr_text(initial),
            expr_text(list),
            expr_text(expression)
        ),
        Expr::AllReduce {
            accumulator,
            initial,
            ident,
            list,
            expression,
            predicate,
        } => format!(
            "allReduce({accumulator} = {}, {ident} IN {} | {} WHERE {})",
            expr_text(initial),
            expr_text(list),
            expr_text(expression),
            expr_text(predicate)
        ),
        Expr::ExistsSubquery { .. } => "EXISTS { … }".to_string(),
        Expr::CountSubquery { .. } => "COUNT { … }".to_string(),
        Expr::CollectSubquery { .. } => "COLLECT { … }".to_string(),
        Expr::PatternPred { .. } => "(pattern)".to_string(),
        Expr::PatternComp { .. } => "[(pattern) | …]".to_string(),
        Expr::ShortestPath { all, .. } => {
            if *all {
                "allShortestPaths(…)".to_string()
            } else {
                "shortestPath(…)".to_string()
            }
        }
        Expr::MapProjection { expr, items } => format!(
            "{} {{{}}}",
            expr_text(expr),
            items
                .iter()
                .map(map_projection_item_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::RuntimeValue(_) => "<value>".to_string(),
        Expr::DistanceMetric(metric) => metric.name().to_string(),
    }
}

/// Render a `graph.*` call as written for CE's `Attempted to access graph
/// …` line (`graph.byName('neo4j')`).
pub(crate) fn graph_call_text(name: &SmallName, args: &[Expr]) -> String {
    let inner = args
        .iter()
        .map(graph_expr_text)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({inner})")
}

/// Best-effort source spelling of a graph expression for CE's `Attempted to
/// access graph …` line. The shapes a `USE` target can take delegate to the
/// one expression renderer; anything else is CE's `<dynamic>` stand-in for a
/// graph reference that is not spelled as a name.
pub(crate) fn graph_expr_text(expr: &Expr) -> String {
    match expr {
        Expr::String(_) | Expr::Variable(_) | Expr::Parameter(_) | Expr::Property { .. } => {
            expr_text(expr)
        }
        Expr::Function { name, args, .. } => graph_call_text(name, args),
        _ => "<dynamic>".to_string(),
    }
}

/// The `NOT ` a negated type/normalization/chronological predicate carries.
fn not_prefix(negated: bool) -> &'static str {
    if negated {
        "NOT "
    } else {
        ""
    }
}

fn chrono_op_text(op: ChronoOp) -> &'static str {
    match op {
        ChronoOp::Before => "BEFORE",
        ChronoOp::After => "AFTER",
        ChronoOp::SameInstant => "SAME INSTANT",
        ChronoOp::SameRepresentation => "SAME REPRESENTATION",
    }
}

fn quantifier_text(kind: QuantifierKind) -> &'static str {
    match kind {
        QuantifierKind::All => "all",
        QuantifierKind::Any => "any",
        QuantifierKind::None => "none",
        QuantifierKind::Single => "single",
    }
}

fn map_projection_item_text(item: &MapProjectionItem) -> String {
    match item {
        MapProjectionItem::KeyValue { key, value } => format!("{key}: {}", expr_text(value)),
        MapProjectionItem::Property { key } => format!(".{key}"),
        MapProjectionItem::Variable { name } => name.to_string(),
        MapProjectionItem::AllProperties => ".*".to_string(),
    }
}

/// The CIP-100 canonical spelling of a type expression, so a diagnostic
/// echoes a type the query could have written.
fn type_expr_text(type_expr: &TypeExpr) -> String {
    let (base, not_null) = match type_expr {
        TypeExpr::Predefined { kind, not_null } => (kind.as_str().to_string(), *not_null),
        TypeExpr::List { element, not_null } => {
            (format!("LIST<{}>", type_expr_text(element)), *not_null)
        }
        TypeExpr::PropertyValue { not_null } => ("[ANY] PROPERTY VALUE".to_string(), *not_null),
        TypeExpr::Union(members) => (
            members
                .iter()
                .map(type_expr_text)
                .collect::<Vec<_>>()
                .join(" | "),
            false,
        ),
        TypeExpr::Vector {
            coord,
            dimension,
            not_null,
        } => {
            let mut text = String::from("VECTOR");
            if let Some(coord) = coord {
                text.push('<');
                text.push_str(coord);
                text.push_str(" NOT NULL>");
            }
            if let Some(dimension) = dimension {
                text.push('(');
                text.push_str(&dimension.to_string());
                text.push(')');
            }
            (text, *not_null)
        }
    };
    if not_null {
        format!("{base} NOT NULL")
    } else {
        base
    }
}

/// The label expression without its leading `:`, as `n:<expr>` expects.
fn label_expr_text(label_expr: &LabelExpr) -> String {
    match label_expr {
        LabelExpr::Label(label) => label.to_string(),
        LabelExpr::Wildcard => "%".to_string(),
        LabelExpr::Dynamic { expr, .. } => format!("$({})", expr_text(expr)),
        LabelExpr::Not(inner) => format!("!{}", label_expr_text(inner)),
        LabelExpr::And(items) => items
            .iter()
            .map(label_expr_text)
            .collect::<Vec<_>>()
            .join("&"),
        LabelExpr::Or(items) => items
            .iter()
            .map(label_expr_text)
            .collect::<Vec<_>>()
            .join("|"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use relate_ir::BinaryOp;

    fn var(name: &str) -> Expr {
        Expr::Variable(name.into())
    }

    #[test]
    fn renders_the_common_projection_shapes() {
        let expr = Expr::Binary {
            op: BinaryOp::Add,
            left: Box::new(var("n")),
            right: Box::new(Expr::Int(1)),
        };
        assert_eq!(expr_text(&expr), "n + 1");
        let expr = Expr::Function {
            name: "collect".into(),
            distinct: false,
            args: vec![var("n")],
        };
        assert_eq!(expr_text(&expr), "collect(n)");
        let expr = Expr::Property {
            key_id: Default::default(),
            expr: Box::new(var("n")),
            key: "prop".into(),
        };
        assert_eq!(expr_text(&expr), "n.prop");
        let expr = Expr::Parameter("p".into());
        assert_eq!(expr_text(&expr), "$p");
        let expr = Expr::String("s".into());
        assert_eq!(expr_text(&expr), "'s'");
        // `IS NULL` is postfix: the rendered expression must be one the
        // query could have written.
        let expr = Expr::Unary {
            op: relate_ir::UnaryOp::IsNull,
            expr: Box::new(var("n")),
        };
        assert_eq!(expr_text(&expr), "n IS NULL");
        let expr = Expr::Unary {
            op: relate_ir::UnaryOp::IsNotNull,
            expr: Box::new(var("n")),
        };
        assert_eq!(expr_text(&expr), "n IS NOT NULL");
        let expr = Expr::Unary {
            op: relate_ir::UnaryOp::Not,
            expr: Box::new(var("n")),
        };
        assert_eq!(expr_text(&expr), "NOT n");
    }

    /// The carried modifiers are rendered, not dropped: a negated predicate
    /// keeps its `NOT` and a chronological comparison keeps its operator.
    #[test]
    fn renders_carried_modifiers_and_type_expressions() {
        let is_type = Expr::IsType {
            expr: Box::new(var("n")),
            type_expr: TypeExpr::Predefined {
                kind: relate_ir::PredefinedType::Integer,
                not_null: true,
            },
            negated: true,
        };
        assert_eq!(expr_text(&is_type), "n IS NOT :: INTEGER NOT NULL");

        let normalized = Expr::IsNormalized {
            expr: Box::new(var("n")),
            form: Some("NFC".to_string()),
            negated: true,
        };
        assert_eq!(expr_text(&normalized), "n IS NOT NFC NORMALIZED");

        let chrono = Expr::ChronoCompare {
            left: Box::new(var("a")),
            op: ChronoOp::SameRepresentation,
            right: Box::new(var("b")),
            negated: true,
        };
        assert_eq!(expr_text(&chrono), "a IS NOT SAME REPRESENTATION b");

        let label = Expr::LabelPredicate {
            expr: Box::new(var("n")),
            label_expr: LabelExpr::And(vec![
                LabelExpr::Label(SmallName::from_name("A")),
                LabelExpr::Not(Box::new(LabelExpr::Label(SmallName::from_name("B")))),
            ]),
            strict: false,
        };
        assert_eq!(expr_text(&label), "n:A&!B");

        let metric = Expr::DistanceMetric(relate_ir::VectorMetric::Cosine);
        assert_eq!(expr_text(&metric), "COSINE");
    }
}

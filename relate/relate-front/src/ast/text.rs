//! The structural expression renderer: the one front-side parser-AST →
//! text spelling for error sentences that quote what the user wrote
//! (ADR-0714's search-filter sentence and the grouping-element rejection
//! read it) — an AST-shaped helper beside `naming` and `search_spec`,
//! not a walker.

use relate_core::{escape_unsafe_output, quote_message};
use relate_model::identifier::{parameter_reference, quote_identifier_shaped};
use relate_model::value::float_text;
use relate_names::SmallName;
use relate_parser::{Expr, LiteralValue};

/// Render a parser-AST expression structurally, for error sentences that
/// quote what the user wrote. All-or-nothing: when any part of the term
/// is a shape the renderer does not model, the whole term renders as
/// `fallback` — the caller's named placeholder, never spliced into an
/// otherwise-real predicate. Covers the shapes a predicate term takes:
/// property accesses, variables, parameters (the one parameter spelling
/// that re-parses as itself), literals (a string as the one quoted
/// message term), lists, the boolean and comparison operators, string
/// matches, `IN`, `IS [NOT] NULL`, calls, subscripts and parentheses.
/// Every name and literal passes the output-safety escape, so nothing
/// the renderer quotes reaches a client surface raw.
pub(crate) fn structural_expr_text(expr: &Expr, fallback: &str) -> String {
    render_expr(expr).unwrap_or_else(|| fallback.to_string())
}

/// One name's sentence spelling: the canonical identifier quoting
/// ([`quote_identifier_shaped`] — plain-shaped bare, anything else
/// backtick-quoted, the spelling that re-parses as itself), with any
/// unsafe output character inside escaped visibly — the output-safety
/// escape every client surface shares.
fn name_text(name: &SmallName) -> String {
    escape_unsafe_output(&name.with_str(quote_identifier_shaped))
}

/// The term's structural spelling, `None` when any part of it is a shape
/// the renderer does not model.
fn render_expr(expr: &Expr) -> Option<String> {
    Some(match expr {
        Expr::Property(property) => {
            format!(
                "{}.{}",
                render_expr(&property.expr)?,
                name_text(&property.property)
            )
        }
        Expr::Variable(name) => name_text(name),
        Expr::Parameter(parameter) => escape_unsafe_output(&parameter_reference(parameter)),
        Expr::Binary(binary) => format!(
            "{} {} {}",
            render_expr(&binary.left)?,
            binary.op,
            render_expr(&binary.right)?
        ),
        Expr::Comparison(comparison) => format!(
            "{} {} {}",
            render_expr(&comparison.left)?,
            comparison.op,
            render_expr(&comparison.right)?
        ),
        Expr::StringMatch(match_expr) => format!(
            "{} {} {}",
            render_expr(&match_expr.left)?,
            match_expr.op,
            render_expr(&match_expr.right)?
        ),
        Expr::In(in_expr) => format!(
            "{} IN {}",
            render_expr(&in_expr.expr)?,
            render_expr(&in_expr.list)?
        ),
        Expr::IsNull(is_null) => format!(
            "{} IS {}NULL",
            render_expr(&is_null.expr)?,
            if is_null.negated { "NOT " } else { "" }
        ),
        Expr::Unary(unary) => format!("{} {}", unary.op, render_expr(&unary.expr)?),
        Expr::Paren(inner) => format!("({})", render_expr(inner)?),
        Expr::FunctionCall(call) => {
            let args = call
                .args
                .iter()
                .map(render_expr)
                .collect::<Option<Vec<_>>>()?;
            format!(
                "{}({}{})",
                name_text(&call.name),
                if call.distinct { "DISTINCT " } else { "" },
                args.join(", ")
            )
        }
        // The subscript's one shape discriminator is `slice`: the index
        // form is `base[index]`, the slice form `base[from..to]` with
        // either bound optional.
        Expr::Subscript(sub) => {
            let base = render_expr(&sub.expr)?;
            if sub.slice {
                let from = match &sub.from {
                    Some(from) => render_expr(from)?,
                    None => String::new(),
                };
                let to = match &sub.to {
                    Some(to) => render_expr(to)?,
                    None => String::new(),
                };
                format!("{base}[{from}..{to}]")
            } else {
                let Some(index) = &sub.index else {
                    return None;
                };
                format!("{base}[{}]", render_expr(index)?)
            }
        }
        Expr::Literal(LiteralValue::Null) => "null".to_string(),
        Expr::Literal(LiteralValue::Bool(value)) => value.to_string(),
        Expr::Literal(LiteralValue::Integer(value)) => value.to_string(),
        Expr::Literal(LiteralValue::Float(value)) => float_text(*value),
        // A string literal is the one quoted message term: the
        // output-safety escape plus the frame's own two, wrapped —
        // control characters and quotes render inert, printable text
        // passes through.
        Expr::Literal(LiteralValue::String(value)) => quote_message(value),
        Expr::List(items) => {
            let items = items.iter().map(render_expr).collect::<Option<Vec<_>>>()?;
            format!("[{}]", items.join(", "))
        }
        _ => return None,
    })
}

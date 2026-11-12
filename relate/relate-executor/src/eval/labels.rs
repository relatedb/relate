//! Dynamic label/property-key evaluation and label-expression matching.

use std::collections::{BTreeMap, HashSet};

use relate_core::{Error, Result, Value};
use relate_ir::{DynamicLabelQuantifier, Expr, LabelExpr};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::eval;

pub(crate) fn dynamic_property_key(value: Value) -> Result<String> {
    // The type and token-name rule lives in `relate-core::dynamic`, the
    // one module the front's compile-time fold shares (LABS-709).
    relate_core::dynamic_property_key(&value).map_err(Error::from)
}

pub(crate) fn dynamic_label_names(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    expr: &Expr,
) -> Result<Vec<String>> {
    let value = eval(graph, row, expr, params)?;
    // Same shared rule as `dynamic_property_key` (STRING or LIST<STRING>,
    // non-empty, NUL-free), reused rather than restated.
    relate_core::dynamic_label_names(&value).map_err(Error::from)
}

pub(crate) fn label_expr_matches(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    expr: &LabelExpr,
    labels: &[String],
    is_rel: bool,
) -> Result<bool> {
    Ok(match expr {
        LabelExpr::Label(name) => labels.iter().any(|label| name == label.as_str()),
        LabelExpr::Wildcard => is_rel || !labels.is_empty(),
        LabelExpr::Dynamic { expr, quantifier } => {
            let names = dynamic_label_names(graph, row, params, expr)?;
            match quantifier {
                DynamicLabelQuantifier::Any => names
                    .iter()
                    .any(|name| labels.iter().any(|label| label == name)),
                DynamicLabelQuantifier::Default | DynamicLabelQuantifier::All => names
                    .iter()
                    .all(|name| labels.iter().any(|label| label == name)),
            }
        }
        LabelExpr::Not(inner) => !label_expr_matches(graph, row, params, inner, labels, is_rel)?,
        LabelExpr::And(items) => {
            let mut matched = true;
            for item in items {
                if !label_expr_matches(graph, row, params, item, labels, is_rel)? {
                    matched = false;
                    break;
                }
            }
            matched
        }
        LabelExpr::Or(items) => {
            let mut matched = false;
            for item in items {
                if label_expr_matches(graph, row, params, item, labels, is_rel)? {
                    matched = true;
                    break;
                }
            }
            matched
        }
    })
}

pub(crate) fn resolve_write_labels(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    items: &[LabelExpr],
) -> Result<Vec<String>> {
    let mut labels = Vec::new();
    for item in items {
        match item {
            // The write-label list is the interner's String domain; a
            // static label renders once, at its push.
            LabelExpr::Label(name) => labels.push(name.to_string()),
            LabelExpr::Dynamic { expr, quantifier } => {
                if *quantifier == DynamicLabelQuantifier::Any {
                    return Err(Error::semantic(
                        "InvalidLabelExpression: $any() is not allowed in SET/REMOVE",
                    ));
                }
                labels.extend(dynamic_label_names(graph, row, params, expr)?);
            }
            _ => {
                return Err(Error::semantic(
                    "InvalidLabelExpression: SET/REMOVE only supports plain or dynamic labels",
                ))
            }
        }
    }
    let mut seen = HashSet::new();
    labels.retain(|label| seen.insert(label.clone()));
    Ok(labels)
}

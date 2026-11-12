//! Property special forms: `property_exists`, whose property-name argument
//! is an identifier literal rather than an evaluated expression.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;

use crate::overlay::ExecGraph;

use super::super::{eval, Row};

/// The `property_exists` form row (CIP-247): the element argument is
/// evaluated, while the property-name argument is an identifier literal
/// that must not be evaluated as a row variable. The row's exactly-two
/// contract is enforced by the dispatcher before the form runs.
pub(super) fn property_exists(
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    args: &[Expr],
) -> Result<Value> {
    let [element, name] =
        <&[Expr; 2]>::try_from(args).expect("the property_exists row's arity is exactly two");
    let element = eval(graph, row, element, params)?;
    // The property-name argument renders once (this is a dedicated
    // two-argument builtin, not a per-row hot read).
    let property = match name {
        Expr::Variable(name) => name.to_string(),
        Expr::Null => "null".to_string(),
        Expr::String(name) => name.clone(),
        _ => {
            return Err(Error::invalid_argument_type(
                "property name must be an identifier",
            ))
        }
    };
    Ok(match element {
        Value::Node(node) => Value::Bool(
            graph
                .node(node.id)
                .and_then(|node| node.properties.get(property.as_str()).cloned())
                .is_some_and(|value| !value.is_null()),
        ),
        Value::Relationship(rel) => Value::Bool(
            graph
                .rel(rel.id)
                .and_then(|rel| rel.properties.get(property.as_str()).cloned())
                .is_some_and(|value| !value.is_null()),
        ),
        Value::Null => Value::Null,
        _ => {
            return Err(Error::invalid_argument_type(
                "expected NODE or RELATIONSHIP",
            ))
        }
    })
}

//! Evaluation of a persisted default. The value's shape lives in the
//! engine contract; running it stays here, on the query evaluator.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_engine_api::DefaultValue;

use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// The default's lowered expression, captured once per statement per property.
pub fn lowered_default(default: &DefaultValue) -> Result<relate_ir::Expr> {
    relate_front::lower_expr(&default.to_expr())
}

/// Evaluate a pre-lowered default through the engine's own evaluator.
pub fn eval_lowered_default(graph: &ExecGraph<'_>, lowered: &relate_ir::Expr) -> Result<Value> {
    eval(graph, &Row::new(), lowered, &BTreeMap::new())
}

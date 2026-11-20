//! The `CALL { … } IN TRANSACTIONS` modifier battery: the per-modifier
//! scope and shape rules (batch size and concurrency arguments, the retry
//! duration, `REPORT STATUS`). Split from `subquery.rs` so each file owns
//! one concern.

use relate_core::{Error, Result};
use relate_ir::{Clause, Expr, InTransactions, UnaryOp};

use crate::binding::Env;
use crate::expr_check::{check_expr, check_positive_int_arg, expr_non_static};

/// Validate one `IN TRANSACTIONS` modifier list. The `REPORT STATUS`
/// variable's binding is the caller's: it joins the CALL body's export set
/// in one post-CALL name-space check, so no half of the name space is
/// inserted (or collision-checked) before the other.
pub(super) fn check_in_transactions_modifiers(
    tx: &InTransactions,
    env: &mut Env,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
    clause_index: usize,
    clauses: &[Clause],
) -> Result<()> {
    // One expression walk over the modifier slots: the enumeration is
    // `InTransactions::exprs`, shared with the IR walker, so no slot is
    // scope-checked here yet skipped there (or vice versa).
    for expr in tx.exprs() {
        check_expr(env, is_write_procedure, expr)?;
    }
    if let Some(n) = &tx.batch_size {
        check_positive_int_arg(n, "batch size")?;
    }
    if let relate_ir::Concurrency::Value(n) = &tx.concurrency {
        check_positive_int_arg(n, "concurrency")?;
        if expr_non_static(n) {
            return Err(Error::syntax(
                "NonStaticConcurrency: concurrency cannot reference row variables",
            ));
        }
    }
    if let relate_ir::OnError::Retry {
        duration: Some(duration),
        ..
    } = &tx.on_error
    {
        if expr_non_static(duration) {
            return Err(Error::syntax(
                "InvalidArgumentExpression: retry duration must be static",
            ));
        }
        match duration {
            Expr::Int(value) if *value >= 0 => {}
            Expr::Float(value) if *value >= 0.0 => {}
            Expr::Unary {
                op: UnaryOp::Minus, ..
            } => {
                return Err(Error::syntax(
                    "NumberOutOfRange: retry duration must be non-negative",
                ))
            }
            Expr::Null | Expr::Bool(_) | Expr::String(_) | Expr::List(_) | Expr::Map(_) => {
                return Err(Error::syntax(
                    "InvalidArgumentType: retry duration must be numeric",
                ))
            }
            _ => {}
        }
    }
    if tx.report_status.is_some() {
        // A failing batch aborts before any status row can be produced:
        // `ON ERROR FAIL` (the default when ON ERROR is absent) and
        // `RETRY … THEN FAIL` both project onto the same outcome.
        if tx.on_error.outcome() == relate_ir::RetryFallback::Fail {
            return Err(Error::syntax(
                "InvalidReportStatusUsage: REPORT STATUS requires ON ERROR CONTINUE, \
                 BREAK, or RETRY with THEN CONTINUE or THEN BREAK",
            ));
        }
        if clause_index + 1 == clauses.len() {
            return Err(Error::syntax(
                "InvalidClauseComposition: REPORT STATUS cannot terminate a query",
            ));
        }
    }
    Ok(())
}

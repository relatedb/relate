//! The ADR-0463 per-allocation expression cap's own tests: the exact
//! single-allocation refusal, the cumulative construction-work telemetry
//! (never enforcement), the configured limit and its explicit opt-out, and
//! the plain-language refusal message.

use super::*;

use crate::memory::EXPRESSION_BUDGET_BYTES;
use relate_core::{CapacityKind, Error};
use relate_planner::LogicalPlan;

#[test]
fn per_allocation_refusal_is_exact_and_stateless() {
    let g = Graph::default();
    let view = ExecGraph::read(&g);
    // A value exactly at the cap is allowed; one byte over is refused.
    view.charge_expression(EXPRESSION_BUDGET_BYTES).unwrap();
    let err = view
        .charge_expression(EXPRESSION_BUDGET_BYTES + 1)
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Capacity { used, limit, kind }
                if used == EXPRESSION_BUDGET_BYTES + 1
                    && limit == EXPRESSION_BUDGET_BYTES
                    && kind == CapacityKind::Expression
        ),
        "{err}"
    );
    // The counter is construction-work telemetry: it accumulates and never
    // refuses a second under-cap allocation (no cumulative enforcement).
    view.charge_expression(EXPRESSION_BUDGET_BYTES).unwrap();
    assert_eq!(view.expression_built_bytes(), 2 * EXPRESSION_BUDGET_BYTES);
}

/// The empty plan [`ExecGraph::begin_statement`] seeds from: no clauses,
/// so nothing is declared — the reset is the only effect.
fn empty_plan() -> LogicalPlan {
    LogicalPlan {
        parts: Vec::new(),
        union_all: Vec::new(),
        next: None,
        use_target: None,
    }
}

/// The statement entry resets the expression-build meter, its own policy
/// and its own test — never a rider on the per-allocation refusal.
#[test]
fn statement_entry_resets_the_expression_budget() {
    let g = Graph::default();
    let view = ExecGraph::read(&g);
    view.charge_expression(EXPRESSION_BUDGET_BYTES).unwrap();
    assert_eq!(view.expression_built_bytes(), EXPRESSION_BUDGET_BYTES);
    view.begin_statement(&empty_plan());
    assert_eq!(view.expression_built_bytes(), 0);
}

#[test]
fn the_configured_limit_and_the_explicit_opt_out_are_honored() {
    let mut limited = Graph::default();
    limited.configure_expression_memory_limit(Some(16));
    let view = ExecGraph::read(&limited);
    assert!(view.charge_expression(17).is_err());
    assert!(view.charge_expression(16).is_ok());
    assert_eq!(view.expression_limit(), Some(16));

    // `none`/`0` disables the cap for known bulk pipelines.
    let mut unlimited = Graph::default();
    unlimited.configure_expression_memory_limit(None);
    let view = ExecGraph::read(&unlimited);
    view.charge_expression(EXPRESSION_BUDGET_BYTES * 4).unwrap();
}

#[test]
fn expression_value_buffer_charge_is_per_allocation() {
    let g = Graph::default();
    let view = ExecGraph::read(&g);
    // One byte past the largest value count whose buffer fits the cap.
    let len = (EXPRESSION_BUDGET_BYTES as usize / std::mem::size_of::<Value>()) + 1;
    let err = view.charge_value_buffer(len).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Capacity { used, limit, kind }
                if used > limit && limit == EXPRESSION_BUDGET_BYTES && kind == CapacityKind::Expression
        ),
        "{err}"
    );
}

#[test]
fn the_expression_refusal_is_plain_user_language() {
    const GB: u64 = 1024 * 1024 * 1024;
    let error = Error::capacity_expression(24 * GB, EXPRESSION_BUDGET_BYTES);
    let text = error.to_string();
    assert!(
        text.contains("tried to build a single value of 24 GB"),
        "{text}"
    );
    assert!(
        text.contains("safety limit protecting against memory exhaustion (not a database limit)"),
        "{text}"
    );
    assert!(text.contains("--expression-memory-limit"), "{text}");
    assert!(
        !text.contains("logical bytes") && !text.contains("value slot"),
        "{text}"
    );
}

#[test]
fn retained_total_owns_the_running_container_check() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(900));
    let view = ExecGraph::read(&g);
    let mut retained = view.retain_slots(1).unwrap();
    assert_eq!(retained.total(), crate::workspace::value_buffer_bytes(1));
    retained.add_owned(&view, &Value::Int(7)).unwrap();
    assert_eq!(retained.total(), crate::workspace::value_buffer_bytes(1));
    // A ~900-byte unique payload pushes the container past the 900-byte cap.
    let big = Value::String("x".repeat(900).into());
    let error = retained.add_owned(&view, &big).unwrap_err();
    assert!(
        matches!(
            error,
            Error::Capacity {
                limit: 900,
                kind: CapacityKind::Expression,
                ..
            }
        ),
        "{error}"
    );
}

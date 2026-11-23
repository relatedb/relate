//! The ADR-0395 nested-evaluation budget error's one shared assertion:
//! the exact authored message **for the budget the statement ran
//! under** (the caller passes the number its store was configured with
//! — a bomb's lowered value, or the published constant) and the `Kind:`
//! prefix parsing back to the ADR's `ErrorKind::
//! NestedEvaluationBudgetExceeded` row. Both budget tests assert
//! through here — the executor's unit bombs and the public-crate
//! integration regression — so the contract is spelled once.

use relate_error::ErrorKind;

/// The authored budget error, byte-exact: the `Kind:` prefix the error
/// channel classifies (`ErrorKind::NestedEvaluationBudgetExceeded`,
/// ADR-0395's new row) naming the limit, in the twin's style (the
/// compile-side `MAX_CLONED_NODES` error, ADR-0382).
fn nested_eval_budget_error_message(budget: u64) -> String {
    format!(
        "NestedEvaluationBudgetExceeded: nested evaluation exceeds {budget} \
         passes per statement; nested subqueries compound per level \
         (security budget)."
    )
}

/// The one budget-error assertion: the exact authored message for the
/// budget the statement ran under, and the kind — the `Kind:` prefix
/// parses back to the ADR-0395 `ErrorKind` row the runtime channel
/// carries (`EngineError::Runtime` with that kind). The caller passes
/// the budget the statement ran under — the number its store was
/// configured with, or the published constant — so the expected text
/// and the enforced guard read one number.
///
/// # Panics
///
/// Panics when `message` is not the byte-exact ADR-0395 budget error.
pub fn assert_nested_eval_budget_error(message: &str, budget: u64) {
    assert_eq!(
        message,
        nested_eval_budget_error_message(budget),
        "the exact authored ADR-0395 budget error"
    );
    let (kind, _) = message
        .split_once(':')
        .expect("the Execution payload is kind-prefixed");
    assert_eq!(
        ErrorKind::from_name(kind),
        Some(ErrorKind::NestedEvaluationBudgetExceeded),
        "the kind prefix names the ADR-0395 row"
    );
}

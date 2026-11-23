//! `EXPLAIN` / `PROFILE` plan introspection (GitHub issue #14).
//!
//! The plan is returned as the structured tree (ADR-0359: text is
//! rendered at the boundary, never in the engine):
//!
//! - `EXPLAIN <statement>` plans the statement and stamps the structured
//!   tree on `QueryResult.plan`, with no result columns or rows of its
//!   own. Execution never touches the wrapped statement: no writes,
//!   no procedures, no `LOAD CSV`.
//! - `PROFILE <statement>` executes the statement through the normal
//!   dispatch and returns the query's own rows and columns unchanged,
//!   with the structured plan tree — per-operator metrics (rows produced,
//!   elapsed time) on every clause operator the dispatch loop executed —
//!   stamped beside them on `QueryResult.plan` (ADR-0344/0345 phase 2;
//!   the deprecated `_profile_*` columns are removed).
//!
//! Both builders also stamp the query notifications (ADR-0363) derived
//! from the same tree and plan. Every statement class the front-end lowers
//! yields at least one plan node; a statement that cannot be planned fails
//! with the planner's typed error instead of a silently empty table.

use std::collections::BTreeMap;

use relate_executor::ExecGraph;
use relate_model::wire::Notification;
use relate_parser::PreambleMode;
use relate_planner::{LogicalPlan, QueryType};

use crate::phase_timing::TimingStamp;
use crate::plan_node;
use crate::statement_notifications;
use crate::{QueryResult, ResultHeader};
use relate_core::{Namespace, Result};

/// The wrapped statement as the plan header echoes it (ADR-0359): the
/// parse classified the preamble (`parsed.preamble`), so
/// the engine strips the one classified keyword token here, once, at the
/// producer — the boundary reads the stamped text and never re-scans for
/// the keyword. `mode` is the parse's classified preamble.
pub(crate) fn statement_echo(cypher: &str, mode: PreambleMode) -> String {
    let keyword = match mode {
        PreambleMode::Explain => "EXPLAIN",
        PreambleMode::Profile => "PROFILE",
        PreambleMode::Execute => return cypher.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    // The keyword is not always the first token: a statement may open with
    // another preamble form (`CYPHER 25 EXPLAIN …`), so the first token
    // equal to it is the one dropped.
    let mut tokens = cypher.split_whitespace().collect::<Vec<_>>();
    if let Some(at) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case(keyword))
    {
        tokens.remove(at);
    }
    tokens.join(" ")
}

/// The two inputs the statement-scoped notification derivation reads —
/// bundled so the result builders carry one argument for them.
pub(crate) struct NotificationInputs<'a> {
    pub view: &'a ExecGraph<'a>,
    pub params: &'a BTreeMap<String, relate_core::Value>,
}

impl NotificationInputs<'_> {
    /// The fallible statement-scoped half. PROFILE's epilogue settles it
    /// before handing over a finished result (a failed catalog read must not
    /// discard rows the statement already produced); EXPLAIN propagates it.
    pub(crate) fn derive(&self, plan: &LogicalPlan) -> Result<Vec<Notification>> {
        statement_notifications::statement_notifications(plan, self.view, self.params)
    }
}

/// The result's notification list (ADR-0363): the tree's operator-scoped
/// entries followed by the statement-scoped half, in the one merge order
/// [`statement_notifications::merge`] defines. Both builders call this, so
/// the two cannot order or deduplicate differently.
fn merged_notifications(
    tree: &plan_node::PlanTree,
    statement: Vec<Notification>,
) -> Vec<Notification> {
    statement_notifications::merge(plan_node::tree_notifications(tree), statement)
}

/// `EXPLAIN <statement>`: the plan tree; the statement is not executed.
/// `query_type` is the wrapped statement's plan-derived type (ADR-0162),
/// stamped onto the result like the namespace. `timing` supplies the run's
/// measured timing as one stamp — the ADR-0221 total and its always-on
/// PARSE / PLAN / EXECUTE split (LABS-832), whose EXECUTE arm is the plan
/// render, this path's execute-equivalent stage. It is taken lazily, at
/// the header construction below the render, so the render's cost is
/// measured into that EXECUTE arm and the total.
pub(crate) fn explain_result(
    plan: &LogicalPlan,
    namespace: Namespace,
    query_type: QueryType,
    timing: impl FnOnce() -> TimingStamp,
    stats: &crate::stats::GraphPatternStats,
    statement_echo: String,
    statement: Vec<Notification>,
) -> QueryResult {
    // The structured tree (ADR-0359): the plan rides `QueryResult.plan`
    // alone — the rendered plan column is gone, so every surface renders
    // its own text through `relate-formatter` on demand.
    let tree = plan_node::plan_tree(plan, stats).with_statement_echo(statement_echo);
    // The notifications (ADR-0363) derive from the same tree and the plan —
    // the one place a tree is built — never on the plain path. The tree walk
    // reads the staged facts the construction walk recorded beside it, so
    // the handed-out tree stays derivable.
    let notifications = merged_notifications(&tree, statement);
    // A plan-producing run executed nothing (ADR-0402 Decision 2): the one
    // construction for a non-executed result names that effect itself, so no
    // construction site spells the placeholder. The timing stamp is taken
    // here — after the render (LABS-832) — so the EXECUTE arm covers the
    // plan render it is documented to cover; `ResultHeader::new` takes it
    // lazily and invokes it at the header's construction.
    QueryResult::not_executed(ResultHeader::new(
        Vec::new(),
        &[],
        query_type,
        namespace,
        timing,
    ))
    .with_plan(tree.root, notifications)
}

/// `PROFILE <statement>`'s epilogue. `statement` is the statement-scoped
/// notification half, already derived by the caller: a finished result must
/// not be discarded when a catalog read fails, so that policy stays at the
/// dispatch boundary.
pub(crate) fn profile_result(
    plan: &LogicalPlan,
    metrics: &crate::profile::ProfileMetrics,
    result: QueryResult,
    stats: &crate::stats::GraphPatternStats,
    statement_echo: String,
    statement: Vec<Notification>,
) -> QueryResult {
    // ADR-0344/0345 phase 2: the structured tree with per-operator
    // metrics rides the result beside the statement's own rows — the
    // deprecated `_profile_*` columns are gone (ADR-0345's sanctioned
    // removal), their information strictly exceeded by the tree.
    let tree = plan_node::profiled_tree(plan, Some(&metrics.collected()), stats)
        .with_statement_echo(statement_echo)
        .with_statement_memos(metrics.statement_memos());
    // ADR-0363: the list derives from the same profiled tree, so the
    // PROFILE-only `EstimationMiss` (stamped beside the metrics) rides it;
    // the caller settled the statement-scoped half from the same plan.
    let notifications = merged_notifications(&tree, statement);
    // The columns and rows are the statement's own — the ADR-0224 stamp
    // stays valid; the tree carries the profile facts (per-operator
    // time, so no wall-clock cell is appended).
    result.with_plan(tree.root, notifications)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_drops_the_classified_keyword_wherever_it_sits() {
        assert_eq!(
            statement_echo("EXPLAIN  MATCH (n) RETURN n", PreambleMode::Explain),
            "MATCH (n) RETURN n"
        );
        assert_eq!(
            statement_echo(
                "CYPHER 25 profile MATCH (n) RETURN n",
                PreambleMode::Profile
            ),
            "CYPHER 25 MATCH (n) RETURN n"
        );
    }
}

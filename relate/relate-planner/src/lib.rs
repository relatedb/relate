//! `relate-planner`: bind a lowered `Query` to a [`LogicalPlan`]
//! (ADR-0009), then rewrite MATCH (ADR-0024).
//!
//! Module map:
//! - [`plan`] — plan types, the `plan` entry point (consumes the `Query`)
//!   and the canonical plan walks ([`walk_plan_clauses`] for the full
//!   clause tree, [`walk_plan_parts`] for part-granular top-level clauses,
//!   [`walk_query_parts`] for the same part-granular walk over a
//!   not-yet-planned `ir::Query`)
//! - [`binding`] — the variable-binding environment
//! - [`validate`] — clause-level semantic validation
//! - [`expr_check`] / [`pattern`] / [`agg`] — expression, pattern, and
//!   aggregation checks used by validation
//! - [`names`] — output-column computation (UNION agreement, subquery
//!   exports, NEXT-stage seeds)
//! - [`rewrite`] — post-bind MATCH rewrites
//! - [`order`] — cardinality estimates and MATCH pattern order
//! - [`clause_kind`] — the Cypher keyword for a parser / planner-IR clause,
//!   the one read/write/schema clause classification behind
//!   [`clause_mutates_store`] and [`clause_projects_rows`] (its published
//!   per-clause answers)
//! - [`access`] — the one read/write/schema clause fold over a statement,
//!   shared by [`namespaces`] and [`query_type`]
//! - [`namespaces`] — the ADR-0116 namespace classifiers (§2
//!   database-administration commands, §4 `system` / `system_ontologies`
//!   gates, read/write classification); the §3 `USE` rules live in
//!   [`use_target`]
//! - [`query_type`] — the Neo4j Query API v2 `queryType` classifier
//!   (r / w / rw / s), a projection of the shared [`access`] fold plus the
//!   statement's returned columns (ADR-0332)
//! - [`inline`] — the bounded-work plan predicate (which planned
//!   statements are quick at this engine's scale and may run on the
//!   calling thread of a worker-stack-asserting connection, ADR-0504)

mod access;
mod agg;
mod binding;
mod clause_kind;
mod expr_check;
mod expr_text;
mod inline;
mod names;
mod namespaces;
mod order;
mod pattern;
mod plan;
mod query_type;
mod resolve;
mod rewrite;
mod stats;
mod use_target;
mod validate;

pub use crate::clause_kind::{clause_keyword, clause_mutates_store, clause_projects_rows};
pub use crate::inline::plan_is_bounded_work;
pub use crate::names::{conditional_declared_columns, query_with_persistent_imports};
pub use crate::namespaces::{
    admin_command_heads_statement, composed_show_disallowed_clause, is_database_admin_command,
    is_show_databases_with_next, is_write, plan_calls_write_procedure,
    unsupported_system_clause_names,
};
pub use crate::order::{
    apply_degree, apply_hop_bound, apply_path_degree, best_seek, endpoint_estimate,
    seed_on_rel_start, seed_rows, walk_cost, where_conjuncts_of, Bound, Estimate, Hop, SeekPick,
};
/// ADR-0493's range-seek crossover policy — `pub` for the executor
/// funnel, the policy's second consumer (the ladder is the first).
pub use crate::order::{crossover_scan_rows, range_seek_gated};
pub use crate::plan::{
    plan, plan_any_clause, plan_contains_dynamic_operands, plan_contains_in_transactions,
    plan_has_bound_float_integer_site, plan_output_names, plan_output_names_expect,
    plan_parameter_names, plan_with_stats, plan_with_tokens, query_any_clause, walk_plan_clauses,
    walk_plan_parts, walk_query_parts, LogicalPlan,
};
pub use crate::query_type::{query_type, QueryType};
pub use crate::stats::{ContainsCount, DegreeMoments, NoPatternStats, PatternStats};
pub use crate::validate::check_union_joiners;

/// Test helper: parse `cypher` and plan it with no write procedures
/// registered. Shared by the `namespaces`, `query_type` and `inline` test
/// modules.
#[cfg(test)]
pub(crate) fn plan_of(cypher: &str) -> LogicalPlan {
    let ast = relate_front::parse_cypher(cypher).unwrap();
    plan(ast, &mut |_| false).unwrap()
}

/// Test helper: the error text of a statement the product parser or the
/// planner rejects. Either stage is a rejection, so both are folded into
/// one string — the one helper every planner test module uses.
#[cfg(test)]
pub(crate) fn rejection(src: &str) -> String {
    match relate_front::parse_and_lower(src) {
        Ok(q) => plan(q, &mut |_| false).unwrap_err().to_string(),
        Err(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests;

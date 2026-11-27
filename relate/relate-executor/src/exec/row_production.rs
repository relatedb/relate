//! The row-production classification — whether a concluding clause, part
//! or body projects statement rows of its own. One home for the WHEN/CALL
//! refinements over the planner's published table, the stage/body
//! FINAL-stage cursor, and the NEXT-seeding vocabulary
//! (`unit_output_clause` / `parts_have_unit_output`), split out of the
//! statement-driver root so the classification lives beside its own
//! vocabulary instead of inside the drivers (REL8-2370's review round).
//! The conditional's branch-set model is its own sibling
//! ([`super::conditional_branches`]); this ladder consumes it downward.

use relate_ir::{Clause, Query};
use relate_planner::clause_projects_rows;

use super::conditional_branches::ConditionalBranches;

/// Whether a conditional clause projects rows in any branch — the fact the
/// NEXT-stage seeding policy reads: a conditional whose branches are all
/// unit bodies is itself unit output, so an empty result still seeds the
/// next stage with one row (ADR-0287) instead of ending the chain. The
/// one model-side fold over the branch set — an allocation-free
/// short-circuit (it runs per outer row through `clause_spills_rows`);
/// the conditional driver reads the same [`body_projects_rows`] fact per
/// taken branch, where it holds the branch's own body.
fn conditional_produces_rows(branches: &ConditionalBranches<'_>) -> bool {
    branches
        .bodies()
        .any(|body| body.is_some_and(body_projects_rows))
}

/// Whether the clause concluding a part spills statement rows when it
/// runs — the planner's published [`clause_projects_rows`] table plus the
/// two refinements only this layer can make, one exhaustive match: a
/// `WHEN`/`ELSE` conditional whose branch bodies are all unit output
/// spills no rows (the per-row driver's buckets stay empty), and a
/// `CALL { … }` whose body concludes unit output spills none either —
/// except the batched form, which keeps the table's unconditional
/// answer: `CALL { … } IN TRANSACTIONS` always spills rows (each batch's
/// pass-through rows, `REPORT STATUS` rows, `ON ERROR` pads), whatever
/// its body — the executor-side spelling of the recursion the IR's
/// `relate_ir::Query::projects_columns` runs for the planner's scope
/// fold (REL8-2370: a body ending in a nested unit call made the
/// enclosing `CALL` merge the nested body's pass-through rows, dropping
/// the outer row on an empty inner stream and duplicating it on a
/// many-row one). Every concluding-clause policy reads this one answer —
/// the `CALL { … }` merge policy ([`part_projects_rows`]), a conditional
/// branch body's row-lessness and the NEXT-stage seeding policy
/// ([`unit_output_clause`]) — so a unit conditional body is a unit body
/// everywhere: its caller's outer rows pass through, never drop, and its
/// `IN TRANSACTIONS` iterations report their status rows.
fn clause_spills_rows(clause: &Clause) -> bool {
    match clause {
        Clause::Conditional {
            branches,
            else_query,
        } => conditional_produces_rows(&ConditionalBranches::new(branches, else_query.as_deref())),
        // The batched call always emits rows of its own — never judge it
        // by its body (rust-review, REL8-2370's fix round: a WHEN branch
        // body concluding in `CALL { … } IN TRANSACTIONS` dropped its
        // batch rows under the body-classifying spelling; main kept
        // them) — the batched shape read through its canonical helper,
        // `in_transactions_body`.
        Clause::CallSubquery { query, .. } => {
            clause.in_transactions_body().is_some() || body_projects_rows(query)
        }
        other => clause_projects_rows(other),
    }
}

/// Whether a part (one UNION arm) projects statement rows of its own — the
/// one classification over the concluding-clause vocabulary
/// ([`clause_spills_rows`]: the planner table plus the all-unit
/// conditional and nested-call refinements), the rung
/// [`stage_projects_rows`] folds over a stage's arms.
fn part_projects_rows(clauses: &[Clause]) -> bool {
    clauses.last().is_none_or(clause_spills_rows)
}

/// Whether any UNION arm of one stage projects a statement result of its
/// own — the stage-level form of the one "does this body project rows"
/// fact, over a stage's parts slice. The slice is the type-level
/// distinction: a whole NEXT-chained body is not a stage, and only
/// [`body_projects_rows`] — the FINAL-stage cursor's one owner — may
/// hand one in. `execute_query_seeded_in_scope` (in `call`) is the one
/// stage-level caller, naming the stage it runs; every other consumer
/// reads [`body_projects_rows`]. The privacy fence covers exec-level
/// consumers; `call`'s descendants stay on it by this doc, since an
/// item fenced to `crate::exec` remains nameable from them.
pub(in crate::exec) fn stage_projects_rows(parts: &[relate_ir::SingleQuery]) -> bool {
    parts.iter().any(|part| part_projects_rows(&part.clauses))
}

/// Whether a `CALL { … }` / `WHEN` body projects a statement result of
/// its own — its FINAL stage's arms ([`stage_projects_rows`]); a body
/// that is itself a NEXT chain is judged by its final stage, never its
/// first, the exact wrong-cursor drop LABS-712 fixed — now with one
/// home instead of per-caller spellings. Read by every body-level
/// consumer: the CALL-subquery driver's merge policy
/// (`call::call_subquery_row`), the conditional classification
/// ([`conditional_produces_rows`]) and the conditional driver's per-row
/// branch read (`call::conditional::exec_conditional`), the batch-pass
/// admission (`call::in_tx_strategy::batch_pass_admission`) and
/// [`clause_spills_rows`]'s nested-`CALL` arm — the executor-side twin
/// of the IR's `relate_ir::Query::projects_columns` recursion. The two
/// deliberately differ by layer (the IR folds `RETURN`-only vocabulary
/// with a first-branch stand-in; this fold runs over the planner's full
/// `clause_projects_rows` table with any-branch/any-arm breadth) and
/// agree only under the shared invariants both docs name — the IR side
/// (`relate_ir::Query::projects_columns`) carries the twin paragraph
/// with them, so the coordination is bidirectional.
pub(in crate::exec) fn body_projects_rows(body: &Query) -> bool {
    stage_projects_rows(&body.final_stage().parts)
}

/// Whether the clause concluding a part makes that part unit output: an
/// empty result still seeds the next `NEXT` stage with one row (ADR-0287).
/// [`clause_spills_rows`] decides it, plus the one arm this seeding policy
/// adds over that classification: `CALL { … }` is a data write in the
/// ADR-0287 vocabulary, even though its merged rows are a statement result
/// of its own.
fn unit_output_clause(clause: &Clause) -> bool {
    matches!(clause, Clause::CallSubquery { .. }) || !clause_spills_rows(clause)
}

/// Whether a finished stage seeds the next `NEXT` stage with one unit row
/// when its own result is empty (ADR-0287). Data-write parts
/// (`CREATE`/`MERGE`/`DELETE`/`SET`/`REMOVE`/`FOREACH`/`CALL { … }`) and
/// `FINISH` count, and so do schema/admin DDL parts — the IR-native
/// index/constraint variants outright, and `Clause::Catalog` exactly when
/// the canonical `relate-front` predicate says the command is NOT
/// row-producing. Row-producing catalog commands (the SHOW family and
/// `TERMINATE TRANSACTIONS`) keep their row semantics: they seed the next
/// stage from their projected rows, never with a unit row.
pub(in crate::exec) fn parts_have_unit_output<'p>(
    mut parts: impl Iterator<Item = &'p [Clause]>,
) -> bool {
    parts.all(|part| part.last().is_some_and(unit_output_clause))
}

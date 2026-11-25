//! The `NEXT`-chain validation driver: the seed one chain validates
//! from ([`StageSeed`], its named constructors the only ways to build
//! one), the stage driver, and the whole-chain walk both the planner's
//! entry and a `CALL { … }` body reach it through. Split from the
//! clause-validation walk so the chain policy has one home.

use relate_core::{Error, Result};
use relate_ir::{Clause, Query};

use crate::binding::{next_stage_scope, Env};

use super::subquery::validate_stage_star_rule;
use super::write::WriteHistory;
use super::Scope;

/// The seed one whole `NEXT` chain (a planned statement, or a `CALL { … }`
/// body's own chain) validates from — the one spelling of it, shared by
/// the planner's entry ([`crate::plan::plan_checked`]) and the validate
/// layer's chain walk, so the chain facts have no second form:
///
/// - `env` is what the chain's first stage may bind, `names` the order its
///   `RETURN *` expands them in;
/// - `excluded_imports` is the scope-clause star exclusion — empty for a
///   planned statement, a scope clause's declared imports for a `CALL`
///   body (threaded unchanged into every later stage);
/// - `scope_clause` is the scope-clause fact itself —
///   `CallScope::has_scope_clause()`'s answer, carried once here instead
///   of re-derived from the exclusion set's emptiness (a planned
///   statement is not a scope-clause body);
/// - `write_history` is the 42I25 fact behind the chain's first stage:
///   empty for a fresh statement and for a body (a subquery body's chain
///   starts fresh; an `IN TRANSACTIONS` subquery inside any body is the
///   nested rejection's, never that rule's).
///
/// Every seed is built by a named constructor — the boundary convention
/// the sibling `Scope` states — so no call site pairs the facts wrong:
/// [`Self::statement`] (the planner's entry), [`Self::body`] (a `CALL`
/// body, deriving the scope-clause fact from the scope itself), and
/// [`Self::successor`] (the one chain-advance policy).
pub(crate) struct StageSeed {
    pub(crate) env: Env,
    pub(crate) names: Vec<String>,
    pub(crate) excluded_imports: std::collections::HashSet<String>,
    pub(crate) scope_clause: bool,
    pub(crate) write_history: WriteHistory,
}

impl StageSeed {
    /// A fresh statement's seed: nothing inherited, nothing excluded, no
    /// write behind — the planner's entry, and nothing else.
    pub(crate) fn statement() -> Self {
        Self {
            env: Env::new(),
            names: Vec::new(),
            excluded_imports: crate::names::no_excluded_imports(),
            scope_clause: false,
            write_history: WriteHistory::empty(),
        }
    }

    /// A `CALL { … }` body's seed: the body's incoming scope (`env`), its
    /// own `RETURN *` expansion order (`names`), and the scope clause's
    /// import exclusion — with the scope-clause fact derived from the
    /// scope itself, the one statement of it. No write history behind: a
    /// body's chain starts fresh.
    pub(crate) fn body(
        scope: &relate_ir::CallScope,
        env: Env,
        names: Vec<String>,
        excluded_imports: std::collections::HashSet<String>,
    ) -> Self {
        Self {
            env,
            names,
            excluded_imports,
            scope_clause: scope.has_scope_clause(),
            write_history: WriteHistory::empty(),
        }
    }

    /// The seed the `NEXT` stage validates from — the one chain-advance
    /// policy: the previous stage's exported columns plus this seed's
    /// inherited names (the chained statement keeps the imported names
    /// visible in *every* of its parts), the same exclusion set and
    /// scope-clause fact, and `write_behind` — the previous stage's 42I25
    /// answer.
    pub(crate) fn successor(self, output_names: &[String], write_behind: WriteHistory) -> Self {
        let (env, names) = next_stage_scope(&self.env, &self.names, output_names);
        Self {
            env,
            names,
            excluded_imports: self.excluded_imports,
            scope_clause: self.scope_clause,
            write_history: write_behind,
        }
    }
}

/// What one validated stage hands its `NEXT` successor: the columns it
/// exports, and the 42I25 write history behind it — the seed's incoming
/// fact folded with this stage's own unbatched writes.
struct StageValidation {
    output_names: Vec<String>,
    write_behind: WriteHistory,
}

/// The nested-body entry: a body's own walk starts with no 42I25 history
/// (a batched subquery inside any body is another rule's — the nested
/// rejection, the expression-body write refusal, the FOREACH CALL
/// refusal), and its answer is this rule's business never — discarded
/// here, by name.
pub(crate) fn validate_nested_body(
    clauses: &[Clause],
    scope: Scope,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<()> {
    let _ = super::validate_clauses_with_env(
        clauses,
        scope,
        WriteHistory::empty(),
        is_write_procedure,
    )?;
    Ok(())
}

/// The stage-level validation driver: the structural composition rules one
/// planned stage must satisfy, its per-part semantic checks, and its output
/// columns (the UNION column-agreement check).
/// [`validate_query_with_env`] runs it once per stage of a chain — the
/// planner's entry ([`crate::plan::plan_checked`]) and a `CALL { … }`
/// body's walk both reach it through that driver, never directly.
///
/// Returns the [`StageValidation`] the stage hands its `NEXT` successor:
/// its output columns, and the 42I25 write history behind it — the
/// seed's incoming fact folded with this stage's own unbatched writes.
/// The stage's parts are parallel `UNION` arms — each validates from the
/// same incoming fact, and their answers fold into the successor's.
fn validate_stage_with_env(
    query: &Query,
    seed: &StageSeed,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<StageValidation> {
    // No empty-`parts` arm: a `Query` invariantly carries at least one
    // part (the IR's own invariant — `Query::projects_columns`' `expect`
    // states it), so the walk below reads `query.parts` unconditionally;
    // a part with no clauses at all is the clause walk's own 42N45
    // refusal, one spelling for the whole family.
    if !query.union_all.is_empty() {
        super::check_union_joiners(&query.union_all)?;
        if let Some(offender) = query
            .parts
            .iter()
            .flat_map(|p| p.clauses.iter())
            .find(|c| crate::clause_kind::clause_cannot_join_union(c))
        {
            return Err(Error::syntax(format!(
                "InvalidClauseComposition: {} cannot be used with UNION",
                crate::clause_kind::ir_clause_keyword(offender)
            )));
        }
    }
    // The successor's write history folds from the parts' answers alone:
    // each already carries the seed's incoming history (the one spelling
    // of the fact entering the fold).
    let mut write_behind = WriteHistory::empty();
    for part in &query.parts {
        write_behind = write_behind.union(super::validate_clauses_with_env(
            &part.clauses,
            Scope::stage(seed.env.clone()),
            seed.write_history,
            is_write_procedure,
        )?);
        if matches!(
            part.clauses.last(),
            Some(
                Clause::Match { .. }
                    | Clause::With { .. }
                    | Clause::Unwind { .. }
                    | Clause::Use { .. }
            )
        ) {
            return Err(Error::syntax(
                "InvalidClauseComposition: query must conclude with RETURN, FINISH, an update, or a unit subquery",
            ));
        }
    }
    // This stage's output columns, computed from the unrewritten parts. The
    // stage's own incoming scope seeds the computation: a bare `RETURN *`
    // stage projects exactly the variables passed through the previous
    // `NEXT`, so an empty seed here would drop them. The
    // exclusion set is the scope-clause import exclusion: a scope-clause
    // body's imports are in scope but a star must not re-expand them
    // (and every UNION arm must agree on that shape).
    let output_names = crate::names::parts_output_names(
        &query.parts,
        seed.names.to_vec(),
        &seed.excluded_imports,
    )?;
    Ok(StageValidation {
        output_names,
        write_behind,
    })
}

/// The whole-`NEXT`-chain validation driver: one stage through
/// [`validate_stage_with_env`], then the same walk over the stage's
/// successor with the seed its predecessor's answer derives — the one
/// seeding policy for a chain, shared by the planner's entry
/// ([`crate::plan::plan_checked`]) and by a `CALL { … }` body's own walk
/// (the caller that holds a query it will not plan, so validation never
/// re-enters the planner). Returns the final stage's output columns —
/// the query's export set.
///
/// The seed's `names` and `excluded_imports` are different facts (the
/// struct's doc): `names` is what the body's own scope sees, so it is what
/// a body-level `RETURN *` expands (the whole outer scope for a naked
/// `CALL { … }`); `excluded_imports` is the scope-clause import exclusion
/// set (non-empty only for a scope clause, whose star must not re-expand
/// the imports). They coincide for a scope clause and differ for a naked
/// body. `call_body_names` derives both once; this walk threads them
/// through the chain, whose later stages still see the same imports.
pub(crate) fn validate_query_with_env(
    query: &Query,
    seed: StageSeed,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> Result<Vec<String>> {
    let StageValidation {
        output_names,
        write_behind,
    } = validate_stage_with_env(query, &seed, is_write_procedure)?;
    // The scope-clause star rule for this stage, folded into the one chain
    // walk: a scope-clause body's star arm must expand at least one name
    // the body itself introduces. The rule is the CALL-scope export
    // policy's (it lives in `subquery.rs`); this driver invokes it as its
    // per-stage hook, gated by the seed's carried scope-clause fact —
    // `CallScope::has_scope_clause()`'s answer, not a re-derivation from
    // the exclusion set's emptiness. Running it here (rather than in a
    // second recursion over the same chain) walks each stage once and
    // reuses the seed names this walk already carries.
    if seed.scope_clause {
        validate_stage_star_rule(query, &seed.names, &seed.excluded_imports)?;
    }
    match query.next.as_deref() {
        // The `NEXT` tail's terminal-alias rule, once per stage — the one
        // shared spelling, this site's text; the driver is where "a NEXT
        // follows" is known, so the rule never rides the per-arm fold
        // above.
        Some(next) => {
            super::refuse_unaliased_terminal_returns(
                query,
                "NoExpressionAlias: expression in RETURN followed by NEXT must use AS",
            )?;
            validate_query_with_env(
                next,
                seed.successor(&output_names, write_behind),
                is_write_procedure,
            )
        }
        None => Ok(output_names),
    }
}

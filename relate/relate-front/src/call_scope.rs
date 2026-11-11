//! CIP-141 `CALL (...) { ... }` variable scope clause + CIP-218 `RETURN *`
//! exclusion, shared between `validate.rs` (compile-time Rule 1/4/5/8/9/10
//! checks) and `engine.rs` (execution: scoped row seeding, Rule 7 "always
//! referenceable" reinjection, Rule 10 star-exclusion filtering).
//!
//! Core model: a scope-clause `CALL (...) { ... }` body is walked clause by
//! clause, threading two sets alongside the ordinary projected columns:
//!
//! - `imported`: the fixed set of variable names the scope clause pulled in
//!   (from `CallScope::Vars`/`Wildcard`), constant for the whole body.
//! - `excluded`: the subset of `imported` that CIP-218 Rule 10 still hides
//!   from a `RETURN *`/`WITH *` star expansion -- initially all of
//!   `imported`, and removed from (i.e. "promoted" to star-visible) only
//!   when a `WITH` clause consists *solely* of bare pass-through references
//!   to already-excluded imported names (mirroring the CIP-218 doc's own
//!   `WITH a` example). A name bundled into a larger WITH item list
//!   alongside anything else (a computed expression, a fresh binding, or a
//!   `*`) is not promoted -- matching the observed (if not fully
//!   doc-justified) Neo4j behavior in the nested-scope-subquery case, where
//!   an inner CALL's imported variable resurfaces via a `WITH x, y, z`
//!   pass-through mixed with newly-computed names without tripping Rule 9.
//!
//! `imported` is also *always* layered back on top of the ordinary
//! projected scope after every clause (Rule 7: "globally referenced ...
//! cannot be delisted by a later WITH"), independent of `excluded`.

use crate::ident_hash::{IdentBuildHasher, IdentHashSet};
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{
    CallScope, CallSubqueryClause, Clause, ConditionalClause, Expr, PatternPath, ReturnClause,
    ReturnItem, Statement, WithClause,
};

use crate::ast::collect::{
    check_expr_vars_defined, check_names_allowed, clause_introduced_names, effective_name,
    pattern_vars, projected_names, search_score_var, union_chain,
};
// The lenient naked-CALL fallback consults the output-vars family at
// the crate root (its other consumer is `validate::scope`, importing
// downward) — no upward edge into `validate` remains.
use crate::output_vars::compute_subquery_output_vars;

/// The one construction site for an outer-scope collision: a subquery
/// output (or CALL export) re-declares a name the enclosing scope already
/// binds. Neo4j's verbatim text names the offender (CIP-218 interactions
/// [1]-[3]); every outer-scope guard routes through here so the diagnostic
/// cannot drift into unnamed variants. Lives at the crate root (its
/// consumers are this module and `validate::scope`, which imports
/// downward from here).
pub(crate) fn duplicate_outer_variable(name: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::VariableAlreadyBound,
        format!("Variable `{name}` already declared in outer scope"),
    )
}

/// The colliding export reported when a Rule 9 / 3.3(f) check fires: the
/// lexicographically smallest colliding name, not whichever a hash
/// iteration yields first — the rejection set is unchanged, the error
/// text is run-to-run deterministic.
fn smallest_collision<'a>(
    exports: &'a IdentHashSet<SmallName>,
    outer: &IdentHashSet<SmallName>,
) -> Option<&'a SmallName> {
    exports.iter().filter(|n| outer.contains(*n)).min()
}

/// Resolves a `CALL (...)` clause's scope against `outer_scope` (the set of
/// variables bound in the enclosing query immediately before this CALL),
/// enforcing Rule 1 (every explicit reference must already be in scope).
/// Returns `None` for a naked `CALL { ... }` (no new restrictions -- the
/// legacy importing-WITH behavior is unaffected by this module).
fn resolve_import_set(
    scope: &CallScope,
    outer_scope: &IdentHashSet<SmallName>,
) -> Result<Option<IdentHashSet<SmallName>>, EngineError> {
    match scope {
        CallScope::None => Ok(None),
        CallScope::Wildcard => Ok(Some(outer_scope.clone())),
        // Every imported name must exist in the outer scope (the canonical
        // fold; the reported offender is sorted-first, like every leaf).
        CallScope::Vars(names) => {
            // Pre-sized (ADR-0505 §4): the import list's length is the
            // exact final size.
            let mut imported: IdentHashSet<SmallName> =
                IdentHashSet::with_capacity_and_hasher(names.len(), IdentBuildHasher);
            imported.extend(names.iter().cloned());
            check_names_allowed(&imported, |n| outer_scope.contains(n))?;
            Ok(Some(imported))
        }
    }
}

/// Inserts a freshly-bound variable name (MATCH/MERGE pattern var, UNWIND
/// alias, CALL procedure yield) into `local_scope`, raising Rule 8's
/// shadowing error (when `strict`) if it collides with an imported name --
/// these binding forms can never be a legitimate pass-through the way a
/// `WITH` item can.
fn check_and_insert(
    name: &SmallName,
    imported: &IdentHashSet<SmallName>,
    local_scope: &mut IdentHashSet<SmallName>,
    strict: bool,
) -> Result<(), EngineError> {
    if strict && imported.contains(name) {
        return Err(name.with_str(shadow_error));
    }
    local_scope.insert(name.clone());
    Ok(())
}

fn shadow_error(name: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::VariableShadowing,
        format!(
            "the variable `{name}` is shadowing a variable with the same name from the outer scope and needs to be renamed"
        ),
    )
}

/// Like `check_and_insert`, but for a MATCH/MERGE/CREATE pattern-element
/// variable specifically: reusing a variable that's already bound (whether
/// imported or bound earlier in this same body) in a pattern position is a
/// *reference* -- constraining the pattern to that already-known node/
/// relationship, or attaching a new relationship to it -- never a fresh
/// declaration, so it's never Rule 8 shadowing (the same "pattern reuse
/// isn't shadowing" carve-out the planner applies to EXISTS/COUNT/COLLECT
/// bodies on the lowered IR). Only a name genuinely new to this pattern
/// goes through the real shadow check.
fn check_and_insert_pattern_var(
    name: &SmallName,
    imported: &IdentHashSet<SmallName>,
    local_scope: &mut IdentHashSet<SmallName>,
    strict: bool,
) -> Result<(), EngineError> {
    if local_scope.contains(name) {
        return Ok(());
    }
    check_and_insert(name, imported, local_scope, strict)
}

/// The MATCH/CREATE/MERGE arms' one pattern-binding loop: every variable of
/// every path — the path variable first, then its elements' — is checked
/// against the imported set and inserted into the scope. MERGE passes its
/// single pattern as a one-element slice, so the three arms state the same
/// policy once.
fn bind_pattern_vars_checked(
    paths: &[PatternPath],
    imported: &IdentHashSet<SmallName>,
    local_scope: &mut IdentHashSet<SmallName>,
    strict: bool,
) -> Result<(), EngineError> {
    for name in pattern_vars(paths) {
        check_and_insert_pattern_var(name, imported, local_scope, strict)?;
    }
    Ok(())
}

/// The branch-seeding policy, the one spelling: a branch body's initial
/// scope is `imported ∪ carry` — the names an earlier NEXT stage declared
/// ride along as reference-visibility on top of the imported set (see
/// [`walk_branch_seeded`]'s doc for the CIP-213 semantics of `carry`).
fn branch_seed(
    imported: &IdentHashSet<SmallName>,
    carry: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    // Pre-sized (ADR-0505 §4): the union of two known-length sets.
    let mut seed =
        IdentHashSet::with_capacity_and_hasher(imported.len() + carry.len(), IdentBuildHasher);
    seed.extend(imported.union(carry).cloned());
    seed
}

/// Walks one branch (`clauses`; callers handle `.union` chains themselves)
/// of a scope-clause CALL subquery body, seeding `local_scope` from
/// `imported` (plus `carry` when the caller has one). Invokes `sink` for
/// every `Clause::Return`/`Clause::Finish` encountered with the exported
/// name set that clause produces. When `strict`, also enforces Rule 4/5
/// (undefined variable references), Rule 8 (shadowing), and Rule 10's
/// `NoVariablesInScope`.
///
/// `carry` seeds extra reference-visible names beyond `imported` -- used
/// by CIP-213's NEXT-chain handling, where a later stage's clauses must
/// see whatever an earlier stage in the *same* body declared (its own
/// bound/projected names), without those carried names being treated as
/// CIP-141 "imported from the outer scope" for Rule 8 shadow-checking or
/// CIP-218 Rule 10 star-exclusion purposes -- only genuine `imported`
/// names get that treatment; `carry` is purely reference-visibility.
fn walk_branch_seeded(
    clauses: &[Clause],
    imported: &IdentHashSet<SmallName>,
    carry: &IdentHashSet<SmallName>,
    strict: bool,
    sink: &mut dyn FnMut(IdentHashSet<SmallName>) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let mut local_scope: IdentHashSet<SmallName> = branch_seed(imported, carry);
    let mut excluded: IdentHashSet<SmallName> = imported.clone();

    for (index, clause) in clauses.iter().enumerate() {
        let last = index + 1 == clauses.len();
        match clause {
            Clause::Match(mc) => {
                bind_pattern_vars_checked(&mc.pattern, imported, &mut local_scope, strict)?;
                if let Some(sv) = search_score_var(mc) {
                    check_and_insert_pattern_var(sv, imported, &mut local_scope, strict)?;
                }
            }
            Clause::Merge(mc) => {
                bind_pattern_vars_checked(
                    std::slice::from_ref(&mc.pattern),
                    imported,
                    &mut local_scope,
                    strict,
                )?;
            }
            Clause::Create(cc) => {
                bind_pattern_vars_checked(&cc.pattern, imported, &mut local_scope, strict)?;
            }
            Clause::Unwind(uw) => {
                if strict {
                    check_expr_vars_defined(&uw.expr, &local_scope)?;
                }
                check_and_insert(&uw.alias, imported, &mut local_scope, strict)?;
            }
            Clause::CallProcedure(cp) => {
                // The effective post-alias yield name comes from the one
                // YieldList model.
                for name in cp.yields.effective_names() {
                    check_and_insert(&name, imported, &mut local_scope, strict)?;
                }
            }
            Clause::Use(uc) => {
                if strict {
                    check_expr_vars_defined(&uc.graph_expr, &local_scope)?;
                }
            }
            // The row-producing catalog commands (e.g. a SHOW clause, a
            // fresh row source like MATCH with no prior clauses) put
            // their effective output columns — YIELD'd, or the full
            // catalog's under a bare `YIELD *` — into scope for a later
            // clause in this same scope-clause body to reference, e.g.
            // `CALL () { SHOW FUNCTIONS YIELD name ...
            // RETURN collect(name) }`: one guard, the same catalog
            // classification the other scope walkers use.
            c if crate::show_columns::is_catalog_clause(c) => {
                // The ordered column `Vec` (not the set-typed
                // `clause_introduced_names` fold) fixes the shadow-error
                // order; `catalog_output_names` is the one point the
                // engine-generated column text is classified.
                for v in crate::show_columns::catalog_output_names(c) {
                    check_and_insert(&v, imported, &mut local_scope, strict)?;
                }
            }
            Clause::With(wc) => {
                apply_with_to_scope(wc, imported, &mut excluded, &mut local_scope, strict)?;
            }
            Clause::CallSubquery(inner) => {
                check_call_return_items_aliased(&inner.statement)?;
                let inner_exports = resolve_nested_call(inner, &local_scope, strict)?;
                let projects = !inner_exports.is_empty();
                local_scope.extend(inner_exports);
                // A scoped body that concludes with a projecting CALL
                // exports the names in scope at that point, minus the
                // still-hidden imports (`CALL () { OPTIONAL CALL () {
                // RETURN 1 AS data } }` exports `data`).
                if last && projects {
                    // Pre-sized (ADR-0505 §4): the filter can only shrink
                    // the set, so the source length is an upper bound.
                    let mut exported =
                        IdentHashSet::with_capacity_and_hasher(local_scope.len(), IdentBuildHasher);
                    exported.extend(
                        local_scope
                            .iter()
                            .filter(|name| !excluded.contains(*name))
                            .cloned(),
                    );
                    sink(exported)?;
                }
            }
            Clause::Return(rc) => {
                let exported = return_exports(rc, &local_scope, &excluded, strict)?;
                sink(exported)?;
            }
            Clause::Finish => {
                sink(IdentHashSet::default())?;
            }
            // CIP-135: [`walk_conditional_bodies`] owns the arm's policy
            // (including the sole-element invariant the grammar gives it).
            Clause::Conditional(cc) => {
                walk_conditional_bodies(cc, imported, carry, strict, &mut *sink)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The CIP-135 Conditional arm's policy, one sibling helper beside
/// [`apply_with_to_scope`]/[`return_exports`] so the walker stays a
/// clause dispatch: a `WHEN/ELSE` clause is always the sole element of
/// the clause list it appears in (see `ConditionalClause`'s doc
/// comment), so nothing beyond the seeded `imported ∪ carry` could have
/// been bound where it stands — the seed is derived here, once, and the
/// predicates are checked against exactly the visibility each branch
/// body is then walked with (`imported` for the Rule 8/10 policy sets,
/// `carry` for reference-visibility). Rule 9/10 (enforced on the
/// lowered IR by the planner's `validate_conditional_returns`)
/// guarantees every branch reports the same exported names, so the
/// resulting union-of-exports is exactly that one shared set.
fn walk_conditional_bodies(
    cc: &ConditionalClause,
    imported: &IdentHashSet<SmallName>,
    carry: &IdentHashSet<SmallName>,
    strict: bool,
    sink: &mut dyn FnMut(IdentHashSet<SmallName>) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let seed = branch_seed(imported, carry);
    if strict {
        for branch in &cc.branches {
            check_expr_vars_defined(&branch.predicate, &seed)?;
        }
    }
    // Every branch body, plus the ELSE body, each with its own UNION chain.
    let bodies = cc
        .branches
        .iter()
        .map(|b| &b.body)
        .chain(cc.else_body.iter());
    for body in bodies {
        for s in union_chain(body) {
            walk_branch_seeded(&s.clauses, imported, carry, strict, &mut *sink)?;
        }
    }
    Ok(())
}

/// The alias policy a pass-through check runs under: Rule 10's star
/// promotion accepts bare pass-throughs only, Rule 8's redeclaration
/// check also accepts the redundant self-alias — the module's subtlest
/// invariant, named at the type level so each rule's call site states
/// its own policy.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AliasPolicy {
    /// `WITH a` — no alias.
    BareOnly,
    /// `WITH a` or the redundant self-alias `WITH a AS a`.
    AllowSelfAlias,
}

/// One WITH item's pass-through variable under `policy`: the item's
/// variable under its own output name when the item is a pass-through
/// the policy accepts, `None` otherwise.
fn pass_through_var(item: &ReturnItem, policy: AliasPolicy) -> Option<&SmallName> {
    let Expr::Variable(v) = &item.expr else {
        return None;
    };
    match item.alias.as_ref() {
        None => Some(v),
        Some(a) if policy == AliasPolicy::AllowSelfAlias && a == v => Some(v),
        _ => None,
    }
}

/// The RETURN arm's export computation, one sibling helper beside
/// [`apply_with_to_scope`] so the walker stays a clause dispatch: the
/// exported names (star expansion minus the still-excluded names, plus
/// every item's effective name) — the strict star-with-empty-scope
/// rejection included, so the arm is the helper call plus the sink.
fn return_exports(
    rc: &ReturnClause,
    local_scope: &IdentHashSet<SmallName>,
    excluded: &IdentHashSet<SmallName>,
    strict: bool,
) -> Result<IdentHashSet<SmallName>, EngineError> {
    if strict {
        check_non_star_items_defined(&rc.items, local_scope)?;
    }
    // The star-visible bound is the incoming scope minus the still-
    // excluded names; the exported set is then the canonical
    // [`projected_names`] projection over that bound (a `*` passes the
    // bound through, every other item contributes its effective name).
    // The bound matters only for a `*` item, so a star-less RETURN
    // passes an empty bound and skips the filtered clone.
    let has_star = rc.items.iter().any(|i| i.all);
    let bound = if has_star {
        local_scope
            .iter()
            .filter(|n| !excluded.contains(*n))
            .cloned()
            .collect()
    } else {
        IdentHashSet::default()
    };
    let exported = projected_names(&rc.items, &bound);
    if strict && has_star && exported.is_empty() {
        return Err(EngineError::semantic(
            ErrorKind::NoVariablesInScope,
            "RETURN * is not allowed when there are no variables in scope",
        ));
    }
    Ok(exported)
}

/// Rule 1's strict pre-check over a projection item list, the one
/// spelling both projection arms share: every non-star item's
/// expression must reference only visible names.
fn check_non_star_items_defined(
    items: &[ReturnItem],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for item in items {
        if !item.all {
            check_expr_vars_defined(&item.expr, scope)?;
        }
    }
    Ok(())
}

/// The WITH arm's four named policies, one sibling helper so the
/// walker stays a clause dispatch (LABS-518): the strict pre-check
/// (Rule 1 — every referenced name visible), the Rule 8
/// imported-name-redeclaration check, the Rule 10 star-exclusion
/// promotion, and the Rule 7 scope rebuild over [`projected_names`].
fn apply_with_to_scope(
    wc: &WithClause,
    imported: &IdentHashSet<SmallName>,
    excluded: &mut IdentHashSet<SmallName>,
    local_scope: &mut IdentHashSet<SmallName>,
    strict: bool,
) -> Result<(), EngineError> {
    // Rule 1 (strict pre-check) and Rule 8 (the redeclaration check)
    // share one gate: both are strict-only policies, spelled one way.
    if strict {
        check_non_star_items_defined(&wc.items, local_scope)?;
        if let Some(w) = &wc.where_expr {
            check_expr_vars_defined(w, local_scope)?;
        }

        // Rule 8: a non-pass-through item whose output name equals
        // an imported name is a forbidden redeclaration — a bare
        // `WITH a` or a redundant self-alias `WITH a AS a` of an
        // imported name is not one.
        for item in wc.items.iter().filter(|i| !i.all) {
            if let Some(name) = effective_name(item) {
                if imported.contains(&name)
                    && pass_through_var(item, AliasPolicy::AllowSelfAlias).is_none()
                {
                    return Err(name.with_str(shadow_error));
                }
            }
        }
    }

    // Rule 10 promotion: only when this WITH consists *solely*
    // of bare pass-throughs of currently-excluded names (see
    // module doc) do those names become star-visible. A `*` item counts
    // as "anything else": `WITH *, x` re-lists an imported name but leaves
    // the body's `RETURN *` still exporting only the body's own bindings
    // (CIP-218 `SubqueryAcceptance` "Return all should not return imported
    // variable #1" `WITH * , x` example). The
    // classification is collected once — every item a bare
    // pass-through (the length equality subsumes the star scan: a
    // `*` item classifies to None) and every one currently
    // excluded — so bundling with a `*` or any other item kind
    // disqualifies promotion exactly as computed expressions and
    // fresh bindings do.
    let promotions: Vec<&SmallName> = wc
        .items
        .iter()
        .filter_map(|i| pass_through_var(i, AliasPolicy::BareOnly))
        .collect();
    if promotions.len() == wc.items.len() && promotions.iter().all(|v| excluded.contains(v)) {
        for v in promotions {
            excluded.remove(v);
        }
    }

    // The star projection is [`projected_names`] (a `*` item
    // passes the incoming local scope through, every other
    // item contributes its effective name) plus the Rule 7
    // stickiness: the imported names stay referenceable.
    let mut new_scope = projected_names(&wc.items, local_scope);
    new_scope.extend(imported.iter().cloned()); // Rule 7 stickiness
    *local_scope = new_scope;
    Ok(())
}

/// Resolves and validates a (possibly nested) `CALL (...) { ... }` clause
/// found *inside* a scope-clause body, returning the set of names it
/// exports back into the enclosing body's local scope. For a naked
/// (`CallScope::None`) nested call, falls back to the pre-existing lenient
/// `compute_subquery_output_vars` (this module's new Rule 1/8/9/10 checks
/// only apply to scope-clause CALLs).
fn resolve_nested_call(
    csq: &CallSubqueryClause,
    outer_scope: &IdentHashSet<SmallName>,
    strict: bool,
) -> Result<IdentHashSet<SmallName>, EngineError> {
    match resolve_import_set(&csq.scope, outer_scope) {
        Err(e) => {
            if strict {
                Err(e)
            } else {
                Ok(IdentHashSet::default())
            }
        }
        Ok(None) => Ok(compute_subquery_output_vars(&csq.statement).unwrap_or_default()),
        Ok(Some(imported)) => {
            branch_exports_with_union(&csq.statement, &imported, outer_scope, strict)
        }
    }
}

/// Computes the union of every branch's (this statement plus its `UNION`
/// chain) exported names for a scope-clause body, enforcing Rule 9 (no
/// exported name may collide with `outer_scope`) when `strict`.
///
/// CIP-213: each branch may itself be a NEXT chain (`s.next`) —
/// [`stage_exports`] walks it — mirroring
/// `Engine::execute_call_scope_stage`'s identical runtime handoff.
fn branch_exports_with_union(
    stmt: &Statement,
    imported: &IdentHashSet<SmallName>,
    outer_scope: &IdentHashSet<SmallName>,
    strict: bool,
) -> Result<IdentHashSet<SmallName>, EngineError> {
    let mut exported = IdentHashSet::default();
    for s in union_chain(stmt) {
        exported.extend(stage_exports(
            s,
            imported,
            &IdentHashSet::default(),
            strict,
            outer_scope,
        )?);
    }
    Ok(exported)
}

/// One NEXT chain's export, stage by stage: walk this stage (so Rule
/// 4/5/8/10 are checked throughout), then either hand its projection to
/// the following stage as `carry` — an earlier stage's export is consumed
/// by the next stage, not exposed to the enclosing scope — or, for the
/// final stage, check Rule 9 and return its exports. Rule 3 (imported
/// names visible in every statement of the chain) holds via
/// [`walk_branch_seeded`] seeding `local_scope` from
/// `imported.union(carry)` — the one owner of that invariant.
fn stage_exports(
    stage: &Statement,
    imported: &IdentHashSet<SmallName>,
    carry: &IdentHashSet<SmallName>,
    strict: bool,
    outer_scope: &IdentHashSet<SmallName>,
) -> Result<IdentHashSet<SmallName>, EngineError> {
    // The sink collects the stage's export set; a stage whose walk never
    // fires a Return/Finish leaves it empty, which is already the
    // intended carry for the next stage.
    let mut stage_export: IdentHashSet<SmallName> = IdentHashSet::default();
    walk_branch_seeded(&stage.clauses, imported, carry, strict, &mut |exports| {
        stage_export = exports;
        Ok(())
    })?;
    match &stage.next {
        Some(next) => {
            // An earlier stage's projection is consumed by the following
            // NEXT stage, so re-projecting an *imported* name there
            // shadows the enclosing binding the chain is meant to keep
            // visible (CIP-213 `NextStatementAcceptance` "Should fail due
            // to redeclaration of constant 2": `CALL (a) { ... RETURN a, b
            // NEXT ... }` is 42N07). The final stage's export is handled
            // below; here it is only the mid-chain re-projection.
            if strict {
                for n in &stage_export {
                    if imported.contains(n) {
                        return Err(n.with_str(duplicate_outer_variable));
                    }
                }
            }
            stage_exports(next, imported, &stage_export, strict, outer_scope)
        }
        None => {
            if strict {
                if let Some(n) = smallest_collision(&stage_export, outer_scope) {
                    return Err(n.with_str(duplicate_outer_variable));
                }
            }
            Ok(stage_export)
        }
    }
}

/// Strict, top-level entry point (CIP-141/CIP-218 Rules 1/4/5/8/9/10):
/// recursively validates every `CALL (...) { ... }` clause anywhere in
/// `stmt` (including inside naked `CALL { ... }` bodies and UNION chains,
/// at arbitrary nesting depth).
pub fn validate_call_scope_clauses(stmt: &Statement) -> Result<(), EngineError> {
    walk_union_chain(stmt, IdentHashSet::default())
}

/// Neo4j 42N21: every `RETURN` item a `CALL { … }` body projects must be
/// aliased, unless it is a bare variable or a map projection over one (both
/// carry an implicit name). `RETURN *` and `RETURN 1 AS x` pass; `RETURN 5`
/// and `RETURN 1 + 1` are rejected. The rule is CALL-subquery-only — a
/// `COUNT { RETURN 5 }` body keeps its unaliased item.
fn check_call_return_items_aliased(stmt: &Statement) -> Result<(), EngineError> {
    check_return_items_aliased(
        stmt,
        "Expression in CALL { RETURN ... } must be aliased (use AS).",
    )
}

/// 42N21 in the CIP-146 brace form: a brace-wrapped statement
/// (`{ MATCH (n) RETURN n.age }`) projects its columns the same way a CALL
/// body does, under the brace wording live 2026.10 reports.
fn check_brace_return_items_aliased(stmt: &Statement) -> Result<(), EngineError> {
    if !stmt.brace_wrapped {
        return Ok(());
    }
    check_return_items_aliased(
        stmt,
        "Expression in { RETURN ... } must be aliased (use AS).",
    )
}

/// The one item rule behind both 42N21 wordings: every RETURN item must
/// carry a name — an alias, a bare variable, or a map projection over one.
fn check_return_items_aliased(stmt: &Statement, message: &str) -> Result<(), EngineError> {
    fn check_return(rc: &ReturnClause, message: &str) -> Result<(), EngineError> {
        for item in &rc.items {
            if item.all {
                continue;
            }
            let named = item.alias.is_some()
                || matches!(item.expr, Expr::Variable(_))
                || matches!(&item.expr, Expr::MapProjection(mp) if matches!(mp.expr, Expr::Variable(_)));
            if !named {
                return Err(EngineError::semantic(
                    ErrorKind::NoExpressionAlias,
                    message.to_string(),
                ));
            }
        }
        Ok(())
    }
    for arm in union_chain(stmt) {
        for clause in &arm.clauses {
            match clause {
                Clause::Return(rc) => check_return(rc, message)?,
                Clause::Conditional(cc) => {
                    for branch in &cc.branches {
                        check_return_items_aliased(&branch.body, message)?;
                    }
                    if let Some(else_body) = &cc.else_body {
                        check_return_items_aliased(else_body, message)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// 42N3C: a naked `CALL { … }` (no scope clause) cannot directly enclose a
/// brace-wrapped SingleQuery or a `WHEN`/`ELSE` conditional query — live
/// 2026.10 reports "Not possible to enclose { ... } in 'CALL { ... }'. Use
/// 'CALL () { ... }' instead." for both; the scope-clause form accepts them.
fn reject_naked_call_shape(csq: &CallSubqueryClause) -> Result<(), EngineError> {
    if !matches!(csq.scope, CallScope::None) {
        return Ok(());
    }
    let conditional = union_chain(&csq.statement).any(|arm| {
        arm.clauses
            .iter()
            .any(|c| matches!(c, Clause::Conditional(_)))
    });
    // A composite brace arm (`CALL { { … UNION … } UNION { … } }`) arrives
    // through the parser's synthetic arm wrapper
    // (`CALL (*) { <braced arm> } RETURN *`), which resets the wrapper's own
    // `brace_wrapped`; the braced statement is the nested CALL's body.
    let braced = union_chain(&csq.statement).any(|arm| {
        arm.brace_wrapped
            || matches!(
                arm.clauses.as_slice(),
                [Clause::CallSubquery(inner), Clause::Return(_)]
                    if matches!(inner.scope, CallScope::Wildcard)
                        && inner.statement.brace_wrapped
                        && inner.statement.union.is_some()
            )
    });
    if conditional {
        return Err(EngineError::semantic(
            ErrorKind::InvalidClauseComposition,
            "InvalidClauseComposition: 42N3C Not possible to enclose WHEN ... THEN ... in 'CALL { ... }'. Use 'CALL () { ... }' instead.".to_string(),
        ));
    }
    if braced {
        return Err(EngineError::semantic(
            ErrorKind::InvalidClauseComposition,
            "InvalidClauseComposition: 42N3C Not possible to enclose { ... } in 'CALL { ... }'. Use 'CALL () { ... }' instead.".to_string(),
        ));
    }
    Ok(())
}

/// Walk one statement plus its whole `UNION` chain at top level: the one
/// spelling of the chain cursor the naked-CALL arm and both Conditional
/// bodies consult.
fn walk_union_chain(
    stmt: &Statement,
    base_scope: IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for s in union_chain(stmt) {
        walk_top_level(s, base_scope.clone())?;
    }
    Ok(())
}

/// Walk one statement's own clauses (no `UNION` chain — [`walk_union_chain`]
/// owns the cursor over the chain, so no statement is walked twice).
fn walk_top_level(
    stmt: &Statement,
    base_scope: IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut scope = base_scope.clone();
    check_brace_return_items_aliased(stmt)?;
    for clause in &stmt.clauses {
        match clause {
            Clause::CallSubquery(csq) => {
                check_call_return_items_aliased(&csq.statement)?;
                reject_naked_call_shape(csq)?;
                match &csq.scope {
                    CallScope::None => {
                        // Naked call: no new *whole-row visibility* restrictions
                        // from this module (the body still sees the outer row),
                        // but still recurse to catch a nested scope-clause CALL
                        // inside its body.
                        walk_union_chain(&csq.statement, scope.clone())?;
                        // 3.3(f): a naked `CALL { ... }` whose terminal
                        // projection *exports* a name already bound in the outer
                        // scope collides when its row is joined back into the
                        // outer row -- a compile-time error, verbatim from live
                        // Neo4j Cypher 25: `WITH 5 AS x CALL { RETURN 1 AS x }`
                        // raises "Variable `x` already declared in outer scope"
                        // (SyntaxError / VariableAlreadyBound). Only the output
                        // collision is rejected; whole-row visibility of the
                        // outer scope inside the body is unchanged.
                        let exports = call_subquery_exports(csq, &scope);
                        if let Some(name) = smallest_collision(&exports, &scope) {
                            return Err(name.with_str(duplicate_outer_variable));
                        }
                        // Also make its exported variables visible to
                        // later clauses (e.g. a subsequent scope-clause
                        // `CALL (x) { ... }` importing a name this naked
                        // call produced), matching the scoped arm below.
                        scope.extend(exports);
                    }
                    _ => {
                        let exports = resolve_nested_call(csq, &scope, true)?;
                        scope.extend(exports);
                    }
                }
            }
            // CIP-135: recurse into every branch/ELSE body (and their own
            // UNION chains) so a nested scope-clause `CALL (...) { ... }`
            // inside a `WHEN`/`ELSE` body still gets Rule 1/4/5/8/9/10
            // checked. `Clause::Conditional` is always the sole clause of
            // whatever list it's in, so there is no "after" scope to
            // thread back out from it.
            Clause::Conditional(cc) => {
                for branch in &cc.branches {
                    walk_union_chain(&branch.body, scope.clone())?;
                }
                if let Some(else_body) = &cc.else_body {
                    walk_union_chain(else_body, scope.clone())?;
                }
            }
            // A WITH replaces the scope (a projection), so it cannot ride
            // the introduction-only fold: names it drops must stop being
            // referenceable by a later clause (and importable by a later
            // scope-clause CALL).
            Clause::With(wc) => {
                scope = projected_names(&wc.items, &scope);
            }
            // Every other clause's introduced names come from the one
            // accumulation policy (`validate::clause_introduced_names`):
            // MATCH/MERGE/CREATE patterns, UNWIND and LOAD CSV aliases,
            // CALL … YIELD columns and the catalog commands' output
            // columns — no second clause-by-clause vocabulary here.
            other => scope.extend(clause_introduced_names(other)),
        }
    }
    Ok(())
}

/// Lenient (never errors) computation of the variable names a `CALL
/// (...) { ... }` clause exports into the outer row, given the outer scope
/// at that point. Used by scope-accumulation passes elsewhere in
/// `validate.rs` that need to know what a CALL clause makes available
/// afterward; any Rule violation is caught separately by
/// `validate_call_scope_clauses`, which always runs first.
pub(crate) fn call_subquery_exports(
    csq: &CallSubqueryClause,
    outer_scope: &IdentHashSet<SmallName>,
) -> IdentHashSet<SmallName> {
    match &csq.scope {
        // CIP-213: `compute_subquery_output_vars` already walks
        // MATCH/WITH/UNWIND/RETURN/FINISH/CREATE/MERGE/CallSubquery/
        // Conditional *and* recurses through a `.next` chain to its true
        // final stage, so it subsumes the old direct "scan for a
        // Return-or-Conditional clause" loop this used to do -- which
        // never looked past a naked CALL body's *first* stage, silently
        // reporting nothing exported by `CALL { RETURN 1 AS x NEXT RETURN
        // x + 1 AS y }` (it would have found no top-level `Return`/
        // `Conditional` clause at all, since the real one is one NEXT
        // stage deeper).
        CallScope::None => compute_subquery_output_vars(&csq.statement).unwrap_or_default(),
        _ => resolve_nested_call(csq, outer_scope, false).unwrap_or_default(),
    }
}

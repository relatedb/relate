//! Cross-clause variable scope and usage-type tracking.
//!
//! [`validate_variable_types`] detects variable-reuse type conflicts at
//! compile time by tracking variable usage types (Node, Relationship, Path)
//! across clauses. A clash involving a path is raised as `SyntaxError:
//! VariableAlreadyBound`; any other clash (node↔relationship, scalar↔node)
//! as `SyntaxError: VariableTypeConflict`, matching the openCypher TCK.
//! Cypher requires that a variable is used consistently: a relationship
//! variable cannot also appear as a node, a path variable cannot appear as a
//! node, and a scalar value cannot be used as a node or relationship in a
//! MATCH pattern.
//!
//! The subquery output-variable analysis this builds on lives in
//! [`super::output_vars`]; `call_scope` builds its CALL-subquery
//! scope-clause checks on it.

use crate::ident_hash::{IdentBuildHasher, IdentHashMap, IdentHashSet};
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{
    CallProcedureClause, CallSubqueryClause, Clause, CreateClause, Expr, ForeachClause,
    LiteralValue, MatchClause, MergeClause, PatternElement, PatternPath, ReturnItem, Statement,
    UnwindClause, WithClause, YieldMode,
};

use super::self_reference::validate_create_self_reference;
use super::writes::{
    declare_new_name, validate_create_bindings, validate_dynamic_label_no_any,
    validate_dynamic_label_scope, validate_merge_bindings,
};
use crate::ast::collect::{bind_pattern_vars, effective_name, projected_names, search_score_var};
use crate::call_scope::duplicate_outer_variable;
use crate::output_vars::{compute_subquery_output_vars, compute_subquery_output_vars_seeded};
use crate::show_columns::{catalog_output_names, is_catalog_clause};

/// The TCK-keyed error kind for a type-conflicting reuse: the kind keys on
/// the NEW (later) usage — reused as a path is `VariableAlreadyBound`
/// (Match6 scenarios 21–24); reused as a node/relationship — including
/// reusing a path variable as one — is `VariableTypeConflict` (Match1
/// scenarios 7/8/10, Match2 9/10). Both the cross-clause and the
/// same-clause reuse checks select through this one predicate.
fn reuse_error_kind(new_type: VarType) -> ErrorKind {
    if new_type == VarType::Path {
        ErrorKind::VariableAlreadyBound
    } else {
        ErrorKind::VariableTypeConflict
    }
}

/// The type of a variable as determined by its usage context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarType {
    Node,
    Relationship,
    Path,
    /// A value projected through WITH that is not a simple variable pass-through.
    /// e.g., `WITH true AS n` or `WITH 123 AS n`
    Scalar,
}

impl VarType {
    fn name(&self) -> &'static str {
        match self {
            VarType::Node => "node",
            VarType::Relationship => "relationship",
            VarType::Path => "path",
            VarType::Scalar => "scalar value",
        }
    }
}
/// Validate a statement for variable type conflicts and VariableAlreadyBound errors.
///
/// Returns `Ok(())` if valid, or `Err(..)` with a `VariableTypeConflict` /
/// `VariableAlreadyBound` kind. Both the cross-clause and within-pattern checks
/// key the kind on the *reuse* (later) usage: reused as a path is
/// `VariableAlreadyBound`, reused as a node/relationship is
/// `VariableTypeConflict` (a path's element variables are treated as bound
/// before the path variable — see the ordering note in the collection loop).
pub fn validate_variable_types(stmt: &Statement) -> Result<(), EngineError> {
    let mut var_types: IdentHashMap<SmallName, VarType> = IdentHashMap::default();
    // Track all variables that are "bound" (defined by MATCH/CREATE/UNWIND/WITH)
    let mut bound_vars: IdentHashSet<SmallName> = IdentHashSet::default();
    // At the top level nothing is shadowable: every bound name is declared in
    // the statement's own scope, so a CREATE/MERGE that re-declares one is
    // `VariableAlreadyBound`.
    let shadowable: IdentHashSet<SmallName> = IdentHashSet::default();
    validate_variable_types_in_clauses(
        &stmt.clauses,
        &mut var_types,
        &mut bound_vars,
        &shadowable,
        stmt.cypher_version,
    )
}

/// The clause-list body of [`validate_variable_types`], factored out so
/// `FOREACH` bodies — which share the enclosing scope, unlike `CALL { }`
/// subqueries — are validated against the enclosing variable state instead
/// of skipping every check (LABS-81: a `FOREACH` body used to lower even
/// when the same clause was rejected at top level).
///
/// `shadowable` names the variables a CREATE/MERGE declaration may *shadow*
/// instead of colliding with: the names every enclosing scope bound. A
/// `FOREACH` body starts a new scope holding only its loop variable, so the
/// enclosing names become shadowable there (Neo4j `ForeachAcceptance`
/// "Shadow outer variable in CREATE/MERGE"), while the loop variable itself
/// — declared in the body's own scope — stays non-shadowable (the same
/// suite's "Variable already defined in CREATE/MERGE").
fn validate_variable_types_in_clauses(
    clauses: &[Clause],
    var_types: &mut IdentHashMap<SmallName, VarType>,
    bound_vars: &mut IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
    cypher_version: Option<u32>,
) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            Clause::Match(mc) => apply_match(mc, var_types, bound_vars)?,
            Clause::With(wc) => apply_with(wc, var_types, bound_vars),
            Clause::Unwind(uw) => apply_unwind(uw, bound_vars, shadowable)?,
            Clause::LoadCSV(lc) => {
                bound_vars.insert(lc.alias.clone());
            }
            Clause::Create(cc) => apply_create(clause, cc, bound_vars, shadowable, cypher_version)?,
            Clause::Merge(mc) => apply_merge(mc, bound_vars, shadowable)?,
            Clause::CallProcedure(cp) => apply_call_procedure(cp, clauses, bound_vars)?,
            Clause::CallSubquery(csq) => apply_call_subquery(csq, bound_vars)?,
            // A SHOW/TERMINATE clause's output columns are new bindings,
            // exactly like a `CALL proc() YIELD ...`'s (see the
            // `apply_call_procedure` policy, which this mirrors): a
            // name that's already bound by an earlier clause in this same
            // statement (CIP-242 errors.feature [5]/[6]: a preceding
            // `MATCH`'s pattern variable colliding with a plain or
            // `AS`-aliased YIELD column) is a compile-time
            // VariableAlreadyBound error, not a silent shadow.
            c if is_catalog_clause(c) => {
                check_yield_names_not_bound(catalog_output_names(c), bound_vars)?;
            }
            // A FOREACH body shares the enclosing scope (it is not an
            // isolated subquery): the body policy recurses with a cloned
            // state (see `apply_foreach`).
            Clause::Foreach(f) => apply_foreach(f, var_types, bound_vars, cypher_version)?,
            _ => {}
        }
    }

    Ok(())
}

/// The MATCH policy: every variable usage in the clause's patterns is
/// type-checked, then the pattern's names (and a SEARCH score variable)
/// become bound.
fn apply_match(
    mc: &MatchClause,
    var_types: &mut IdentHashMap<SmallName, VarType>,
    bound_vars: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    validate_match_patterns(&mc.pattern, var_types)?;
    bind_pattern_vars(&mc.pattern, bound_vars);
    if let Some(sv) = search_score_var(mc) {
        bound_vars.insert(sv.clone());
    }
    Ok(())
}

/// The WITH policy: the projection resets the scope. Variables projected
/// through WITH get their types from the expression. If the expression is
/// a literal (not a variable reference), the resulting variable is
/// Scalar; a variable reference passes its type through; anything else
/// constrains nothing.
fn apply_with(
    wc: &WithClause,
    var_types: &mut IdentHashMap<SmallName, VarType>,
    bound_vars: &mut IdentHashSet<SmallName>,
) {
    // Pre-sized (ADR-0505 §4): a `*` passes every existing
    // typed variable through and each other item contributes
    // at most one alias, so this is an upper bound.
    let mut new_types: IdentHashMap<SmallName, VarType> =
        IdentHashMap::with_capacity_and_hasher(var_types.len() + wc.items.len(), IdentBuildHasher);
    for item in &wc.items {
        if item.all {
            // WITH * passes through all existing variables with their types
            for (k, v) in var_types.iter() {
                new_types.insert(k.clone(), *v);
            }
            continue;
        }
        let Some(alias) = effective_name(item) else {
            continue;
        };

        // Determine the type of the projected expression
        match &item.expr {
            Expr::Variable(v) => {
                // Pass through the existing type
                if let Some(t) = var_types.get(v) {
                    new_types.insert(alias, *t);
                }
                // If unknown, don't constrain it
            }
            // Null literal doesn't constrain the variable type (null is compatible with any type)
            Expr::Literal(LiteralValue::Null) => {}
            // Known scalar types: literals, list literals, map literals
            // These definitely can't be nodes or relationships
            Expr::Literal(_) | Expr::List(_) | Expr::Map(_) => {
                new_types.insert(alias, VarType::Scalar);
            }
            // The literal a plan-cache placeholder replaced.
            // Null still constrains nothing.
            Expr::Parameter(name) => {
                if matches!(
                    relate_core::auto_literal(name),
                    Some(value) if !value.is_null()
                ) {
                    new_types.insert(alias, VarType::Scalar);
                }
            }
            // For function calls and other complex expressions,
            // we can't determine the type at compile time
            // (e.g., coalesce(b, c) could return a node)
            _ => {}
        }
    }
    *var_types = new_types;
    // The bound-name set comes from the canonical WITH/RETURN
    // projection rule — the same skeleton every other scope
    // walk uses (a `*` passes the incoming names through, an
    // item contributes its effective name) — so the name set
    // and the type map built above can never disagree on what
    // the clause projected.
    *bound_vars = projected_names(&wc.items, bound_vars);
}

/// The UNWIND policy: a binding clause introduces a NEW variable.
fn apply_unwind(
    uw: &UnwindClause,
    bound_vars: &mut IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // A binding clause introduces a NEW variable: re-using an
    // in-scope name is 42N59 ("variable already defined"), not a
    // silent shadow (`WITH 1 AS x UNWIND [1] AS x`,
    // AggregationAcceptance "Should not duplicate errors"). Only
    // a name a FOREACH body may shadow is exempt.
    if bound_vars.contains(&uw.alias) && !shadowable.contains(&uw.alias) {
        return Err(uw.alias.with_str(EngineError::duplicate_variable));
    }
    bound_vars.insert(uw.alias.clone());
    Ok(())
}

/// The CREATE policy: the binding, dynamic-label and 42I58 self-reference
/// gates, then the pattern's names become bound. `clause` re-arms the
/// one-clause slice the self-reference battery takes (its API is
/// clause-list-shaped for the conservative tier's whole-list spans).
fn apply_create(
    clause: &Clause,
    cc: &CreateClause,
    bound_vars: &mut IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
    cypher_version: Option<u32>,
) -> Result<(), EngineError> {
    // Check for VariableAlreadyBound: if a variable used in CREATE
    // for a node is already bound (from MATCH/previous CREATE), error
    validate_create_bindings(&cc.pattern, bound_vars, shadowable)?;
    // CIP-156: `$any()` is a MATCH-only disjunction quantifier;
    // a dynamic label/type expression can't reference a
    // variable this very CREATE clause creates.
    validate_dynamic_label_no_any(&cc.pattern)?;
    validate_dynamic_label_scope(&cc.pattern, bound_vars)?;
    // 42I58 (Cypher 25): the property values may not read an
    // entity this clause creates. This pass knows the exact
    // enclosing bindings -- and reaches `FOREACH` bodies with
    // them -- so a bare element variable the enclosing scope
    // already binds is a reference, not a declaration. The rule
    // owns its own Cypher-5 gate.
    validate_create_self_reference(std::slice::from_ref(clause), bound_vars, cypher_version)?;
    // Track new variables created by CREATE
    bind_pattern_vars(&cc.pattern, bound_vars);
    Ok(())
}

/// The MERGE policy: the same binding and dynamic-label gates as CREATE
/// (see `apply_create`), then the pattern's names become bound.
fn apply_merge(
    mc: &MergeClause,
    bound_vars: &mut IdentHashSet<SmallName>,
    shadowable: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check for VariableAlreadyBound in MERGE: node/rel variables that
    // are already bound should not be redefined
    validate_merge_bindings(&mc.pattern, bound_vars, shadowable)?;
    // CIP-156: same rules as CREATE (see above).
    let merge_pattern = std::slice::from_ref(&mc.pattern);
    validate_dynamic_label_no_any(merge_pattern)?;
    validate_dynamic_label_scope(merge_pattern, bound_vars)?;
    // Track new variables from MERGE
    bind_pattern_vars(merge_pattern, bound_vars);
    Ok(())
}

/// The CALL-procedure policy: `YIELD *` is in-query-context only, and the
/// yielded names (alias-resolved, decoded once) follow the one
/// yield-binding policy.
fn apply_call_procedure(
    cp: &CallProcedureClause,
    clauses: &[Clause],
    bound_vars: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check for YIELD * in in-query context (not standalone)
    // In-query = there are other clauses besides this CALL
    let is_in_query = clauses.len() > 1;
    if cp.yields.mode() == YieldMode::Star && is_in_query {
        return Err(EngineError::semantic(
            ErrorKind::UnexpectedSyntax,
            "YIELD * is not allowed in in-query CALL".to_string(),
        ));
    }

    // The effective variable names (alias takes precedence) come
    // from the one YieldList model, decoded once.
    let effective_names: Vec<SmallName> = cp.yields.effective_names().collect();

    // YIELD variables shadow already-bound names, and a
    // duplicate alias is a collision too — one policy, shared
    // with the SHOW family (`check_yield_names_not_bound`).
    check_yield_names_not_bound(effective_names, bound_vars)
}

/// The CALL-subquery policy: the legacy-importing collision check, then
/// the subquery's exported names (canonical lenient derivation) and an
/// `IN TRANSACTIONS … REPORT STATUS` variable become bound.
fn apply_call_subquery(
    csq: &CallSubqueryClause,
    bound_vars: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    validate_legacy_import_collision(csq, bound_vars)?;

    // Bind the names the subquery exports through the canonical
    // lenient derivation (the same one the expression validator's
    // CALL arm and the SEARCH scope walk use).
    let exports = crate::call_scope::call_subquery_exports(csq, bound_vars);
    bound_vars.extend(exports);

    // CIP-30 syntax [7]: `REPORT STATUS AS <var>` must bind a
    // NEW variable name -- one not already in scope when the
    // CALL clause is reached.
    if let Some(cit) = &csq.in_transactions {
        if let Some(var) = &cit.report_status {
            declare_new_name(var, |name| bound_vars.contains(name), |_| false, false)?;
            bound_vars.insert(var.clone());
        }
    }
    Ok(())
}

/// The FOREACH policy: the body shares the enclosing scope (it is not an
/// isolated subquery), so the body clauses are validated against the
/// enclosing bindings plus the loop variable. Bindings the body creates
/// do not escape (FOREACH exports nothing), so the body is walked with a
/// cloned state.
fn apply_foreach(
    f: &ForeachClause,
    var_types: &mut IdentHashMap<SmallName, VarType>,
    bound_vars: &mut IdentHashSet<SmallName>,
    cypher_version: Option<u32>,
) -> Result<(), EngineError> {
    let mut body_types = var_types.clone();
    let mut body_bound = bound_vars.clone();
    body_bound.insert(f.variable.clone());
    // The body is a fresh scope: its own declarations (the loop
    // variable and whatever the body creates) are non-shadowable,
    // but every enclosing name is. The loop variable is declared
    // in the body's own scope even when an enclosing scope bound
    // the same name (`MATCH p = (n) FOREACH (n IN … | CREATE (n))`
    // is `VariableAlreadyBound`), so it is excluded.
    let mut body_shadowable = body_bound.clone();
    body_shadowable.remove(&f.variable);
    validate_variable_types_in_clauses(
        &f.clauses,
        &mut body_types,
        &mut body_bound,
        &body_shadowable,
        cypher_version,
    )
}

/// The one yield-binding policy of `validate_variable_types`, shared by a
/// `CALL … YIELD` and by every SHOW/TERMINATE arm: `cols` (already
/// alias-resolved) must contain no duplicates and none already in
/// `bound_vars` -- both raise the same `VariableAlreadyBound` error -- then
/// every name is added to `bound_vars`.
fn check_yield_names_not_bound(
    cols: impl IntoIterator<Item = SmallName>,
    bound_vars: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Two passes, one policy: every shadow error against the pre-clause
    // bound set fires before any duplicate error, so a query with both
    // violations always names the shadowed outer variable, never the
    // earlier duplicate. The SHOW family and `CALL … YIELD` share it.
    let cols: Vec<SmallName> = cols.into_iter().collect();
    for name in &cols {
        declare_new_name(name, |name| bound_vars.contains(name), |_| false, false)?;
    }
    let mut seen = IdentHashSet::default();
    for name in &cols {
        if !seen.insert(name.clone()) {
            return Err(name.with_str(EngineError::duplicate_variable));
        }
    }
    bound_vars.extend(cols);
    Ok(())
}
/// Validate match patterns for variable type conflicts.
///
/// Checks each pattern path in a MATCH clause. For each variable:
/// - Node positions => VarType::Node
/// - Relationship positions => VarType::Relationship
/// - Named path variables => VarType::Path
///
/// If a variable already has a different type, it's a conflict.
fn validate_match_patterns(
    patterns: &[PatternPath],
    var_types: &mut IdentHashMap<SmallName, VarType>,
) -> Result<(), EngineError> {
    let usages = collect_pattern_usages(patterns)?;
    check_usage_conflicts(&usages, var_types)?;
    // Commit all new types.
    for usage in usages {
        var_types.insert(usage.name, usage.var_type);
    }
    Ok(())
}

/// One variable usage a MATCH pattern introduces: the name, the type its
/// pattern position gives it, and whether a variable-length relationship
/// bound it (the scalar→varlen rebinding exception
/// [`check_usage_conflicts`] honors).
struct PatternUsage {
    name: SmallName,
    var_type: VarType,
    is_varlen: bool,
}

/// Phase one: every variable usage this MATCH introduces, collected before
/// any type is committed — in binding order, a path's element variables
/// (nodes/relationships) before the path variable that names the whole
/// pattern, and a quantified path's inner elements as group variables
/// (CIP-28; the group-specific reuse rules are enforced on the lowered IR
/// by the planner's quantified-path binding, `GroupVariableReuse` in
/// `relate-planner`). A nested quantified path is the typed
/// `NestedQuantification` error here (the parser rejects it — CIP-28 Rule
/// 4 — but a hand-built AST can still reach this validator, LABS-70).
fn collect_pattern_usages(patterns: &[PatternPath]) -> Result<Vec<PatternUsage>, EngineError> {
    let mut usages = Vec::new();
    for path in patterns {
        // Order matters for the reuse-keyed error kind in
        // `check_usage_conflicts`: a path's element variables are
        // considered bound BEFORE the path variable that names the whole
        // pattern. So in `p = (p)-[]-()` the node `p` is the original
        // binding and the path variable `p` is the reuse
        // (VariableAlreadyBound), while in `p = ()-[]-(), (p)` the path
        // variable is the original and the later node `(p)` is the reuse
        // (VariableTypeConflict) — matching clauses/match/Match1 scenario
        // 10 and Match6 scenario 23. Hence the path-variable push is
        // deferred to after this path's elements.
        for elem in &path.elements {
            collect_element_usages(elem, false, &mut usages)?;
        }

        // Path variable binds after its elements (see the ordering note above).
        // A named parenthesized subpath binds the same path value, so it is a
        // path usage too.
        for name in path.path_names() {
            usages.push(PatternUsage {
                name: name.clone(),
                var_type: VarType::Path,
                is_varlen: false,
            });
        }
    }
    Ok(usages)
}

/// The usages one pattern element contributes — the
/// element-to-`(name, type, varlen)` mapping written once for the
/// top-level walk and a quantified path's inner walk alike: a `qpp_inner`
/// relationship is a group variable (CIP-28), never varlen, and a
/// quantified path nested inside another is the typed rejection above.
fn collect_element_usages(
    elem: &PatternElement,
    qpp_inner: bool,
    usages: &mut Vec<PatternUsage>,
) -> Result<(), EngineError> {
    match elem {
        PatternElement::Node(np) => {
            if let Some(var) = &np.variable {
                usages.push(PatternUsage {
                    name: var.clone(),
                    var_type: VarType::Node,
                    is_varlen: false,
                });
            }
            Ok(())
        }
        PatternElement::Relationship(rp) => {
            if let Some(var) = &rp.variable {
                let is_varlen = !qpp_inner && rp.is_variable_length();
                usages.push(PatternUsage {
                    name: var.clone(),
                    var_type: VarType::Relationship,
                    is_varlen,
                });
            }
            Ok(())
        }
        PatternElement::Quantified(_) if qpp_inner => {
            // The parser rejects nested QPPs (CIP-28 Rule 4), but a
            // hand-built AST can still reach this validator — return a
            // typed error, never abort (LABS-70).
            Err(EngineError::semantic(
                ErrorKind::SyntaxError,
                "NestedQuantification: quantified path patterns cannot be nested".to_string(),
            ))
        }
        PatternElement::Quantified(qpp) => {
            for inner in &qpp.elements {
                collect_element_usages(inner, true, usages)?;
            }
            Ok(())
        }
    }
}

/// Phases two and three over the collected usages: the cross-clause check
/// against types earlier clauses established, then the within-MATCH check
/// against earlier usages of the same list — the current usage is the
/// later one (the reuse), so its type keys the error kind in both
/// ([`reuse_error_kind`]).
fn check_usage_conflicts(
    usages: &[PatternUsage],
    var_types: &IdentHashMap<SmallName, VarType>,
) -> Result<(), EngineError> {
    for (i, usage) in usages.iter().enumerate() {
        let PatternUsage {
            name: var,
            var_type: new_type,
            is_varlen,
        } = usage;
        // Check against already-established types from previous clauses
        if let Some(existing_type) = var_types.get(var) {
            if *existing_type != *new_type {
                // A Scalar variable from WITH can be rebound as a Relationship
                // in a variable-length pattern (e.g., WITH [r1, r2] AS rs ... [rs*])
                // because variable-length rel patterns accept list variables.
                let allowed = *existing_type == VarType::Scalar
                    && *new_type == VarType::Relationship
                    && *is_varlen;
                if !allowed {
                    // Cross-clause reuse (Match6 21/22 vs Match1 7/8, Match2 9/10).
                    let kind = reuse_error_kind(*new_type);
                    return Err(EngineError::semantic(
                        kind,
                        format!(
                            "variable '{}' is already used as a {} but is used as a {} here",
                            var,
                            existing_type.name(),
                            new_type.name(),
                        ),
                    ));
                }
            }
        }

        // Check against EARLIER usages within the same MATCH clause: the
        // current usage is the later one (the reuse), so its type keys the
        // error kind — reused as a path is VariableAlreadyBound (Match6 23/24),
        // reused as a node/relationship is VariableTypeConflict (Match1 10).
        for other in &usages[..i] {
            if other.name == *var && other.var_type != *new_type {
                let kind = reuse_error_kind(*new_type);
                return Err(EngineError::semantic(
                    kind,
                    format!(
                        "variable '{}' is used as both a {} and a {} in the same pattern",
                        var,
                        new_type.name(),
                        other.var_type.name(),
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// CIP-141 "legacy importing WITH": `CALL { WITH a, b ... }` (no scope-clause
/// parens) is the pre-CIP-141 idiom for correlating outer variables into a
/// subquery — a bare WITH of plain, unaliased variable references as the
/// subquery's very first clause. When the subquery's terminal projection
/// re-exposes one of those imported names, the CALL block's output column
/// collides with the already-bound outer variable on the join back
/// (CIP-218 NOTES.md "interaction with existing features" [1]-[3]: Neo4j
/// rejects this with "already declared in outer scope"). `WITH *` alone is
/// also a valid legacy importing form — it pulls in every outer variable,
/// same as naming them all explicitly (interaction [3]) — and is the one
/// predicate that picks both the import set and the seeded output
/// computation.
/// The legacy-importing form a subquery's leading WITH takes: a lone `*`
/// item imports every outer variable, a list of plain unaliased variable
/// references imports exactly those, and any other shape is not the
/// legacy form. Classified once, so the imported set and the seeded vs.
/// unseeded output computation below follow one match — not a bool plus an
/// `Option` that silently degrades into "not the legacy form".
enum LegacyImportForm {
    Star,
    Named(IdentHashSet<SmallName>),
}

/// Classify a leading WITH's items into the legacy-importing form above,
/// or `None` when it is not one (an aliased or non-variable item, or an
/// empty list).
fn legacy_import_form(items: &[ReturnItem]) -> Option<LegacyImportForm> {
    if items.len() == 1 && items[0].all {
        return Some(LegacyImportForm::Star);
    }
    let mut named = IdentHashSet::default();
    for item in items {
        match (&item.expr, &item.alias) {
            (Expr::Variable(v), None) if !item.all => {
                named.insert(v.clone());
            }
            _ => return None,
        }
    }
    (!named.is_empty()).then_some(LegacyImportForm::Named(named))
}

fn validate_legacy_import_collision(
    csq: &CallSubqueryClause,
    bound_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let Some(Clause::With(first)) = csq.statement.clauses.first() else {
        return Ok(());
    };
    let Some(form) = legacy_import_form(&first.items) else {
        return Ok(());
    };
    let (imported, exported) = match form {
        LegacyImportForm::Star => (
            bound_vars.clone(),
            compute_subquery_output_vars_seeded(&csq.statement, bound_vars),
        ),
        LegacyImportForm::Named(imported) => {
            (imported, compute_subquery_output_vars(&csq.statement))
        }
    };
    if let Some(exported) = exported {
        if let Some(name) = imported
            .iter()
            .filter(|v| exported.contains(*v) && bound_vars.contains(*v))
            .min()
        {
            // `.min()`, not first-found: the reported name must not follow
            // the process-seeded hash order (ADR-0505 §4).
            return Err(name.with_str(duplicate_outer_variable));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_and_validate(query: &str) -> Result<(), String> {
        let stmt = relate_parser::parse(query).map_err(|e| e.to_string())?;
        validate_variable_types(&stmt).map_err(|e| e.to_string())
    }
    // 42N59: a binding clause introduces a new name, it never silently
    // shadows one (`WITH 1 AS x UNWIND [1] AS x`,
    // AggregationAcceptance "Should not duplicate errors"). A WITH/RETURN
    // projection may still re-alias a name.
    #[test]
    fn unwind_rebinding_an_in_scope_variable_is_rejected() {
        for query in [
            "WITH 1 AS x UNWIND [1] AS x RETURN x",
            "UNWIND [1] AS x UNWIND [2] AS x RETURN x",
        ] {
            let err = parse_and_validate(query).unwrap_err();
            assert!(err.contains("VariableAlreadyBound"), "{query}: {err}");
        }
        for query in [
            "WITH 1 AS x WITH 2 AS x RETURN x",
            "WITH 1 AS x RETURN x AS x",
            "UNWIND [1] AS x UNWIND [2] AS y RETURN x, y",
        ] {
            assert!(parse_and_validate(query).is_ok(), "{query}");
        }
    }

    /// A `*` item in a WITH passes the incoming scope through *and keeps
    /// the names the same projection introduced before it* — the canonical
    /// `projected_names` rule (the ad-hoc reset-on-star loop this replaced
    /// dropped them, leaving the name set behind the type map). Pinned
    /// through 42N59: `a` is still in scope after `WITH n AS a, *`, so
    /// rebinding it is rejected.
    #[test]
    fn with_star_keeps_names_projected_before_it() {
        let err = parse_and_validate("WITH 1 AS n WITH n AS a, * UNWIND [1] AS a RETURN a")
            .expect_err("a name projected before a `*` item stays in scope");
        assert!(err.contains("VariableAlreadyBound"), "{err}");
    }

    #[test]
    fn test_rel_var_used_as_node_same_pattern() {
        // node↔relationship clash (no path) is VariableTypeConflict (Match1[7]).
        let result = parse_and_validate("MATCH ()-[r]-() MATCH (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_rel_var_used_as_node_in_same_match() {
        let result = parse_and_validate("MATCH ()-[r]-(r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_path_var_used_as_node() {
        // Cross-clause: reusing a path variable AS A NODE keys on the new
        // (node) usage -> VariableTypeConflict (Match1[8]).
        let result = parse_and_validate("MATCH r = ()-[]-() MATCH (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_path_var_used_as_node_same_pattern() {
        // The path variable `r` binds first, then the later `(r)` node reuses it
        // -> the reuse is a node -> VariableTypeConflict (Match1[10]).
        let result = parse_and_validate("MATCH r = ()-[]-(), (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_node_then_path_same_pattern_is_already_bound() {
        // Node `p` binds first (element), then the path variable `p` reuses it
        // -> the reuse is a path -> VariableAlreadyBound (Match6[23]).
        let result = parse_and_validate("MATCH p = (p)-[]-() RETURN p");
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    #[test]
    fn test_scalar_used_as_node() {
        // scalar↔node clash (no path) is VariableTypeConflict.
        let result = parse_and_validate("WITH true AS n MATCH (n) RETURN n");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_valid_node_reuse() {
        // Same variable used as node in multiple patterns — valid
        let result = parse_and_validate("MATCH (n) MATCH (n)-[]-() RETURN n");
        assert!(result.is_ok(), "Expected valid query: {result:?}");
    }

    #[test]
    fn test_call_subquery_legacy_importing_with_return_star_rebinds_outer_var() {
        // CIP-218 NOTES.md "interaction with existing features" [2]: a bare
        // `WITH a` (no scope-clause parens) imports the outer `a`; `RETURN *`
        // re-exposes it, colliding with the already-bound outer `a`.
        let result = parse_and_validate("WITH 1 AS a CALL { WITH a RETURN * } RETURN a");
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    #[test]
    fn test_call_subquery_legacy_importing_with_star_and_extra_item() {
        // CIP-218 interaction [1]: `WITH *, a AS x` re-selects the imported
        // `a` before the terminal `RETURN *` re-exports it.
        let result = parse_and_validate(
            "WITH 1 AS a, 2 AS b CALL { WITH a WITH *, a AS x RETURN * } RETURN *",
        );
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    /// Several imported names collide: the reported one is the smallest,
    /// never the process-seeded hash order's first (ADR-0505 §4).
    #[test]
    fn test_call_subquery_multiple_collisions_report_the_smallest_name() {
        let error = parse_and_validate(
            "WITH 1 AS e, 2 AS d, 3 AS c, 4 AS b, 5 AS a \
             CALL { WITH e, d, c, b, a RETURN * } RETURN a",
        )
        .expect_err("Expected VariableAlreadyBound");
        assert!(error.contains("Variable `a` already declared"), "{error}");
    }

    #[test]
    fn test_call_subquery_new_variable_name_is_fine() {
        // A CALL {} subquery that returns a *new* name (not colliding with
        // anything bound outside it) must not be rejected.
        let result = parse_and_validate("WITH 1 AS a CALL { WITH a RETURN a AS b } RETURN a, b");
        assert!(result.is_ok(), "Expected valid query: {result:?}");
    }

    #[test]
    fn test_call_subquery_without_importing_with_is_unaffected() {
        // No bare importing WITH at the head of the subquery -- the new
        // check must not fire at all.
        let result = parse_and_validate("CALL { MATCH (n) RETURN n } RETURN n");
        assert!(result.is_ok(), "Expected valid query: {result:?}");
    }

    #[test]
    fn test_call_in_transactions_importing_with_write_only_body_is_valid() {
        // ADR-0210 / LABS-264: the importing `WITH n` is not the terminal
        // projection — a trailing write clause ends the body, which exports
        // nothing, so no collision with the outer `n` can exist. This is
        // the standard bulk-delete idiom.
        for query in [
            "MATCH (n) CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF 10 ROWS",
            "MATCH (n) CALL { WITH n SET n.seen = true } IN TRANSACTIONS",
            "MATCH (n) CALL { WITH n REMOVE n.seen } IN TRANSACTIONS OF 2 ROWS",
            "MATCH (n) CALL { WITH n FOREACH (i IN [1] | SET n.i = i) } IN TRANSACTIONS",
        ] {
            let result = parse_and_validate(query);
            assert!(result.is_ok(), "Expected valid query `{query}`: {result:?}");
        }
    }

    #[test]
    fn test_call_in_transactions_importing_with_reexported_still_rejected() {
        // ADR-0210 keeps the collision rule for bodies whose terminal
        // projection DOES re-export the imported name.
        let result =
            parse_and_validate("MATCH (n) CALL { WITH n RETURN n } IN TRANSACTIONS OF 10 ROWS");
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    #[test]
    fn test_write_then_return_in_subquery_body_still_projects() {
        // A write before the terminal RETURN does not hide the projection:
        // the RETURN still re-exports the imported name and collides.
        let result =
            parse_and_validate("MATCH (n) CALL { WITH n SET n.seen = true RETURN n } RETURN n");
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    #[test]
    fn test_valid_rel_reuse() {
        // Same rel variable in different MATCHes is fine as long as it's always a rel
        let result = parse_and_validate("MATCH ()-[r]-() RETURN r");
        assert!(result.is_ok(), "Expected valid query: {result:?}");
    }

    #[test]
    fn nested_quantified_path_pattern_is_a_typed_error_not_a_panic() {
        // LABS-70: the parser forbids nested QPPs (CIP-28 Rule 4), but a
        // hand-built AST can still reach this validator through the pub
        // entry point — it must return a typed error, never `unreachable!`.
        use relate_parser::{MatchClause, MatchMode, PathQuantifier, QuantifiedPathPattern};
        let nested_qpp = |elements: Vec<PatternElement>| {
            PatternElement::Quantified(QuantifiedPathPattern {
                subpath_variable: None,
                elements,
                where_expr: None,
                quantifier: PathQuantifier { min: 1, max: None },
            })
        };
        let stmt = Statement {
            clauses: vec![Clause::Match(MatchClause {
                optional: false,
                match_mode: MatchMode::DifferentRelationships,
                using_scan: Vec::new(),
                using_index: Vec::new(),
                using_join: Vec::new(),
                pattern: vec![PatternPath {
                    variable: None,
                    subpath_variable: None,
                    elements: vec![nested_qpp(vec![nested_qpp(vec![])])],
                    where_expr: None,
                    mode: None,
                    selector: None,
                    is_shortest_path_function: false,
                }],
                where_expr: None,
                search: None,
            })],
            ..Statement::default()
        };
        let err = validate_variable_types(&stmt).unwrap_err();
        assert!(
            err.to_string().contains("NestedQuantification"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn foreach_bodies_share_enclosing_scope_for_write_checks() {
        // LABS-81: a FOREACH body is not a validation blind spot — the same
        // write-pattern rules as top level apply, against the enclosing
        // scope.
        let result = parse_and_validate("FOREACH (x IN [1] | CREATE (a)-[:R]-(b))");
        assert!(
            result.unwrap_err().contains("RequiresDirectedRelationship"),
            "undirected CREATE in a FOREACH body must be rejected"
        );
        let result = parse_and_validate("CREATE (n) FOREACH (x IN [1] | CREATE (n))");
        assert!(
            result.is_ok(),
            "a FOREACH body may shadow an enclosing-scope variable: {result:?}"
        );
        // Regression: ordinary FOREACH bodies still validate.
        for query in [
            "FOREACH (x IN [1] | CREATE (:L))",
            "MATCH (n) FOREACH (x IN [1] | SET n.p = x)",
            "FOREACH (x IN [1] | FOREACH (y IN [2] | CREATE (:L)))",
        ] {
            let result = parse_and_validate(query);
            assert!(result.is_ok(), "Expected valid query {query}: {result:?}");
        }
    }

    #[test]
    fn foreach_body_shadows_enclosing_names_but_not_its_loop_variable() {
        // Neo4j `ForeachAcceptance` "Shadow outer variable in CREATE/MERGE":
        // the body is a fresh scope, so a CREATE/MERGE declaration may shadow
        // any name an enclosing scope bound — with or without labels and
        // properties.
        for query in [
            "MATCH (n) FOREACH (_ IN [1] | CREATE (n))",
            "MATCH (n) FOREACH (_ IN [1] | CREATE (n)-[:R]->(n))",
            "MATCH ()-[r:R]->() FOREACH (_ IN [1] | CREATE ()-[r:R]->())",
            "MATCH (n) FOREACH (_ IN [1] | MERGE (n {prop: 1}))",
            "MATCH ()-[r:R]->() FOREACH (_ IN [1] | MERGE ()-[r:R {prop: 1}]->())",
            "WITH 1 AS target FOREACH (_ IN [1] | CREATE (target:Lead))",
            "MATCH p = (n) FOREACH (n IN nodes(p) | FOREACH (m IN nodes(p) | CREATE (n)))",
        ] {
            let result = parse_and_validate(query);
            assert!(result.is_ok(), "Expected valid query {query}: {result:?}");
        }
        // The loop variable itself is declared in the body's own scope, so
        // re-declaring it is still `VariableAlreadyBound` (the same suite's
        // "Variable already defined in CREATE/MERGE").
        for query in [
            "MATCH p = () FOREACH (n IN nodes(p) | CREATE (n))",
            "MATCH p = ()-[:R]->() FOREACH (r IN relationships(p) | MERGE ()-[r:R]->())",
        ] {
            let result = parse_and_validate(query);
            assert!(
                result.unwrap_err().contains("VariableAlreadyBound"),
                "re-declaring the FOREACH loop variable must be rejected: {query}"
            );
        }
    }
}

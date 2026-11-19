//! Expression-position rules: pattern predicates in WHERE, patterns in
//! RETURN/WITH projections and SET values, property access and `labels()`/
//! `type()` on statically-known path/node variables, and
//! undefined-variable checks for the expression positions they appear in.
//! The DELETE-target admission policy lives in its own concern module,
//! [`delete`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, PatternElement, Statement};

use super::delete::check_delete_expr;
use super::writes::{walk_write_elements, WriteClause};
use crate::agg::contains_aggregation;
use crate::ast::collect::{
    bind_node_name_refs, check_expr_vars_defined, check_name_defined, clause_introduced_names,
    collect_expr_variables, collect_pattern_element_vars, interp_embeds, projected_names,
    projected_node_scope, search_score_var, union_chain, walk_expr_scoped, ScopedExprVisitor,
};
use crate::ast::tree::{walk_clause_expr_roots, walk_expr};

/// Validate expression-position constraints:
/// - Pattern predicates in WHERE must not introduce new (unbound) variables for relationships
///   or nodes that aren't already in scope.
/// - A single-node pattern predicate like `(n)` is not valid (InvalidArgumentType).
/// - Pattern predicates are legal only in the homes the pattern-position
///   policy names (predicate positions, a comprehension's WHERE filter,
///   the legacy `exists(<pattern>)` argument) and rejected in every value
///   position — a RETURN/WITH projection item, a SET value (the policy
///   lives in [`super::pattern`]).
/// - size() on a pattern predicate is invalid (UnexpectedSyntax, owned by
///   [`super::pattern`]).
/// - Property access and labels() on named path variables are invalid (InvalidArgumentType).
/// - `type()` on a statically-known node variable (a MATCH/CREATE/MERGE
///   pattern node, carried through the clause walk) is invalid
///   (InvalidArgumentType).
/// - DELETE targets are admitted by [`super::delete::check_delete_expr`]'s
///   one stated rule, and MERGE/CREATE patterns reject element-pattern
///   predicates.
pub fn validate_expression_constraints(stmt: &Statement) -> Result<(), EngineError> {
    let mut scope: IdentHashSet<SmallName> = IdentHashSet::default();
    let mut path_vars: IdentHashSet<SmallName> = IdentHashSet::default();
    // The names statically known to be nodes, carried clause by clause
    // through the canonical node-scope facts (`clause_node_names` for the
    // pattern-binding clauses, `projected_node_scope` for a WITH
    // projection) — the set the `type()`-on-node rule reads, checked in
    // this same walk so the WITH projection actually scopes it.
    let mut node_vars: IdentHashSet<SmallName> = IdentHashSet::default();
    validate_expression_constraints_in_clauses(
        &stmt.clauses,
        &mut scope,
        &mut path_vars,
        &mut node_vars,
        None,
    )
}

/// The clause-list walk behind [`validate_expression_constraints`], factored
/// out so a CALL-subquery body and an expression subquery (`COUNT`/`EXISTS`/
/// `COLLECT`) are checked against their own enclosing scope with the same
/// rules — a reference to an outer variable the body does not import is the
/// front end's `UndefinedVariable`, not a silently bound null.
fn validate_expression_constraints_in_clauses(
    clauses: &[Clause],
    scope: &mut IdentHashSet<SmallName>,
    path_vars: &mut IdentHashSet<SmallName>,
    node_vars: &mut IdentHashSet<SmallName>,
    outer_visibility: Option<&IdentHashSet<SmallName>>,
) -> Result<(), EngineError> {
    for clause in clauses {
        // Expression subqueries and expression-form `shortestPath()` read the
        // clause's *input* scope (a WITH/RETURN projects after its items are
        // evaluated); a MATCH's own pattern names are visible to its WHERE,
        // so they join the snapshot for that clause.
        let mut expr_scope = scope.clone();
        if matches!(clause, Clause::Match(_)) {
            expr_scope.extend(clause_introduced_names(clause));
        }
        validate_subquery_expressions(clause, &expr_scope)?;
        match clause {
            Clause::Match(mc) => {
                // This clause's own pattern nodes are in the set for its
                // WHERE check, which runs inside the arm below.
                bind_node_name_refs(clause, node_vars);
                validate_match_clause(mc, scope, path_vars, node_vars)?;
            }
            Clause::With(wc) => {
                validate_with_clause(wc, scope, path_vars, node_vars, outer_visibility)?;
            }
            Clause::Return(rc) => {
                validate_return_clause(rc, scope, path_vars, node_vars, outer_visibility)?;
            }
            Clause::Unwind(uw) => {
                validate_unwind_clause(uw, scope)?;
            }
            Clause::LoadCSV(lc) => {
                validate_loadcsv_clause(lc, scope)?;
            }
            Clause::Create(cc) => {
                validate_create_clause(cc, scope)?;
                // The created node variables are nodes for every later
                // clause's `type()` check.
                bind_node_name_refs(clause, node_vars);
            }
            Clause::Set(sc) => {
                validate_set_clause(sc, scope)?;
            }
            Clause::Remove(rc) => {
                validate_remove_clause(rc, scope)?;
            }
            Clause::Delete(dc) => {
                validate_delete_clause(dc, scope)?;
            }
            Clause::CallProcedure(cp) => {
                validate_callprocedure_clause(cp, scope)?;
            }
            // The row-producing catalog commands, through the one catalog
            // classification (the same guard the scope walkers use): each
            // introduces its effective output columns into scope.
            c if crate::show_columns::is_catalog_clause(c) => {
                scope.extend(clause_introduced_names(c));
            }
            Clause::CallSubquery(csq) => {
                // The body is checked against the scope *before* the CALL's
                // own exports join it.
                validate_call_body(csq, scope)?;
                validate_callsubquery_clause(csq, scope)?;
            }
            Clause::Merge(mc) => {
                validate_merge_clause(mc, scope)?;
                // The merged node variables are nodes for every later
                // clause's `type()` check.
                bind_node_name_refs(clause, node_vars);
            }
            _ => {}
        }
    }
    Ok(())
}

/// The scope-aware expression visitor behind [`validate_subquery_expressions`]:
/// it collects the subquery bodies to recurse into. The binder events carry
/// no rule here, so they are no-ops.
struct SubqueryExprVisitor {
    bodies: Vec<Statement>,
}

impl ScopedExprVisitor for SubqueryExprVisitor {
    fn enter_expr(&mut self, _expr: &Expr) -> Result<(), EngineError> {
        Ok(())
    }

    fn read_name(&mut self, _name: &SmallName) -> Result<(), EngineError> {
        Ok(())
    }

    fn bind_name(&mut self, _name: &SmallName) -> Result<(), EngineError> {
        Ok(())
    }

    fn unbind_name(&mut self) -> Result<(), EngineError> {
        Ok(())
    }

    fn subquery_body(&mut self, statement: &Statement) -> Result<(), EngineError> {
        self.bodies.push(statement.clone());
        Ok(())
    }
}

/// Walk one clause's expression roots with the scope-aware visitor: every
/// subquery body recursed into with the scope at that clause.
fn validate_subquery_expressions(
    clause: &Clause,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut visitor = SubqueryExprVisitor { bodies: Vec::new() };
    let mut roots =
        |expr: &Expr| -> Result<(), EngineError> { walk_expr_scoped(expr, &mut visitor) };
    walk_clause_expr_roots(clause, &mut roots)?;
    for body in visitor.bodies {
        let mut inner_scope = scope.clone();
        let mut path_vars = IdentHashSet::default();
        let mut node_vars = IdentHashSet::default();
        validate_expression_constraints_in_clauses(
            &body.clauses,
            &mut inner_scope,
            &mut path_vars,
            &mut node_vars,
            None,
        )?;
    }
    Ok(())
}

/// The CALL-subquery body's expression rules under live 2026.10's import
/// semantics: a scope-clause body sees exactly its imported names; a naked
/// `CALL { … }` body sees only what each UNION arm's leading importing
/// `WITH` names (nothing at all when the arm has none). Without this, an
/// outer name the body never imported resolved to the outer row and a query
/// live Neo4j rejects (`Variable `x` not defined`) ran silently.
fn validate_call_body(
    csq: &relate_parser::CallSubqueryClause,
    outer_scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    use relate_parser::CallScope;
    for arm in union_chain(&csq.statement) {
        let (mut scope, leak_scope): (IdentHashSet<SmallName>, Option<&IdentHashSet<SmallName>>) =
            match &csq.scope {
                // A scope clause's imports stay visible for the whole body,
                // so a later WITH dropping one is not a
                // leak; the planner's binder owns unimported names there.
                CallScope::Vars(vars) => (vars.iter().cloned().collect(), None),
                CallScope::Wildcard => (outer_scope.clone(), None),
                // The importing WITH is evaluated against the outer row and
                // then replaces the scope; an arm without one imports
                // nothing, and an outer name read past either boundary is
                // the leak this pass must reject.
                CallScope::None => {
                    // A leading run of `USE` clauses routes the body; the
                    // importing WITH after it is the one that names the
                    // imports.
                    let imports = arm
                        .clauses
                        .iter()
                        .find(|clause| !matches!(clause, Clause::Use(_)));
                    let seed = if matches!(imports, Some(Clause::With(_))) {
                        outer_scope.clone()
                    } else {
                        IdentHashSet::default()
                    };
                    (seed, Some(outer_scope))
                }
            };
        let mut stage = arm;
        loop {
            let mut path_vars = IdentHashSet::default();
            let mut node_vars = IdentHashSet::default();
            validate_expression_constraints_in_clauses(
                &stage.clauses,
                &mut scope,
                &mut path_vars,
                &mut node_vars,
                leak_scope,
            )?;
            match &stage.next {
                Some(next) => stage = next,
                None => break,
            }
        }
    }
    Ok(())
}

/// The Match clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_match_clause(
    mc: &relate_parser::MatchClause,
    scope: &mut IdentHashSet<SmallName>,
    path_vars: &mut IdentHashSet<SmallName>,
    node_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Collect bound variables from match pattern
    //
    // The same path variable cannot be assigned to more than one
    // top-level path pattern of a single MATCH clause's
    // comma-separated graph pattern: a path variable is always an
    // unconditional singleton and cannot appear in a later constituent
    // of the conjunction.
    let mut path_vars_this_match: IdentHashSet<SmallName> = IdentHashSet::default();
    for path in &mc.pattern {
        for name in path.path_names() {
            if !path_vars_this_match.insert(name.clone()) {
                return Err(name.with_str(EngineError::duplicate_variable));
            }
            scope.insert(name.clone());
            path_vars.insert(name.clone());
        }
        for elem in &path.elements {
            collect_pattern_element_vars(elem, &mut *scope);
        }
    }
    // A WHERE on either side of SEARCH may reference the
    // score variable -- add it to scope before any WHERE below
    // (element-pattern, path-pattern, or this MATCH's own) is
    // checked.
    if let Some(sv) = search_score_var(mc) {
        scope.insert(sv.clone());
    }
    // Validate element-pattern WHERE clauses, e.g. `(n WHERE
    // n.age > 30)` / `[r WHERE r.p > 0]` (CIP-17/CIP-18). Scope
    // was fully populated above from every path in this MATCH
    // clause before this check runs, so peer element variables
    // are visible -- including forward references to a variable
    // declared later in the same path (per CIP-17/18 5.2.1) --
    // as well as variables from prior clauses.
    for path in &mc.pattern {
        for elem in &path.elements {
            // A `Quantified` element's own per-iteration WHERE
            // (and any element-pattern WHERE inside its operand)
            // is deliberately not scope-checked here: the
            // operand's local group variables are visible only
            // inside it. The planner's quantified-predicate
            // binding (`NonLocalGroupVariableReference`/
            // `UndefinedVariable` in `relate-planner`)
            // checks those on the lowered IR.
            let where_expr = match elem {
                PatternElement::Node(np) => np.where_expr.as_ref(),
                PatternElement::Relationship(rp) => rp.where_expr.as_ref(),
                PatternElement::Quantified(_) => None,
            };
            if let Some(where_expr) = where_expr {
                check_expr_vars_defined(where_expr, scope)?;
                check_property_on_paths(where_expr, path_vars)?;
            }
        }
        // Validate the parenthesized path pattern's own WHERE,
        // e.g. `((n)-[r]->(m) WHERE n.p > m.q)`.
        // It may reference element variables local to the same
        // path, variables from a prior clause, or unconditional
        // singleton variables of the same MATCH -- all already
        // present in `scope` -- but not an undeclared variable.
        if let Some(where_expr) = &path.where_expr {
            check_expr_vars_defined(where_expr, scope)?;
            check_property_on_paths(where_expr, path_vars)?;
        }
    }
    // Validate WHERE expression
    if let Some(where_expr) = &mc.where_expr {
        super::pattern::validate_expr_in_where(where_expr, scope)?;
        // Check for undefined variables in WHERE
        check_expr_vars_defined(where_expr, scope)?;
        // Check for property access on path variables
        check_property_on_paths(where_expr, path_vars)?;
        // `type()` on a statically-known node variable — the nodes bound
        // by this clause's pattern and every earlier clause.
        check_type_on_nodes(where_expr, node_vars)?;
    }
    Ok(())
}

/// The With clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_with_clause(
    wc: &relate_parser::WithClause,
    scope: &mut IdentHashSet<SmallName>,
    path_vars: &mut IdentHashSet<SmallName>,
    node_vars: &mut IdentHashSet<SmallName>,
    outer_visibility: Option<&IdentHashSet<SmallName>>,
) -> Result<(), EngineError> {
    // Check for patterns in WITH projection items
    for item in &wc.items {
        if !item.all {
            super::pattern::check_no_pattern_in_value_position(&item.expr, "WITH projection")?;
            // Inside a CALL-subquery body, an outer name the body never
            // imported must not fall through to the enclosing row.
            reject_unimported_outer_refs(&item.expr, scope, outer_visibility)?;
            // labels() on a named-path variable (Graph3 [8]).
            check_labels_on_paths(&item.expr, path_vars)?;
            // `type()` on a statically-known node variable — the node set
            // this clause received, before the projection replaces it.
            check_type_on_nodes(&item.expr, node_vars)?;
        }
    }
    // WITH WHERE can have pattern predicates
    if let Some(where_expr) = &wc.where_expr {
        super::pattern::validate_expr_in_where(where_expr, scope)?;
    }
    // Reset scope to the projected names through the one WITH/RETURN
    // projection rule (`projected_names` also keeps names projected before
    // a `*` item, unlike an ad-hoc reset-on-star loop).
    *scope = projected_names(&wc.items, scope);
    // The node set projects through the same rule's node-safe twin: a
    // bare node variable keeps node-ness under its alias, a `*` passes
    // the set through, anything else stops being a node.
    *node_vars = projected_node_scope(&wc.items, node_vars);
    Ok(())
}

/// The Return clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_return_clause(
    rc: &relate_parser::ReturnClause,
    scope: &mut IdentHashSet<SmallName>,
    path_vars: &mut IdentHashSet<SmallName>,
    node_vars: &IdentHashSet<SmallName>,
    outer_visibility: Option<&IdentHashSet<SmallName>>,
) -> Result<(), EngineError> {
    // Check for patterns in RETURN projection items
    for item in &rc.items {
        if !item.all {
            super::pattern::check_no_pattern_in_value_position(&item.expr, "RETURN projection")?;
            // Inside a CALL-subquery body, an outer name the body never
            // imported must not fall through to the enclosing row.
            reject_unimported_outer_refs(&item.expr, scope, outer_visibility)?;
            // labels() on a named-path variable (Graph3 [8]).
            check_labels_on_paths(&item.expr, path_vars)?;
            // `type()` on a statically-known node variable.
            check_type_on_nodes(&item.expr, node_vars)?;
        }
    }
    Ok(())
}

/// The Unwind clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_unwind_clause(
    uw: &relate_parser::UnwindClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    scope.insert(uw.alias.clone());
    Ok(())
}

/// The LoadCSV clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_loadcsv_clause(
    lc: &relate_parser::LoadCSVClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    scope.insert(lc.alias.clone());
    Ok(())
}

/// The Create clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_create_clause(
    cc: &relate_parser::CreateClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for path in &cc.pattern {
        if let Some(pv) = &path.variable {
            scope.insert(pv.clone());
        }
        walk_write_elements(&path.elements, WriteClause::Create, scope)?;
    }
    Ok(())
}

/// The Set clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_set_clause(
    sc: &relate_parser::SetClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // The one SET-item rule, shared with MERGE's ON CREATE/ON MATCH SET.
    check_set_items(&sc.items, scope)
}

/// The Delete clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_delete_clause(
    dc: &relate_parser::DeleteClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Check undefined variables and invalid argument types in DELETE
    for expr in &dc.expressions {
        // Check that DELETE expressions are variables, property access, or subscripts
        // Arithmetic expressions like `1 + 1` are not valid
        check_delete_expr(expr)?;
        // Check for undefined root variable in DELETE expression
        if let Some(root_var) = find_root_variable(expr) {
            check_name_defined(&root_var, scope)?;
        }
    }
    Ok(())
}

/// The CallProcedure clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_callprocedure_clause(
    cp: &relate_parser::CallProcedureClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // Aggregation functions are not allowed in procedure arguments
    for arg in &cp.args {
        if contains_aggregation(arg) {
            return Err(EngineError::semantic(
                ErrorKind::InvalidAggregation,
                "aggregation functions are not allowed in procedure call arguments".to_string(),
            ));
        }
    }
    // Add yielded variables to scope (the effective post-alias
    // name comes from the one YieldList model)
    for name in cp.yields.effective_names() {
        scope.insert(name.clone());
    }
    Ok(())
}

/// The CallSubquery clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_callsubquery_clause(
    csq: &relate_parser::CallSubqueryClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // CIP-141/CIP-218: what a CALL clause exports depends on
    // its scope clause (a scope-clause `RETURN *` excludes
    // imported names per the star exclusion); delegate to the shared
    // analysis rather than assuming `RETURN *` always inherits
    // outer scope (only true for a naked `CALL { ... }`).
    let exports = crate::call_scope::call_subquery_exports(csq, scope);
    scope.extend(exports);
    // `REPORT STATUS AS <var>` binds `<var>` into scope
    // for every clause after the CALL (newness of the name
    // itself is checked separately by `validate_variable_types`).
    if let Some(cit) = &csq.in_transactions {
        if let Some(var) = &cit.report_status {
            scope.insert(var.clone());
        }
        // A `DISJOINT BY` key partitions the batch's rows by
        // value, which only holds when the key is deterministic per row —
        // `randomUUID()` would put every row in its own partition (42N7B).
        if let Some(relate_parser::DisjointBy::Expressions(keys)) = &cit.disjoint {
            if keys
                .iter()
                .any(super::aggregation::contains_non_deterministic)
            {
                return Err(EngineError::semantic(
                    ErrorKind::NonDeterministicDisjointBy,
                    "'DISJOINT BY' expressions must be deterministic",
                ));
            }
        }
    }
    Ok(())
}

/// The Merge clause's expression constraints: the body of the clause
/// dispatch above, over the shared scope and path-variable sets.
fn validate_merge_clause(
    mc: &relate_parser::MergeClause,
    scope: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    reject_merge_subquery_expressions(mc)?;
    // Add MERGE pattern variables to scope
    if let Some(pv) = &mc.pattern.variable {
        scope.insert(pv.clone());
    }
    walk_write_elements(&mc.pattern.elements, WriteClause::Merge, scope)?;
    // The ON CREATE SET / ON MATCH SET checks are one rule
    // over two item lists (the clause's own two fields).
    check_set_items(&mc.on_create, scope)?;
    check_set_items(&mc.on_match, scope)?;
    Ok(())
}

/// 42001: a MERGE clause admits no subquery expression — neither in a
/// pattern element's property map nor in an `ON CREATE`/`ON MATCH SET` value
/// (live 2026.10: "Subquery expressions are not allowed in a MERGE clause.").
/// One walk over both value positions, so the rule cannot drift between them.
fn reject_merge_subquery_expressions(mc: &relate_parser::MergeClause) -> Result<(), EngineError> {
    // A pattern comprehension is a legal property value inside a MERGE
    // pattern (live 2026.10 evaluates it before the merge), but not inside
    // an ON CREATE/ON MATCH SET value; an EXISTS/COUNT/COLLECT subquery is
    // rejected in both positions.
    let check = |expr: &Expr, set_value: bool| -> Result<(), EngineError> {
        let mut found = false;
        walk_expr(expr, &mut |node| {
            found |= matches!(
                node,
                Expr::ExistsSubquery(_) | Expr::CountSubquery(_) | Expr::CollectSubquery(_)
            ) || (set_value && matches!(node, Expr::PatternComprehension(_)));
            Ok(())
        })?;
        if found {
            return Err(EngineError::semantic(
                ErrorKind::UnexpectedSyntax,
                "Subquery expressions are not allowed in a MERGE clause.".to_string(),
            ));
        }
        Ok(())
    };
    for elem in &mc.pattern.elements {
        let (properties, properties_param) = match elem {
            PatternElement::Node(np) => (&np.properties, np.properties_param.as_ref()),
            PatternElement::Relationship(rp) => (&rp.properties, rp.properties_param.as_ref()),
            PatternElement::Quantified(_) => continue,
        };
        for (_, value) in properties {
            check(value, false)?;
        }
        if let Some(param) = properties_param {
            check(param, false)?;
        }
    }
    for item in mc.on_create.iter().chain(&mc.on_match) {
        if let Some(value) = &item.value {
            check(value, true)?;
        }
    }
    Ok(())
}

/// The one SET-item rule, shared by a `SET` clause and MERGE's ON CREATE
/// SET / ON MATCH SET: no pattern predicate in a value, the value
/// expression's variables must be bound, and the item's target — a
/// property access's variable or a bare variable — must be too.
fn check_set_items(
    items: &[relate_parser::SetItem],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for item in items {
        if let Some(val) = &item.value {
            super::pattern::check_no_pattern_in_value_position(val, "SET values")?;
            check_expr_vars_defined(val, scope)?;
        }
        check_dynamic_label_expressions(&item.labels, scope)?;
        if let Some(prop) = &item.property {
            if let Expr::Variable(var_name) = &prop.expr {
                check_name_defined(var_name, scope)?;
            }
        } else if let Some(var_name) = &item.variable {
            check_name_defined(var_name, scope)?;
        }
    }
    Ok(())
}

/// The CALL-subquery import boundary: a name the enclosing statement bound
/// but the body never imported is `UndefinedVariable`, not a silent read of
/// the enclosing row (live 2026.10: a naked `CALL { … }` body sees only what
/// its leading importing `WITH` names). A name bound *nowhere* is left to
/// the planner's binder, which owns its own diagnostic.
fn reject_unimported_outer_refs(
    expr: &Expr,
    scope: &IdentHashSet<SmallName>,
    outer_visibility: Option<&IdentHashSet<SmallName>>,
) -> Result<(), EngineError> {
    let Some(outer) = outer_visibility else {
        return Ok(());
    };
    let mut vars = IdentHashSet::default();
    collect_expr_variables(expr, &mut vars);
    let mut leaked: Vec<&SmallName> = vars
        .iter()
        .filter(|name| !scope.contains(*name) && outer.contains(*name))
        .collect();
    leaked.sort();
    match leaked.first() {
        Some(name) => Err(name.with_str(EngineError::undefined_variable)),
        None => Ok(()),
    }
}

/// A REMOVE item's own rules: its dynamic label/type expressions and its
/// property root must be bound (the SET policy's mirror for `REMOVE n:$(x)`).
fn validate_remove_clause(
    rc: &relate_parser::RemoveClause,
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for item in &rc.items {
        check_dynamic_label_expressions(&item.labels, scope)?;
        if let Some(prop) = &item.property {
            if let Expr::Variable(var_name) = &prop.expr {
                check_name_defined(var_name, scope)?;
            }
        } else if let Some(var_name) = &item.variable {
            check_name_defined(var_name, scope)?;
        }
    }
    Ok(())
}

/// The CIP-162 dynamic label/type atom (`SET n:$(expr)`) resolves its
/// expression against the clause's scope: an unbound name is the front
/// end's `UndefinedVariable` (live 2026.10: "Variable `expr` not defined").
fn check_dynamic_label_expressions(
    labels: &[relate_parser::LabelExpr],
    scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for label in labels {
        if let relate_parser::LabelExpr::Dynamic(dynamic) = label {
            check_expr_vars_defined(&dynamic.expr, scope)?;
        }
    }
    Ok(())
}

/// Check for property access on path variables.
fn check_property_on_paths(
    expr: &Expr,
    path_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    walk_expr(expr, &mut |e| {
        if let Expr::Property(p) = e {
            if let Expr::Variable(v) = &p.expr {
                if path_vars.contains(v) {
                    return Err(EngineError::semantic(
                        ErrorKind::InvalidArgumentType,
                        "property access is not allowed on paths".to_string(),
                    ));
                }
            }
        }
        Ok(())
    })
}

/// Reject a single-argument builtin `name` whose sole argument is directly a
/// variable drawn from `vars` — the one shape both statically-certain
/// builtin/type mismatches share ([`check_labels_on_paths`]'s `labels()` on a
/// path, [`check_type_on_nodes`]'s `type()` on a node). Relate has no
/// general type inferencer, so the check is deliberately scoped to this
/// case; anything else (a variable that might be fine, a property access, …)
/// is left to the runtime type check.
fn reject_builtin_on_var_set(
    expr: &Expr,
    name: &str,
    in_set: &impl Fn(&SmallName) -> bool,
    message: &str,
) -> Result<(), EngineError> {
    walk_expr(expr, &mut |e| {
        if let Expr::FunctionCall(fc) = e {
            if fc.name.with_str(|n| n.eq_ignore_ascii_case(name)) && fc.args.len() == 1 {
                if let Expr::Variable(v) = &fc.args[0] {
                    if in_set(v) {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidArgumentType,
                            message.to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    })
}

/// `labels()` applied to a named-path variable is a compile-time
/// `InvalidArgumentType` (TCK expressions/graph/Graph3 [8]: `MATCH p = (a)
/// RETURN labels(p)` — `p` is statically a PATH, and `labels()` requires a
/// node).
fn check_labels_on_paths(
    expr: &Expr,
    path_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    reject_builtin_on_var_set(
        expr,
        "labels",
        &|v: &SmallName| path_vars.contains(v),
        "labels() requires a node, got PATH",
    )
}

/// Find the root variable name from an expression (traversing property access and subscripts).
fn find_root_variable(expr: &Expr) -> Option<SmallName> {
    match expr {
        Expr::Variable(v) => Some(v.clone()),
        Expr::Property(p) => find_root_variable(&p.expr),
        Expr::Subscript(s) => find_root_variable(&s.expr),
        Expr::Paren(inner) => find_root_variable(inner),
        Expr::StringInterp(parts) => interp_embeds(parts).find_map(find_root_variable),
        _ => None,
    }
}

/// `type()` on a statically-known node variable is a compile-time
/// `InvalidArgumentType`. The node set is carried clause by clause through
/// the canonical node-scope facts in [`validate_expression_constraints`]'s
/// walk (`clause_node_names`/`projected_node_scope`), so the check runs in
/// the same pass as the expression-scope checks around it and a WITH
/// projection actually scopes it.
fn check_type_on_nodes(
    expr: &Expr,
    node_vars: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    // `type(<node variable>)` is illegal anywhere (type() is for
    // relationships).
    reject_builtin_on_var_set(
        expr,
        "type",
        &|v: &SmallName| node_vars.contains(v),
        "type() can only be applied to relationships, not nodes",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::parse_and_validate_expression_constraints;

    /// `type()` on a statically-known node variable is rejected in the
    /// clause walk, with node-ness carried through the canonical
    /// node-scope facts: a WITH projection keeps a bare node variable's
    /// node-ness under its alias, CREATE/MERGE pattern nodes are nodes for
    /// later clauses, and a relationship variable never enters the node
    /// set — so `type()` on it stays legal.
    #[test]
    fn type_on_nodes_is_scoped_through_the_clause_walk() {
        // Node-ness projects through the alias: `m` is `n`, a node.
        let error =
            parse_and_validate_expression_constraints("MATCH (n) WITH n AS m RETURN type(m)")
                .expect_err("type() on a node projected through a WITH alias is rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
        // A relationship keeps type() legal — it never joins the node set.
        parse_and_validate_expression_constraints("MATCH (n)-[r]->() WITH r RETURN type(r)")
            .expect("type() on a relationship stays legal after a WITH");
        // A created node variable is a node for the RETURN's check.
        let error = parse_and_validate_expression_constraints("CREATE (n) RETURN type(n)")
            .expect_err("type() on a CREATE-bound node variable is rejected");
        assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
        // A node dropped by the projection is out of node scope, so this
        // pass raises no type() rejection for it (the undefined-name
        // report is the scope battery's, not this walk's) — the node set
        // is a projection, not a history of every name seen.
        parse_and_validate_expression_constraints("MATCH (n) WITH 1 AS x RETURN type(n)")
            .expect("a node dropped by a WITH projection is out of node scope");
    }
}

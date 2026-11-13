//! Clause-composition structure rules: USE placement (CIP-202), NEXT-chain
//! admin-command restrictions (CIP-213), UNION column shapes (CIP-140/146),
//! duplicate WITH/RETURN column names, relationship-variable uniqueness
//! in MATCH patterns, and the traversal-depth admission limit (LABS-610).
//! The CIP-224 SEARCH-clause rules live in [`super::search`]; the opt-in
//! safe-mode shape rules live in [`super::safe_mode`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind, COUNT_UNION_MIX_MESSAGE, COUNT_UNION_OMITTED_MESSAGE};
use relate_model::name_list::same_name_multiset;
use relate_parser::{Clause, Expr, PatternElement, ReturnItem, Statement};

use crate::ast::collect::{effective_name, tail_stage, union_arms};

mod traversal;

use crate::ast::stmt_ref::StmtRef;
pub use traversal::{validate_expr_traversal_bounds, validate_traversal_bounds};

/// `USE` has its own placement restriction (unrelated
/// to and not lifted by CIP-202's removal of the update-boundary rule) --
/// within any one linear statement body (the top-level query, or a `CALL {}`
/// subquery body, checked recursively so a nested `CALL` inside a `CALL`
/// body is covered too), a `USE` clause is only valid as that body's very
/// first clause, or as its second clause immediately after an importing
/// `WITH`. Any other position raises `InvalidPlacementOfUseClause`, e.g. a
/// `USE` sandwiched between an update and a read
/// (`CREATE (:A) USE g MATCH (v:A) RETURN v`) -- CIP-202 only removed the
/// syntactic *update-boundary* restriction; USE's own placement rule is
/// orthogonal and still applies, verified against live Neo4j.
///
/// Takes the tree walk's borrowed [`StmtRef`]: a FOREACH body has no UNION
/// chain of its own, exactly like the synthetic statement the walk used to
/// fabricate for it.
///
/// The UNION-suffix scan runs only at the chain's *head* (a statement not
/// itself reached via a `union` edge). The tree walk invokes this validator
/// on every arm too, and the walk is pre-order — the head's visitor runs
/// before any arm is visited — so an arm's own suffix scan could only
/// re-scan bodies the head already checked (finding nothing new: if the
/// head's in-order scan errored, the walk stopped there) or re-report the
/// head's verdict. Skipping it keeps a length-L chain O(L) instead of
/// O(L²) with a byte-identical first error. An arm still checks its own
/// body (and, via `check_use_placement_in_body`, its `CALL { }` bodies).
pub fn validate_use_placement(stmt: StmtRef<'_>) -> Result<(), EngineError> {
    check_use_placement_in_body(stmt.clauses())?;
    if stmt.is_union_arm() {
        return Ok(());
    }
    for (arm, _) in union_arms(stmt.union()) {
        check_use_placement_in_body(&arm.clauses)?;
    }
    Ok(())
}

fn check_use_placement_in_body(clauses: &[Clause]) -> Result<(), EngineError> {
    for (idx, clause) in clauses.iter().enumerate() {
        if matches!(clause, Clause::Use(_)) {
            // Valid at the very front (any run of leading USE clauses is
            // itself valid -- CIP-146's nested-USE-brace collapse can stack
            // more than one same-graph USE at the front of a body), or as
            // the clause immediately following a leading importing WITH.
            let all_use_before = clauses[..idx].iter().all(|c| matches!(c, Clause::Use(_)));
            let valid_position =
                all_use_before || (idx == 1 && matches!(clauses[0], Clause::With(_)));
            if !valid_position {
                return Err(EngineError::semantic(ErrorKind::InvalidPlacementOfUseClause,
                    "USE clause must be either the first clause in a query, or preceded by an importing WITH clause"
                        .to_string(),
                ));
            }
        }
        if let Clause::CallSubquery(csq) = clause {
            validate_use_placement(StmtRef::Real(&csq.statement))?;
        }
    }
    Ok(())
}
/// An administrative command (`SHOW DATABASES`/`SHOW
/// ROLES`/`SHOW SERVERS`/`SHOW USERS`/`TERMINATE TRANSACTIONS`) cannot
/// participate in a NEXT chain. In live Neo4j this isn't actually a
/// NEXT-grammar rule -- these commands require the `system` database
/// (`51N28` if run elsewhere), and `NEXT` is separately disallowed on the
/// system database itself (`42N17`) -- so the combination is rejected in
/// both directions for different underlying reasons.
/// Modeled here as a single compile-time check on any
/// statement that is part of a NEXT chain (has a `.next`, or is reached by
/// following one): non-admin SHOW commands (FUNCTIONS/CONSTRAINTS/INDEXES/
/// PROCEDURES/SETTINGS/TRANSACTIONS) are schema/catalog introspection, not
/// DBMS administration, and are unaffected.
///
/// The chain scan runs only at the chain's head (a statement carrying
/// `.next` that was not itself reached via a `next` edge). The tree walk
/// visits every stage after the head, and a non-head stage's own suffix
/// scan could only re-check bodies the head's in-order scan already
/// checked — re-reporting the head's verdict or nothing — so skipping it
/// keeps a length-L chain O(L) instead of O(L²) with a byte-identical
/// first error (a violation at the tail is still reported at the head's
/// invocation, exactly as before).
pub fn validate_next_no_admin_commands(stmt: StmtRef<'_>) -> Result<(), EngineError> {
    if stmt.next().is_none() || stmt.is_next_stage() {
        return Ok(());
    }
    check_no_admin_commands_in_body(stmt.clauses())?;
    let mut cur = stmt.next();
    while let Some(s) = cur {
        check_no_admin_commands_in_body(&s.clauses)?;
        cur = s.next.as_deref();
    }
    Ok(())
}

fn check_no_admin_commands_in_body(clauses: &[Clause]) -> Result<(), EngineError> {
    if clauses.iter().any(|c| {
        matches!(
            c,
            Clause::ShowDatabases(_)
                | Clause::ShowRoles(_)
                | Clause::ShowServers(_)
                | Clause::ShowUsers(_)
                | Clause::TerminateTransactions(_)
        )
    }) {
        return Err(EngineError::semantic(
            ErrorKind::SystemDatabaseRequired,
            "an administrative SHOW/TERMINATE command cannot participate in a NEXT chain"
                .to_string(),
        ));
    }
    Ok(())
}

/// A FOREACH body is side effects only: the admitted set is exactly
/// [`is_updating_clause`](crate::ast::collect::is_updating_clause) — the
/// updating clauses plus a nested FOREACH — so a `RETURN`, `WITH`,
/// `UNWIND` (or its CIP-250 `FOR` spelling), `MATCH`, `LOAD CSV` or `CALL`
/// there is invalid. Like the writes-in-`EXISTS { }` rule, the composition
/// is statically decidable from the statement's own text, so it is rejected
/// here, before lowering reaches execution — this is the one home of the
/// rule (the executor keeps no second copy: it would have to re-spell the
/// classification over the IR clause vocabulary). Every rejection is
/// Neo4j's 42I01 "invalid FOREACH. Invalid use of <clause> inside
/// 'FOREACH'." (live-verified against neo4j:2026.09-enterprise: `CALL` and
/// `LOAD CSV` included).
///
/// Only ever invoked on a FOREACH body: the tree walk presents each body as
/// a borrowed [`StmtRef::Body`] (the only clause-list body there is), and
/// `validate_statement_node` routes it here — a RETURN anywhere else is a
/// projection, not a violation.
pub fn validate_foreach_body(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        if crate::ast::collect::is_updating_clause(clause) {
            continue;
        }
        return Err(EngineError::semantic(
            ErrorKind::InvalidClauseComposition,
            format!(
                "invalid FOREACH: invalid use of {} inside 'FOREACH'",
                foreach_body_clause_name(clause)
            ),
        ));
    }
    Ok(())
}
/// The FOREACH-body rejection's clause name, as Neo4j spells it in
/// "invalid FOREACH. Invalid use of <clause> inside 'FOREACH'."
///
/// The parser's [`Clause::keyword`] is the one keyword table; only the
/// spellings it cannot serve stay here — the `FOR` alias (reported as
/// `FOR`, not `UNWIND`), the grouped catalog names Neo4j words as a phrase,
/// and the updating-clause fallback (those never reach this renderer:
/// `is_updating_clause` admits them first).
fn foreach_body_clause_name(clause: &Clause) -> &'static str {
    match clause {
        Clause::Unwind(unwind) if unwind.for_syntax => "FOR",
        Clause::CreateIndex(_)
        | Clause::DropIndex(_)
        | Clause::CreateConstraint(_)
        | Clause::DropConstraint(_) => "schema command",
        Clause::ShowFunctions(_)
        | Clause::ShowConstraints(_)
        | Clause::ShowIndexes(_)
        | Clause::ShowDatabases(_)
        | Clause::ShowRoles(_)
        | Clause::ShowServers(_)
        | Clause::ShowUsers(_)
        | Clause::ShowProcedures(_)
        | Clause::ShowSettings(_)
        | Clause::ShowTransactions(_)
        | Clause::TerminateTransactions(_) => "SHOW/TERMINATE",
        Clause::Create(_)
        | Clause::Merge(_)
        | Clause::Delete(_)
        | Clause::Set(_)
        | Clause::Remove(_)
        | Clause::Foreach(_) => "updating clause",
        other => other.keyword(),
    }
}

/// A UNION branch's result shape, as far as it's staticaly checkable: either
/// a concrete list of RETURN column names, `RETURN *` (can't determine the
/// columns until runtime, so skipped for name comparison), or no RETURN
/// clause at all (an "omitted" result -- CIP-140 FINISH, or an updating
/// clause with no trailing RETURN).
enum UnionBranchShape {
    Cols(Vec<String>),
    Wildcard,
    Omitted,
}

fn union_branch_shape(clauses: &[Clause]) -> UnionBranchShape {
    // Note: a union arm carrying its *own* private CIP-213 NEXT chain
    // (e.g. a top-level-brace block used as one arm, whose own content
    // chains internally) never reaches here with `.next` set -- the
    // parser's `maybe_wrap_composite_union_arm` absorbs that chain into a
    // self-contained `CALL (*) { ... } RETURN *`/`FINISH` node before it
    // is ever attached as an arm, specifically so this function (and every
    // other "what does this arm project" analysis) never has to
    // distinguish "this arm's own continuation" from "the whole
    // composite's shared trailing NEXT stage" (this `stmt` itself, when
    // it's the outermost composite node, may still carry `.next` for the
    // *latter* meaning -- deliberately not consulted here, since it
    // applies uniformly after every arm, not per-arm).
    //
    // CIP-135: a `{ WHEN ... }`/`{ WHEN ... ELSE ... }` UNION arm has no
    // direct `Clause::Return` of its own -- its shape is whatever its
    // branches project. Every branch/ELSE body has the same shape
    // (validated separately), so any one of them stands in
    // for the whole clause.
    if let [Clause::Conditional(cc)] = clauses {
        let inner = cc
            .branches
            .first()
            .map(|b| &b.body)
            .or(cc.else_body.as_ref());
        return match inner {
            Some(s) => union_branch_shape(&s.clauses),
            None => UnionBranchShape::Omitted,
        };
    }
    if !clauses.iter().any(|c| matches!(c, Clause::Return(_))) {
        // An arm whose final clause is a CALL subquery exports that body's
        // columns (plus the in-scope names before it), so the exact set is
        // only known once the scope fold runs: a wildcard. A unit subquery
        // call with no RETURN leaves the arm columnless.
        if let Some(Clause::CallSubquery(csq)) = clauses.last() {
            if call_subquery_projects(csq) {
                return UnionBranchShape::Wildcard;
            }
        }
        return UnionBranchShape::Omitted;
    }
    match get_return_columns(clauses) {
        Some(cols) => UnionBranchShape::Cols(cols),
        None => UnionBranchShape::Wildcard,
    }
}

/// Whether a CALL subquery body projects columns: its final stage is a
/// RETURN (or a conditional / nested CALL that projects). A unit subquery
/// call with no RETURN makes the enclosing arm a no-column arm.
fn call_subquery_projects(csq: &relate_parser::ast::CallSubqueryClause) -> bool {
    fn stmt_projects(s: &relate_parser::ast::Statement) -> bool {
        // CIP-213: an arm's projection is its true *final* stage — the end of
        // its own NEXT chain. A UNION head owns the whole composite, so it
        // stays itself.
        let body = if s.union.is_some() { s } else { tail_stage(s) };
        match body.clauses.last() {
            Some(Clause::Return(_)) => true,
            Some(Clause::Conditional(cc)) => {
                // The grammar guarantees at least one WHEN branch
                // (`parse_when_query` pushes one before its loop can
                // break), so an empty branch list is a broken invariant to
                // surface — never a fabricated "does not project" verdict.
                let branch = cc
                    .branches
                    .first()
                    .expect("a WHEN clause has at least one branch");
                stmt_projects(&branch.body)
            }
            Some(Clause::CallSubquery(inner)) => stmt_projects(&inner.statement),
            _ => false,
        }
    }
    stmt_projects(&csq.statement)
}

/// Validate UNION column consistency: all branches must have the same column
/// names, and either every branch returns a tabular result or none do (a
/// "some RETURN, some omitted/FINISH" mix is also a
/// `DifferentColumnsInUnion` error -- CIP-140 `03_union.feature` scenarios
/// 4-6, CIP-146 `union-braces.feature` scenario 9).
///
/// The whole-chain scan runs only at the chain's head (a statement not
/// itself reached via a `union` edge). The tree walk invokes this
/// validator on every arm too; an arm's scan covers a *suffix* of the
/// shapes the head's in-order scan already covered, so it can only
/// re-report the head's verdict (the omitted/tabular mix test and the
/// first-column-set comparison are both monotone under shrinking the shape
/// set) — and it allocated a fresh `shapes` Vec per link, making a
/// length-L chain O(L²). Skipping non-head scans is byte-identical and
/// O(L) per chain.
pub fn validate_union_columns(stmt: StmtRef<'_>) -> Result<(), EngineError> {
    if stmt.union().is_none() || stmt.is_union_arm() {
        return Ok(());
    }

    let mut shapes = vec![union_branch_shape(stmt.clauses())];
    for (arm, _) in union_arms(stmt.union()) {
        shapes.push(union_branch_shape(&arm.clauses));
    }

    let any_omitted = shapes
        .iter()
        .any(|s| matches!(s, UnionBranchShape::Omitted));
    let any_tabular = shapes
        .iter()
        .any(|s| !matches!(s, UnionBranchShape::Omitted));
    if any_omitted && any_tabular {
        return Err(EngineError::semantic(
            ErrorKind::DifferentColumnsInUnion,
            "cannot combine a UNION branch that returns results with one that does not".to_string(),
        ));
    }

    let mut first_cols: Option<&Vec<String>> = None;
    for shape in &shapes {
        if let UnionBranchShape::Cols(cols) = shape {
            match first_cols {
                None => first_cols = Some(cols),
                Some(fc) if !same_name_multiset(fc, cols) => {
                    return Err(EngineError::semantic(
                        ErrorKind::DifferentColumnsInUnion,
                        "all UNION branches must have the same column names".to_string(),
                    ));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}
/// Get the column names from a RETURN clause in a statement body.
///
/// Each column is named by the one output-name rule
/// ([`effective_name`], which matches the planner's
/// `Projection::output_name`): an alias, a bare variable, and a
/// map-projection over a variable all take the variable's name — so
/// `RETURN n {.p}` and `RETURN n` agree across UNION branches.
fn get_return_columns(clauses: &[Clause]) -> Option<Vec<String>> {
    let rc = clauses.iter().find_map(|clause| match clause {
        Clause::Return(rc) => Some(rc),
        _ => None,
    })?;
    if rc.items.iter().any(|item| item.all) {
        return None; // RETURN * - can't easily determine columns
    }
    // Rendered to text: the column list feeds the model's
    // `same_name_multiset` comparison (a value-domain list, not a scope
    // map).
    Some(
        rc.items
            .iter()
            .filter_map(effective_name)
            .map(|name| name.to_string())
            .collect(),
    )
}

/// The two statement-shape UNION rules a `COUNT { … }` body's own UNION
/// chain must satisfy (CIP-66): its branches must make the same
/// explicit-vs-omitted RETURN choice, and a body whose every branch omits
/// RETURN may join its branches with UNION ALL only. Both compositions
/// are statically decidable from the body's own text, so the validate
/// battery owns them (the FOREACH pattern: like `validate_foreach_body`,
/// the rule lives here and the executor's `eval_count_query` keeps the
/// same scan as defense-in-depth, raising the identical messages). The
/// battery's expression visitor routes every `Expr::CountSubquery` body
/// here: the statement visitor's `StmtRef` cannot tell a COUNT body from
/// an EXISTS/COLLECT one, and only COUNT carries these rules (CIP-63:
/// EXISTS has no RETURN-omission restriction; a top-level or `CALL { }`
/// omitted-RETURN union is CIP-140 FINISH semantics, not an error).
///
/// The predicate is the one the executor's per-row scan spelled: a branch
/// is "explicit" when its own clause list ends in a RETURN (a UNION arm
/// never carries a NEXT chain of its own — the parser's
/// `maybe_wrap_composite_union_arm` absorbs it — and the body head's NEXT
/// chain lowers out of `parts`, so each branch's clause list is exactly
/// what the executor classifies). For parsed text the parser's own
/// COUNT-site check (`check_subquery_union_consistency`) rejects both
/// shapes even earlier, with its parse-time wording — that wording is
/// the one user queries see; this validator is the front's canonical
/// home of the rule, reached by any statement tree the parser did not
/// itself screen (a hand-built AST, a future grammar gap).
pub fn validate_count_subquery_union_shapes(body: &Statement) -> Result<(), EngineError> {
    // The same classification the executor's per-row scan spells, in one
    // spelling over every branch (head plus arms): a lone branch is never
    // mixed and has no joiners, which the classification itself answers.
    let arms = union_arms(body.union.as_deref()).collect::<Vec<_>>();
    let any_distinct = arms.iter().any(|(_, all)| !*all);
    let branches = std::iter::once(body)
        .chain(arms.into_iter().map(|(arm, _)| arm))
        .collect::<Vec<_>>();
    // A CIP-135 conditional part is explicit when its branch bodies return:
    // its own clause list has no `Clause::Return` of its own (the same
    // classification `union_branch_shape` owns).
    let explicit = branches
        .iter()
        .filter(|branch| {
            !matches!(
                union_branch_shape(&branch.clauses),
                UnionBranchShape::Omitted
            )
        })
        .count();
    if explicit > 0 && explicit < branches.len() {
        return Err(EngineError::semantic(
            ErrorKind::DifferentColumnsInUnion,
            COUNT_UNION_MIX_MESSAGE,
        ));
    }
    if explicit == 0 && any_distinct {
        return Err(EngineError::semantic(
            ErrorKind::DifferentColumnsInUnion,
            COUNT_UNION_OMITTED_MESSAGE,
        ));
    }
    Ok(())
}

/// Validate that WITH/RETURN don't have duplicate column names.
pub fn validate_column_name_conflicts(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            // WITH's items must be aliased unless they are bare variables;
            // RETURN projects any expression under its output name.
            Clause::With(wc) => check_projected_names(&wc.items, true)?,
            Clause::Return(rc) => check_projected_names(&rc.items, false)?,
            _ => {}
        }
    }
    Ok(())
}

/// The one WITH/RETURN duplicate-column check over a projection's items:
/// every item's output name comes from the one name rule
/// ([`effective_name`] — alias, else the variable, else the source text,
/// the same rule `get_return_columns` renders), and a second item
/// contributing a name already taken is the `ColumnNameConflict`
/// rejection — spelled once for both clauses instead of once per arm.
/// `require_alias` is WITH's own rule (a non-variable projection item
/// must be aliased with `AS`); RETURN has no such requirement.
fn check_projected_names(items: &[ReturnItem], require_alias: bool) -> Result<(), EngineError> {
    let mut names: IdentHashSet<SmallName> = IdentHashSet::default();
    for item in items {
        if item.all {
            continue;
        }
        if require_alias && item.alias.is_none() && !matches!(&item.expr, Expr::Variable(_)) {
            return Err(EngineError::semantic(
                ErrorKind::NoExpressionAlias,
                "expressions in WITH must be aliased with AS".to_string(),
            ));
        }
        let Some(name) = effective_name(item) else {
            continue;
        };
        if !names.insert(name.clone()) {
            return Err(EngineError::semantic(
                ErrorKind::ColumnNameConflict,
                format!("multiple result columns with the same name '{name}'"),
            ));
        }
    }
    Ok(())
}
/// Validate that relationship variables are not reused within a single top-
/// level path pattern, when that reuse also closes a cycle back onto a
/// repeated *node* variable, and that two `shortestPath()` patterns of one
/// MATCH do not share a relationship variable.
///
/// The repeated-node/repeated-relationship case is the openCypher TCK's own
/// pinned expectation (`clauses/match/Match3.feature` #29, `MATCH
/// (a)-[r]->()-[r]->(a)`): a compile-time `RelationshipUniquenessViolation`.
/// The TCK is this project's language floor and wins over the newer Neo4j
/// acceptance behavior (an empty result plus the 01N63 warning). CIP-19's
/// deviation record and the GPM worked examples say Neo4j (CYPHER
/// 25 GPM) no longer raises this error for a repeated relationship variable
/// in general (e.g. `MATCH (n)-[r]->(m)-[r]->(o)`, with no repeated node
/// variable) -- those still compile and return no rows at runtime, via the
/// engine's own relationship-uniqueness pool in `match_rel_and_node`. Reuse
/// *across* comma-separated top-level path patterns of one MATCH clause
/// (CIP-60 5.3.3.3/8.1.1) is likewise not rejected here: the engine's
/// relationship-uniqueness pool (which spans the whole graph pattern) makes
/// it unsatisfiable at runtime.
pub fn validate_relationship_uniqueness(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        if let Clause::Match(mc) = clause {
            // Under the `REPEATABLE ELEMENTS` match
            // mode, a relationship variable declared twice is a legitimate
            // equijoin (the same relationship bound at both positions), not
            // a compile-time error.
            if mc.match_mode == relate_parser::MatchMode::RepeatableElements {
                continue;
            }
            let mut shortest_relationships = IdentHashSet::default();
            for path in &mc.pattern {
                if path.is_shortest_path_function {
                    for elem in &path.elements {
                        if let PatternElement::Relationship(rp) = elem {
                            if let Some(variable) = &rp.variable {
                                if !shortest_relationships.insert(variable.clone()) {
                                    return Err(EngineError::semantic(
                                        ErrorKind::RelationshipUniquenessViolation,
                                        format!(
                                            "relationship '{variable}' is reused by two shortest-path patterns"
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
                let mut seen_rels: IdentHashSet<SmallName> = IdentHashSet::default();
                let mut seen_nodes: IdentHashSet<SmallName> = IdentHashSet::default();
                let mut repeated_rel: Option<SmallName> = None;
                let mut repeated_node = false;
                for elem in &path.elements {
                    match elem {
                        PatternElement::Relationship(rp) => {
                            if let Some(v) = &rp.variable {
                                if !seen_rels.insert(v.clone()) && repeated_rel.is_none() {
                                    repeated_rel = Some(v.clone());
                                }
                            }
                        }
                        PatternElement::Node(np) => {
                            if let Some(v) = &np.variable {
                                if !seen_nodes.insert(v.clone()) {
                                    repeated_node = true;
                                }
                            }
                        }
                        PatternElement::Quantified(_) => {}
                    }
                }
                if repeated_node {
                    if let Some(v) = repeated_rel {
                        return Err(EngineError::semantic(
                            ErrorKind::RelationshipUniquenessViolation,
                            format!(
                                "relationship '{v}' used more than once in the same MATCH pattern"
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// CIP-40's mixing rule inside a SUBQUERY body (42I39): a
/// `shortestPath()`/`allShortestPaths()` pattern may not share the body
/// with a path selector, an explicit match mode or an explicit path mode.
/// The parser rejects the same-MATCH-clause combinations; this rejects the
/// combination two separate clauses of one body make (Neo4j
/// GpmSyntaxMixingAcceptance "Conflicting syntax in separate statements in
/// same EXISTS sub-query" / "… within a CALL subquery").
///
/// Deliberately NOT applied to a top-level statement: CIP-40
/// path-selector-mixing [11]/[13]/[18] pin separate top-level MATCH
/// clauses mixing a shortest-path function with a selector or match mode as
/// legal.
pub fn validate_shortest_path_mixing(stmt: &relate_parser::Statement) -> Result<(), EngineError> {
    for statement in crate::ast::collect::union_chain(stmt) {
        validate_shortest_path_mixing_in_clauses(&statement.clauses)?;
    }
    Ok(())
}

fn validate_shortest_path_mixing_in_clauses(clauses: &[Clause]) -> Result<(), EngineError> {
    let mut shortest_path_function = false;
    // Recorded for the diagnostic only: the corpus names the offending
    // combination (selector, match mode, path mode) in the message.
    let mut explicit = None;
    for clause in clauses {
        let Clause::Match(mc) = clause else {
            continue;
        };
        if mc.match_mode == relate_parser::MatchMode::RepeatableElements {
            explicit.get_or_insert("an explicit match mode");
        }
        for path in &mc.pattern {
            if path.is_shortest_path_function {
                shortest_path_function = true;
                // The function form's own selector slot carries the
                // normalized implicit selector, so only an explicit path
                // mode on the same pattern counts as mixing there.
                if path.mode.is_some() {
                    explicit.get_or_insert("an explicit path mode");
                }
            } else if path.selector.is_some() || path.mode.is_some() {
                explicit.get_or_insert("a path selector or explicit path mode");
            }
        }
    }
    match (shortest_path_function, explicit) {
        (true, Some(what)) => Err(EngineError::semantic(
            ErrorKind::InvalidShortestPathMixing,
            format!("mixing shortestPath()/allShortestPaths() with {what} is not allowed in one statement"),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_variable_cannot_span_two_shortest_path_patterns() {
        let stmt = relate_parser::parse(
            "MATCH p = shortestPath((a)-[r*]->(b)), \
             q = shortestPath((c)-[r*]->(d)) RETURN p, q",
        )
        .unwrap();
        let error = validate_relationship_uniqueness(&stmt.clauses).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::RelationshipUniquenessViolation);
    }

    /// A map-projection RETURN item is named by its variable — the one rule
    /// [`effective_name`] and the IR `Projection::output_name` share — so it
    /// is the same UNION column as the plain variable in another branch.
    #[test]
    fn union_names_a_map_projection_by_its_variable() {
        let accepted = "MATCH (n) RETURN n {.p} UNION MATCH (n) RETURN n";
        crate::validate::validate_statement_tree(&relate_parser::parse(accepted).unwrap())
            .expect("a map projection is named by its variable");
        // A differently named variable stays a different column.
        let rejected = "MATCH (n) RETURN n {.p} UNION MATCH (m) RETURN m";
        let error =
            crate::validate::validate_statement_tree(&relate_parser::parse(rejected).unwrap())
                .expect_err("different column names stay rejected");
        assert_eq!(error.kind(), ErrorKind::DifferentColumnsInUnion);
    }

    /// A RETURN inside a FOREACH body is rejected by the validate battery —
    /// before lowering, never at execution — with the authored composition
    /// error text (the executor's FOREACH boundary keeps the identical
    /// message as defense-in-depth).
    #[test]
    fn foreach_body_with_return_is_rejected_before_execution() {
        let error = crate::validate::validate_statement_tree(
            &relate_parser::parse("FOREACH (i IN [1] | RETURN 1)").unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidClauseComposition);
        assert_eq!(
            error.to_string(),
            "InvalidClauseComposition: invalid FOREACH: invalid use of RETURN inside 'FOREACH'"
        );
    }

    /// The rule fires wherever the body sits: the tree walk reaches a
    /// FOREACH inside a `CALL { }` body (and every other nested body) as
    /// its own `StmtRef::Body`.
    #[test]
    fn foreach_body_with_return_is_rejected_inside_nesting() {
        let error = crate::validate::validate_statement_tree(
            &relate_parser::parse(
                "CALL { FOREACH (i IN [1] | CREATE (:N {v: i}) RETURN i) } RETURN 1",
            )
            .unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidClauseComposition);
        assert_eq!(
            error.to_string(),
            "InvalidClauseComposition: invalid FOREACH: invalid use of RETURN inside 'FOREACH'"
        );
    }

    /// A FOREACH body admits only updating clauses: `MATCH`, `WITH`,
    /// `UNWIND` and the CIP-250 `FOR` spelling are each invalid there
    /// (Neo4j 42I01, live-verified; the class is what the corpus pins).
    #[test]
    fn foreach_body_rejects_non_updating_clauses() {
        for (query, clause) in [
            (
                "FOREACH (i IN [1] | WITH 1 AS z CREATE (:N {v: z}))",
                "WITH",
            ),
            (
                "FOREACH (i IN [1] | UNWIND [1] AS z CREATE (:N {v: z}))",
                "UNWIND",
            ),
            (
                "FOREACH (i IN [1] | FOR z IN [1] CREATE (:N {v: z}))",
                "FOR",
            ),
            ("FOREACH (i IN [1] | MATCH (m) CREATE (:N))", "MATCH"),
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidClauseComposition, "{query}");
            assert!(
                error
                    .to_string()
                    .contains(&format!("invalid use of {clause}")),
                "{query}: {error}"
            );
        }
        // A write-only body still passes.
        crate::validate::validate_statement_tree(
            &relate_parser::parse("FOREACH (i IN [1] | CREATE (:N {v: i}))").unwrap(),
        )
        .expect("a write-only FOREACH body validates");
    }

    /// A write-only FOREACH body (the only legal shape) passes, and a
    /// top-level RETURN is a projection, not a violation.
    #[test]
    fn foreach_body_without_return_and_top_level_return_pass() {
        crate::validate::validate_statement_tree(
            &relate_parser::parse("FOREACH (i IN [1] | CREATE (:N {v: i}))").unwrap(),
        )
        .expect("a write-only FOREACH body is valid");
        crate::validate::validate_statement_tree(&relate_parser::parse("RETURN 1").unwrap())
            .expect("a top-level RETURN is a projection");
    }

    /// Splice a parsed statement in as the body of a parsed `count { … }`
    /// wrapper: the bodies the two COUNT-subquery UNION rules reject are
    /// legal top-level queries, so the only way to hand the battery one
    /// is to build it from a wrapper the parser admits and a body it
    /// never screens in that position (its own COUNT-site check,
    /// `check_subquery_union_consistency`, is what rejects the spelled
    /// form at parse time — with its parse-time wording, the one user
    /// queries see).
    fn count_subquery_over(body: &str) -> relate_parser::Statement {
        let mut wrapper = relate_parser::parse("RETURN count { MATCH (n) UNION ALL MATCH (m) }")
            .expect("the all-omitted UNION ALL count body parses");
        let body = relate_parser::parse(body).expect("the body parses at the top level");
        let Clause::Return(rc) = &mut wrapper.clauses[0] else {
            panic!("expected the wrapper's RETURN clause");
        };
        let Expr::CountSubquery(cs) = &mut rc.items[0].expr else {
            panic!("expected the wrapper's count subquery item");
        };
        cs.statement = body;
        wrapper
    }

    /// A COUNT subquery body whose UNION branches mix explicit and omitted
    /// RETURN is rejected by the validate battery — before lowering, never
    /// at execution — with the executor scan's byte-identical message (the
    /// executor's `eval_count_query` keeps the same scan as
    /// defense-in-depth for callers that bypass validation).
    #[test]
    fn count_subquery_union_mix_is_rejected_before_execution() {
        let error = crate::validate::validate_statement_tree(&count_subquery_over(
            "RETURN 1 UNION ALL MATCH (n)",
        ))
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DifferentColumnsInUnion);
        assert_eq!(
            error.to_string(),
            "DifferentColumnsInUnion: cannot mix explicit and omitted RETURN"
        );
    }

    /// A COUNT subquery body whose every branch omits RETURN may join its
    /// branches with UNION ALL only — the second of the two CIP-66 shape
    /// rules, pinned with the executor scan's byte-identical message.
    #[test]
    fn count_subquery_union_omitted_return_requires_union_all() {
        let error = crate::validate::validate_statement_tree(&count_subquery_over(
            "MATCH (n) UNION MATCH (m)",
        ))
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DifferentColumnsInUnion);
        assert_eq!(
            error.to_string(),
            "DifferentColumnsInUnion: omitted RETURN requires UNION ALL"
        );
    }

    /// The two legal shapes pass: every branch omitted with UNION ALL
    /// joiners, and every branch explicit (a plain UNION is fine there —
    /// its dedup has named columns to compare).
    #[test]
    fn count_subquery_union_legal_shapes_pass() {
        for body in [
            "MATCH (n) UNION ALL MATCH (m)",
            "MATCH (n) RETURN n UNION MATCH (n) RETURN n",
        ] {
            crate::validate::validate_statement_tree(&count_subquery_over(body))
                .unwrap_or_else(|e| panic!("a legal COUNT union body was rejected: {e}"));
        }
    }

    /// The rules are COUNT-scoped, not general: an EXISTS body with the
    /// same all-omitted plain-UNION chain stays valid (CIP-63 — existence
    /// does not care about column identity), and so does the same chain
    /// at the top level (CIP-140 omitted-RETURN unions are FINISH
    /// semantics, not errors).
    #[test]
    fn exists_and_top_level_omitted_unions_stay_valid() {
        for query in [
            "RETURN exists { MATCH (n) UNION MATCH (m) }",
            "CREATE (:A) UNION CREATE (:B)",
        ] {
            crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                .unwrap_or_else(|e| {
                    panic!("an omitted-RETURN union outside COUNT was rejected: {e}")
                });
        }
    }

    /// LABS-610: an explicit hop bound above the admission limit is a
    /// `TraversalDepthExceeded` syntax error at the lowering boundary, in
    /// every spelling the parser produces: the legacy bracket forms
    /// (`*1..N`, `*..N`), the CIP-28 group quantifier (`{1,N}`), and the
    /// REPEATABLE ELEMENTS bypass (a bounded group quantifier that the
    /// parser accepts because only the *unbounded* form is refused there).
    #[test]
    fn explicit_traversal_bounds_above_the_limit_are_rejected_at_lowering() {
        for query in [
            "MATCH (a)-[*1..1000000]->(b) RETURN a",
            "MATCH (a)-[*..1000000]->(b) RETURN a",
            "MATCH (a)-->(b)((x)-[e]->(y)){1,1000000}(c) RETURN a",
            "MATCH REPEATABLE ELEMENTS ((x)-[e]->(y)){1,1000000} RETURN 1",
        ] {
            let error = crate::parse_and_lower(query).unwrap_err().to_string();
            assert!(
                error.starts_with("syntax error: TraversalDepthExceeded: "),
                "{query}: wrong error: {error}"
            );
            assert!(
                error.contains("paths up to 1,000,000 hops"),
                "{query}: the offending bound must be named: {error}"
            );
            assert!(
                error.contains("--max-traversal-depth"),
                "{query}: the knob must be named: {error}"
            );
            assert!(
                error.contains("(up to 2000)"),
                "{query}: the ceiling must be named: {error}"
            );
        }
    }

    /// The limit is admission, not a semantic ban: a bound at or below the
    /// default (500) lowers, and an unbounded upper bound (`*`, `*1..`,
    /// `{1,}`) is exempt because the executor already caps the classic walk
    /// at 32 hops.
    #[test]
    fn traversal_bounds_at_or_below_the_limit_and_unbounded_forms_pass() {
        for query in [
            "MATCH (a)-[*1..100]->(b) RETURN a",
            "MATCH (a)-[*1..500]->(b) RETURN a",
            "MATCH (a)-[*]->(b) RETURN a",
            "MATCH (a)-[*1..]->(b) RETURN a",
            "MATCH (a)-->(b)((x)-[e]->(y)){1,500}(c) RETURN a",
            "MATCH REPEATABLE ELEMENTS ((x)-[e]->(y)){1,500} RETURN 1",
        ] {
            crate::parse_and_lower(query)
                .unwrap_or_else(|e| panic!("{query}: a legal traversal bound was rejected: {e}"));
        }
    }

    /// The expression-embedded patterns carry hop bounds too; they are
    /// checked by the same walk so a predicate cannot bypass admission.
    #[test]
    fn expression_embedded_traversal_bounds_are_checked() {
        for query in [
            "MATCH (a) WHERE (a)-[*1..1000000]->() RETURN a",
            "RETURN [(a)-[*1..1000000]->(b) | b] AS xs",
            "RETURN shortestPath((a)-[*1..1000000]->(b)) AS p",
        ] {
            let error = crate::parse_and_lower(query).unwrap_err().to_string();
            assert!(
                error.contains("TraversalDepthExceeded"),
                "{query}: wrong error: {error}"
            );
        }
    }

    /// The knob threads into lowering: the same statement is rejected under
    /// a lowered limit and accepted under a raised one.
    #[test]
    fn the_resolved_limit_raises_and_lowers_the_admission_bound() {
        let query = "MATCH (a)-[*1..600]->(b) RETURN a";
        let rejected = match crate::parse_and_lower_statement_with_limits(
            query,
            relate_core::PreviewFeatures::none(),
            relate_core::MAX_TRAVERSAL_DEPTH,
        ) {
            Ok(_) => panic!("the default limit must reject 600 hops"),
            Err(error) => error.to_string(),
        };
        assert!(rejected.contains("TraversalDepthExceeded"), "{rejected}");
        crate::parse_and_lower_statement_with_limits(
            query,
            relate_core::PreviewFeatures::none(),
            600,
        )
        .expect("600 hops is admissible under --max-traversal-depth 600");
    }
}

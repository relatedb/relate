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
use relate_parser::{Clause, PatternElement, PatternPath, ReturnItem, Statement};

use crate::ast::collect::union_arms;
use crate::ast::naming::{effective_name, item_needs_alias};

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
/// `FOR`, not `UNWIND`) and the grouped catalog names Neo4j words as a phrase
/// (an updating clause never reaches this renderer: `is_updating_clause`
/// admits it first).
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
        let inner = cc.representative_body();
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
            if csq.statement.projects_columns() {
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

/// Validate a WITH/RETURN projection's names: no duplicate output columns
/// in either clause, and WITH's alias rule (an item without an implicit
/// name must carry `AS`) — [`check_projected_names`] is the one fold.
pub fn validate_projection_names(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        match clause {
            // WITH's items must be aliased unless they carry an implicit
            // name (a bare variable, or a map projection over one);
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
/// `require_alias` is WITH's own rule (an item that needs an alias —
/// [`item_needs_alias`]: no `AS`, and no implicit name — must carry one);
/// RETURN has no such requirement. Redundant parens are transparent to
/// the shape tests — through the one paren-transparency home,
/// [`effective_expr`] (LABS-1616) — so `WITH (n)` is the bare variable
/// `n` and `WITH (n {.*})` the projection over it, and neither needs an
/// alias, while `WITH (n.p1)` stays an expression item and still
/// requires `AS`.
fn check_projected_names(items: &[ReturnItem], require_alias: bool) -> Result<(), EngineError> {
    let mut names: IdentHashSet<SmallName> = IdentHashSet::default();
    for item in items {
        if item.all {
            continue;
        }
        if require_alias && item_needs_alias(item) {
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
        let Clause::Match(mc) = clause else {
            continue;
        };
        // Under the `REPEATABLE ELEMENTS` match mode, a relationship
        // variable declared twice is a legitimate equijoin (the same
        // relationship bound at both positions), not a compile-time error.
        if mc.match_mode == relate_parser::MatchMode::RepeatableElements {
            continue;
        }
        let mut shortest_relationships: IdentHashSet<SmallName> = IdentHashSet::default();
        for path in &mc.pattern {
            if path.is_shortest_path_function {
                check_shortest_path_relationships(path, &mut shortest_relationships)?;
            }
            check_cycle_reuses_relationship(path)?;
        }
    }
    Ok(())
}

/// The shortest-path half of [`validate_relationship_uniqueness`]: records
/// each of one `shortestPath()` pattern's relationship variables in the
/// MATCH-wide `seen` set; a variable another shortest-path pattern of the
/// same MATCH already took is the violation.
fn check_shortest_path_relationships(
    path: &PatternPath,
    seen: &mut IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    for elem in &path.elements {
        let PatternElement::Relationship(rp) = elem else {
            continue;
        };
        if let Some(variable) = &rp.variable {
            if !seen.insert(variable.clone()) {
                return Err(EngineError::semantic(
                    ErrorKind::RelationshipUniquenessViolation,
                    format!("relationship '{variable}' is reused by two shortest-path patterns"),
                ));
            }
        }
    }
    Ok(())
}

/// The cycle half of [`validate_relationship_uniqueness`]: one path whose
/// first repeated relationship variable shares the path with a repeated
/// node variable (the TCK `Match3` #29 shape).
fn check_cycle_reuses_relationship(path: &PatternPath) -> Result<(), EngineError> {
    let mut seen_rels: IdentHashSet<SmallName> = IdentHashSet::default();
    let mut seen_nodes: IdentHashSet<SmallName> = IdentHashSet::default();
    let mut repeated_rel: Option<&SmallName> = None;
    let mut repeated_node = false;
    for elem in &path.elements {
        match elem {
            PatternElement::Relationship(rp) => {
                if let Some(v) = &rp.variable {
                    if !seen_rels.insert(v.clone()) && repeated_rel.is_none() {
                        repeated_rel = Some(v);
                    }
                }
            }
            PatternElement::Node(np) => {
                if let Some(v) = &np.variable {
                    repeated_node |= !seen_nodes.insert(v.clone());
                }
            }
            PatternElement::Quantified(_) => {}
        }
    }
    match (repeated_node, repeated_rel) {
        (true, Some(v)) => Err(EngineError::semantic(
            ErrorKind::RelationshipUniquenessViolation,
            format!("relationship '{v}' used more than once in the same MATCH pattern"),
        )),
        _ => Ok(()),
    }
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
mod tests;

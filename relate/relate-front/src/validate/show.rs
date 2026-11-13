//! `SHOW …`/`TERMINATE …` validation: per-command YIELD column checks
//! (CIP-42/46/79/84/157/184/187), the CIP-123/242/253 composability rules
//! for composing SHOW/TERMINATE clauses with each other and with regular
//! Cypher clauses, and the duplicate-output-name check for the two
//! graph-schema catalogs.
//!
//! Two system-database rules share one routing predicate
//! ([`show_targets_system`]): the per-database graph-schema catalogs
//! (`SHOW INDEXES`/`SHOW CONSTRAINTS`) are rejected when the statement
//! routes to `system`, and any graph-reading expression (`COUNT`/`EXISTS`/
//! `COLLECT` subquery, pattern predicate, pattern comprehension) is
//! rejected there. The graph-read rule is evaluated per UNION arm, so an
//! arm's routing never reaches a sibling arm's expressions.
//!
//! The SHOW column tables themselves live in [`crate::show_columns`] (the
//! canonical registry shared with the planner and executor).

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_core::Namespace;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, Statement, YieldList, YieldMode};

use crate::ast::collect::{check_expr_vars_defined, clause_introduced_names, projected_names};

/// Every explicitly YIELD'd column must be one the command actually
/// projects: the one validity guard the SHOW validator family shares.
fn check_yield_column_names(
    yields: &YieldList,
    full: &[String],
    command: &str,
) -> Result<(), EngineError> {
    if yields.mode() == YieldMode::Star {
        return Ok(());
    }
    for y in yields.names() {
        if !full.iter().any(|c| c.as_str() == y) {
            return Err(EngineError::semantic(
                ErrorKind::InvalidShowColumn,
                format!("'{y}' is not a valid {command} column"),
            ));
        }
    }
    Ok(())
}

/// The one per-command SHOW column policy: every explicit YIELD name must
/// be a real column, and the clause's own WHERE may reference the scope the
/// yield mode defines plus any variable an earlier clause bound.
fn validate_show_columns(
    info: &crate::show_columns::CatalogInfo<'_>,
    outer_scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    check_yield_column_names(info.yields, &info.full_columns, info.command)?;
    validate_show_row_expressions(info, outer_scope)
}

/// The one row-expression scope rule for a SHOW clause: its `WHERE` and its
/// `ORDER BY` items read the yield scope (source and output names alike) plus
/// any variable an earlier clause bound. An unknown name is the front end's
/// `UndefinedVariable`, never a silently null sort key.
fn validate_show_row_expressions(
    info: &crate::show_columns::CatalogInfo<'_>,
    outer_scope: &IdentHashSet<SmallName>,
) -> Result<(), EngineError> {
    let mut in_scope = show_where_scope(info);
    in_scope.extend(outer_scope.iter().cloned());
    if let Some(where_expr) = info.where_expr {
        check_expr_vars_defined(where_expr, &in_scope)?;
    }
    for stage in info.stages {
        // Every stage that carries an expression: `SKIP`/`LIMIT` read the
        // same yield scope `ORDER BY` does.
        match stage {
            relate_parser::PageStage::OrderBy(items) => {
                for item in items {
                    check_expr_vars_defined(&item.expr, &in_scope)?;
                }
            }
            relate_parser::PageStage::Skip(expr) | relate_parser::PageStage::Limit(expr) => {
                check_expr_vars_defined(expr, &in_scope)?;
            }
        }
    }
    Ok(())
}

/// The one SHOW-family WHERE-scope rule, derived from the descriptor's one
/// projection: `YIELD *` makes the full column set referenceable, and every
/// other mode both each source (raw) name and its output (alias-resolved)
/// name (`show::where_probe` makes both resolvable at runtime). Composed-
/// SHOW outer bindings are the caller's addition.
fn show_where_scope(info: &crate::show_columns::CatalogInfo<'_>) -> IdentHashSet<SmallName> {
    match info.projection() {
        // The catalog descriptor's column vocabulary is text; this is the
        // one point the WHERE scope classifies it into the name domain.
        crate::show_columns::CatalogProjection::Star => info
            .full_columns
            .iter()
            .map(|column| SmallName::from_name(column))
            .collect(),
        crate::show_columns::CatalogProjection::Columns(pairs) => pairs
            .iter()
            .flat_map(|(output, source)| [output, source])
            .map(|column| SmallName::from_name(column))
            .collect(),
    }
}

/// Runs every SHOW-clause-specific column-name check: `SHOW DATABASES`'s
/// CIP-42 checks, and `SHOW FUNCTIONS`/`SHOW PROCEDURES`/`SHOW SETTINGS`'s
/// CIP-84/CIP-157 default/Full-only column split. The remaining CIP-68 SHOW
/// commands have no non-default columns yet, so their `show_*_columns` full
/// sets already double as valid-YIELD-name whitelists with nothing further
/// to enforce.
pub fn validate_show_column_names(stmt: &Statement) -> Result<(), EngineError> {
    // CIP-123/CIP-242: a composed SHOW clause's own `WHERE` may reference a
    // variable bound by an earlier clause in the same statement.
    let mut outer_scope: IdentHashSet<SmallName> = IdentHashSet::default();
    for clause in &stmt.clauses {
        if let Some(info) = crate::show_columns::catalog_info(clause) {
            if info.validate_columns {
                validate_show_columns(&info, &outer_scope)?;
            } else {
                // The CIP-68 / graph-type families keep their own YIELD
                // column behavior, but their row expressions (WHERE/ORDER BY)
                // share the one scope rule.
                validate_show_row_expressions(&info, &outer_scope)?;
            }
        }
        // A WITH replaces the scope; every other clause extends it.
        if let Clause::With(wc) = clause {
            outer_scope = projected_names(&wc.items, &outer_scope);
        } else {
            outer_scope.extend(clause_introduced_names(clause));
        }
    }
    Ok(())
}

/// True when a `USE` clause's graph expression is the bare literal name
/// `system` (`USE system`) -- the one spelling the
/// `SHOW INDEXES`/`SHOW CONSTRAINTS`-on-`system` restriction tests.
/// Namespace names match case-insensitively (ADR-0116 §1), so `USE SYSTEM`
/// composes under the same rule now that `USE` routes (ADR-0116 §3).
/// Doesn't attempt to resolve `graph.byName(...)`/parameterized forms to
/// "system" -- those aren't part of this rule's tested surface.
fn use_targets_system(uc: &relate_parser::UseClause) -> bool {
    matches!(&uc.graph_expr, Expr::Variable(name)
        if name.with_str(Namespace::parse) == Some(Namespace::System))
}

/// True for every SHOW/TERMINATE clause kind CIP-123/CIP-242/CIP-253's
/// composability rules apply to.
fn is_show_or_terminate(clause: &Clause) -> bool {
    // The CIP-123/242/253 composability family: the descriptor owns which
    // catalog commands belong to it (the graph-type commands keep their own
    // CIP-184 rules and error ordering).
    crate::show_columns::catalog_info(clause).is_some_and(|info| info.composable)
}

/// True for the two graph-schema catalogs (`SHOW INDEXES`/`SHOW
/// CONSTRAINTS`): the commands that belong to an individual graph database
/// rather than the `system` database, which CIP-123's composability rule
/// singles out. The one home of that two-command classification.
fn is_graph_schema_catalog(clause: &Clause) -> bool {
    crate::show_columns::catalog_info(clause).is_some_and(|info| info.graph_schema_catalog)
}

/// Whether the clauses route the statement to the `system` database: the
/// leading run of `USE` clauses — the routing prefix, ending at the first
/// catalog command — names `system`, and nothing but `USE` precedes that
/// command (a `USE` after ordinary Cypher does not route the statement;
/// `validate_use_placement` rejects such a body on its own). The one routing
/// predicate the composability rule and the system-graph-read rule share, so
/// the two can never disagree about what "on the system database" means.
fn show_targets_system(clauses: &[Clause]) -> bool {
    let routing: Vec<&Clause> = clauses
        .iter()
        .take_while(|clause| !crate::show_columns::is_catalog_clause(clause))
        .collect();
    routing
        .iter()
        .all(|clause| matches!(clause, Clause::Use(_)))
        && routing
            .iter()
            .any(|clause| matches!(clause, Clause::Use(uc) if use_targets_system(uc)))
}

/// The typed yield mode of a SHOW/TERMINATE clause, uniformly across every
/// variant; `None` for anything else. The mode (not a derived bool pair) is
/// the classification the composability rule branches on.
fn show_yield_state(clause: &Clause) -> Option<YieldMode> {
    crate::show_columns::catalog_info(clause).map(|info| info.yields.mode())
}

/// CIP-123/CIP-242/CIP-253: structural rules for composing SHOW/TERMINATE
/// clauses with each other and with regular Cypher clauses. A SHOW/
/// TERMINATE clause keeps its pre-CIP-123 baseline freedom -- YIELD and
/// RETURN both optional, `YIELD *` allowed -- only in the single canonical
/// standalone shape: the clause alone, or the clause immediately followed
/// by its own trailing RETURN and nothing else (CIP-123
/// `composability-boundaries.feature` [7]/[8]). Any other arrangement -- a
/// preceding clause, an intervening clause, more than one SHOW/TERMINATE
/// clause, or no trailing RETURN at all -- is "composed": every SHOW/
/// TERMINATE clause in it must carry an explicit, non-`*` YIELD, a SHOW/
/// TERMINATE clause can never be the statement's own last clause (its
/// yielded columns would otherwise vanish -- a *different* clause, such as
/// a trailing write with no RETURN, is still a perfectly ordinary terminal
/// clause), and a RETURN clause, if present at all, must be the last one
/// (rejected outright otherwise, whether or not it's the statement's only
/// RETURN).
///
/// Applied to `stmt`'s own clause list and, independently, to each arm of
/// its `.union` chain (each arm's clause list stands on its own for this
/// check). Deliberately does *not* recurse into nested `CALL {}`/`EXISTS
/// {}`/`COUNT {}` subquery bodies -- a SHOW/TERMINATE clause that is the
/// sole clause of such a body (e.g. `CALL { SHOW CONSTRAINTS }`) is exactly
/// as unconstrained as any other bare, non-composed SHOW/TERMINATE command,
/// so leaving it unchecked here matches the canonical-standalone case
/// anyway.
pub fn validate_show_composability(stmt: &Statement) -> Result<(), EngineError> {
    // A UNION arm — the head included — is never the canonical standalone
    // shape (live 2026.10: `SHOW CONSTRAINTS YIELD * RETURN name UNION …`
    // reports 22N94), so the leniency is off for the whole chain.
    let has_union = stmt.union.is_some();
    validate_show_composability_clauses(&stmt.clauses, stmt.brace_wrapped || has_union)?;
    let mut current = stmt;
    while let Some(union) = &current.union {
        validate_show_composability_clauses(&union.statement.clauses, true)?;
        current = &union.statement;
    }
    Ok(())
}

/// The composability rules for a SHOW/TERMINATE clause inside a subquery
/// body (`CALL { … }`, `COUNT`/`EXISTS`/`COLLECT { … }`, `WHEN` branches):
/// such a clause is never the canonical standalone shape either, so a body
/// that is a bare `SHOW SETTINGS` is the 22N93 missing YIELD, a bare
/// `SHOW SETTINGS YIELD name` is the 42N71 conclusion error and
/// `YIELD *` is the 22N94 invalid star — live-verified 2026.10. The body's
/// own UNION arms are checked like top-level arms.
pub(crate) fn validate_nested_show_composability(stmt: &Statement) -> Result<(), EngineError> {
    let mut current = stmt;
    loop {
        validate_show_composability_clauses(&current.clauses, true)?;
        match &current.union {
            Some(union) => current = &union.statement,
            None => return Ok(()),
        }
    }
}

/// Any `SHOW`/`TERMINATE` clause followed by a `RETURN` must carry an
/// explicit `YIELD` (22N93 `MissingYield`). The lenient bare
/// `SHOW X RETURN col` shape `validate_show_composability_clauses` tolerates
/// is not Neo4j's: live-verified for `SHOW CURRENT GRAPH TYPE` (CIP-184
/// syntax[12]) and in the programmatic corpus for `SHOW CONSTRAINTS`,
/// `SHOW PROCEDURES`, `SHOW INDEXES`, `SHOW FUNCTIONS`, `SHOW SETTINGS` and
/// `SHOW TRANSACTIONS` alike. Applied to `stmt`'s own clause list and each
/// union arm, same recursion shape as `validate_show_composability`.
pub fn validate_show_requires_yield_before_return(stmt: &Statement) -> Result<(), EngineError> {
    validate_show_requires_yield_before_return_clauses(&stmt.clauses)?;
    let mut current = stmt;
    while let Some(union) = &current.union {
        validate_show_requires_yield_before_return_clauses(&union.statement.clauses)?;
        current = &union.statement;
    }
    Ok(())
}

fn validate_show_requires_yield_before_return_clauses(
    clauses: &[Clause],
) -> Result<(), EngineError> {
    if !clauses.iter().any(|c| matches!(c, Clause::Return(_))) {
        return Ok(());
    }
    for clause in clauses {
        if let Some(info) = crate::show_columns::catalog_info(clause) {
            if !info.yields.present() {
                return Err(EngineError::semantic(
                    ErrorKind::InvalidClauseComposition,
                    "MissingYield: 22N93 an explicit YIELD is \
                     required before RETURN"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_show_composability_clauses(
    clauses: &[Clause],
    brace_wrapped: bool,
) -> Result<(), EngineError> {
    if !clauses.iter().any(is_show_or_terminate) {
        return Ok(());
    }

    // The graph-schema catalogs (`SHOW INDEXES`,
    // `SHOW CONSTRAINTS`) are per-database, so `USE system` rejects them
    // outright -- composed or not (live corpus: `ShowConstraintCommands`
    // Part2's `USE system SHOW CONSTRAINTS YIELD name RETURN name`; the
    // composed scenarios [8]/[9] raise the same code for the same reason).
    // Composed SHOW on `system` in general is fine (verified live: `USE
    // system SHOW PROCEDURES ... SHOW FUNCTIONS ... SHOW SETTINGS ...`
    // succeeds).
    if show_targets_system(clauses) && clauses.iter().any(is_graph_schema_catalog) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidClauseComposition,
            "NotAllowedOnSystemDatabase: SHOW INDEXES and SHOW CONSTRAINTS are \
             per-database catalogs and are not available on the system database"
                .to_string(),
        ));
    }

    // Once any SHOW/TERMINATE clause is present, a
    // RETURN clause may only be the statement's very last clause -- it
    // can't be followed by a further composed command (or anything else).
    if let Some(return_idx) = clauses.iter().position(|c| matches!(c, Clause::Return(_))) {
        if return_idx != clauses.len() - 1 {
            return Err(EngineError::semantic(
                ErrorKind::InvalidClauseComposition,
                "ReturnNotLast: RETURN is only allowed as the last \
                 clause when composing a SHOW/TERMINATE command"
                    .to_string(),
            ));
        }
    }

    // A leading `USE <graph>` routes the statement; it is not composition.
    // Live Neo4j accepts `USE neo4j SHOW INDEXES` (no YIELD, no RETURN) and
    // `USE system SHOW PROCEDURES YIELD * ... RETURN ...` exactly like the
    // unprefixed canonical standalone forms, so the leniency below is
    // evaluated on the clauses after any leading run of `USE`.
    let body: Vec<&Clause> = clauses
        .iter()
        .skip_while(|clause| matches!(clause, Clause::Use(_)))
        .collect();

    // Explicit top-level braces (`{ SHOW INDEXES }`) mark
    // the body as a `SingleQuery` known to be composed/embedded per CIP-146,
    // even when its clause list alone would otherwise qualify for the
    // lenient "canonical standalone" leniency below -- so a brace-wrapped
    // bare SHOW/TERMINATE still needs its own explicit YIELD.
    let canonical_standalone = !brace_wrapped
        && match body.as_slice() {
            [_] => true,
            [show, Clause::Return(_)] => is_show_or_terminate(show),
            _ => false,
        };
    if canonical_standalone {
        return Ok(());
    }

    // Composed: every SHOW/TERMINATE clause needs its own explicit,
    // non-`*` YIELD.
    for clause in &body {
        match show_yield_state(clause) {
            Some(YieldMode::Default) => {
                return Err(EngineError::semantic(
                    ErrorKind::InvalidClauseComposition,
                    "MissingYield: 22N93 an explicit YIELD is \
                     required when composing a SHOW/TERMINATE command"
                        .to_string(),
                ));
            }
            Some(YieldMode::Star) => {
                return Err(EngineError::semantic(
                    ErrorKind::InvalidClauseComposition,
                    "InvalidYieldStar: 22N94 YIELD * is not allowed \
                     when composing a SHOW/TERMINATE command"
                        .to_string(),
                ));
            }
            Some(YieldMode::Named) | None => {}
        }
    }

    // Composed: a SHOW/TERMINATE clause can never be the statement's own
    // final clause -- its yielded columns would otherwise just vanish
    // (`SHOW PROCEDURES YIELD ... SHOW FUNCTIONS YIELD
    // ...` with no trailing RETURN; `MATCH (n) SHOW
    // INDEXES YIELD name` ending on a bare YIELD). Any *other* clause is a
    // legitimate terminal clause on its own ordinary terms -- a trailing
    // write clause with no RETURN (`SHOW INDEXES YIELD
    // ... CREATE (...)`) is exactly as valid after a composed SHOW as it
    // is anywhere else in Cypher, so this deliberately does *not* require
    // RETURN unconditionally, only that a SHOW/TERMINATE clause never has
    // the last word.
    if is_show_or_terminate(body.last().expect("checked non-empty above")) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidClauseComposition,
            "MissingReturn: 42N71 a composed SHOW/TERMINATE command \
             cannot be the statement's last clause without a trailing RETURN"
                .to_string(),
        ));
    }

    Ok(())
}

/// The graph-reading expression kinds a SHOW command on the system database
/// must not contain, each mapped to the complete noun phrase Neo4j's
/// diagnostic uses ("a COUNT expression", "an EXISTS expression", …) — the
/// phrase travels with the variant, so the article is never re-derived from
/// the keyword's first letter. A pattern comprehension is the list-producing
/// construct (Neo4j reports it as a COLLECT expression) and a bare pattern
/// predicate as an EXISTS expression; all of them read a graph the `system`
/// database does not have.
fn system_graph_read_phrase(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::ExistsSubquery(_) | Expr::PatternPredicate(_) => Some("an EXISTS expression"),
        Expr::CountSubquery(_) => Some("a COUNT expression"),
        Expr::CollectSubquery(_) | Expr::PatternComprehension(_) => Some("a COLLECT expression"),
        _ => None,
    }
}

/// A SHOW command routed to the `system` database must not read a graph: any
/// `COUNT`/`EXISTS`/`COLLECT` subquery, pattern expression or pattern
/// comprehension anywhere in the statement is rejected (live Neo4j
/// 2026.09: top-level 42001 with cause 42N17 "not allowed on the system
/// database"; the harness corpus `ShowCommandsGraphLeakTest` and the
/// SHOW SETTINGS/FUNCTIONS system-database scenarios). Non-admin SHOW
/// commands themselves are allowed on `system` (CIP-123); only the
/// graph-reading expressions are not.
///
/// Applied to `stmt`'s own clause list and, independently, to each arm of
/// its `.union` chain — the routing predicate is evaluated per arm, so a
/// graph read in a later arm is rejected on that arm's own routing and a
/// sibling arm's expressions are never judged by it.
pub fn validate_show_system_graph_reads(stmt: &Statement) -> Result<(), EngineError> {
    validate_show_system_graph_reads_clauses(&stmt.clauses)?;
    let mut current = stmt;
    while let Some(union) = &current.union {
        validate_show_system_graph_reads_clauses(&union.statement.clauses)?;
        current = &union.statement;
    }
    Ok(())
}

/// One arm's half of [`validate_show_system_graph_reads`]: when the arm
/// routes to `system` (the shared [`show_targets_system`] predicate) and
/// carries a catalog command, every graph-reading expression in its own
/// clauses — nested subquery bodies included, sibling UNION arms excluded —
/// is rejected.
fn validate_show_system_graph_reads_clauses(clauses: &[Clause]) -> Result<(), EngineError> {
    if !show_targets_system(clauses) || !clauses.iter().any(crate::show_columns::is_catalog_clause)
    {
        return Ok(());
    }
    let mut visit = |expr: &Expr| -> Result<(), EngineError> {
        match system_graph_read_phrase(expr) {
            Some(phrase) => Err(EngineError::semantic(
                ErrorKind::SyntaxError,
                format!("You cannot include {phrase} on a system database"),
            )),
            None => Ok(()),
        }
    };
    crate::ast::tree::walk_clause_list_tree(
        clauses,
        crate::ast::tree::SubqueryDescent::Enter,
        &mut visit,
    )
}

/// Duplicate-output-name check for the two graph-schema catalog commands
/// (`SHOW CONSTRAINTS` / `SHOW INDEXES`): no output column name -- an
/// explicit YIELD name or its non-empty `AS` alias -- may repeat, whether
/// within one command's YIELD list or across the commands of one statement
/// (a name collision would silently drop one of the row's columns). This
/// runs on every statement the lowering sees, including nested subquery
/// bodies: unlike `scope::validate_variable_types` (which enforces the same
/// rule at top level as part of its full scope walk), it needs no outer
/// scope, so it is safe to apply to a nested body whose enclosing
/// bindings are not visible here.
pub fn validate_show_output_uniqueness(clauses: &[Clause]) -> Result<(), EngineError> {
    let mut command_outputs = IdentHashSet::default();
    for clause in clauses {
        let yields = crate::show_columns::catalog_info(clause)
            .filter(|info| info.graph_schema_catalog)
            .map(|info| info.yields);
        if let Some(yields) = yields {
            for (_, _, output) in yields.pairs() {
                if !command_outputs.insert(output.clone()) {
                    return Err(output.with_str(EngineError::duplicate_variable));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Through the public lowering path: the pass is wired for top-level
    /// and nested statements alike.
    fn lower(query: &str) -> Result<(), String> {
        crate::parse_and_lower(query)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn duplicate_show_output_names_are_rejected() {
        for query in [
            // Duplicated within one YIELD list, plain and via `AS` alias.
            "SHOW CONSTRAINTS YIELD name, name RETURN name",
            "SHOW INDEXES YIELD name, owningConstraint AS name RETURN name",
            // Duplicated across the two graph-schema catalogs of one
            // statement.
            "SHOW CONSTRAINTS YIELD name SHOW INDEXES YIELD name RETURN name",
            // Nested bodies are checked too (no outer scope needed).
            "CALL { SHOW CONSTRAINTS YIELD name, name RETURN name } RETURN name",
        ] {
            let err = lower(query).expect_err(&format!("`{query}` must be rejected"));
            assert!(err.contains("VariableAlreadyBound"), "{query}: {err}");
        }
    }

    #[test]
    fn distinct_show_output_names_are_accepted() {
        assert!(lower("SHOW CONSTRAINTS YIELD name, id RETURN name").is_ok());
        assert!(
            lower("SHOW CONSTRAINTS YIELD name SHOW INDEXES YIELD name AS ix RETURN ix").is_ok()
        );
        // Other SHOW commands are not this pass's surface (the top-level
        // scope tracker covers them).
        assert!(lower("SHOW FUNCTIONS YIELD name RETURN name").is_ok());
    }

    #[test]
    fn a_leading_use_prefix_is_not_composition() {
        // Live Neo4j accepts every canonical-standalone SHOW shape with a
        // routing `USE` in front: no YIELD, a bare YIELD, a trailing RETURN.
        for query in [
            "USE neo4j SHOW INDEXES",
            "USE neo4j SHOW INDEXES YIELD name",
            "USE neo4j SHOW PROCEDURES YIELD * WHERE mode = 'SCHEMA' RETURN name",
            "USE system SHOW SETTINGS YIELD name WHERE name = 'db.format' RETURN name",
        ] {
            assert!(lower(query).is_ok(), "{query}: {:?}", lower(query));
        }
        // A `USE` prefix does not make a composed SHOW any more legal.
        assert!(lower("MATCH (n) USE neo4j SHOW INDEXES YIELD name RETURN name").is_err());
    }

    #[test]
    fn a_show_on_the_system_database_rejects_graph_reads() {
        for query in [
            "USE system SHOW PROCEDURES YIELD * RETURN COUNT{()--()}",
            "USE system SHOW PROCEDURES YIELD name WHERE EXISTS { ()--() } RETURN name",
            "USE system SHOW SETTINGS YIELD name WHERE COLLECT { MATCH (a) RETURN a } <> [] RETURN name",
            "USE system SHOW PROCEDURES YIELD name WHERE ()-->() RETURN name",
            "USE system SHOW FUNCTIONS YIELD name WHERE exists(()--()) RETURN name",
            "USE system SHOW PROCEDURES YIELD * RETURN [(a)--(b) | a]",
        ] {
            let err = lower(query).expect_err(&format!("`{query}` must be rejected"));
            assert!(err.contains("system database"), "{query}: {err}");
        }
        // Plain filtering on the system database stays legal.
        assert!(lower(
            "USE system SHOW PROCEDURES YIELD name WHERE name STARTS WITH 'db.' RETURN name"
        )
        .is_ok());
    }

    #[test]
    fn a_non_use_routing_prefix_agrees_across_the_two_system_rules() {
        // Both rules read the one `show_targets_system` predicate: a `WITH`
        // before the routing `USE` means neither treats the statement as
        // routed to `system` (the removed second derivation accepted any
        // `USE` before the first catalog command).
        for query in [
            "WITH 1 AS x USE system SHOW SETTINGS YIELD name \
             WHERE COUNT { () } > 0 RETURN name",
            "WITH 1 AS x USE system SHOW INDEXES YIELD name RETURN name",
        ] {
            let stmt = relate_parser::parse(query).expect("parses");
            assert!(
                super::validate_show_composability(&stmt).is_ok(),
                "{query}: composability must not treat the statement as system-routed"
            );
            assert!(
                super::validate_show_system_graph_reads(&stmt).is_ok(),
                "{query}: the graph-read rule must agree"
            );
        }
    }

    #[test]
    fn system_graph_reads_are_judged_per_union_arm() {
        // The second arm routes to `system`, so its graph read is rejected
        // even though the first arm (routed to `neo4j`) carries none.
        let err = lower(
            "USE neo4j SHOW PROCEDURES YIELD name RETURN 1 AS c \
             UNION USE system SHOW PROCEDURES YIELD name RETURN COUNT { () } AS c",
        )
        .expect_err("the system-routed arm's graph read must be rejected");
        assert!(err.contains("system database"), "{err}");

        // A graph read in the arm routed to `neo4j` is not judged by the
        // sibling `system` arm's routing.
        assert!(lower(
            "USE neo4j SHOW PROCEDURES YIELD name RETURN COUNT { () } AS c \
             UNION USE system SHOW PROCEDURES YIELD name RETURN 1 AS c",
        )
        .is_ok());
    }

    #[test]
    fn show_skip_and_limit_expressions_are_scope_checked() {
        // Every paging stage carries an expression; an unknown name in any
        // of them is `UndefinedVariable`, never a silently null offset.
        for query in [
            "SHOW CONSTRAINTS YIELD name ORDER BY nope RETURN name",
            "SHOW CONSTRAINTS YIELD name SKIP nope RETURN name",
            "SHOW CONSTRAINTS YIELD name LIMIT nope RETURN name",
        ] {
            let err = lower(query).expect_err(&format!("`{query}` must be rejected"));
            assert!(err.contains("UndefinedVariable"), "{query}: {err}");
        }
    }
}

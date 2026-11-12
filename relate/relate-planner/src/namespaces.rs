//! ADR-0116 namespace classifiers: the §2 database-administration
//! classifiers (the admin-command classifier and the `SHOW DATABASES` +
//! `NEXT` rule), the §4 `system` gate and the statement read/write
//! classification they build on. This module is the one home for the
//! namespace rules — the sibling §3 `USE` rules live in
//! [`crate::use_target`]; the facade (`relate::connection`) calls here
//! and keeps only `CALL`-shape validation. The `system_ontologies` gate is
//! ADR-0229's — every non-administration statement routed there fails with
//! one fixed text, so it needs no classifier here (the facade raises it in
//! `Connection::namespace_gate_error`).
//!
//! The procedure-aware check ([`plan_calls_write_procedure`]) takes the
//! write-procedure predicate as an argument: the canonical procedure
//! signature registry lives in the executor, which depends on this crate
//! (never the reverse).

use relate_ir::Clause;
use relate_parser::DatabaseFilter;

use crate::clause_kind::ir_clause_keyword;
use crate::LogicalPlan;

/// The write classifier is a projection of the one shared clause fold
/// ([`crate::access::plan_access`]): each part's TOP-LEVEL clauses (plus
/// `NEXT` stages and `WHEN`/`ELSE` branch bodies) only — it deliberately
/// does NOT descend into `CALL { … }` bodies or expression subqueries,
/// because the fold already classifies a top-level `CALL { … }` as a
/// write outright and a read-only `CALL { … }` nested in an
/// `EXISTS`/`COUNT`/`COLLECT` expression must not reclassify the statement
/// as a write. Schema (index/constraint DDL) counts as a write here; see
/// [`crate::access::Access::is_write`].
pub fn is_write(plan: &LogicalPlan) -> bool {
    crate::access::plan_access(plan, &mut |_| false).is_write()
}

/// Whether any `CALL` of a write procedure appears anywhere in the plan.
/// [`crate::walk_plan_clauses`] descends into `CALL { … }` subqueries,
/// `WHEN`/`ELSE` branches, `FOREACH` bodies, expression subqueries and
/// `NEXT` stages, so a write procedure nested in any of those positions
/// still counts — the query-type classification reads it
/// ([`crate::query_type`]).
pub fn plan_calls_write_procedure(
    plan: &LogicalPlan,
    is_write_procedure: &mut impl FnMut(&str) -> bool,
) -> bool {
    let mut calls_write = false;
    crate::walk_plan_clauses(plan, &mut |clause| {
        if let Clause::Call { name, .. } = clause {
            calls_write |= name.with_str(|text| is_write_procedure(text));
        }
    });
    calls_write
}

/// ADR-0116 §2: the database-administration `Catalog` vocabulary, matched
/// once. [`is_database_admin_command`] asks whether a clause is one of the
/// two commands; the `SHOW DATABASES` + `NEXT` rule
/// ([`is_show_databases_with_next`]) and ADR-0590's composition rule
/// ([`composed_show_databases`]) ask for a `SHOW DATABASES` spelling
/// specifically.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DatabaseAdminCommand {
    ShowDatabases(ShowDatabasesSpelling),
    AlterDatabase,
}

/// The `SHOW DATABASE[S]` spelling a clause is — the four CE refuses
/// composed, each named as written. The parser's clause-keyword table names
/// the clause *kind* (always the plural), so the spelling has to come from
/// the clause's own filter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShowDatabasesSpelling {
    /// Bare `SHOW DATABASE[S]` — every database.
    All,
    /// `SHOW DATABASE <name>` — the singular form.
    Named,
    /// `SHOW DEFAULT DATABASE`.
    Default,
    /// `SHOW HOME DATABASE`.
    Home,
}

impl ShowDatabasesSpelling {
    /// The spelling as CE's rejection carries it (live 2026.10.1).
    fn label(self) -> &'static str {
        match self {
            ShowDatabasesSpelling::All => "SHOW DATABASES",
            ShowDatabasesSpelling::Named => "SHOW DATABASE",
            ShowDatabasesSpelling::Default => "SHOW DEFAULT DATABASE",
            ShowDatabasesSpelling::Home => "SHOW HOME DATABASE",
        }
    }
}

/// ADR-0116 §2: classify a parser `Catalog` command as one of the
/// database-administration commands, or `None` for every other `Catalog`
/// clause. Matched once here, lowered once per clause by
/// [`admin_command_clause`] — every §2 classifier reads one of the two.
fn database_admin_command(command: &relate_parser::Clause) -> Option<DatabaseAdminCommand> {
    use relate_parser::Clause as C;
    match command {
        C::ShowDatabases(show) => Some(DatabaseAdminCommand::ShowDatabases(match show.filter {
            DatabaseFilter::All => ShowDatabasesSpelling::All,
            DatabaseFilter::Named(_) => ShowDatabasesSpelling::Named,
            DatabaseFilter::Default => ShowDatabasesSpelling::Default,
            DatabaseFilter::Home => ShowDatabasesSpelling::Home,
        })),
        C::AlterDatabase(_) => Some(DatabaseAdminCommand::AlterDatabase),
        _ => None,
    }
}

/// ADR-0116 §2: the §2 command a top-level IR clause is, or `None` for
/// every other clause — the one clause-level lowering every §2 classifier
/// reads: [`is_database_admin_command`]'s all-quantifier,
/// [`is_show_databases_with_next`]'s and [`composed_show_databases`]'s
/// `SHOW DATABASES` refinement, [`admin_command_heads_statement`]'s head
/// probe and the §4 composability walk's containment test all go through
/// it, so the vocabulary is matched once.
fn admin_command_clause(clause: &Clause) -> Option<DatabaseAdminCommand> {
    match clause {
        Clause::Catalog(command) => database_admin_command(command.as_ref()),
        _ => None,
    }
}

/// ADR-0116 §2: database-administration commands run from any namespace and
/// execute against the store's catalog (the `neo4j` graph). A plan qualifies
/// when every clause is a database-administration `Catalog` command.
pub fn is_database_admin_command(plan: &LogicalPlan) -> bool {
    !plan.parts.is_empty()
        && plan.next.is_none()
        && plan
            .parts
            .iter()
            .flatten()
            .all(|clause| admin_command_clause(clause).is_some())
}

/// ADR-0590: the command spelling for a plan that *composes* `SHOW
/// DATABASES` with other clauses, or `None` when every occurrence stands in
/// the canonical shape (the command alone, or with a trailing `RETURN` over
/// its yielded columns, as the statement's whole body). Off the `system`
/// namespace such a plan is CE's 51N28 `NotSystemDatabaseError`; on `system`
/// the command is where it belongs and §4's clause whitelist governs
/// instead.
///
/// `SHOW DATABASES` is the whole subject: the other §2 command, `ALTER
/// DATABASE`, cannot be composed at all — the front end rejects every
/// composed form of it (`WITH … ALTER …` and `MATCH … ALTER …` as an
/// invalid composition, the trailing-`RETURN`/`NEXT` forms as 51N27), and
/// live's grammar does not parse them either. The spelling returned is the
/// one live refuses — see [`show_databases_label`].
///
/// Part-granular over each part's TOP-LEVEL clauses, the granularity the §4
/// whitelist and the `NEXT` backstop already use. A part's command is
/// composed when anything substantive precedes it (a leading routing `USE`
/// run is not composition), when its part is not the statement's only one (a
/// `UNION` arm is never the standalone shape), when a `NEXT` stage follows,
/// or when a clause other than a trailing `RETURN` follows it. Live
/// 2026.10.1 raises the same rejection for the head and the mid-body
/// position alike.
pub fn composed_show_databases(plan: &LogicalPlan) -> Option<&'static str> {
    let standalone = plan.parts.len() == 1 && plan.next.is_none();
    for part in plan.parts() {
        let mut clauses = part
            .iter()
            .skip_while(|clause| crate::clause_kind::clause_is_routing(clause));
        let Some((index, label)) = clauses
            .by_ref()
            .enumerate()
            .find_map(|(index, clause)| show_databases_label(clause).map(|label| (index, label)))
        else {
            continue;
        };
        let composed = !standalone
            || index != 0
            || clauses.any(|clause| !matches!(clause, Clause::Return { .. }));
        if composed {
            return Some(label);
        }
    }
    None
}

/// The `SHOW DATABASE[S]` spelling a clause is, or `None` for every other
/// clause — the one `SHOW DATABASES` test the §2 classifiers share, and the
/// command name CE's rejection carries. A projection of the vocabulary
/// match, never a second reading of the clause.
fn show_databases_label(clause: &Clause) -> Option<&'static str> {
    match admin_command_clause(clause)? {
        DatabaseAdminCommand::ShowDatabases(spelling) => Some(spelling.label()),
        DatabaseAdminCommand::AlterDatabase => None,
    }
}

/// ADR-0116 §2 / CIP-213 NE1.4: whether the plan combines `SHOW DATABASES`
/// with a `NEXT` stage. The front-end's NEXT-chain validation rejects the
/// combination before planning; this classifier is the execution-phase
/// backstop the facade checks after its EXPLAIN early return (EXPLAIN plans
/// without executing, so the rule does not apply there). Part-granular over
/// every stage's TOP-LEVEL clauses, matching the front-end's per-body scan:
/// a `SHOW DATABASES` nested in an expression subquery does not participate
/// in a `NEXT` chain.
pub fn is_show_databases_with_next(plan: &LogicalPlan) -> bool {
    plan.next.is_some()
        && plan.parts().any(|part| {
            part.iter()
                .any(|clause| show_databases_label(clause).is_some())
        })
}

/// ADR-0116 §2, amended 2026-11-04: whether the statement's body is
/// *headed* by a database-administration command — the first top-level
/// clause after any leading `USE` clauses is one of the §2 commands. This
/// is the shape the 42N79 auto-routing gate is scoped to: every headed
/// spelling under a non-system `USE` (bare, `YIELD … RETURN …`, `UNION`
/// arm, `ALTER DATABASE`) is 42N79. A command later in the body (after
/// e.g. a `MATCH`, or behind a leading `WITH`) is not headed, so that gate
/// passes it through to the namespace gates — where, off `system`, it is
/// the composed-form rejection instead (ADR-0590). Part-granular on the
/// head part only: a command headed in a `NEXT` stage is already refused
/// by the NEXT-chain rule ([`is_show_databases_with_next`]), and `USE`
/// clauses never split parts.
pub fn admin_command_heads_statement(plan: &LogicalPlan) -> bool {
    plan.parts.first().is_some_and(|head| {
        head.iter()
            .find(|clause| !matches!(clause, Clause::Use { .. }))
            .and_then(admin_command_clause)
            .is_some()
    })
}

/// ADR-0116 §4: on a `system` session only SHOW commands, supported
/// administration commands and standalone procedure calls
/// (`CALL … [YIELD …] [RETURN …]`) run. Returns the offending clause kinds
/// (deduplicated, in order of appearance) for CE's 42N17 text, or `None`
/// when the statement is allowed.
pub fn unsupported_system_clause_names(plan: &LogicalPlan) -> Option<String> {
    let mut offending: Vec<&'static str> = Vec::new();
    // Part-granular coverage is deliberate: only each part's TOP-LEVEL
    // clauses (plus `NEXT` stages, via `walk_plan_parts`) are judged — a
    // read MATCH nested in an expression subquery attached to a standalone
    // CALL is allowed on `system`. `walk_plan_clauses` would descend into
    // those nested positions and is the wrong walk here.
    //
    // Deliberate contrast with the facade's `validate_calls`, which walks
    // the FULL clause tree (`walk_plan_clauses`) so a `CALL` nested in a
    // `CALL { … }` subquery is still validated against the procedure
    // registry: 42N17 judges the statement's top-level shape, while CALL
    // validation must cover every call site. Do not "unify" the two walks.
    crate::walk_plan_parts(plan, &mut |clauses| {
        collect_system_offenders(clauses, &mut offending);
    });
    if offending.is_empty() {
        None
    } else {
        Some(offending.join(", "))
    }
}

/// ADR-0116 §4: the tail-clause keywords CE suppresses from the 42N17
/// offender list when a non-tail offender is present. Encoded ONCE — both
/// the offender collection (`is_tail` below) and the suppression
/// (`partition` below) read this list, so the two can never drift apart.
const SYSTEM_TAIL_KEYWORDS: &[&str] = &["RETURN", "WITH", "FINISH"];

fn collect_system_offenders(clauses: &[Clause], out: &mut Vec<&'static str>) {
    // A RETURN/WITH/FINISH is only allowed as the tail of a SHOW or a
    // standalone procedure call in the same query part. The anchor set is
    // encoded ONCE: both `has_show_or_call` and `allowed` read
    // `is_system_head`, so the two can never drift apart. `Use` is
    // deliberately absent — it is allowed on `system` but does not anchor
    // a tail.
    let is_system_head =
        |clause: &Clause| is_system_show(clause) || matches!(clause, Clause::Call { .. });
    let has_show_or_call = clauses.iter().any(&is_system_head);
    let is_tail = |clause: &Clause| SYSTEM_TAIL_KEYWORDS.contains(&ir_clause_keyword(clause));
    let allowed = |clause: &Clause| {
        is_system_head(clause)
            || matches!(clause, Clause::Use { .. })
            || (is_tail(clause) && has_show_or_call)
    };
    // CE lists only the clause kinds outside its supported
    // CALL/YIELD/RETURN structure: `MATCH (n) RETURN count(n)` reports
    // `MATCH`, not `MATCH, RETURN` (testdata/ce-errors/neo4j-ce.json
    // `system_data_clause`). A lone tail offender (`RETURN 1`) still
    // reports itself.
    let (non_tail, tail): (Vec<&'static str>, Vec<&'static str>) = clauses
        .iter()
        .filter(|clause| !allowed(clause))
        .map(ir_clause_keyword)
        .partition(|name| !SYSTEM_TAIL_KEYWORDS.contains(name));
    let part_offenders = if non_tail.is_empty() { tail } else { non_tail };
    for name in part_offenders {
        if !out.contains(&name) {
            out.push(name);
        }
    }
}

/// SHOW commands and database-administration commands (ADR-0116 §2) are the
/// only `Catalog` clauses allowed on `system`.
fn catalog_allowed_on_system(command: &relate_parser::Clause) -> bool {
    use relate_parser::Clause as C;
    matches!(
        command,
        C::ShowFunctions(_)
            | C::ShowConstraints(_)
            | C::ShowIndexes(_)
            | C::ShowDatabases(_)
            | C::ShowRoles(_)
            | C::ShowServers(_)
            | C::ShowUsers(_)
            | C::ShowProcedures(_)
            | C::ShowSettings(_)
            | C::ShowTransactions(_)
            | C::ShowGraphType(_)
            | C::ShowCurrentGraphType(_)
            | C::AlterDatabase(_)
    )
}

/// The clauses a `system` query part treats as its SHOW head: every catalog
/// command the namespace admits, plus `SHOW INDEXES` (its own IR variant).
/// The one predicate both the §4 valid-head set (`collect_system_offenders`,
/// which ORs in `Call`) and `composed_show_disallowed_clause`'s composition
/// test read.
fn is_system_show(clause: &Clause) -> bool {
    match clause {
        Clause::Catalog(command) => catalog_allowed_on_system(command.as_ref()),
        Clause::ShowIndexes => true,
        _ => false,
    }
}

/// ADR-0116 §4: the clause kind CE forbids in a query part that composes a
/// SHOW command on the `system` namespace — `WITH`, the one tail clause the
/// §4 whitelist (`collect_system_offenders`) still allows after a SHOW's
/// `YIELD`. The composition test is [`is_system_show`], the same predicate
/// the whitelist's head set reads. Live CE 2026.09/2026.10 rejects the
/// composed command with
/// `The following clauses are not allowed on a system database: WITH.`
/// (GQLSTATUS 42NA9) for both the `… YIELD … WITH …` and the
/// `WITH … <command> …` spelling (position within the part is not part of
/// the rule); the other §4 offenders keep CE's 42N17 text. Scope is every
/// catalog command the `system` namespace admits (`SHOW PROCEDURES`,
/// `SHOW FUNCTIONS`, `SHOW SETTINGS`, `SHOW CURRENT GRAPH TYPE`,
/// `SHOW TRANSACTIONS`, …), not only the §2 administration commands.
/// Returns the offending clause's keyword, or `None` when no part composes
/// a SHOW with a `WITH`.
///
/// The caller applies this only under `Namespace::System`; off-system
/// namespaces compose `SHOW … WITH … RETURN` freely.
///
/// The engine's 42N17 text still lists an `UNWIND`/`MATCH` tail after a
/// SHOW's `YIELD` (the whitelist above rejects it); live CE words those
/// with this family's text instead — same outcome, different message,
/// recorded in ADR-0116 §4's Later references.
pub fn composed_show_disallowed_clause(plan: &LogicalPlan) -> Option<&'static str> {
    let mut offending = None;
    crate::walk_plan_parts(plan, &mut |clauses| {
        let composes_show = clauses.iter().any(is_system_show);
        if !composes_show {
            return;
        }
        for clause in clauses {
            if offending.is_none() && matches!(clause, Clause::With { .. }) {
                offending = Some(ir_clause_keyword(clause));
            }
        }
    });
    offending
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_of;

    #[test]
    fn writes_classify_as_writes() {
        for cypher in [
            "CREATE (n)",
            "MERGE (n:N {id: 1})",
            "MATCH (n) SET n.x = 1",
            "MATCH (n) DELETE n",
            "MATCH (n) REMOVE n.x",
            "CREATE INDEX i FOR (n:N) ON (n.p)",
            "CREATE CONSTRAINT c FOR (n:N) REQUIRE n.id IS UNIQUE",
            "FOREACH (x IN [1] | CREATE (n))",
        ] {
            assert!(is_write(&plan_of(cypher)), "{cypher}");
        }
    }

    #[test]
    fn reads_classify_as_reads() {
        for cypher in [
            "RETURN 1 AS n",
            "MATCH (n) RETURN n",
            "SHOW INDEXES",
            "CALL db.labels()",
        ] {
            assert!(!is_write(&plan_of(cypher)), "{cypher}");
        }
    }

    #[test]
    fn read_only_call_in_expression_subqueries_classifies_as_read() {
        // Regression: `is_write` must not descend into expression
        // subqueries — a read-only `CALL { … }` there is legal (plan-time
        // validation only rejects actual writes), and classifying the
        // statement as a write would reject a pure read on the read-only
        // `system_ontologies` namespace and misroute it on `neo4j`.
        for cypher in [
            "RETURN EXISTS { CALL { RETURN 1 AS x } } AS x",
            "RETURN COUNT { CALL { RETURN 1 AS x } } AS x",
            "RETURN COLLECT { CALL { RETURN 1 AS x } RETURN x } AS x",
        ] {
            assert!(!is_write(&plan_of(cypher)), "{cypher}");
        }
    }

    #[test]
    fn nested_queries_recurse_through_one_predicate() {
        // WHEN branches, ELSE bodies and NEXT tails are all recursed into
        // by `is_write`'s shared fold (`access::plan_access`), so the one
        // clause classification classifies them.
        assert!(is_write(&plan_of("WHEN true THEN { CREATE (n) }")));
        assert!(!is_write(&plan_of(
            "WHEN true THEN { RETURN 1 AS x } ELSE { RETURN 2 AS x }"
        )));
        assert!(is_write(&plan_of(
            "WHEN false THEN { MATCH (n) RETURN count(n) AS x } \
             ELSE { CREATE (m) RETURN 0 AS x }"
        )));
        assert!(is_write(&plan_of("RETURN 1 AS x NEXT CREATE (n)")));
        assert!(!is_write(&plan_of("RETURN 1 AS x UNION RETURN 2 AS x")));
    }

    #[test]
    fn write_procedure_calls_are_found_anywhere_in_the_clause_tree() {
        // The facade's predicate classifies `db.createLabel` as a write
        // (it is flagged `writes` in the canonical registry); the tests
        // here stand in with a name predicate.
        let is_write_proc = &mut |name: &str| name == "db.createLabel";
        for cypher in [
            "CALL db.createLabel('X')",
            "WHEN true THEN { CALL db.createLabel('X') }",
            "WHEN false THEN { RETURN 1 AS x } ELSE { CALL db.createLabel('X') RETURN 1 AS x }",
            "CALL { CALL db.createLabel('X') }",
            // A FOREACH body admits only updating clauses, so a procedure
            // call cannot appear there (42I01, live Neo4j).
            "RETURN 1 AS x NEXT CALL db.createLabel('X')",
        ] {
            assert!(
                plan_calls_write_procedure(&plan_of(cypher), is_write_proc),
                "{cypher}"
            );
        }
        // Read procedures and write-free statements still classify as reads.
        for cypher in [
            "CALL db.labels()",
            "WHEN true THEN { CALL db.labels() YIELD label RETURN label }",
            "RETURN 1 AS x",
        ] {
            assert!(
                !plan_calls_write_procedure(&plan_of(cypher), is_write_proc),
                "{cypher}"
            );
        }
    }

    #[test]
    fn database_admin_commands_classify() {
        assert!(is_database_admin_command(&plan_of("SHOW DATABASES")));
        assert!(is_database_admin_command(&plan_of(
            "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25"
        )));
        assert!(!is_database_admin_command(&plan_of("MATCH (n) RETURN n")));
        assert!(!is_database_admin_command(&plan_of("SHOW INDEXES")));
        // A non-administrative clause disqualifies the statement.
        assert!(!is_database_admin_command(&plan_of(
            "SHOW DATABASES YIELD name RETURN name"
        )));
    }

    /// A lowered `SHOW DATABASES` catalog clause. The front-end's
    /// NEXT-chain validation rejects the command in a chain before
    /// planning, so the classifier's chain inputs are assembled directly
    /// here — this is exactly the shape the execution-phase backstop sees.
    fn show_databases_clause() -> Clause {
        Clause::Catalog(Box::new(relate_parser::Clause::ShowDatabases(
            relate_parser::ShowDatabasesClause {
                filter: relate_parser::DatabaseFilter::All,
                yields: Default::default(),
                where_expr: None,
                stages: Vec::new(),
            },
        )))
    }

    #[test]
    fn composed_show_databases_follows_ce() {
        // The canonical shapes: the command alone, with a trailing RETURN
        // over its own yielded columns, and under a leading routing `USE`
        // run — which routes the statement but is not composition.
        for cypher in [
            "SHOW DATABASES",
            "SHOW DATABASES YIELD name RETURN name",
            "USE neo4j SHOW DATABASES",
            "USE system SHOW DATABASES YIELD name RETURN name",
            // The other §2 command cannot be composed at all (the front end
            // rejects every composed form), so it never classifies here.
            "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25",
            "MATCH (n) RETURN n",
        ] {
            assert_eq!(composed_show_databases(&plan_of(cypher)), None, "{cypher}");
        }
        // Composed: an intervening clause after the YIELD, a clause before
        // the command, another catalog command ahead of it (the harness's
        // `SHOW FUNCTIONS … SHOW DATABASES …` scenario), a UNION arm, and a
        // NEXT stage — live 2026.10.1 rejects each with the command named.
        for cypher in [
            "SHOW DATABASES YIELD name WITH 1 AS n RETURN n",
            "SHOW DATABASES YIELD name MATCH (n) RETURN n",
            "MATCH (n) SHOW DATABASES YIELD name RETURN name",
            "SHOW FUNCTIONS YIELD name AS function SHOW DATABASES YIELD name AS database RETURN *",
            "UNWIND ['a'] AS name RETURN name AS x \
             UNION SHOW DATABASES YIELD name RETURN name AS x",
        ] {
            assert_eq!(
                composed_show_databases(&plan_of(cypher)),
                Some("SHOW DATABASES"),
                "{cypher}"
            );
        }
        // Every `SHOW DATABASE[S]` spelling is refused composed, and the
        // message carries the spelling live refuses (verified 2026.10.1).
        for (cypher, label) in [
            (
                "SHOW DEFAULT DATABASE YIELD name WITH 1 AS n RETURN n",
                "SHOW DEFAULT DATABASE",
            ),
            (
                "SHOW HOME DATABASE YIELD name WITH 1 AS n RETURN n",
                "SHOW HOME DATABASE",
            ),
            (
                "SHOW DATABASE neo4j YIELD name WITH 1 AS n RETURN n",
                "SHOW DATABASE",
            ),
        ] {
            assert_eq!(
                composed_show_databases(&plan_of(cypher)),
                Some(label),
                "{cypher}"
            );
        }
        // The `NEXT` condition is defensive — the front-end NEXT-chain rule
        // rejects that combination before planning — so it is pinned on a
        // spliced plan, the way the sibling `NEXT` classifier's test does.
        let mut chained = plan_of("RETURN 1 AS x NEXT RETURN x");
        chained.parts[0] = vec![show_databases_clause()];
        assert_eq!(composed_show_databases(&chained), Some("SHOW DATABASES"));
    }

    #[test]
    fn show_databases_with_next_classifies_the_chain() {
        // The command in the head stage.
        let mut head = plan_of("RETURN 1 AS x NEXT RETURN x");
        head.parts[0] = vec![show_databases_clause()];
        assert!(is_show_databases_with_next(&head));

        // The command in a NEXT stage: `parts()` spans every stage.
        let mut tail = plan_of("RETURN 1 AS x NEXT RETURN x");
        tail.next.as_mut().unwrap().parts[0] = vec![show_databases_clause()];
        assert!(is_show_databases_with_next(&tail));

        // SHOW DATABASES without a NEXT stage is the plain §2 command.
        let mut plain = plan_of("RETURN 1 AS x");
        plain.parts[0] = vec![show_databases_clause()];
        assert!(!is_show_databases_with_next(&plain));

        // ALTER DATABASE is §2 but not a SHOW DATABASES chain participant.
        let mut alter = plan_of("ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25");
        alter.next = Some(Box::new(plan_of("RETURN 1 AS x")));
        assert!(!is_show_databases_with_next(&alter));

        // An ordinary NEXT chain does not classify.
        assert!(!is_show_databases_with_next(&plan_of(
            "RETURN 1 AS x NEXT RETURN x + 1 AS y"
        )));
    }

    #[test]
    fn system_offender_names_follow_ce() {
        // Data clauses offend; CE suppresses the tail when a non-tail
        // offender is present.
        assert_eq!(
            unsupported_system_clause_names(&plan_of("MATCH (n) RETURN count(n) AS c")),
            Some("MATCH".to_string())
        );
        // A lone tail offender still reports itself.
        assert_eq!(
            unsupported_system_clause_names(&plan_of("RETURN 1 AS x")),
            Some("RETURN".to_string())
        );
        // Standalone procedure calls, SHOW commands and administration
        // commands are allowed.
        for cypher in [
            "CALL db.labels()",
            "CALL db.labels() YIELD label RETURN label",
            "SHOW INDEXES",
            "SHOW DATABASES",
        ] {
            assert_eq!(
                unsupported_system_clause_names(&plan_of(cypher)),
                None,
                "{cypher}"
            );
        }
    }

    /// ADR-0116 §4: a `WITH` in a part that composes any SHOW command — the
    /// classifier is namespace-blind (the facade gate applies it to
    /// `system` only), and position within the part is not part of the rule
    /// (live CE rejects the `WITH … <command> …` spelling with the same
    /// text).
    #[test]
    fn composed_show_disallowed_clause_flags_with_around_show_commands() {
        for cypher in [
            "SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
            "USE system SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
            "WITH 1 AS n SHOW DATABASES YIELD name RETURN name",
            "SHOW DEFAULT DATABASE YIELD name WITH 1 AS n RETURN n",
            "SHOW HOME DATABASE YIELD name WITH 1 AS n RETURN n",
            "SHOW DATABASE neo4j YIELD name WITH 1 AS n RETURN n",
            "SHOW PROCEDURES YIELD name AS p1 WITH 1 AS n RETURN n",
            "SHOW FUNCTIONS YIELD name AS f1 WITH 1 AS n RETURN n",
            "SHOW SETTINGS YIELD name AS s1 WITH 1 AS n RETURN n",
            "SHOW CURRENT GRAPH TYPE YIELD specification AS spec1 WITH 1 AS n RETURN n",
        ] {
            assert_eq!(
                composed_show_disallowed_clause(&plan_of(cypher)),
                Some("WITH"),
                "{cypher}"
            );
        }
        // No SHOW composed: the tail keeps its `WITH` composability.
        for cypher in [
            "CALL db.labels() YIELD label WITH 1 AS n RETURN n",
            "WITH 1 AS n RETURN n",
        ] {
            assert_eq!(
                composed_show_disallowed_clause(&plan_of(cypher)),
                None,
                "{cypher}"
            );
        }
        // Administration commands without a `WITH` stay composable with
        // their CE tails (WHERE/RETURN live in or after the command).
        for cypher in [
            "SHOW DATABASES",
            "SHOW DATABASES YIELD name RETURN name",
            "USE system SHOW DEFAULT DATABASE YIELD name AS defaultDb SHOW DATABASES YIELD name AS db, access WHERE defaultDb = db RETURN *",
            "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25",
        ] {
            assert_eq!(
                composed_show_disallowed_clause(&plan_of(cypher)),
                None,
                "{cypher}"
            );
        }
    }

    /// ADR-0116 §2 amendment (2026-11-04): the statement-body head shape
    /// CE's 42N79 rule guards — first clause after any leading `USE` is a
    /// database-administration command.
    #[test]
    fn admin_command_heads_statement_classifies_the_head() {
        for cypher in [
            "SHOW DATABASES",
            "USE system SHOW DATABASES",
            "USE neo4j SHOW DATABASES",
            "USE neo4j SHOW DEFAULT DATABASE",
            "USE neo4j SHOW HOME DATABASE",
            "USE neo4j SHOW DATABASE neo4j",
            "USE neo4j ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 5",
            "USE neo4j SHOW DATABASES YIELD name RETURN name",
            // A UNION arm's head counts through the statement's own head
            // part (live CE 2026.09: 42N79 for this shape).
            "USE neo4j SHOW DATABASES YIELD name AS a RETURN a UNION SHOW DATABASES YIELD name AS a RETURN a",
        ] {
            assert!(
                admin_command_heads_statement(&plan_of(cypher)),
                "{cypher}"
            );
        }
        for cypher in [
            "MATCH (n) RETURN n",
            // A command later in the body is not headed: the 42N79 gate
            // passes it to the namespace gates, where off-system it is the
            // composed-form rejection (ADR-0590, live 2026.10.1).
            "USE neo4j MATCH (n) SHOW DATABASES YIELD name RETURN name",
            // A leading WITH heads the body instead — same outcome, the
            // other rule.
            "USE neo4j WITH 1 AS n SHOW DATABASES YIELD name RETURN name",
            "SHOW INDEXES",
        ] {
            assert!(!admin_command_heads_statement(&plan_of(cypher)), "{cypher}");
        }
    }
}

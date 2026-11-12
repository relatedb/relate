//! Neo4j Community Edition error texts for the fixed namespaces
//! (ADR-0116 §2-§4, §9; ADR-0146).
//!
//! Each formatter returns the error *class* its CE text belongs to: the
//! `SemanticError` family ([`Error::Semantic`]) renders
//! `"semantic error: <Kind>: <CE text>"`, while the arity formatter
//! ([`graph_selector_arity_error`]) is Neo4j's 42I13 under top-level 42001 —
//! a `SyntaxError` ([`Error::Syntax`]) rendering `"syntax error: …"`. The
//! kind token stays recoverable by the bindings' name scan
//! (`relate::binding_error_metadata`) in both cases. The
//! unknown-graph pair ([`graph_not_found`], [`database_does_not_exist`])
//! returns the typed [`Error::DatabaseNotFound`] with the same rendering
//! (ADR-0307), so surfaces map it without the scan. These
//! functions are the single place each CE text lives; later nodes (planner,
//! executor, surfaces) must call them rather than restating the text. The
//! `<Kind>` token is never a literal here: it comes from the typed error
//! model (`ErrorKind::as_str`, or `TckErrorClass::SemanticError.as_str()`
//! for the class-level `SemanticError` token, which has no `ErrorKind`
//! variant) so the wire token and the typed model cannot drift (ADR-0149).

use crate::error_kinds::{ErrorKind, TckErrorClass};

use crate::Error;

/// ADR-0116 §2: every database administration command other than the
/// supported CE set. The statement is echoed verbatim, without a leading
/// `CYPHER n`.
pub fn unsupported_administration_command(statement: &str) -> Error {
    Error::semantic(format!(
        "{}: Unsupported administration command: {statement}",
        ErrorKind::UnsupportedAdministrationCommand.as_str()
    ))
}

/// ADR-0116 §3: a static `USE` target that is not one of the fixed
/// namespaces. The typed [`Error::DatabaseNotFound`] (ADR-0307), whose
/// Display renders `semantic error: DatabaseNotFound: Graph not found:
/// <name>` from the CE text in `error.rs`.
pub fn graph_not_found(name: &str) -> Error {
    Error::database_not_found(name)
}

/// ADR-0116 §3: `graph.byName`, `graph.names` and `graph.propertiesByName`
/// are only valid on composite databases (GQLSTATUS 42N72). `attempted` is
/// the attempted graph reference as written (e.g. `graph.byName('neo4j')`),
/// echoed on CE's second line. The CE wording is pinned by
/// `testdata/ce-errors/neo4j-ce.json` (`use_graph_byname`).
pub fn composite_only_graph_function(attempted: &str) -> Error {
    Error::semantic(format!(
        "{}: Dynamic graph lookup not allowed here. \
         This feature is only available on composite databases.\n\
         Attempted to access graph {attempted}",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0116 §3 / 42I13: a `graph.*` selector call with the wrong argument
/// count. The arity failure is reported before the composite-database
/// refusal (`USE graph.byElementId()` with no argument), and the one
/// spelling is shared by the planner's `USE`-target resolver and its
/// expression checker. `attempted` is the call as written
/// (`graph.byElementId()`).
pub fn graph_selector_arity_error(attempted: &str, expected: usize, got: usize) -> Error {
    Error::syntax(format!(
        "{}: {attempted} expects {expected} argument(s), got {got}",
        ErrorKind::InvalidNumberOfArguments.as_str()
    ))
}

/// The one arity guard for a `graph.*` selector call: `attempted` is the
/// call as written, and the selector's expected count comes from
/// [`GRAPH_SELECTOR_FUNCTIONS`](crate::GRAPH_SELECTOR_FUNCTIONS) — so a
/// fifth selector cannot silently acquire arity 1. Both planner sites (the
/// `USE`-target resolver and the expression checker) call this, and a
/// correct arity is a no-op (the composite-database refusal is theirs).
pub fn check_graph_selector_arity(attempted: &str, name: &str, got: usize) -> Result<(), Error> {
    match crate::graph_selector_arity(name) {
        Some(expected) if got != expected => {
            Err(graph_selector_arity_error(attempted, expected, got))
        }
        _ => Ok(()),
    }
}

/// ADR-0116 §3: different graph targets in one statement (across `UNION`
/// arms, `CALL` subqueries or `NEXT`). `attempted` is the second distinct
/// target, echoed on CE's second line (`testdata/ce-errors/neo4j-ce.json`
/// `multi_graph_union`/`multi_graph_call`).
pub fn multiple_graphs(attempted: &str) -> Error {
    // CE's own class is Neo.ClientError.Statement.SyntaxError (42NA5 under
    // 42001), and its message carries no class tag; the front end rejects the
    // statement, so it is the SyntaxError class here too.
    Error::syntax(format!(
        "Multiple graphs in the same query not allowed here. \
         This feature is only available on composite databases.\n\
         Attempted to access graph {attempted}"
    ))
}

/// ADR-0116 §3: `graph.byElementId` with an id whose middle segment names no
/// namespace (GQLSTATUS 42NA7, "fails as in Neo4j"). `id` is the element id
/// as written, echoed inside CE's backticks
/// (`testdata/ce-errors/neo4j-ce.json` `byelementid_unknown`).
pub fn element_id_no_namespace(id: &str) -> Error {
    Error::semantic(format!(
        "{}: No database is corresponding to `graph.byElementId({id})`. \
         Verify that the elementId is correct.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0116 §4: anything other than SHOW commands, supported administration
/// commands and standalone procedure calls in the `system` namespace
/// (GQLSTATUS 42N17). CE text pinned by `testdata/ce-errors/neo4j-ce.json`
/// (`system_data_clause`); note the space before the newline is CE's.
pub fn unsupported_system_clauses(clauses: &str) -> Error {
    Error::semantic(format!(
        "{}: The following unsupported clauses were used: {clauses}. \n\
         The system database supports a restricted set of Cypher clauses. \
         The supported clause structure for procedure calls is: CALL, YIELD, RETURN. \
         YIELD and RETURN clauses are optional. The order of the clauses is fixed \
         and each can only occur once.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0116 §4, completed 2026-11-04: a clause CE forbids in a query part
/// that composes a database-administration command on the `system`
/// namespace — `WITH`, the one tail clause the §4 whitelist above still
/// allowed after an administration command's `YIELD`. Live CE 2026.09
/// rejects both the `… YIELD … WITH …` and the `WITH … <command> …`
/// spelling with this text (wire GQLSTATUS 42001,
/// `Neo.ClientError.Statement.SemanticError`, cause chain 42N17 → 42NA9);
/// pinned by `testdata/ce-errors/neo4j-ce.json`
/// (`use_system_show_databases_with`). The class-level `SemanticError`
/// token matches the 42N17 family: CE sends both texts under the same
/// status code.
pub fn clauses_not_allowed_on_system_database(clauses: &str) -> Error {
    Error::semantic(format!(
        "{}: The following clauses are not allowed on a system database: {clauses}.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0116 §2, amended 2026-11-04: a `USE` clause targeting a non-system
/// namespace on a statement headed by a database-administration command —
/// CE's auto-routing rejection (GQLSTATUS 42N79,
/// `Neo.ClientError.Statement.SyntaxError`), pinned by
/// `testdata/ce-errors/neo4j-ce.json` (`use_neo4j_show_databases`).
/// `USE system` + administration command stays accepted: live CE rejects
/// even that spelling for `ALTER DATABASE` (42N79), but Relate keeps
/// the whole `USE system` + §2-command surface runnable — the composable
/// SHOW contract the Desktop/Browser compatibility row of ADR-0142 and the
/// harness's `USE system SHOW DEFAULT DATABASE … SHOW DATABASES …`
/// scenario pin.
pub fn use_clause_with_administration_command() -> Error {
    Error::syntax(format!(
        "{}: The `USE` clause is not required for administration commands. \
         Retry your query without the `USE` clause, and it will be routed automatically.",
        ErrorKind::UseClauseWithAdministrationCommand.as_str()
    ))
}

/// ADR-0590: a database-administration command composed with anything but
/// its own `YIELD` and a trailing `RETURN`, on a statement not routed to
/// `system` — CE's `NotSystemDatabaseError` (GQLSTATUS 51N28,
/// `Neo.ClientError.Statement.NotSystemDatabaseError`). Live raises it
/// whether the command heads the statement or follows other clauses (live
/// 2026.10.1, verified for `SHOW DATABASES`), and the spec harness pins the
/// composed-off-system scenarios to it.
pub fn administration_command_needs_system_database(command: &str) -> Error {
    Error::semantic(format!(
        "{}: This is an administration command and it should be executed \
         against the system database: {command}",
        ErrorKind::SystemDatabaseRequired.as_str()
    ))
}

/// ADR-0229 §1/§2: every statement routed to the `system_ontologies`
/// namespace other than the ADR-0116 §2 database-administration commands —
/// reads included, `EXPLAIN`/`PROFILE`-wrapped included (GQLSTATUS 42N17,
/// the family of [`unsupported_system_clauses`]). Authored text, not
/// CE-pinned: live Neo4j CE 2026.09 allows reads on the namespace, so the
/// fixture (`testdata/ce-errors/neo4j-ce.json`) carries no case for it and
/// this wording is the source of truth (ADR-0229 Consequences).
pub fn system_ontologies_not_queryable() -> Error {
    Error::semantic(format!(
        "{}: This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0116 §4/§9: any write in a read-only database (GQLSTATUS 42N18),
/// the recorded CE text for read-only databases. CE appends "on this Neo4j
/// instance." to the wording quoted in the ADR
/// (`testdata/ce-errors/neo4j-ce.json` `read_only_write`). The
/// `system_ontologies` gate no longer uses this text
/// ([`system_ontologies_not_queryable`] replaced it, ADR-0229 §2).
pub fn read_only_write() -> Error {
    Error::semantic(format!(
        "{}: No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance.",
        ErrorKind::WriteOnReadOnlyAccessDatabase.as_str()
    ))
}

/// ADR-0116 §2: `ALTER DATABASE` of a name that is not a fixed namespace
/// (without `IF EXISTS`).
pub fn alter_unknown_database(name: &str) -> Error {
    Error::semantic(format!(
        "{}: Failed to alter the specified database '{name}': Database does not exist.",
        ErrorKind::DatabaseNotFound.as_str()
    ))
}

/// ADR-0146: selecting a namespace that does not exist, over the embedding
/// APIs or Bolt. Live Neo4j CE 2026.09 answers this path with the same text
/// as an unknown static `USE` target (wire 42001, cause 42N00,
/// `Neo.ClientError.Database.DatabaseNotFound`), so this delegates to
/// [`graph_not_found`]; the named entry point stays because the CE path —
/// session database selection, not statement routing — is a distinct call
/// site.
pub fn database_does_not_exist(name: &str) -> Error {
    graph_not_found(name)
}

/// ADR-0121 §2 as amended by ADR-0227 §1 (CIP-287): the `db.ontology.*`
/// procedures act on the session/`USE`-target database, which must be
/// `neo4j`. Raised at the facade's target-namespace check when a statement
/// calling one of them resolves to another namespace and no earlier
/// namespace gate (42N17/42N18) fired.
pub fn ontology_neo4j_only() -> Error {
    Error::semantic(format!(
        "{}: Ontologies can only be stored for the user database 'neo4j'.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0121 §3: `db.ontology.set`/`db.ontology.drop` run only in implicit
/// (auto-commit) transactions. The wording follows Neo4j's error for
/// `CALL { … } IN TRANSACTIONS`; `procedure` is the called name
/// (`db.ontology.set` or `db.ontology.drop`).
pub fn ontology_implicit_transaction_only(procedure: &str) -> Error {
    Error::semantic(format!(
        "{}: '{procedure}' can only be executed in an implicit transaction, \
         but tried to execute in an explicit transaction.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0570: a statement carrying `CALL { … } IN TRANSACTIONS` owns its own
/// batches, so it can only run in an implicit (auto-commit) transaction.
/// Live Neo4j 2026.10.1 rejects it inside an open explicit transaction with
/// GQLSTATUS 25N17 (`TransactionStartFailed`); the wording is Neo4j's own
/// and covers the `IN CONCURRENT TRANSACTIONS` spelling too (the batch forms
/// share one message).
pub fn call_in_transactions_implicit_only() -> Error {
    Error::semantic(format!(
        "{}: A query with 'CALL {{ ... }} IN TRANSACTIONS' can only be executed \
         in an implicit transaction, but tried to execute in an explicit transaction.",
        ErrorKind::TransactionError.as_str()
    ))
}

/// ADR-0121 §3: a statement that calls `db.ontology.set` or
/// `db.ontology.drop` may not write anything else (Neo4j's 42N03 text).
pub fn cross_database_write() -> Error {
    Error::semantic(format!(
        "{}: Writing to more than one database per transaction is not allowed.",
        TckErrorClass::SemanticError.as_str()
    ))
}

/// ADR-0117 §2: `Connection::set_namespace` while a transaction is open.
pub fn namespace_change_in_transaction() -> Error {
    Error::semantic(format!(
        "{}: Cannot change the namespace while a transaction is open.",
        ErrorKind::TransactionError.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every formatter returns a `Semantic` error whose `Display` ends with
    /// `"<Kind>: <CE text>"` (the variant prefix is `"semantic error: "`).
    fn assert_ce_text(error: &Error, expected_suffix: &str) {
        assert!(
            matches!(error, Error::Semantic(_)),
            "expected Error::Semantic, got {error:?}"
        );
        let display = error.to_string();
        assert!(
            display.ends_with(expected_suffix),
            "Display {display:?} does not end with {expected_suffix:?}"
        );
    }

    /// A formatter whose CE class is Statement.SyntaxError (42NA5's
    /// multiple-graph rejection): the variant is `Syntax` and the text
    /// carries no class tag, matching the CE fixture.
    fn assert_ce_syntax_text(error: &Error, expected_suffix: &str) {
        assert!(
            matches!(error, Error::Syntax(_)),
            "expected Error::Syntax, got {error:?}"
        );
        let display = error.to_string();
        assert!(
            display.ends_with(expected_suffix),
            "Display {display:?} does not end with {expected_suffix:?}"
        );
    }

    #[test]
    fn unsupported_administration_command_echoes_statement() {
        assert_ce_text(
            &unsupported_administration_command("CREATE DATABASE foo"),
            "UnsupportedAdministrationCommand: Unsupported administration command: CREATE DATABASE foo",
        );
    }

    /// ADR-0590's 51N28 formatter names the command it refused, in CE's own
    /// wording.
    #[test]
    fn administration_command_needs_system_database_names_the_command() {
        for command in ["SHOW DATABASES", "ALTER DATABASE"] {
            assert_ce_text(
                &administration_command_needs_system_database(command),
                &format!(
                    "SystemDatabaseRequired: This is an administration command and \
                     it should be executed against the system database: {command}"
                ),
            );
        }
    }

    /// The unknown-graph pair returns the typed variant (ADR-0307) with the
    /// historical `Semantic("DatabaseNotFound: Graph not found: <name>")`
    /// rendering, byte for byte.
    fn assert_database_not_found(error: &Error, name: &str) {
        assert!(
            matches!(error, Error::DatabaseNotFound { name: n } if n == name),
            "expected Error::DatabaseNotFound, got {error:?}"
        );
        let expected = format!("DatabaseNotFound: Graph not found: {name}");
        assert_eq!(
            error.to_string(),
            Error::semantic(expected.clone()).to_string()
        );
        assert_eq!(
            error.typed_detail_text().as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn graph_not_found_names_the_graph() {
        assert_database_not_found(&graph_not_found("movies"), "movies");
    }

    #[test]
    fn graph_selector_arity_guard_reads_the_one_table() {
        // A wrong count is the 42I13 SyntaxError class, naming the call.
        for (name, _) in crate::GRAPH_SELECTOR_FUNCTIONS {
            let error = check_graph_selector_arity(&format!("{name}()"), name, 9)
                .expect_err("nine arguments are never the right count");
            assert!(
                matches!(error, Error::Syntax(_)),
                "{name}: the arity refusal is a SyntaxError"
            );
            assert!(error.to_string().contains(name), "{error}");
            // The kind token is the typed model's, never a literal: the
            // bindings' name scan must recover it from the rendered text.
            assert!(
                error.to_string().contains(&format!(
                    "{}:",
                    ErrorKind::InvalidNumberOfArguments.as_str()
                )),
                "{name}: {error}"
            );
        }
        // The expected count is a no-op.
        for (name, arity) in crate::GRAPH_SELECTOR_FUNCTIONS {
            check_graph_selector_arity(name, name, arity).expect("the documented arity passes");
        }
        // A non-selector name is not this guard's business.
        check_graph_selector_arity("other()", "other", 0).expect("not a selector");
    }

    #[test]
    fn composite_only_graph_function_names_the_attempted_graph() {
        assert_ce_text(
            &composite_only_graph_function("graph.byName('neo4j')"),
            "SemanticError: Dynamic graph lookup not allowed here. This feature is only available on composite databases.\nAttempted to access graph graph.byName('neo4j')",
        );
    }

    #[test]
    fn multiple_graphs_names_the_attempted_graph() {
        assert_ce_syntax_text(
            &multiple_graphs("system"),
            "Multiple graphs in the same query not allowed here. This feature is only available on composite databases.\nAttempted to access graph system",
        );
    }

    #[test]
    fn element_id_no_namespace_echoes_the_id() {
        assert_ce_text(
            &element_id_no_namespace("4:00000000-0000-0000-0000-000000000000:n1"),
            "SemanticError: No database is corresponding to `graph.byElementId(4:00000000-0000-0000-0000-000000000000:n1)`. Verify that the elementId is correct.",
        );
    }

    #[test]
    fn unsupported_system_clauses_lists_clauses() {
        assert_ce_text(
            &unsupported_system_clauses("MATCH"),
            "SemanticError: The following unsupported clauses were used: MATCH. \nThe system database supports a restricted set of Cypher clauses. The supported clause structure for procedure calls is: CALL, YIELD, RETURN. YIELD and RETURN clauses are optional. The order of the clauses is fixed and each can only occur once.",
        );
    }

    /// The 2026-11-04 §4 completion's CE text, pinned exactly (live CE
    /// 2026.09, `use_system_show_databases_with`).
    #[test]
    fn clauses_not_allowed_on_system_database_lists_clauses() {
        assert_ce_text(
            &clauses_not_allowed_on_system_database("WITH"),
            "SemanticError: The following clauses are not allowed on a system database: WITH.",
        );
    }

    /// The 42N79 auto-routing rejection: a `Syntax` error (CE's
    /// `Neo.ClientError.Statement.SyntaxError`) whose payload carries the
    /// typed kind token.
    #[test]
    fn use_clause_with_administration_command_text_is_stable() {
        let error = use_clause_with_administration_command();
        assert!(
            matches!(error, Error::Syntax(_)),
            "expected Error::Syntax, got {error:?}"
        );
        assert!(
            error.to_string().ends_with(
                "UseClauseWithAdministrationCommand: The `USE` clause is not required \
                 for administration commands. Retry your query without the `USE` clause, \
                 and it will be routed automatically."
            ),
            "{}",
            error
        );
    }

    #[test]
    fn read_only_write_text_is_stable() {
        assert_ce_text(
            &read_only_write(),
            "WriteOnReadOnlyAccessDatabase: No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance.",
        );
    }

    /// The authored 42N17-family text of ADR-0229 §2, pinned exactly: the
    /// engine renders `semantic error: SemanticError: This database is not
    /// queryable. …`.
    #[test]
    fn system_ontologies_not_queryable_text_is_stable() {
        assert_ce_text(
            &system_ontologies_not_queryable(),
            "SemanticError: This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.",
        );
    }

    #[test]
    fn alter_unknown_database_names_the_database() {
        assert_ce_text(
            &alter_unknown_database("movies"),
            "DatabaseNotFound: Failed to alter the specified database 'movies': Database does not exist.",
        );
    }

    #[test]
    fn database_does_not_exist_matches_graph_not_found() {
        assert_database_not_found(&database_does_not_exist("movies"), "movies");
        assert_eq!(database_does_not_exist("movies"), graph_not_found("movies"));
    }

    #[test]
    fn ontology_procedure_texts_are_stable() {
        assert_ce_text(
            &ontology_neo4j_only(),
            "SemanticError: Ontologies can only be stored for the user database 'neo4j'.",
        );
        assert_ce_text(
            &ontology_implicit_transaction_only("db.ontology.set"),
            "SemanticError: 'db.ontology.set' can only be executed in an implicit transaction, but tried to execute in an explicit transaction.",
        );
        assert_ce_text(
            &ontology_implicit_transaction_only("db.ontology.drop"),
            "SemanticError: 'db.ontology.drop' can only be executed in an implicit transaction, but tried to execute in an explicit transaction.",
        );
        assert_ce_text(
            &cross_database_write(),
            "SemanticError: Writing to more than one database per transaction is not allowed.",
        );
    }

    #[test]
    fn namespace_change_in_transaction_text_is_stable() {
        assert_ce_text(
            &namespace_change_in_transaction(),
            "TransactionError: Cannot change the namespace while a transaction is open.",
        );
    }
}

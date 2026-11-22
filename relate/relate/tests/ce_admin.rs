//! ADR-0116 §2/§6: fixed namespaces with Neo4j Community Edition database
//! semantics. Every database administration command outside CE's
//! supported set fails with `UnsupportedAdministrationCommand` (51N27),
//! echoing the statement verbatim (without a leading `CYPHER n`);
//! `ALTER DATABASE <name> [IF EXISTS] SET DEFAULT LANGUAGE CYPHER 25`
//! is the one supported alteration (ADR-0584 narrowed it from `CYPHER
//! 5|25`: every other integer fails with the authored
//! unsupported-version error), and `SHOW DATABASES` reports the
//! three fixed namespaces — this suite opts into the `ontology`
//! preview feature (`common::ontology::preview_ontology`) so `system_ontologies`
//! stays visible; ADR-0593 hides it while the opt-in is off. Amended 2026-11-04: a `USE` clause targeting a
//! non-system namespace on an administration-command statement is
//! rejected with CE's 42N79 auto-routing text, and a `WITH` composing an
//! administration command on `system` is rejected with CE's
//! composability text (the §4 whitelist completed) — both pinned against
//! live CE 2026.09 (testdata/ce-errors/neo4j-ce.json).

use relate::{Database, Namespace, Value};

mod common;

use common::ce_errors::{
    NEEDS_SYSTEM_DATABASE_SUFFIX, USE_WITH_ADMIN_SUFFIX, WITH_ON_SYSTEM_SUFFIX,
};
use common::ontology::preview_ontology;
use common::{
    assert_ce_rejection, assert_ce_rejection_with_params, connection, connection_in, err_text,
    list, params, scalar_one, string,
};

/// The statement is echoed verbatim; a leading `CYPHER n` preamble is
/// stripped from the echo.
fn assert_unsupported(src: &str, echoed: &str) {
    let err = connection().query(src).unwrap_err().to_string();
    let expected =
        format!("UnsupportedAdministrationCommand: Unsupported administration command: {echoed}");
    assert!(
        err.ends_with(&expected),
        "wrong outcome for {src:?}: {err:?} does not end with {expected:?}"
    );
}

/// The unsupported-administration command table (statement, echo), from
/// the one shared home (testdata/unsupported-admin-commands.tsv).
fn unsupported_admin_commands() -> impl Iterator<Item = (&'static str, &'static str)> {
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/unsupported-admin-commands.tsv"
    ))
    .lines()
    .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    .map(|l| l.split_once('\t').expect("TSV row: statement<TAB>echo"))
}

#[test]
fn unsupported_administration_commands_echo_the_statement() {
    for (src, echoed) in unsupported_admin_commands() {
        assert_unsupported(src, echoed);
    }
}

#[test]
fn alter_database_set_default_language_succeeds_on_every_fixed_namespace() {
    preview_ontology();
    let mut conn = connection();
    for name in [
        "neo4j",
        "system",
        "system_ontologies",
        // Names match case-insensitively (ADR-0116 §1).
        "NEO4J",
    ] {
        conn.query(&format!(
            "ALTER DATABASE {name} SET DEFAULT LANGUAGE CYPHER 25"
        ))
        .unwrap();
        let shown = conn
            .query(&format!("SHOW DATABASE {name} YIELD name, defaultLanguage"))
            .unwrap();
        assert_eq!(
            shown.rows,
            vec![vec![
                Value::String(
                    Namespace::parse(name)
                        .unwrap_or_else(|| panic!("unexpected namespace {name}"))
                        .as_str()
                        .into(),
                ),
                Value::String("CYPHER 25".into()),
            ]],
            "wrong row after ALTER DATABASE {name}"
        );
    }
}

/// ADR-0584 (narrowing ADR-0116 §2): CYPHER 25 is the one admitted default
/// language; every other integer fails the command with the same authored
/// unsupported-version error the statement preamble raises, and the failed
/// ALTERs leave the stored language untouched.
#[test]
fn alter_database_rejects_every_language_but_25_with_the_authored_error() {
    let mut conn = connection();
    conn.query("ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25")
        .unwrap();
    for version in [4, 5, 99] {
        let err = err_text(conn.query(&format!(
            "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER {version}"
        )));
        let expected = relate_test_support::ce_text::unsupported_cypher_version(version);
        assert!(
            err.contains(&expected),
            "CYPHER {version}: wrong error: {err}"
        );
    }
    // The rejections changed nothing: the stored default language stays 25.
    let shown = conn
        .query("SHOW DATABASE neo4j YIELD defaultLanguage")
        .unwrap();
    assert_eq!(shown.rows, vec![vec![Value::String("CYPHER 25".into())]]);
}

#[test]
fn alter_database_unknown_name_fails_unless_if_exists() {
    let mut conn = connection();
    // An unknown name fails with the authored DatabaseNotFound error —
    // the NULL keyword included (LABS-1614: the admin-name readers share
    // one token set), which parses and fails at execution like any
    // unknown name, not as a parse error.
    for name in ["movies", "null"] {
        let err = err_text(conn.query(&format!(
            "ALTER DATABASE {name} SET DEFAULT LANGUAGE CYPHER 25"
        )));
        assert!(
            err.ends_with(&relate_test_support::ce_text::alter_unknown_database(name)),
            "wrong error for {name}: {err}"
        );
    }
    // `IF EXISTS` makes the same command a no-op.
    conn.query("ALTER DATABASE movies IF EXISTS SET DEFAULT LANGUAGE CYPHER 25")
        .unwrap();
}

#[test]
fn show_databases_reports_the_three_fixed_namespaces() {
    preview_ontology();
    let mut conn = connection();
    let result = conn
        .query(
            "SHOW DATABASES YIELD name, type, access, requestedStatus, currentStatus, \
             default, home, aliases, constituents, defaultLanguage \
             RETURN name, type, access, requestedStatus, currentStatus, \
             default, home, aliases, constituents, defaultLanguage",
        )
        .unwrap();
    let row = |name: &str, ty: &str, access: &str, default: bool, language: &str| -> Vec<Value> {
        vec![
            Value::String(name.into()),
            Value::String(ty.into()),
            Value::String(access.into()),
            Value::String("online".into()),
            Value::String("online".into()),
            Value::Bool(default),
            Value::Bool(default),
            Value::List(Vec::new().into()),
            Value::List(Vec::new().into()),
            Value::String(language.into()),
        ]
    };
    assert_eq!(
        result.rows,
        vec![
            row("neo4j", "standard", "read-write", true, "CYPHER 25"),
            row("system", "system", "read-write", false, "CYPHER 25"),
            row(
                "system_ontologies",
                "standard",
                "read-only",
                false,
                "CYPHER 25"
            ),
        ]
    );
}

/// ADR-0116 §8/§10 regression: the databases catalog is store-level, so an
/// `ALTER DATABASE` write (routed to the `neo4j` graph's catalog) and a
/// `SHOW DATABASES ... RETURN ...` read on a `system` session — composed
/// with RETURN, so the plan is not an all-Catalog admin command and runs on
/// the `system` graph — can never diverge. The historical per-graph
/// theater copies showed a stale row; under the one-version contract
/// (ADR-0584) the language value is always `CYPHER 25`, so the pin is that
/// every spelling finds the same store-level neo4j row — and that a bare
/// SHOW on the same system session still runs (42N17 must not misclassify
/// SHOW commands composed on `system`).
#[test]
fn alter_and_system_session_show_databases_agree_on_default_language() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25")
        .unwrap();

    // Column order varies with the projection (`RETURN *` sorts), so find
    // the columns by name and return neo4j's `defaultLanguage`.
    let neo4j_language = |result: relate::QueryResult| {
        let name_col = result
            .header
            .columns
            .iter()
            .position(|c| c == "name")
            .expect("a name column");
        let language_col = result
            .header
            .columns
            .iter()
            .position(|c| c == "defaultLanguage")
            .expect("a defaultLanguage column");
        let row = result
            .rows
            .into_iter()
            .find(|row| row[name_col] == Value::String("neo4j".into()))
            .expect("SHOW DATABASES carries a neo4j row");
        row[language_col].clone()
    };

    // The all-Catalog spelling routes to the store's catalog (neo4j graph).
    let admin = conn
        .query("SHOW DATABASES YIELD name, defaultLanguage")
        .unwrap();
    assert_eq!(
        neo4j_language(admin),
        Value::String("CYPHER 25".into()),
        "admin-routed SHOW DATABASES"
    );

    // The RETURN-composed spelling runs on the session's own graph; on a
    // `system` session it reads the same store-level catalog.
    let mut system = db.connect_namespace("system").unwrap();
    for src in [
        "SHOW DATABASES YIELD name, defaultLanguage RETURN *",
        "SHOW DATABASES YIELD name, defaultLanguage RETURN name, defaultLanguage",
    ] {
        let result = system.query(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(
            neo4j_language(result),
            Value::String("CYPHER 25".into()),
            "{src}: system session disagrees with ALTER DATABASE"
        );
    }
    // A bare SHOW DATABASES on the same system session still runs (42N17
    // must not misclassify SHOW commands composed on `system`).
    system.query("SHOW DATABASES").unwrap();
}

/// ADR-0116 §2: `SHOW DATABASES` composed with a `NEXT`
/// stage is rejected before execution (`SystemDatabaseRequired`). The
/// front-end's NEXT-chain validation fires first; the connection's
/// post-EXPLAIN gates keep the execution-phase backstop.
#[test]
fn show_databases_with_next_is_rejected() {
    let error = connection()
        .query(
            "SHOW DATABASES YIELD name RETURN count(DISTINCT name) AS count \
             NEXT RETURN count * 2 AS res",
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("SystemDatabaseRequired"),
        "{error}"
    );
}

#[test]
fn show_default_and_home_database_return_neo4j() {
    let mut conn = connection();
    for src in ["SHOW DEFAULT DATABASE", "SHOW HOME DATABASE"] {
        let result = conn.query(src).unwrap();
        assert_eq!(result.rows.len(), 1, "{src}");
        assert_eq!(result.rows[0][0], Value::String("neo4j".into()), "{src}");
    }
}

#[test]
fn show_database_matches_case_insensitively_and_unknown_names_return_no_rows() {
    preview_ontology();
    let mut conn = connection();
    for src in [
        "SHOW DATABASE NEO4J YIELD name RETURN name",
        "SHOW DATABASE System_Ontologies YIELD name RETURN name",
    ] {
        let result = conn.query(src).unwrap();
        assert_eq!(result.rows.len(), 1, "{src}");
    }
    // An unknown name returns no rows — the NULL keyword included
    // (LABS-1614's shared admin-name token set): a keyword name is an
    // unknown name like any other, zero rows, not a parse error.
    for name in ["movies", "null"] {
        let result = conn
            .query(&format!("SHOW DATABASE {name} YIELD name RETURN name"))
            .unwrap();
        assert!(result.rows.is_empty(), "{name}");
    }
}

#[test]
fn start_stop_alias_stay_usable_as_identifiers() {
    let mut conn = connection();
    conn.query("CREATE (start {alias: 'stop'})").unwrap();
    let result = conn
        .query("MATCH (start) WHERE start.alias = 'stop' RETURN start.alias AS alias")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::String("stop".into())]]);
}

/// ADR-0116 §4: a `WITH` in a query part that composes a SHOW command is
/// rejected when the statement is routed to `system` — by a `USE system`
/// clause from any session, or by the session namespace itself without a
/// `USE` clause. Position within the part is not part of the rule (live CE
/// rejects the `WITH … <command> …` spelling with the same text), and every
/// catalog command the `system` namespace admits is covered, not only the
/// §2 administration commands (live Neo4j 2026.10.1: 42NA9).
#[test]
fn with_composing_a_show_command_is_rejected_on_system() {
    let mut conn = connection();
    for src in [
        "USE system SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
        "USE system SHOW DEFAULT DATABASE YIELD name WITH 1 AS n RETURN n",
        "USE system SHOW HOME DATABASE YIELD name WITH 1 AS n RETURN n",
        "USE system SHOW DATABASE neo4j YIELD name WITH 1 AS n RETURN n",
        "USE system WITH 1 AS n SHOW DATABASES YIELD name RETURN name",
        "USE system SHOW PROCEDURES YIELD name AS p1 WITH 1 AS n RETURN n",
        "USE system SHOW FUNCTIONS YIELD name AS f1 WITH 1 AS n RETURN n",
        "USE system SHOW SETTINGS YIELD name AS s1 WITH 1 AS n RETURN n",
        "USE system SHOW CURRENT GRAPH TYPE YIELD specification AS spec1 WITH 1 AS n RETURN n",
    ] {
        assert_ce_rejection(
            &mut conn,
            src,
            &format!("SemanticError: {WITH_ON_SYSTEM_SUFFIX}"),
            "SemanticError",
        );
    }
    // The same rejection through the session namespace alone (no `USE`
    // clause): §4 gates the *routed* namespace.
    let mut system = connection_in("system");
    assert_ce_rejection(
        &mut system,
        "SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
        &format!("SemanticError: {WITH_ON_SYSTEM_SUFFIX}"),
        "SemanticError",
    );
    // The gate is plan-based, so EXPLAIN sees it too.
    assert_ce_rejection(
        &mut conn,
        "EXPLAIN USE system SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
        &format!("SemanticError: {WITH_ON_SYSTEM_SUFFIX}"),
        "SemanticError",
    );
    // Off the `system` namespace the same composition is legal.
    conn.query("SHOW SETTINGS YIELD name AS s1 WITH 1 AS n RETURN n")
        .expect("SHOW … WITH … RETURN composes off-system");
}

/// ADR-0116 §2, amended 2026-11-04: a `USE` clause targeting any
/// non-system namespace on a statement headed by a database-administration
/// command is CE's 42N79 auto-routing rejection. Bare and
/// `YIELD … RETURN …`-composed heads alike (live CE rejects both), on
/// every §2 command, for both non-system targets — and through EXPLAIN,
/// since the gate is plan-based.
#[test]
fn use_targeting_non_system_with_administration_command_is_rejected() {
    preview_ontology();
    let mut conn = connection();
    for src in [
        "USE neo4j SHOW DATABASES",
        "USE neo4j SHOW DEFAULT DATABASE",
        "USE neo4j SHOW HOME DATABASE",
        "USE neo4j SHOW DATABASE neo4j",
        "USE neo4j ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25",
        "USE neo4j SHOW DATABASES YIELD name RETURN name",
        // A UNION arm headed by the command is still a headed statement
        // (live CE 2026.09 answers 42N79 for this shape).
        "USE neo4j SHOW DATABASES YIELD name AS a RETURN a \
         UNION SHOW DATABASES YIELD name AS a RETURN a",
        "USE system_ontologies SHOW DATABASES",
        "EXPLAIN USE neo4j SHOW DATABASES",
    ] {
        assert_ce_rejection(
            &mut conn,
            src,
            &format!("UseClauseWithAdministrationCommand: {USE_WITH_ADMIN_SUFFIX}"),
            "SyntaxError", // 42N79 is a Neo.ClientError.Statement.SyntaxError
        );
    }
    // A command later in the body is not headed, so it is not this rule's
    // subject: it is the off-system 51N28 instead (ADR-0590), asserted in
    // `composed_show_databases_is_rejected_off_system` below. The distinction
    // is live's, and the earlier record that live *ran* those mid-body shapes
    // is superseded — live 2026.10.1 rejects them, as does Neo4j's own
    // acceptance suite, whose mid-body `SHOW FUNCTIONS … SHOW DATABASES …`
    // scenario the harness pins.
}

/// The complement the 42N79 rule keeps: `USE system` + administration
/// command stays accepted — bare, composed, and the multi-command shape
/// the harness's "SHOW DEFAULT DATABASE and SHOW DATABASES return the same
/// database row" scenario pins. Live CE 2026.09 rejects even `USE system
/// ALTER DATABASE …` with 42N79; Relate deliberately keeps the whole
/// `USE system` + §2 surface runnable (ADR-0116 §2's Later references).
#[test]
fn use_system_with_administration_command_stays_accepted() {
    preview_ontology();
    let mut conn = connection();
    for src in [
        "USE system SHOW DATABASES",
        "USE system SHOW DATABASES YIELD name RETURN name",
        "USE system SHOW DEFAULT DATABASE YIELD name AS defaultDb \
         SHOW DATABASES YIELD name AS db, access WHERE defaultDb = db RETURN *",
        "USE system ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25",
        // A CYPHER 25 preamble rides along (the composable-SHOW contract
        // is a Cypher 25 feature in Neo4j).
        "CYPHER 25 USE system SHOW DATABASES",
    ] {
        conn.query(src)
            .unwrap_or_else(|e| panic!("{src} should stay accepted: {e}"));
    }
    // The rows are real, not just tolerated: the composed SHOW carries
    // the three fixed namespaces, and the DEFAULT DATABASE × DATABASES
    // cross-join (the harness pin) carries the one matching row.
    let result = conn
        .query("USE system SHOW DATABASES YIELD name RETURN name")
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    let result = conn
        .query(
            "USE system SHOW DEFAULT DATABASE YIELD name AS defaultDb \
             SHOW DATABASES YIELD name AS db, access WHERE defaultDb = db RETURN *",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
}

/// The off-system 51N28 boundary (ADR-0590): a composed administration
/// command with no `USE` clause fails from the user namespace, as it does on
/// the server. The `USE system` forms stay accepted — that database is where
/// the command belongs, and what limits the clauses there is the
/// system-namespace whitelist (the test above).
///
/// `SHOW DATABASES` is the whole scope: `ALTER DATABASE`, the other §2
/// command, cannot be composed at all — every composed form of it is
/// rejected before this gate (the front end's stand-alone rule, or 51N27).
/// The user- and role-management commands (`SHOW USERS`/`ROLES`/`SERVERS`,
/// `TERMINATE TRANSACTIONS`) are a follow-up: the §2 vocabulary also decides
/// which commands run from any namespace (ADR-0116 §2, ADR-0229 §1), so
/// extending it is its own decision.
#[test]
fn composed_show_databases_is_rejected_off_system() {
    let mut conn = connection();
    for src in [
        "SHOW DATABASES YIELD name AS d1 WITH 1 AS n RETURN n",
        "SHOW DATABASES YIELD name MATCH (n) RETURN n",
        "SHOW DATABASES YIELD name UNWIND [1] AS n RETURN n",
        "SHOW FUNCTIONS YIELD name AS function SHOW DATABASES YIELD name AS database RETURN *",
        "UNWIND ['a'] AS name RETURN name AS x \
         UNION SHOW DATABASES YIELD name RETURN name AS x",
        // Mid-body behind a `USE neo4j`: not headed, so not the 42N79 rule's
        // subject either (live 2026.10.1 rejects both).
        "USE neo4j MATCH (n) SHOW DATABASES YIELD name RETURN name",
        "USE neo4j WITH 1 AS n SHOW DATABASES YIELD name RETURN name",
        // The gate is plan-based, so EXPLAIN sees the rejection too — the
        // sibling gate tests pin their own EXPLAIN shape the same way.
        "EXPLAIN SHOW DATABASES YIELD name WITH 1 AS n RETURN n",
    ] {
        assert_ce_rejection(
            &mut conn,
            src,
            &format!("SystemDatabaseRequired: {NEEDS_SYSTEM_DATABASE_SUFFIX}SHOW DATABASES"),
            "SemanticError", // 51N28 rides the nested SystemDatabaseRequired token
        );
    }
}

/// The bare §2 commands keep their ADR-0116 §2 any-namespace run from the
/// user namespace (live CE 2026.09 runs the bare `SHOW DATABASES` from
/// `neo4j` as well).
#[test]
fn bare_administration_commands_still_run_on_the_user_namespace() {
    preview_ontology();
    let mut conn = connection();
    for (src, expected_rows) in [
        ("SHOW DATABASES", 3),
        ("SHOW DEFAULT DATABASE", 1),
        ("SHOW HOME DATABASE", 1),
    ] {
        let result = conn
            .query(src)
            .unwrap_or_else(|e| panic!("{src} should still run: {e}"));
        assert_eq!(result.rows.len(), expected_rows, "{src}");
    }
}

/// ADR-0116 §4's whitelist admits `TERMINATE TRANSACTIONS` on `system`
/// alongside the SHOW family and `ALTER DATABASE`, so it runs there exactly
/// as it does from `neo4j`. (It is not a §2 member — that set is
/// `SHOW DATABASES` and `ALTER DATABASE`.) Live CE 2026.10.1 accepts both
/// spellings on `system` with the `YIELD … FINISH` tail.
#[test]
fn terminate_transactions_runs_on_the_system_namespace() {
    let mut conn = connection();
    for src in [
        "USE system TERMINATE TRANSACTION 'foo-transaction-123' YIELD message AS message FINISH",
        "USE system TERMINATE TRANSACTIONS ['foo-transaction-123'] YIELD transactionId AS id FINISH",
    ] {
        let result = conn
            .query(src)
            .unwrap_or_else(|e| panic!("{src} should run on `system`: {e}"));
        assert!(result.rows.is_empty(), "{src}: FINISH yields no rows");
    }
}

/// The transaction commands' id argument is a STRING or NULL — a NULL is an
/// absent id and gets a row, anything else is the argument's own type error.
/// Live CE 2026.10.1 raises it for `SHOW TRANSACTIONS 123` and `TERMINATE
/// TRANSACTIONS 123` alike, so neither command may quietly drop such a value
/// (which is what made a numeric id vanish instead of failing).
#[test]
fn a_non_string_transaction_id_is_an_argument_type_error() {
    let mut conn = connection();
    let expected_suffix = "InvalidArgumentType: transaction id must be STRING";
    for src in [
        "SHOW TRANSACTIONS 123 YIELD transactionId AS id RETURN id",
        "TERMINATE TRANSACTIONS 123 YIELD transactionId AS id RETURN id",
        "TERMINATE TRANSACTIONS ['none-transaction-1', 5] YIELD transactionId AS id RETURN id",
        "TERMINATE TRANSACTIONS true YIELD transactionId AS id RETURN id",
    ] {
        // Live reports `Neo.ClientError.Statement.TypeError` under 22G03
        // for both spellings.
        assert_ce_rejection(&mut conn, src, expected_suffix, "TypeError");
    }
    // A NULL id is not a type error: it is an absent id, and yields the
    // NULL id as its own row.
    assert_eq!(
        scalar_one(
            &mut conn,
            "TERMINATE TRANSACTIONS null YIELD transactionId AS id RETURN id"
        ),
        Value::Null,
        "a NULL id is an absent id, not a type error"
    );
}

/// `SHOW TRANSACTIONS` reports the SHOW's own transaction with the values
/// an embedded connection has: no authenticated user (`username` empty),
/// one embedded connection (`connectionId` empty), no client socket
/// (`clientAddress` null under Cypher 25), and the running query.
#[test]
fn show_transactions_self_row_carries_the_default_columns() {
    let mut conn = connection();
    let result = conn.query("SHOW TRANSACTIONS").unwrap();
    // Against the canonical table, so the rendered set is stated once.
    assert_eq!(
        result.header.columns,
        relate_front::show_columns::DEFAULT_TRANSACTION_COLUMNS
    );
    assert_eq!(
        result.rows,
        vec![vec![
            Value::String("neo4j-transaction-1".into()),
            Value::String("SHOW TRANSACTIONS".into()),
            Value::String("neo4j".into()),
            Value::String("".into()),
            Value::String("Running".into()),
            Value::String("".into()),
            Value::Null,
        ]]
    );
}

/// The self row's `database` follows the session namespace: a connection
/// opened on `system` reports its own SHOW there.
#[test]
fn show_transactions_self_row_reports_the_session_namespace() {
    let mut conn = connection_in("system");
    assert_eq!(
        scalar_one(
            &mut conn,
            "SHOW TRANSACTIONS YIELD database RETURN database"
        ),
        Value::String("system".into())
    );
}

/// `SHOW TRANSACTIONS` rejects a YIELD name it does not accept: live CE
/// 2026.10.1 answers `YIELD nope` with a SyntaxError ("Trying to YIELD
/// non-existing column: `nope`", 42001), never a silently null column. The
/// accepted set is wider than the rendered one (see
/// `CatalogInfo::accepted_columns`), so a server-observability column the
/// embedded engine has no value for stays a valid name and composes as
/// Null, while the rendered set must not widen with it.
#[test]
fn show_transactions_rejects_an_unknown_yield_column() {
    let mut conn = connection();
    assert_ce_rejection(
        &mut conn,
        "SHOW TRANSACTIONS YIELD nope",
        "is not a valid SHOW TRANSACTIONS column",
        "SyntaxError",
    );
    // Driven from the canonical table, so a name added to the accepted set
    // is covered here without the test re-spelling it.
    for column in relate_front::show_columns::ACCEPTED_ONLY_TRANSACTION_COLUMNS {
        let src = format!("SHOW TRANSACTIONS YIELD {column} RETURN {column}");
        assert_eq!(
            scalar_one(&mut conn, &src),
            Value::Null,
            "{src}: a live-valid column the self row does not render composes as Null"
        );
    }
    // The accepted set is wider than the rendered one, so it must not reach
    // the `YIELD *` shape: the star row and the zero-row header both stay
    // the rendered columns, matched or not.
    for src in [
        "SHOW TRANSACTIONS YIELD *",
        "SHOW TRANSACTIONS 'none-transaction-9' YIELD *",
    ] {
        let result = conn.query(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(
            result.header.columns,
            relate_front::show_columns::DEFAULT_TRANSACTION_COLUMNS,
            "{src}: YIELD * stays the rendered set"
        );
    }
}

/// `TERMINATE TRANSACTIONS` projects over each input row independently: a
/// scalar argument yields that row's own value (duplicates and nulls
/// included), while a *list* argument deduplicates within itself. Live CE
/// 2026.10.1 is the authority for both halves — the per-row half is why
/// the projection may not scan the whole input set, and the list half is
/// why it may not deduplicate across rows.
#[test]
fn terminate_transactions_projects_per_input_row() {
    let mut conn = connection();
    let inputs = "['none-transaction-42', null, 'none-transaction-43', null, \
                  'none-transaction-43']";
    for (src, expected) in [
        // A scalar per row: one row per input value, nothing collapsed.
        (
            format!(
                "WITH {inputs} AS inputs UNWIND inputs AS input \
                 TERMINATE TRANSACTIONS input YIELD transactionId AS output RETURN output"
            ),
            vec![
                Some("none-transaction-42"),
                None,
                Some("none-transaction-43"),
                None,
                Some("none-transaction-43"),
            ],
        ),
        // The whole list at once: deduplicated within itself.
        (
            format!(
                "WITH {inputs} AS inputs \
                 TERMINATE TRANSACTIONS inputs YIELD transactionId AS output RETURN output"
            ),
            vec![
                Some("none-transaction-42"),
                None,
                Some("none-transaction-43"),
            ],
        ),
    ] {
        let result = conn
            .query(&src)
            .unwrap_or_else(|e| panic!("{src} failed: {e}"));
        let got: Vec<Option<&str>> = result
            .rows
            .iter()
            .map(|row| match &row[0] {
                Value::Null => None,
                other => Some(
                    other
                        .as_string()
                        .expect("a transaction id is STRING or NULL"),
                ),
            })
            .collect();
        assert_eq!(got, expected, "{src}");
    }
}

/// The reported id is live's canonical form: the database name of a
/// `<database>-transaction-<id>` id is lowercased, so the corpus's
/// `'noDB-transaction-123'` reports `'nodb-transaction-123'` (verified
/// against live 2026.10.1 enterprise, which also lowercases a mixed-case
/// `'NoDB-transaction-123'`).
#[test]
fn terminate_transactions_reports_the_lowercased_database_name() {
    let mut conn = connection();
    for (src, expected) in [
        (
            "TERMINATE TRANSACTIONS 'noDB-transaction-123' YIELD transactionId RETURN transactionId",
            "nodb-transaction-123",
        ),
        (
            "TERMINATE TRANSACTIONS 'SYSTEM-transaction-7' YIELD * RETURN transactionId",
            "system-transaction-7",
        ),
    ] {
        assert_eq!(scalar_one(&mut conn, src), string(expected), "{src}");
    }
}

/// An id argument that names no transaction at all — the empty list, literal
/// or parameter — is live's 22N06 missing-required-input rejection (the
/// `SemanticError` class), not an empty successful result. A NULL id stays
/// one absent id and gets its own row.
#[test]
fn an_empty_transaction_id_list_is_a_missing_id() {
    let mut conn = connection();
    let message = "Missing transaction id to terminate, the transaction id can be \
                   found using `SHOW TRANSACTIONS`.";
    for (src, params) in [
        ("TERMINATE TRANSACTIONS []", params(&[])),
        (
            "TERMINATE TRANSACTIONS $id",
            params(&[("id", list(Vec::new()))]),
        ),
    ] {
        assert_ce_rejection_with_params(&mut conn, src, params, message, "SemanticError");
    }
}

/// An explicitly YIELD'd column the command does not report is live's
/// invalid-column rejection — the `SyntaxError` class, like every other SHOW
/// command's YIELD check.
#[test]
fn terminate_transactions_rejects_an_unknown_yield_column() {
    let mut conn = connection();
    assert_ce_rejection(
        &mut conn,
        "TERMINATE TRANSACTION 'neo4j-transaction-1' YIELD nope",
        "'nope' is not a valid TERMINATE TRANSACTIONS column",
        "SyntaxError",
    );
}

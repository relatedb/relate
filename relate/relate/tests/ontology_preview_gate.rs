//! The fail-closed `RELATE_PREVIEW` gate on the ontology feature's whole
//! dialect surface (ADR-0378, ADR-0593): with the opt-in absent, every
//! `db.ontology.*` call — `get` included — fails with ADR-0371's authored
//! preview error before arity/`YIELD` validation and before the namespace
//! gates, `SHOW PROCEDURES` omits the four rows in every composed form,
//! and the `system_ontologies` namespace is invisible: unlisted by
//! `SHOW DATABASES`, refused by `USE` (the element-id route included),
//! session connect and `set_namespace` with the ordinary
//! unknown-database rejection (42N00), and treated as an unknown target
//! by `ALTER DATABASE` — never the authored preview error, which would
//! advertise a name the catalog must not list (ADR-0371's name-gate
//! rule). Stored state is unaffected: a store holding a document opens,
//! checkpoints and reloads with the gate off.
//!
//! The enabled side (the calls succeed, the four rows are listed and the
//! namespace is visible and not queryable) lives in the four ontology
//! test crates and the namespace suites, which opt in through
//! `common::preview_ontology`. This crate must never opt in — it exists
//! to pin the closed gate.

mod common;

use relate::test_opts::assert_preview_unset;
use relate::{Database, Namespace, Value};

use common::{err_text, set_doc, ONTOLOGY_DOC};

/// ADR-0371's authored disabled-feature text for the `ontology` row.
const DISABLED: &str = "semantic error: SemanticError: preview feature 'ontology' is not enabled: \
     set RELATE_PREVIEW=ontology";

/// A fresh in-memory store and connection, opened only after the shared
/// closed-gate precondition holds: a dev shell exporting `RELATE_PREVIEW`
/// must fail loudly instead of silently testing the enabled path.
fn open_closed() -> (Database, relate::Connection) {
    assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

/// Assert every statement in `statements` is refused with exactly the
/// authored preview text; the statement text rides in the failure message
/// so a regression names the offending form.
fn assert_all_disabled(conn: &mut relate::Connection, statements: &[&str]) {
    for &cypher in statements {
        assert_eq!(err_text(conn.query(cypher)), DISABLED, "{cypher}");
    }
}

/// Every standalone call fails closed with exactly the authored text —
/// including the parameterized `set` form with a valid document, so the
/// refusal is the preview gate, not validation.
#[test]
fn every_standalone_procedure_fails_closed() {
    let (_db, mut conn) = open_closed();
    let err = err_text(set_doc(&mut conn, ONTOLOGY_DOC));
    assert_eq!(err, DISABLED, "CALL db.ontology.set($ontology)");
    assert_all_disabled(
        &mut conn,
        &[
            "CALL db.ontology.get()",
            "CALL db.ontology.drop()",
            "CALL db.ontology.apply()",
        ],
    );
}

/// The gate runs before anything is written: a refused `set` leaves the
/// store without a document (ADR-0378's placement above all later gates).
#[test]
fn failed_set_leaves_the_store_without_a_document() {
    let (db, mut conn) = open_closed();
    let err = err_text(set_doc(&mut conn, ONTOLOGY_DOC));
    assert_eq!(err, DISABLED, "CALL db.ontology.set($ontology)");
    assert_eq!(db.ontology_document(), Ok(None));
}

/// The preview error precedes arity validation, `YIELD` validation, the
/// `CALL { … }` body, and the EXPLAIN/PROFILE wrappers.
#[test]
fn preview_error_precedes_arity_yield_and_wrappers() {
    let (_db, mut conn) = open_closed();
    assert_all_disabled(
        &mut conn,
        &[
            "CALL db.ontology.set()",
            "CALL db.ontology.get() YIELD ontology RETURN ontology",
            "CALL { CALL db.ontology.get() YIELD ontology RETURN ontology } RETURN ontology",
            "EXPLAIN CALL db.ontology.get()",
            "PROFILE CALL db.ontology.drop()",
        ],
    );
}

/// The same full descent `validate_calls` uses: a `CALL { … }` body, an
/// expression subquery (ADR-0378's `EXISTS` position), a `WHEN`/`ELSE`
/// branch and a `NEXT` stage. (A `FOREACH` body admits only updating
/// clauses, so a procedure call cannot appear there — 42I01.)
#[test]
fn nested_calls_are_gated() {
    let (_db, mut conn) = open_closed();
    assert_all_disabled(
        &mut conn,
        &[
            "CALL { CALL db.ontology.get() YIELD ontology RETURN ontology } RETURN 1 AS x",
            "RETURN EXISTS { CALL db.ontology.get() } AS x",
            "WHEN true THEN { CALL db.ontology.apply() }",
            "CREATE (:L) RETURN 1 AS x NEXT CALL db.ontology.set('x')",
        ],
    );
}

/// The preview error precedes the namespace gates (`USE`-routed and via the
/// session namespace) and ADR-0121 §3's implicit-transaction-only rule for
/// `apply`/`set`/`drop`.
#[test]
fn preview_error_precedes_namespace_and_transaction_gates() {
    let (db, mut conn) = open_closed();
    // Routed to `system_ontologies`, the preview error precedes the
    // namespace visibility gate itself (ADR-0593: the procedures gate
    // keeps ADR-0378's pinned first position) and ADR-0229 §2's 42N17
    // text; on `system` it precedes ADR-0227 §1's target-namespace text.
    assert_all_disabled(
        &mut conn,
        &[
            "USE system CALL db.ontology.get()",
            "USE system_ontologies CALL db.ontology.get()",
        ],
    );
    let mut system = db.connect_namespace("system").unwrap();
    assert_eq!(err_text(system.query("CALL db.ontology.drop()")), DISABLED);
    // A `system_ontologies` *session* cannot exist while the opt-in is
    // off — the connect refuses (see
    // `session_connect_refuses_the_hidden_namespace`) — so the session-
    // namespace precedence case exists only for `system`.
    // In an open explicit transaction the preview gate still wins over the
    // implicit-transaction-only rule.
    conn.begin().unwrap();
    assert_eq!(err_text(conn.query("CALL db.ontology.apply()")), DISABLED);
    conn.rollback().unwrap();
}

/// Discovery agrees with callability: no `db.ontology.*` row is listed in
/// any composed form, while the rest of the catalog is untouched.
#[test]
fn show_procedures_omits_the_ontology_family() {
    let (_db, mut conn) = open_closed();
    // Plain, WHERE and UNION-composed discovery all agree with
    // callability: no `db.ontology.*` row is visible.
    for cypher in [
        "SHOW PROCEDURES YIELD name WHERE name STARTS WITH 'db.ontology.' RETURN name",
        "SHOW PROCEDURES YIELD name WHERE name STARTS WITH 'db.ontology.' RETURN name \
         UNION SHOW PROCEDURES YIELD name WHERE name STARTS WITH 'db.ontology.' RETURN name",
    ] {
        let result = conn.query(cypher).unwrap();
        assert!(result.rows.is_empty(), "{cypher}: {:?}", result.rows);
    }
    // The aggregate form still answers, with the empty set's count.
    let result = conn
        .query(
            "SHOW PROCEDURES YIELD name WHERE name STARTS WITH 'db.ontology.' RETURN count(*) AS c",
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(0)]]);
    // An `EXISTS` subquery sees the same filtered catalog.
    let result = conn
        .query(
            "RETURN EXISTS { SHOW PROCEDURES YIELD name \
             WHERE name STARTS WITH 'db.ontology.' } AS e",
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(false)]]);
    // Hiding the gated family leaves every other row listed.
    let result = conn
        .query("SHOW PROCEDURES YIELD name WHERE name = 'db.labels' RETURN count(*) AS c")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

/// ADR-0593: the namespace is invisible while the opt-in is off — the
/// listing omits the row in every composed form, the named forms find
/// nothing (exactly as for an unknown name), and an expression subquery
/// sees the same filtered catalog. The flag-off default listing is CE's
/// exact two rows.
#[test]
fn show_databases_omits_system_ontologies_in_every_composed_form() {
    let (_db, mut conn) = open_closed();
    let result = conn.query("SHOW DATABASES YIELD name RETURN name").unwrap();
    let mut names = common::first_column_strings(&result);
    names.sort();
    assert_eq!(names, ["neo4j", "system"], "the flag-off listing");
    // The named form answers like an unknown name: no rows.
    let result = conn
        .query("SHOW DATABASE system_ontologies YIELD name RETURN name")
        .unwrap();
    assert!(result.rows.is_empty());
    // So does every composed filter over the listing.
    let result = conn
        .query("SHOW DATABASES YIELD name WHERE name = 'system_ontologies' RETURN name")
        .unwrap();
    assert!(result.rows.is_empty());
    let result = conn
        .query("RETURN EXISTS { SHOW DATABASES YIELD name WHERE name = 'system_ontologies' } AS e")
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Bool(false)]]);
}

/// ADR-0593: every `USE` route to the hidden namespace fails with the
/// site's ordinary unknown-database rejection — 42N00 `Graph not found`,
/// case-insensitively, the element-id route included (the resolved
/// namespace's name) — never the authored preview error, which would
/// advertise a name the catalog must not list (ADR-0371's name-gate
/// rule). The rejection precedes the 42N79 admin gate, so
/// `USE system_ontologies SHOW DATABASES` is an unknown database, not a
/// mis-routed administration command.
#[test]
fn use_routes_to_system_ontologies_fail_as_an_unknown_name() {
    let (_db, mut conn) = open_closed();
    for statement in [
        "USE system_ontologies MATCH (n) RETURN count(n) AS c",
        "USE SYSTEM_ONTOLOGIES RETURN 1 AS one",
        "USE system_ontologies SHOW DATABASES",
        "EXPLAIN USE system_ontologies MATCH (n) RETURN n",
        "USE graph.byElementId('4:rl-onto:n1') MATCH (n) RETURN count(n) AS c",
    ] {
        let err = err_text(conn.query(statement));
        assert!(
            err.ends_with("Graph not found: system_ontologies"),
            "{statement}: {err}"
        );
    }
}

/// ADR-0593: the session routes refuse the hidden namespace with the same
/// ordinary rejection an unknown name gets — typed and by name, at
/// connect and on `set_namespace` — and the session keeps its namespace
/// on refusal.
#[test]
fn session_connect_refuses_the_hidden_namespace() {
    let (db, mut conn) = open_closed();
    for name in ["system_ontologies", "System_Ontologies"] {
        let err = err_text(db.connect_namespace(name));
        assert!(
            err.ends_with("Graph not found: system_ontologies"),
            "{name}: {err}"
        );
    }
    let err = err_text(db.connect_typed(Namespace::SystemOntologies));
    assert!(err.ends_with("Graph not found: system_ontologies"), "{err}");
    let err = err_text(conn.set_namespace("system_ontologies"));
    assert!(err.ends_with("Graph not found: system_ontologies"), "{err}");
    assert_eq!(conn.namespace(), Namespace::Neo4j);
}

/// ADR-0593: `ALTER DATABASE` treats the hidden namespace as an unknown
/// target — the command's own unknown-database text, and a no-op under
/// `IF EXISTS`, exactly as for a name that never existed.
#[test]
fn alter_database_treats_the_hidden_namespace_as_unknown() {
    let (_db, mut conn) = open_closed();
    let err =
        err_text(conn.query("ALTER DATABASE system_ontologies SET DEFAULT LANGUAGE CYPHER 25"));
    assert!(
        err.ends_with(
            "Failed to alter the specified database 'system_ontologies': Database does not exist."
        ),
        "{err}"
    );
    conn.query("ALTER DATABASE system_ontologies IF EXISTS SET DEFAULT LANGUAGE CYPHER 25")
        .unwrap();
}

/// Persistence is unaffected (ADR-0378): the hidden facade writes and
/// publishes a document, and a store that holds one opens, checkpoints and
/// reloads with the gate off — the bytes survive unchanged.
#[test]
fn stored_documents_survive_reopen_with_the_gate_closed() {
    assert_preview_unset();
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().to_str().unwrap();
    let db = Database::open(uri).unwrap();
    db.commit_ontology_set(ONTOLOGY_DOC.to_string()).unwrap();
    db.checkpoint().unwrap();
    drop(db);

    let db = Database::open(uri).expect("a store holding an ontology opens with the gate off");
    assert_eq!(
        db.ontology_document().unwrap().as_deref(),
        Some(ONTOLOGY_DOC)
    );
    drop(db);

    let db = Database::open(uri).expect("the second reopen still opens with the gate off");
    assert_eq!(
        db.ontology_document().unwrap().as_deref(),
        Some(ONTOLOGY_DOC),
        "the document survives the second reopen byte-for-byte"
    );
}

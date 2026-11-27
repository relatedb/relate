//! The composition rules this crate owns are pinned here. The
//! `CALL … IN TRANSACTIONS` rule reads the one per-clause write answer
//! ([`crate::clause_mutates_store`]) — the schema DDL commands are
//! writes in the classification (a catalog change persists while moving
//! no ADR-0309 counter); in practice they die earlier, at the standalone
//! rule pinned below, so the write-before pin rides a plain write. The `USE` rule is positional, not
//! write-dependent (a `USE` after any preceding clause is misplaced). The
//! third write-predicate consumer, the expression-subquery rejection in
//! `expr_check`, is pinned end to end in the facade's
//! `relate/tests/namespace.rs`.

use relate_front::parse_and_lower;
use relate_ir::{Clause, Expr};

use crate::rejection;

/// The `USE` placement rule is positional: any clause ahead of it — write or
/// read — makes it misplaced. (An earlier revision asked the write predicate
/// here; the rule no longer reads write history, so the test pins the
/// positional answer with a non-writing preceding clause.)
///
/// The `USE` clause is inserted by hand because the front-end's own placement
/// gate refuses a misplaced `USE` during lowering — for every statement the
/// product admits, that gate answers first. This planner arm is the backstop
/// for a plan that reached the planner some other way.
#[test]
fn use_after_a_preceding_clause_is_rejected() {
    let mut query = parse_and_lower("MATCH (n) RETURN n").expect("the statement lowers");
    let [part] = query.parts.as_mut_slice() else {
        panic!("expected one part");
    };
    part.clauses.insert(
        1,
        Clause::Use {
            graph: Expr::String("neo4j".to_string()),
        },
    );
    let error = crate::part_error(&query);
    assert!(
        error.contains("InvalidPlacementOfUseClause: USE must be the first clause"),
        "unexpected error: {error}"
    );
}

/// A write cannot precede `CALL { … } IN TRANSACTIONS`; the rule asks
/// "did a write run before the batched subquery?" and rides a plain
/// write head — the catalog DDL heads that once served here die earlier,
/// at the standalone rule below.
#[test]
fn write_before_call_in_transactions_is_rejected() {
    // A plain write head reaches the rule. The catalog DDL heads that
    // once served here die earlier now, at the standalone rule below, so
    // this pin rides a plain `CREATE`; no catalog command reaches the
    // rule — the DDL family is standalone and the row-producing commands
    // (`TERMINATE TRANSACTIONS`, the SHOWs) are `ClauseClass::Read`, not
    // writes.
    let error = crate::part_error(&crate::query_of(
        "CREATE (n:Boxed) \
         CALL { CREATE (m:Other) } IN TRANSACTIONS OF 1 ROWS",
    ));
    assert!(
        error.contains("WriteBeforeCallInTransactions"),
        "unexpected error: {error}"
    );
}

/// The same rule across `NEXT` stages: a writing part refuses a batched
/// subquery in a later part (42I25) — the chained statement's parts are
/// one statement, so the writing part precedes the batched part exactly
/// like a write clause does. A chain-level fact, so these pins ride the
/// whole-statement plan, not the part-scoped helper.
#[test]
fn a_writing_next_part_refuses_call_in_transactions_in_a_later_part() {
    for cypher in [
        // The ON ERROR CONTINUE spelling.
        "CREATE (n:Start) WITH n RETURN n \
         NEXT CALL (n) { CREATE (:Made) } IN TRANSACTIONS ON ERROR CONTINUE \
         RETURN count(*) AS c",
        // The bare spelling: the modifier battery is not the rule's.
        "CREATE (n:Start) RETURN n \
         NEXT CALL (n) { CREATE (:Made) } IN TRANSACTIONS \
         RETURN count(*) AS c",
    ] {
        let error = rejection(cypher);
        assert!(
            error.contains("WriteBeforeCallInTransactions"),
            "{cypher}: unexpected error: {error}"
        );
    }
}

/// The refusal is the *unbatched* writes': a batched part's own writes run
/// in its batches' transactions, so nothing behind them counts as a write
/// clause for a later part. The reading half — a later part reading what
/// the statement's own batches committed — is the shape the rule must not
/// over-reach past; the batched half is the same policy's second
/// spelling, pinned with it so the chain fact cannot over-reach past the
/// batches.
#[test]
fn a_batched_part_does_not_refuse_a_later_part() {
    for cypher in [
        // The reading half: a NEXT part reading what the batches
        // committed.
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { CREATE (z:Z {i: i}) } IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS _ \
         NEXT MATCH (z:Z) RETURN z.i AS zi",
        // The batched half: the same policy's second spelling, pinned so
        // the chain fact cannot over-reach past the batches.
        "UNWIND [1, 2, 3] AS i \
         CALL (i) { CREATE (z:Z {i: i}) } IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS _ \
         NEXT CALL { CREATE (:W) } IN TRANSACTIONS OF 1 ROW \
         RETURN count(*) AS c",
    ] {
        crate::plan_of(cypher);
    }
}

/// `LOAD CSV` only reads a file: the bulk-load idiom feeds it into
/// `CALL { … } IN TRANSACTIONS` (REL8-1224), while a real write before the
/// same call is still refused.
#[test]
fn load_csv_may_precede_call_in_transactions() {
    crate::plan_of(
        "LOAD CSV FROM 'file:///rows.csv' AS row \
         CALL (row) { CREATE (:X {v: row[0]}) } IN TRANSACTIONS",
    );
    // The same answer below the top level: a body that only reads a file.
    crate::plan_of(
        "CALL { LOAD CSV FROM 'file:///rows.csv' AS row RETURN row } \
         CALL (row) { CREATE (:X {v: row[0]}) } IN TRANSACTIONS",
    );
    let error = rejection(
        "LOAD CSV FROM 'file:///rows.csv' AS row \
         CREATE (:Y) \
         CALL (row) { CREATE (:X {v: row[0]}) } IN TRANSACTIONS",
    );
    assert!(
        error.contains("WriteBeforeCallInTransactions"),
        "unexpected error: {error}"
    );
}

/// A grammar-ending DDL command is standalone whichever AST carries it
/// (`clause_kind::clause_is_ddl_command`'s family — schema DDL plus
/// `ALTER DATABASE`): its grammar ends at its own `OPTIONS`/EOF, so every
/// continuation is rejected — `WITH`, `RETURN` and `FINISH` alike (live
/// 2026.10.1, probed 2026-11-08; the front end's own scope check owns the
/// `SET` form). The SHOW commands project rows and compose freely; their
/// acceptance is pinned in `relate/relate/tests/integration/ddl_result_shape.rs`.
/// `ALTER DATABASE` is a family member by grammar (live rejects every
/// continuation) but cannot serve as this pin's head: the front end's
/// own administration gate answers first with its unsupported-command
/// rejection, so the planner rule is its backstop.
#[test]
fn a_grammar_ending_ddl_command_is_standalone() {
    for cypher in [
        "CREATE CONSTRAINT c FOR (n:L) REQUIRE n.p IS UNIQUE WITH 1 AS x",
        "CREATE CONSTRAINT c FOR (n:L) REQUIRE n.p IS UNIQUE RETURN 1",
        "CREATE CONSTRAINT c FOR (n:L) REQUIRE n.p IS UNIQUE FINISH",
        "CREATE INDEX i FOR (n:L) ON (n.p) WITH 1 AS x",
    ] {
        let err = rejection(cypher);
        assert!(
            err.contains(
                "InvalidClauseComposition: schema and database-alteration commands must be standalone clauses"
            ),
            "{cypher}: {err}"
        );
    }
}

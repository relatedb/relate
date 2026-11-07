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

use crate::binding::Env;
use crate::validate::validate_part_with_env;

/// Lower `cypher` and run the semantic clause validation on its one part,
/// returning the rejection's text.
fn rejection(cypher: &str) -> String {
    let query = parse_and_lower(cypher).unwrap_or_else(|error| panic!("{cypher}: {error}"));
    let [part] = query.parts.as_slice() else {
        panic!("{cypher}: expected one part");
    };
    validate_part_with_env(part, super::Scope::stage(Env::new()), &mut |_| false)
        .expect_err("the composition rule rejects this statement")
        .to_string()
}

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
    let error = validate_part_with_env(part, super::Scope::stage(Env::new()), &mut |_| false)
        .expect_err("the placement rule rejects a USE after another clause")
        .to_string();
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
    let error = rejection(
        "CREATE (n:Boxed) \
         CALL { CREATE (m:Other) } IN TRANSACTIONS OF 1 ROWS",
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
/// acceptance is pinned in `relate/relate/tests/ddl_result_shape.rs`.
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

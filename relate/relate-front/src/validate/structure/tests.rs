//! The structure battery's tests, offloaded by the file-size gate
//! (the ast/tree.rs shape): UNION columns, FOREACH bodies, traversal
//! bounds, and the paren-projection batteries (the WITH alias rule's
//! variable, paren and map-projection shapes).

use super::*;
use relate_parser::Expr;

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
/// openCypher TCK Match3 #29: a relationship variable reused in a pattern
/// that closes back onto a repeated node variable is rejected here, the
/// one owner of the rule (the planner's binder does not re-state it).
#[test]
fn relationship_reused_in_a_closed_pattern_is_rejected() {
    let stmt = relate_parser::parse("MATCH (a)-[r]->(b)-[r]->(a) RETURN a").unwrap();
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
    let error = crate::validate::validate_statement_tree(&relate_parser::parse(rejected).unwrap())
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
        &relate_parser::parse("CALL { FOREACH (i IN [1] | CREATE (:N {v: i}) RETURN i) } RETURN 1")
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
        let error = crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
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
    let error =
        crate::validate::validate_statement_tree(&count_subquery_over("MATCH (n) UNION MATCH (m)"))
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
            .unwrap_or_else(|e| panic!("an omitted-RETURN union outside COUNT was rejected: {e}"));
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
    crate::parse_and_lower_statement_with_limits(query, relate_core::PreviewFeatures::none(), 600)
        .expect("600 hops is admissible under --max-traversal-depth 600");
}
/// LABS-1616: redundant parens around a lone variable are transparent
/// to WITH's alias rule — `WITH (n)` is the bare variable `n` and needs
/// no `AS`, DISTINCT or not, however deep the parens nest. The lowered
/// item is the bare variable, so past the front end the projection is
/// exactly `WITH n`.
#[test]
fn with_parenthesized_variable_needs_no_alias() {
    for query in [
        "MATCH (n:L1) WITH (n) RETURN n",
        "MATCH (n:L1) WITH DISTINCT(n) RETURN n",
        "MATCH (n:L1) WITH ((n)) RETURN n",
        "MATCH (n:L1) WITH DISTINCT ((n)) RETURN n",
        "MATCH (n:L1) WITH (n) AS m RETURN m",
    ] {
        crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
            .unwrap_or_else(|e| panic!("{query}: a parenthesized variable needs no alias: {e}"));
    }
}
/// Parens make no non-variable item aliasable: `WITH (n.p1)` and
/// `WITH (1)` are expressions exactly like their unparenthesized forms.
#[test]
fn with_parenthesized_non_variable_still_requires_alias() {
    for query in [
        "MATCH (n:L1) WITH (n.p1) RETURN n",
        "MATCH (n:L1) WITH DISTINCT (n.p1) RETURN n",
        "WITH (1) RETURN 1",
    ] {
        let error = crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::NoExpressionAlias,
            "{query}: parens must not make a non-variable item aliasable"
        );
    }
}
/// REL8-1866: an unaliased map projection over a variable is named
/// after its base variable (`WITH m {.*}` projects the column `m` — the
/// map is a view of that variable, Neo4j 2026.10 live-confirmed), so
/// WITH's alias rule admits it exactly like the bare variable: every
/// selector spelling, DISTINCT, redundant parens, and the aliased form
/// beside them.
#[test]
fn with_map_projection_over_a_variable_needs_no_alias() {
    for query in [
        "MATCH (m:L1) WITH m{.*} RETURN m",
        "MATCH (m:L1) WITH m{.p1} RETURN m",
        "MATCH (m:L1) WITH m{ .p1, .p2 } RETURN m",
        "MATCH (m:L1) WITH DISTINCT m{.*} RETURN m",
        "MATCH (m:L1) WITH (m{.*}) RETURN m",
        "MATCH (m:L1) WITH m{.*} AS x RETURN x",
    ] {
        crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
            .unwrap_or_else(|e| {
                panic!("{query}: a map projection over a variable needs no alias: {e}")
            });
    }
}
/// Only the *lone* projection over a variable carries the name: embedded
/// in any other expression it is an expression item that still requires
/// `AS`, and a second item under the base variable's name is the
/// duplicate rejection — the projection contributes `m` to the duplicate
/// rule (Neo4j 42N38 live-confirmed, REL8-1866).
#[test]
fn with_map_projection_embedded_or_duplicated_still_rejects() {
    for query in [
        "MATCH (m:L1) WITH m{.*} + 1 RETURN m",
        "MATCH (m:L1) WITH size(m{.*}) RETURN m",
        "MATCH (m:L1) WITH m.p1 RETURN m",
    ] {
        let error = crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
            .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::NoExpressionAlias,
            "{query}: only the lone projection over a variable carries its name"
        );
    }
    let conflict = crate::validate::validate_statement_tree(
        &relate_parser::parse("MATCH (m:L1) WITH m{.*}, m RETURN m").unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        conflict.kind(),
        ErrorKind::ColumnNameConflict,
        "the projection contributes the base variable's name to the duplicate rule"
    );
}
/// UNION compares columns by their resulting names as strings —
/// Neo4j (probed live 2026.10): a bare variable and a map projection
/// on one canonicalize (paren-transparent), so `(n) UNION n` and
/// `(n {.p}) UNION n` agree; a property access names by verbatim
/// source text, so `(n.p1) UNION n.p1` and the spaced spelling are
/// incompatible (42N39), and two spellings of one property in one
/// projection are two distinct columns — only an exact-text
/// duplicate conflicts (42N38).
#[test]
fn union_compares_columns_by_their_verbatim_names() {
    for accepted in [
        "MATCH (n) RETURN (n) UNION MATCH (n) RETURN n",
        "MATCH (n) RETURN (n {.p}) UNION MATCH (n) RETURN n",
    ] {
        crate::validate::validate_statement_tree(&relate_parser::parse(accepted).unwrap())
            .unwrap_or_else(|e| panic!("{accepted}: {e}"));
    }
    // A different verbatim spelling is a different column: the UNION
    // is incompatible (Neo4j 42N39).
    for rejected in [
        "MATCH (n) RETURN (n.p1) UNION MATCH (n) RETURN n.p1",
        "MATCH (n) RETURN n . p1 UNION MATCH (n) RETURN n.p1",
    ] {
        let error =
            crate::validate::validate_statement_tree(&relate_parser::parse(rejected).unwrap())
                .unwrap_err();
        assert_eq!(
            error.kind(),
            ErrorKind::DifferentColumnsInUnion,
            "{rejected}: {error}"
        );
    }
    // Two spellings of one property are two distinct columns —
    // accepted, not a conflict; only the exact-text duplicate
    // conflicts (42N38).
    crate::validate::validate_statement_tree(
        &relate_parser::parse("MATCH (n) RETURN (n.p1), n.p1").unwrap(),
    )
    .expect("two verbatim spellings are two columns");
    let conflict = crate::validate::validate_statement_tree(
        &relate_parser::parse("MATCH (n) RETURN n.p1, n.p1").unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        conflict.kind(),
        ErrorKind::ColumnNameConflict,
        "the exact-text duplicate still conflicts"
    );
}

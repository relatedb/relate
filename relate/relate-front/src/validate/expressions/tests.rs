//! The constraints walk's tests, split from the module body
//! (LABS-311 size gate).

use super::*;
use crate::validate::parse_and_validate_expression_constraints;

/// `type()` on a statically-known node variable is rejected in the
/// clause walk, with node-ness carried in the scope carrier's kind map:
/// bound per pattern clause by the pre-arm step, projected by a WITH
/// through the kind fold — a WITH keeps a bare node variable's
/// node-ness under its alias, CREATE/MERGE pattern nodes are nodes for
/// later clauses, and a relationship variable never carries the Node
/// kind — so `type()` on it stays legal.
#[test]
fn type_on_nodes_is_scoped_through_the_clause_walk() {
    // Node-ness projects through the alias: `m` is `n`, a node.
    let error = parse_and_validate_expression_constraints("MATCH (n) WITH n AS m RETURN type(m)")
        .expect_err("type() on a node projected through a WITH alias is rejected");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    // A relationship keeps type() legal — it never joins the node set.
    parse_and_validate_expression_constraints("MATCH (n)-[r]->() WITH r RETURN type(r)")
        .expect("type() on a relationship stays legal after a WITH");
    // A created node variable is a node for the RETURN's check.
    let error = parse_and_validate_expression_constraints("CREATE (n) RETURN type(n)")
        .expect_err("type() on a CREATE-bound node variable is rejected");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    // A node dropped by the projection is out of node scope, so this
    // pass raises no type() rejection for it (the undefined-name
    // report is the scope battery's, not this walk's) — the node set
    // is a projection, not a history of every name seen.
    parse_and_validate_expression_constraints("MATCH (n) WITH 1 AS x RETURN type(n)")
        .expect("a node dropped by a WITH projection is out of node scope");
}

/// The static-kind rule family runs at every expression position the
/// walk validates — the same three rules everywhere, no per-position
/// subset: `labels()` on a path in a MATCH's WHERE, property access
/// on a path at an item and in a WITH's WHERE (against the
/// projection view built there), and `type()` on a node alias in a
/// WITH's WHERE. The legal shapes stay legal at the same positions.
#[test]
fn static_kind_rules_run_at_every_expression_position() {
    let error = parse_and_validate_expression_constraints(
        "MATCH p=(a)-[r]->(b) WHERE labels(p) = [] RETURN p",
    )
    .expect_err("labels() on a path is rejected in a WHERE");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    for query in [
        "MATCH p=(a)-[r]->(b) RETURN p.foo",
        "MATCH p=(a)-[r]->(b) WITH p WHERE p.x = 1 RETURN p",
    ] {
        let error = parse_and_validate_expression_constraints(query)
            .expect_err(&format!("{query}: property access on a path"));
        assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
        assert!(
            error
                .to_string()
                .contains("property access is not allowed on paths"),
            "{query}: {error}"
        );
    }
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) WITH n AS m WHERE type(m) = 'X' RETURN m",
    )
    .expect_err("type() on a node alias is rejected in a WITH's WHERE");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    for query in [
        "MATCH (n) WHERE labels(n) = [] RETURN n",
        "MATCH (n)-[r]->() WITH r WHERE type(r) = 'X' RETURN r",
        "MATCH (n) WITH n AS m WHERE labels(m) = [] RETURN m",
    ] {
        parse_and_validate_expression_constraints(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    }
}

/// A subquery body inherits the enclosing statement's classification
/// for the names it sees, and only those names: an outer node
/// variable is still a node inside an EXISTS body (`(n)` keeps the
/// single-node reading, `type(n)` the node rejection), a CALL-scope
/// body classifies its imports the same way, and a name the scope
/// clause did not import carries no kind into the body — its report
/// stays the binder's undefined-name rejection, never a leaked
/// kind's.
#[test]
fn subquery_bodies_inherit_the_visible_kinds() {
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) WHERE EXISTS { MATCH (m) WHERE (n) RETURN count(m) } RETURN n",
    )
    .expect_err("an outer node keeps the single-node reading inside a body");
    assert_eq!(
        error.to_string(),
        "InvalidArgumentType: a single node is not a valid pattern predicate"
    );
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) WHERE EXISTS { MATCH (m) RETURN type(n) } RETURN n",
    )
    .expect_err("type() on an outer node is rejected inside a body");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    let error =
        parse_and_validate_expression_constraints("MATCH (n) CALL (n) { RETURN type(n) AS t }")
            .expect_err("a scope-clause import is classified inside the body");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
    parse_and_validate_expression_constraints("MATCH (n), (y) CALL (n) { RETURN type(y) AS t }")
        .expect("an unimported name carries no kind; the undefined-name report is the binder's");
}

/// The classification flows across a CALL body's NEXT stages with the
/// name scope: an alias a stage's WITH carries keeps its kind in
/// every later stage's checks.
#[test]
fn kinds_flow_across_next_stages_with_the_scope() {
    let error = parse_and_validate_expression_constraints(
        "MATCH (n) CALL (n) { MATCH (a) WITH a AS m RETURN m NEXT MATCH (b) WHERE (m) \
         RETURN b }",
    )
    .expect_err("a NEXT stage reads the kinds earlier stages established");
    assert_eq!(
        error.to_string(),
        "InvalidArgumentType: a single node is not a valid pattern predicate"
    );
}

/// The WHERE view overlays the projection, so a projected name's
/// classification — including the decision that it has none — always
/// supersedes the input's: a same-clause re-projection of a node name
/// to a per-row expression leaves the name unclassified, and its
/// lone-parenthesized form reads as grouping (the front passes it; the
/// boolean coercion is the executor's report), not as the stale
/// single-node rejection.
#[test]
fn a_reprojection_overlays_the_kinds_with_the_projection() {
    for query in [
        "MATCH (n) WITH n AS x WITH x.p1 AS x WHERE (x) RETURN x",
        "MATCH (n) WITH n AS x WITH 1 AS x WHERE (x) RETURN x",
        "MATCH (n) WITH n AS x WITH x.p1 AS x WHERE NOT(x) RETURN x",
    ] {
        parse_and_validate_expression_constraints(query)
            .unwrap_or_else(|e| panic!("{query}: the re-bound name is a value: {e}"));
    }
    // Without the re-projection the input's classification stands.
    let error =
        parse_and_validate_expression_constraints("MATCH (n) WITH n AS x WHERE (x) RETURN x")
            .expect_err("an unrebound node alias keeps the single-node rejection");
    assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
}

/// A subquery body nested in a WITH's WHERE sees what the WHERE sees —
/// the clause's expression view, same-clause aliases included: an
/// EXISTS body referencing the alias the same WITH introduced validates
/// (it was the front end's `UndefinedVariable` before the view was
/// unified), while the name an outer MATCH bound keeps flowing in as
/// ever.
#[test]
fn an_exists_body_sees_the_withs_own_aliases() {
    parse_and_validate_expression_constraints(
        "MATCH (n) WITH n AS m WHERE EXISTS { MATCH (x) WHERE m.p1 = x.p1 \
         RETURN count(*) } RETURN m",
    )
    .expect("the body seed derives from the clause's expression view");
    parse_and_validate_expression_constraints(
        "MATCH (n) WHERE EXISTS { MATCH (x) WHERE n.p1 = x.p1 RETURN count(*) } RETURN n",
    )
    .expect("a MATCH's own names flow into bodies as before");
}

/// The static-kind family reads its operands through the canonical
/// recognizers: a property access's base through `var_property` and a
/// builtin's argument through the paren-transparency helper, so the
/// parenthesized spellings `(p).foo` and `labels((p))` resolve through
/// the same paren policy as the classification itself.
#[test]
fn static_kind_rules_read_paren_transparent_operands() {
    let error = parse_and_validate_expression_constraints("MATCH p=(a)-[r]->(b) RETURN (p).foo")
        .expect_err("(p).foo is property access on a path");
    assert!(
        error
            .to_string()
            .contains("property access is not allowed on paths"),
        "{error}"
    );
    let error =
        parse_and_validate_expression_constraints("MATCH p=(a)-[r]->(b) RETURN labels((p))")
            .expect_err("labels((p)) is labels() on a path");
    assert_eq!(
        error.to_string(),
        "InvalidArgumentType: labels() requires a node, got PATH"
    );
}

/// A quantified path pattern's subpath variable binds a name (a list of
/// the sub-paths its group matched — a value, no kind): the name
/// survives its clause for every later name check, and its lone
/// parenthesized form is the value grouping, never the single-node
/// rejection.
#[test]
fn a_quantified_subpath_variable_binds_its_name() {
    parse_and_validate_expression_constraints(
        "MATCH (x)(p2 = (a)-->(b))*(y) WHERE p2 IS NOT NULL RETURN y",
    )
    .expect("the subpath name survives its own clause");
    parse_and_validate_expression_constraints(
        "MATCH (x)(p2 = (a)-->(b))*(y) WITH 1 AS z WHERE (p2) RETURN z",
    )
    .expect("the subpath variable is a value: its parens are grouping");
}

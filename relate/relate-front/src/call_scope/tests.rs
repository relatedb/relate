use super::*;

fn accepts(query: &str) {
    validate_call_scope_clauses(&relate_parser::parse(query).unwrap())
        .unwrap_or_else(|error| panic!("{query}: {error}"));
}

/// A scope-clause body's `WITH` filter may reference the name its own
/// projection renames: the WHERE trails the projection (`WITH p AS z
/// WHERE z < 2`), so `z` is visible to it. An earlier cut checked the
/// WHERE against the pre-projection scope alone and rejected the
/// rename as `UndefinedVariable: variable 'z' not defined` -- inside a
/// scope-clause body only; the same forms always ran at top level and
/// inside a naked `CALL { ... }`.
#[test]
fn a_renaming_with_filter_inside_a_scope_clause_body_is_visible_to_the_where() {
    accepts(
        "CALL () { \
             UNWIND [1, 2] AS p \
             WITH p AS z WHERE z < 2 \
             RETURN count(z) AS k } \
             RETURN k",
    );
    accepts(
        "UNWIND [1, 2] AS q \
             CALL (q) { WITH q AS z WHERE z < 2 RETURN count(z) AS k } \
             RETURN k",
    );
}

/// The filter's visibility keeps what the WITH received: the
/// unrenamed form (`WITH p WHERE p < 2`) and a read of a name the
/// projection drops both stay legal -- the same union the engine's
/// WITH-WHERE evaluation reads.
#[test]
fn a_with_filter_still_reads_the_names_the_clause_received() {
    accepts(
        "CALL () { \
             UNWIND [1, 2] AS p \
             WITH p WHERE p < 2 \
             RETURN count(p) AS k } \
             RETURN k",
    );
    accepts(
        "CALL () { \
             UNWIND [1, 2] AS p \
             WITH p AS z WHERE p < 2 \
             RETURN count(z) AS k } \
             RETURN k",
    );
}

/// A name neither the body's clauses nor the WITH's projection binds
/// stays the Rule 4/5 rejection.
#[test]
fn a_with_filter_on_a_name_the_body_never_binds_still_fails() {
    let error = validate_call_scope_clauses(
        &relate_parser::parse(
            "CALL () { \
                 UNWIND [1, 2] AS p \
                 WITH p AS z WHERE nosuch < 2 \
                 RETURN count(z) AS k } \
                 RETURN k",
        )
        .unwrap(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("variable 'nosuch' not defined"),
        "{error}"
    );
}

/// LABS-1615/1616's paren family, the call-scope side: a strict
/// scope-clause body's `WITH (a)` is the bare pass-through of `a` — the
/// paren-transparent spelling of `WITH a`, the lowered item being the
/// variable — so it is not a forbidden redeclaration of the imported
/// name, exactly like its unparenthesized twin. Before the paren
/// transparency reached `pass_through_var`, this spelling failed the
/// redeclaration gate its bare twin passed.
#[test]
fn a_parenthesized_with_pass_through_in_a_scope_clause_body_is_not_a_redeclaration() {
    accepts("WITH 1 AS a CALL (a) { WITH (a) RETURN a AS b } RETURN a, b");
    accepts("WITH 1 AS a CALL (a) { WITH a RETURN a AS b } RETURN a, b");
}

/// The 42N21 item rule reads the paren-transparent form too: a CALL
/// body's `RETURN (n)` carries the variable's name like the top-level
/// alias rule, not a divergent verdict.
#[test]
fn a_call_body_return_item_in_parens_carries_its_name() {
    accepts("CALL { MATCH (n) RETURN (n) } RETURN n");
    accepts("CALL { MATCH (n) RETURN n } RETURN n");
}

/// The 42N21 exemption vocabulary is a recorded, deliberately narrower
/// subset of the canonical-name shapes: a `v.key` property access names
/// a column at top level but is NOT exempt in a CALL body — the
/// rejection its unparenthesized twin has always gotten, pinned here so
/// the gate's member set is a decision, not a synchronization accident.
#[test]
fn a_call_body_return_of_a_property_access_still_requires_an_alias() {
    for query in [
        "CALL { MATCH (n) RETURN (n.p1) } RETURN n",
        "CALL { MATCH (n) RETURN n.p1 } RETURN n",
    ] {
        let error = validate_call_scope_clauses(&relate_parser::parse(query).unwrap()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "NoExpressionAlias: Expression in CALL { RETURN ... } must be aliased (use AS).",
            "{query}"
        );
    }
}

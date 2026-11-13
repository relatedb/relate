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

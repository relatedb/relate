//! The CALL-scope `RETURN *` rule: the star must expand
//! at least one variable the body itself introduces. Pinned at the planner
//! backstop, whose body walk is the one per-clause scope fold — so a
//! `CALL … YIELD` export counts like any other binding, not only the clause
//! kinds the rule used to spell out by hand.

use crate::binding::EnvBridge;
use relate_front::parse_and_lower;
use relate_ir::CallScope;
use relate_names::SmallName;

/// Lower and plan `cypher`, rendering a rejection as its error text.
fn plan(cypher: &str) -> Result<(), String> {
    let query = parse_and_lower(cypher).map_err(|error| error.to_string())?;
    crate::plan::plan(query, &mut |_| false)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// A body whose only export is a `CALL … YIELD` name satisfies the rule.
#[test]
fn a_call_yield_export_satisfies_the_scoped_return_star_rule() {
    plan("MATCH (n) CALL (n) { CALL db.labels() YIELD label RETURN * } RETURN n")
        .expect("a CALL ... YIELD export is a non-imported variable");
}

/// The other clause kinds the hand-spelled scan missed are exports too.
#[test]
fn merge_and_catalog_exports_satisfy_the_scoped_return_star_rule() {
    plan("MATCH (n) CALL (n) { MERGE (m:Bad) RETURN * } RETURN n")
        .expect("a MERGE pattern variable is a non-imported variable");
    plan("CALL () { SHOW FUNCTIONS YIELD name RETURN * } RETURN name")
        .expect("a catalog row's YIELD name is a non-imported variable");
}

/// A SEARCH clause's `SCORE AS` variable is a binding of the MATCH that
/// carries it, so a body whose MATCH binds only that (everything else it
/// projects is an import) still exports a non-imported name.
#[test]
fn a_search_score_variable_satisfies_the_scoped_return_star_rule() {
    plan(
        "MATCH (n) CALL (n) { MATCH (n:Person) \
         SEARCH n IN (FULLTEXT INDEX ft FOR 'x' LIMIT 10) SCORE AS s RETURN * } RETURN n",
    )
    .expect("a SEARCH SCORE AS variable is a non-imported variable");
}

/// The star exclusion itself stays: a body that introduces nothing but imports is
/// still rejected.
#[test]
fn a_star_over_imports_only_is_still_rejected() {
    let error = plan("MATCH (n) CALL (n) { RETURN * } RETURN n")
        .expect_err("a star over imports only is rejected");
    assert!(
        error.contains("NoVariablesInScope"),
        "expected the NoVariablesInScope rejection, got: {error}"
    );
}

/// LABS-712: a `RETURN *` arm of a scoped CALL body must not re-expand the
/// body's imports, so a UNION arm listing the same exported names agrees
/// with it instead of seeing the extra import columns.
#[test]
fn a_star_union_arm_excludes_the_body_imports() {
    plan(
        "WITH 1 AS a \
         CALL (a) { LET x = a + 1 RETURN * UNION RETURN 2 AS x } \
         RETURN a, x",
    )
    .expect("the star arm exports only `x`, matching the explicit arm");
}

/// The star-exclusion refusal walks the whole `NEXT` chain — a
/// final stage's `RETURN *` over an import-only scope is rejected even when
/// the first stage has no star to check.
#[test]
fn a_next_chained_star_over_imports_only_is_rejected() {
    let error = plan("MATCH (n) CALL (n) { CREATE (m:X) NEXT RETURN * } RETURN n")
        .expect_err("the chain's final star is import-only");
    assert!(
        error.contains("NoVariablesInScope"),
        "expected the NoVariablesInScope rejection, got: {error}"
    );
}

/// The same chain walk accepts a final-stage star that expands the previous
/// stage's export.
#[test]
fn a_next_chained_star_over_a_body_export_is_accepted() {
    plan("MATCH (n) CALL (n) { RETURN n AS m NEXT RETURN * } RETURN m")
        .expect("the star expands the body's own `m`");
}

/// Validate one body and return its export set, given the body's seed and
/// its import exclusion set.
fn body_exports(
    body: &str,
    env_names: &[String],
    seed_names: &[String],
    import_names: &std::collections::HashSet<String>,
) -> Vec<String> {
    let query = parse_and_lower(body).expect("the body lowers");
    let mut env = crate::binding::Env::new();
    for name in env_names {
        env.insert_name(
            SmallName::from_name(name.as_str()),
            crate::binding::Binding::Value,
        );
    }
    crate::validate::validate_query_with_env(&query, &env, seed_names, import_names, &mut |_| false)
        .expect("the body validates")
}

/// LABS-712: the two name lists a `CALL { … }` body validates with come
/// from one derivation. A naked body's seed is the whole outer scope — so
/// its `RETURN *` expands those names — while its star-exclusion set is
/// empty.
#[test]
fn a_naked_call_body_star_exports_the_outer_scope() {
    let outer = vec!["n".to_string()];
    let (seed, imports) = crate::names::call_body_names(&CallScope::None, &outer);
    assert_eq!(seed, outer);
    assert!(
        imports.is_empty(),
        "a naked body has no import exclusion set"
    );
    assert_eq!(
        body_exports("RETURN *", &outer, &seed, &imports),
        outer,
        "the naked body's star exports the outer scope"
    );
}

/// A scope-clause body's seed and its star-exclusion set are
/// both its named imports, so its `RETURN *` expands only what the body
/// itself binds.
#[test]
fn a_scope_clause_body_star_exports_only_its_own_names() {
    let scope = CallScope::Vars(vec![SmallName::from_name("a")]);
    let (seed, imports) = crate::names::call_body_names(&scope, &["a".to_string()]);
    assert_eq!(seed, vec!["a".to_string()]);
    assert_eq!(
        imports,
        seed.iter().cloned().collect(),
        "a scope clause's imports are its seed"
    );
    assert_eq!(
        body_exports("LET x = a + 1 RETURN *", &seed, &seed, &imports),
        vec!["x".to_string()],
        "the scope-clause body's star exports only its own `x`"
    );
}

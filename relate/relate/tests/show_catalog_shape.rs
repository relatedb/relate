//! `SHOW FUNCTIONS` / `SHOW PROCEDURES` row shape, live-verified against
//! Neo4j 2026.10.1 (Cypher 25): the catalog renders in case-sensitive name
//! order, and the `YIELD *`-only roles columns carry the reference server's
//! constant role lists.

mod common;

use relate::Value;

use common::{connection, err_text, rows, strings};

fn names(result: &[Vec<Value>]) -> Vec<&str> {
    result
        .iter()
        .map(|row| row[0].as_string().expect("name is a string"))
        .collect()
}

#[test]
fn show_functions_renders_the_reference_name_order() {
    let mut c = connection();
    // Live Neo4j 2026.10.1, no ORDER BY: the catalog's own order is the
    // name order.
    let r = rows(
        &mut c,
        "SHOW FUNCTIONS YIELD name, category WHERE category = 'Numeric'",
    );
    assert_eq!(
        names(&r),
        ["abs", "ceil", "ceiling", "floor", "isNaN", "rand", "round", "round", "round", "sign",]
    );
    // Case-sensitive: `percentileCont`/`percentileDisc` precede the GQL
    // `percentile_*` spellings; a case-insensitive sort would invert them.
    let r = rows(
        &mut c,
        "SHOW FUNCTIONS YIELD name WHERE name STARTS WITH 'percentile'",
    );
    assert_eq!(
        names(&r),
        [
            "percentileCont",
            "percentileDisc",
            "percentile_cont",
            "percentile_disc",
        ]
    );
}

#[test]
fn show_functions_carries_the_reference_roles_columns() {
    let mut c = connection();
    let r = rows(
        &mut c,
        "SHOW FUNCTIONS YIELD name, rolesExecution, rolesBoostedExecution WHERE name = 'abs'",
    );
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0][1],
        strings(&[
            "PUBLIC",
            "admin",
            "architect",
            "editor",
            "publisher",
            "reader"
        ])
    );
    assert_eq!(r[0][2], strings(&[]));
    // YIELD *-only: a bare WHERE cannot reference them.
    let err = err_text(c.query("SHOW FUNCTIONS WHERE rolesExecution IS NOT NULL RETURN name"));
    assert!(err.contains("UndefinedVariable"), "{err}");
}

#[test]
fn show_procedures_carries_the_reference_roles_columns() {
    let mut c = connection();
    let r = rows(
        &mut c,
        "SHOW PROCEDURES YIELD name, admin, rolesExecution, rolesBoostedExecution \
         WHERE name IN ['db.clearQueryCaches', 'db.labels']",
    );
    // Catalog order: db.clearQueryCaches (admin-only) before db.labels.
    assert_eq!(names(&r), ["db.clearQueryCaches", "db.labels"]);
    assert_eq!(r[0][1], Value::Bool(true));
    assert_eq!(r[0][2], strings(&["admin"]));
    assert_eq!(r[0][3], strings(&["admin"]));
    assert_eq!(r[1][1], Value::Bool(false));
    assert_eq!(r[1][2], strings(&["PUBLIC", "admin"]));
    assert_eq!(r[1][3], strings(&["admin"]));
    // YIELD *-only: a bare WHERE cannot reference them.
    let err = err_text(c.query("SHOW PROCEDURES WHERE isEmpty(rolesBoostedExecution) RETURN name"));
    assert!(err.contains("UndefinedVariable"), "{err}");
}

//! The closed side of ADR-0497's function-name admission gate: with no
//! `RELATE_PREVIEW` opt-in, the gated names are not surface at all, and
//! the removed/renamed-away spellings are unknown functions in every dialect
//! (the default-dialect half of the same disposition). This binary never
//! sets the variable (the enabled side is `labs384_coll_zip_has.rs` for
//! `coll-zip`/`coll-has`), so it must stay in its own file.
//!
//! The disabled form is the registry's ordinary unknown-function rejection
//! (42N48), not ADR-0371's authored "set RELATE_PREVIEW" error: a name
//! that must not exist cannot be named in an error without advertising it.

mod common;

use common::connection;

#[test]
fn gated_function_names_are_unknown_without_their_opt_in() {
    for query in [
        "RETURN coll.zip([1], [2]) AS x",
        "RETURN coll.hasAnyOf([1], [1]) AS x",
        "RETURN coll.hasAllOf([1], [1]) AS x",
        "RETURN coll.hasNoneOf([1], [2]) AS x",
        "RETURN stddev_samp(1.0) AS x",
        "RETURN stddev_pop(1.0) AS x",
        // Nested positions cannot hide a gated call.
        "CALL { RETURN coll.zip([1], [2]) AS x } RETURN x",
        "RETURN EXISTS { RETURN stddev_samp(1.0) AS x } AS e",
    ] {
        let error = connection()
            .query(query)
            .expect_err(&format!("`{query}` must be rejected without the opt-in"))
            .to_string();
        assert!(
            error.contains("UnknownFunction"),
            "`{query}`: the rejection must be the ordinary unknown function, got {error}"
        );
        assert!(
            !error.contains("RELATE_PREVIEW"),
            "`{query}`: the rejection must not advertise the gated name, got {error}"
        );
    }
}

#[test]
fn show_functions_lists_no_gated_name_without_its_opt_in() {
    // Derived from the one front table: hand-copied spellings go stale
    // silently, and `SHOW FUNCTIONS` renders the display spellings
    // (`coll.hasAnyOf`), so the comparison lowercases both sides.
    let gated: Vec<String> = relate_front::validate::gated_function_names()
        .map(str::to_ascii_lowercase)
        .collect();
    let mut conn = connection();
    let rows = conn.query("SHOW FUNCTIONS YIELD name RETURN name").unwrap();
    let listed: Vec<String> = rows
        .rows
        .iter()
        .map(|row| {
            row[0]
                .as_string()
                .expect("every catalogue row carries a string name")
                .to_ascii_lowercase()
        })
        .collect();
    // Positive controls: the unfiltered catalogue is non-empty and lists a
    // default-dialect name, so "no gated row" cannot pass vacuously.
    assert!(!listed.is_empty(), "SHOW FUNCTIONS listed no rows at all");
    assert!(
        listed.iter().any(|name| name == "coll.sort"),
        "the default-dialect `coll.sort` row must be listed, got {listed:?}"
    );
    for name in &gated {
        assert!(
            !listed.contains(name),
            "a disabled gate must not list `{name}`, got {listed:?}"
        );
    }
}

#[test]
fn removed_and_renamed_away_names_are_unknown_in_every_dialect() {
    // ADR-0497 rule (c): a removed name leaves both the callable and the
    // advertised registry, so the binder's ordinary unknown-function
    // rejection fires -- under the default dialect and an explicit
    // `CYPHER 25` preamble alike.
    for query in [
        "RETURN distance(point({x: 0, y: 0}), point({x: 1, y: 1})) AS d",
        "CYPHER 25 RETURN distance(point({x: 0, y: 0}), point({x: 1, y: 1})) AS d",
        "MATCH p = ()-->() RETURN rels(p) AS r",
        "RETURN typename(1) AS t",
        "CYPHER 25 RETURN typename(1) AS t",
        "RETURN vector.normalize([1.0]) AS v",
        "RETURN vector.add([1.0], [2.0]) AS v",
        "RETURN vector.manhattan([1.0], [2.0]) AS v",
        "RETURN vector.dotProduct([1.0], [2.0]) AS v",
        "RETURN spatial.contains(point({x: 0, y: 0}), point({x: 0, y: 0})) AS b",
        "RETURN linestring({coordinates: [[0.0, 0.0], [1.0, 1.0]]}) AS l",
        // `apoc.coll.sort` was deleted outright (LABS-912): the APOC
        // spelling left the registry in every dialect, so the ordinary
        // rejection replaces both the old Cypher-25 dialect gate and the
        // Cypher-5 preview opt-in.
        "RETURN apoc.coll.sort([2, 1]) AS x",
        "CYPHER 25 RETURN apoc.coll.sort([2, 1]) AS x",
    ] {
        let error = connection()
            .query(query)
            .expect_err(&format!("`{query}` must be rejected"))
            .to_string();
        assert!(
            error.contains("UnknownFunction"),
            "`{query}`: expected the ordinary unknown function, got {error}"
        );
    }
    // The canonical spellings the renames target stay callable.
    let mut conn = connection();
    conn.query("CREATE (:A)-[:T]->(:B)").unwrap();
    let row = conn
        .query(
            "MATCH p = ()-[r]->() RETURN relationships(p) AS r, valueType(1) AS t, \
             point.distance(point({x: 0.0, y: 0.0}), point({x: 3.0, y: 4.0})) AS d, \
             vector_distance(vector([1.0, 0.0], 2, FLOAT32), vector([1.0, 0.0], 2, FLOAT32), EUCLIDEAN) AS v, \
             stdev_samp(1.0) AS s",
        )
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(row[1], relate::Value::String("INTEGER NOT NULL".into()));
    assert_eq!(row[2], relate::Value::Float(5.0));
    assert_eq!(row[3], relate::Value::Float(0.0));
    // The canonical aggregate spelling dispatches too (one value: 0.0).
    assert_eq!(row[4], relate::Value::Float(0.0));
}

#[test]
fn the_added_names_are_listed_with_the_corpus_metadata() {
    let mut conn = connection();
    // CIP-217's `allReduce` and the `toBooleanList` list conversion are the
    // two names the alignment adds (ADR-0497): the display spelling, the
    // corpus category and the corpus description are the catalogue's own.
    let rows = conn
        .query(
            "SHOW FUNCTIONS YIELD name, category, description, aggregating \
             WHERE name IN ['allReduce', 'toBooleanList'] RETURN name, category, aggregating",
        )
        .unwrap();
    let listed: Vec<(String, String, bool)> = rows
        .rows
        .iter()
        .map(|row| {
            (
                row[0].as_string().expect("name is a string").to_string(),
                row[1]
                    .as_string()
                    .expect("category is a string")
                    .to_string(),
                row[2].as_bool().expect("aggregating is a bool"),
            )
        })
        .collect();
    // The catalogue renders in name order, so `allReduce` precedes
    // `toBooleanList`.
    assert_eq!(
        listed,
        [
            ("allReduce".to_string(), "Predicate".to_string(), false),
            ("toBooleanList".to_string(), "List".to_string(), false),
        ]
    );
}

#[test]
fn the_canonical_spellings_and_the_internal_trim_forms_stay_default_surface() {
    let mut conn = connection();
    // The rename targets are the advertised surface; `coll.sort()` and the
    // trim special forms keep working through the lowering's internal names.
    let row = conn
        .query(
            "RETURN coll.sort([2, 1]) AS a, ltrim(' x') AS b, rtrim('x ') AS c, \
             trim(LEADING 'x' FROM 'xxy') AS d, trim(TRAILING 'x' FROM 'yxx') AS e, \
             toBooleanList([1, 0, 'true', null]) AS f, valueType(1) AS g",
        )
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(
        row[0],
        relate::Value::List(vec![relate::Value::Int(1), relate::Value::Int(2)].into())
    );
    assert_eq!(row[1], relate::Value::String("x".into()));
    assert_eq!(row[2], relate::Value::String("x".into()));
    assert_eq!(row[3], relate::Value::String("y".into()));
    assert_eq!(row[4], relate::Value::String("y".into()));
    assert_eq!(
        row[5],
        relate::Value::List(
            vec![
                relate::Value::Bool(true),
                relate::Value::Bool(false),
                relate::Value::Bool(true),
                relate::Value::Null,
            ]
            .into()
        )
    );
    assert_eq!(row[6], relate::Value::String("INTEGER NOT NULL".into()));
}

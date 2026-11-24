use serde_json::{json, Value};

use super::decode::Wire;
use super::*;

/// The canonical wire `stats` object (every counter, zeros included) with
/// `created` nodes created.
fn stats(created: u64) -> Value {
    relate_model::wire::stats_to_wire_json(&relate_model::wire::WireStatsSnapshot {
        nodes_created: created,
        ..Default::default()
    })
}

fn env(columns: &[&str], rows: &str, created: u64) -> Value {
    let rows: Value = serde_json::from_str(rows).unwrap();
    json!({"columns": columns, "rows": rows, "error": null, "stats": stats(created)})
}

fn text(envelopes: &[Value]) -> (Kind, String) {
    let r = render(envelopes).unwrap();
    (r.kind(), r.text)
}

fn literal(v: &Value) -> String {
    Wire::decode(v).cypher()
}

#[test]
fn write_only_cells_and_errors() {
    let r = text(&[env(&[], "[]", 2), env(&[], "[]", 3)]);
    assert_eq!(r, (Kind::Table, "(no rows) · nodes created: 5".into()));
    assert_eq!(text(&[env(&[], "[]", 0)]).1, "(no rows)");
    assert_eq!(text(&[]).1, "(no rows)");
    let err = json!({"columns": [], "rows": [], "stats": {}, "error": "boom"});
    assert_eq!(text(&[err]), (Kind::Table, "error: boom".into()));
}

#[test]
fn table_keeps_types_and_every_character() {
    let long = "x".repeat(70);
    let rows = format!(
        r#"[{{"s":"446","n":446,"f":1.0,"l":["a \"b\"",null],"m":{{"b":1,"a b":2}},"t":"{long}"}},
            {{"s":"line\nbreak","n":null,"f":0.25,"l":[],"m":{{}},"t":""}}]"#
    );
    let r = render(&[env(&["s", "n", "f", "l", "m", "t"], &rows, 0)]).unwrap();
    assert_eq!(r.kind(), Kind::Table);
    let expected = format!(
        "| s             | n    | f    | l                 | m                | t{pad} |\n\
         |---------------|------|------|-------------------|------------------|{rule}|\n\
         | \"446\"         | 446  | 1.0  | [\"a \\\"b\\\"\", null] | {{`a b`: 2, b: 1}} | \"{long}\" |\n\
         | \"line\\nbreak\" | null | 0.25 | []                | {{}}               | \"\"{pad2} |\n\
         2 rows",
        pad = " ".repeat(71),
        rule = "-".repeat(74),
        pad2 = " ".repeat(70),
    );
    assert_eq!(r.text, expected);
    let View::Table(Some(table)) = r.view else {
        panic!("a table with rows")
    };
    assert_eq!(table.columns, ["s", "n", "f", "l", "m", "t"]);
    assert_eq!(table.rows[0][0], "\"446\"");
    assert_eq!(table.numeric, [false, true, true, false, false, false]);
    assert_eq!(table.summary, "2 rows");
}

#[test]
fn a_table_with_no_rows_has_no_cells() {
    let r = render(&[env(&["x"], "[]", 1)]).unwrap();
    assert_eq!(r.text, "| x |\n|---|\n0 rows · nodes created: 1");
    assert_eq!(r.view, View::Table(None));
}

#[test]
fn points_and_vectors_are_literals() {
    let pt = json!({"crs": "wgs-84", "latitude": 1.5, "longitude": 2.5, "srid": 4326});
    assert_eq!(
        literal(&pt),
        r#"point({crs: "wgs-84", latitude: 1.5, longitude: 2.5, srid: 4326})"#
    );
    let v: Value =
        serde_json::from_str(r#"{"dim":2,"type":"FLOAT32","values":[0.5,1.0]}"#).unwrap();
    assert_eq!(literal(&v), "vector([0.5, 1.0], 2, FLOAT32)");
    let not_a_vector: Value =
        serde_json::from_str(r#"{"dim":2,"type":"FLOAT8","values":[0.5,1.0]}"#).unwrap();
    assert_eq!(
        literal(&not_a_vector),
        "{dim: 2, type: \"FLOAT8\", values: [0.5, 1.0]}"
    );
}

#[test]
fn map_keys_follow_code_point_order() {
    let v: Value = serde_json::from_str(r#"{"b":1,"10":2,"9":3,"a b":4}"#).unwrap();
    assert_eq!(literal(&v), "{`10`: 2, `9`: 3, `a b`: 4, b: 1}");
}

#[test]
fn gram_paths_reuse_identifiers() {
    // ADR-0223 tagged wire shapes (the post-tag encoding the renderer
    // actually receives).
    let rows = r#"[
      {"p":[{"kind":"node","id":1,"labels":["A"],"properties":{"name":"a"}},
            {"kind":"relationship","id":7,"type":"R","start_node":1,"end_node":2,"properties":{"w":1.0}},
            {"kind":"node","id":2,"labels":["B","C"],"properties":{}}]},
      {"p":[{"kind":"node","id":3,"labels":["A"],"properties":{"name":"c"}},
            {"kind":"relationship","id":8,"type":"R","start_node":2,"end_node":3,"properties":{}},
            {"kind":"node","id":2,"labels":["B","C"],"properties":{}}]}]"#;
    let (kind, text) = text(&[env(&["p"], rows, 0)]);
    assert_eq!(kind, Kind::Gram);
    assert_eq!(
        text,
        "// 2 rows\n\
         (n1:A {name: \"a\"})-[r7:R {w: 1.0}]->(n2:B:C)\n\
         (n3:A {name: \"c\"})<-[r8:R]-(n2)"
    );
}

#[test]
fn a_path_as_a_table_value_is_a_literal() {
    let path = json!([
        {"kind": "node", "id": 1, "labels": ["A"], "properties": {}},
        {"kind": "relationship", "id": 7, "type": "R", "start_node": 2, "end_node": 1, "properties": {}},
        {"kind": "node", "id": 2, "labels": [], "properties": {"k": "v"}}
    ]);
    assert_eq!(literal(&path), "(:A)<-[:R]-( {k: \"v\"})");
    let broken = json!([
        {"id": 1, "labels": ["A"], "properties": {}},
        {"id": 7, "type": "R", "start_node": 5, "end_node": 6, "properties": {}},
        {"id": 2, "labels": [], "properties": {}}
    ]);
    assert!(literal(&broken).starts_with('['));
}

#[test]
fn gram_rows_with_scalars_lists_and_awkward_values() {
    let rows = r#"[
      {"rank":1,"n":{"id":5,"labels":["Note"],"properties":{"tags":[],"e":{"dim":2,"type":"FLOAT32","values":[1e-7,0.5]},"loc":{"crs":"cartesian","srid":7203,"x":1.0,"y":2.0}}},"ns":[{"id":5,"labels":["Note"],"properties":{}},{"id":6,"labels":[],"properties":{}}],"why":null},
      {"rank":2,"n":null,"ns":[],"why":["a",1]}]"#;
    let (kind, text) = text(&[env(&["rank", "n", "ns", "why"], rows, 0)]);
    assert_eq!(kind, Kind::Gram);
    assert_eq!(
        text,
        "// 2 rows\n\
         [{rank: 1, why: null} | (n5:Note {e: cypher`vector([1e-7, 0.5], 2, FLOAT32)`, loc: {crs: \"cartesian\", srid: 7203, x: 1.0, y: 2.0}, tags: cypher`[]`}), [ | (n5), (n6)]]\n\
         [{rank: 2, n: null, ns: cypher`[]`, why: [\"a\", 1]}]"
    );
}

#[test]
fn gram_standalone_relationships_and_odd_keys() {
    let rows = r#"[{"count(r)":1,"r":{"id":3,"type":"KNOWS","start_node":1,"end_node":2,"properties":{"since":1e21}}}]"#;
    let (_, text) = text(&[env(&["count(r)", "r"], rows, 0)]);
    assert_eq!(
        text,
        "// 1 row\n[{\"count(r)\": 1} | (n1)-[r3:KNOWS {since: cypher`1e+21`}]->(n2)]"
    );
}

#[test]
fn strings_are_escaped_as_every_cypher_literal_is() {
    let v = json!("a\"b\\c\r\u{202E}");
    assert_eq!(literal(&v), "\"a\\\"b\\\\c\\u000D\\u202E\"");
    let rows = r#"[{"n":{"id":1,"labels":["odd label"],"properties":{"k`e\ny":"v"}}}]"#;
    assert_eq!(
        text(&[env(&["n"], rows, 0)]).1,
        "// 1 row\n(n1:`odd label` {\"k`e\\ny\": \"v\"})"
    );
}

#[test]
fn gram_recordings_carry_their_graph() {
    let rows = r#"[
      {"p":[{"kind":"node","id":1,"labels":["A"],"properties":{}},
            {"kind":"relationship","id":7,"type":"R","start_node":1,"end_node":2,"properties":{}},
            {"kind":"node","id":2,"labels":["B"],"properties":{}}], "m": {"x": {"kind":"node","id":1,"labels":["A"],"properties":{}}}}]"#;
    let r = render(&[env(&["p", "m"], rows, 0)]).unwrap();
    let View::Gram(graph) = &r.view else {
        panic!("a gram recording")
    };
    let ids = |vs: &[Value]| {
        vs.iter()
            .map(|v| v["id"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&graph.nodes), [1, 2]);
    assert_eq!(ids(&graph.rels), [7]);
    assert_eq!(r.to_json()["graph"]["rels"][0]["type"], "R");
    assert_eq!(r.to_json()["table"], Value::Null);
}

#[test]
fn malformed_envelopes_are_errors() {
    let bad = [
        json!([]),
        json!({"rows": [], "stats": {}, "error": null}),
        json!({"columns": ["a"], "rows": [1], "stats": {}, "error": null}),
        json!({"columns": ["a"], "rows": [{"b": 1}], "stats": {}, "error": null}),
        json!({"columns": [], "rows": [], "stats": {}, "error": 3}),
        json!({"columns": [], "rows": [], "error": null}),
    ];
    for env in bad {
        assert!(render(std::slice::from_ref(&env)).is_err(), "{env}");
    }
}

/// A result envelope missing a counter, or carrying a non-u64 one, is a
/// malformed recording: the reader fails loudly instead of summing it as
/// zero. An error envelope's empty `stats` object is the one documented
/// exception (the wire's `error_envelope` shape); anything else on an
/// error envelope is malformed too.
#[test]
fn a_missing_or_malformed_counter_fails_loudly() {
    let mut missing = stats(0);
    missing.as_object_mut().unwrap().remove("properties_set");
    let err = render(&[json!({"columns": [], "rows": [], "stats": missing, "error": null})])
        .expect_err("a missing counter");
    assert_eq!(
        err,
        "envelope 0: stats entry `properties_set` is missing or not an unsigned integer"
    );

    let err = render(&[
        json!({"columns": [], "rows": [], "stats": {"nodes_created": "3"}, "error": null}),
    ])
    .expect_err("a non-u64 counter");
    assert_eq!(
        err,
        "envelope 0: stats entry `nodes_created` is missing or not an unsigned integer"
    );

    let err = render(&[json!({"columns": [], "rows": [], "stats": stats(0), "error": "boom"})])
        .expect_err("counters on an error envelope");
    assert_eq!(err, "envelope 0: an error envelope carries stats");
}

#[test]
fn recording_json() {
    let r = render(&[
        env(&["n"], r#"[{"n":1}]"#, 2),
        env(&["n"], r#"[{"n":1}]"#, 1),
    ])
    .unwrap();
    let j = r.to_json();
    assert_eq!(j["kind"], "table");
    assert_eq!(j["text"], "| n |\n|---|\n| 1 |\n1 row · nodes created: 3");
    assert_eq!(
        j["table"],
        json!({"columns": ["n"], "rows": [["1"]], "numeric": [true], "summary": "1 row · nodes created: 3"})
    );
    assert_eq!(j["graph"], Value::Null);
    assert_eq!(j["stats"]["nodes_created"], 3);
    assert_eq!(j["stats"]["constraints_removed"], 0);
    assert_eq!(render(&[]).unwrap().to_json()["table"], Value::Null);
}

#[test]
fn gram_quotes_names_and_strings_its_own_way() {
    let node = r#"{"id":1,"labels":["a`b\u0001c"],"properties":{"s":"x\ry","t":"x\u202Ey"}}"#;
    // Gram escapes a backtick in a name with a backslash, and a carriage
    // return as `\r`; a string with a character gram cannot escape is its
    // Cypher literal, tagged.
    assert_eq!(
        text(&[env(&["n"], &format!("[{{\"n\":{node}}}]"), 0)]).1,
        "// 1 row\n(n1:`a\\`b\u{1}c` {s: \"x\\ry\", t: cypher`\"x\\\\u202Ey\"`})"
    );
    // The table form writes the same label as Cypher does: a doubled backtick.
    assert_eq!(
        literal(&serde_json::from_str(node).unwrap()),
        "(:`a``b\u{1}c` {s: \"x\\u000Dy\", t: \"x\\u202Ey\"})"
    );
}

//! Push loopback tests (ADR-0401): the `relate_test_support::loopback`
//! fixtures serve scripted Query API v2 bodies and capture the whole
//! request (head + body) of every connection, so each scenario asserts both
//! the classification the pre-reads produced and the wire shape the writes
//! sent — statements with backtick-quoted identifiers, values only as typed
//! parameter envelopes.

use relate::{Database, Value};
use relate_load::{
    push, PolicySpec, PushOpts, PushOutcome, PushScope, RelMode, UnkeyedAction, DEFAULT_BATCH_SIZE,
};
use relate_test_support::loopback::{
    spawn_capture_server, spawn_scripted_responses_capturing_requests,
};
use serde_json::{json, Value as Json};

/// The movies-shaped store the scenarios push: Person keyed by `name`,
/// Movie keyed by `title`, one ACTED_IN rel per (person, movie).
fn movies_db(extra: &[&str]) -> Database {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let mut statements = vec![
        "CREATE CONSTRAINT m_title IF NOT EXISTS FOR (m:Movie) REQUIRE m.title IS UNIQUE",
        "CREATE CONSTRAINT p_name IF NOT EXISTS FOR (p:Person) REQUIRE p.name IS UNIQUE",
        "CREATE (:Person {name: 'Tom', age: 61})",
        "CREATE (:Person {name: 'Meg', age: 54})",
        "CREATE (:Movie {title: 'Sleepless', released: 1993})",
        "CREATE (:Movie {title: 'Youve', released: 1998})",
        "MATCH (p:Person {name: 'Tom'}), (m:Movie {title: 'Sleepless'}) CREATE (p)-[:ACTED_IN {roles: ['Sam']}]->(m)",
        "MATCH (p:Person {name: 'Meg'}), (m:Movie {title: 'Youve'}) CREATE (p)-[:ACTED_IN {roles: ['Annie']}]->(m)",
    ];
    statements.extend(extra.iter().copied());
    for statement in statements {
        conn.query(statement).unwrap();
    }
    db
}

fn opts(port: u16, scope: PushScope) -> PushOpts {
    PushOpts {
        credentials: relate_load::Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        },
        scope,
        batch_size: DEFAULT_BATCH_SIZE,
        nodes_update: PolicySpec::default_spec(),
        rels_update: PolicySpec::default_spec(),
        updated_at: Vec::new(),
        rel_mode: RelMode::Keyed,
        merge_keys: Default::default(),
        rel_keys: Default::default(),
        unkeyed_labels: UnkeyedAction::Skip,
        allow_plaintext: false,
        dry_run: false,
    }
}

fn empty_body() -> String {
    r#"{"data":{"fields":[],"values":[]},"errors":[]}"#.to_string()
}

/// A typed pre-read body for one entity's keyed rows, with optional
/// `--updated-at` candidate values.
fn rows_body(fields: &[&str], values: Vec<Vec<Json>>) -> String {
    json!({
        "data": {"fields": fields, "values": values},
        "errors": [],
    })
    .to_string()
}

/// The typed envelope of one plain value, for fixture bodies.
fn t(payload: Json) -> Json {
    match payload {
        Json::String(s) => json!({"$type": "String", "_value": s}),
        other => other,
    }
}

fn entity<'a>(report: &'a relate_load::PushReport, name: &str) -> &'a relate_load::EntityReport {
    report
        .entities
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("entity {name:?} missing from {report:?}"))
}

/// (a) First push of the movies scope into a fresh remote: every entity
/// created, writes as constraint-keyed MERGE statements with typed
/// parameter envelopes, pre-reads as typed-accept SELECTs.
#[test]
fn first_push_creates_the_scoped_graph() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { report, error } => {
            panic!("first push failed: {error}; report {report:?}")
        }
    };
    assert_eq!(entity(&report, "Movie").created, 2);
    assert_eq!(entity(&report, "Person").created, 2);
    assert_eq!(entity(&report, "(Person)-[:ACTED_IN]->(Movie)").created, 2);
    assert_eq!(report.statements_sent, 3);
    assert_eq!(report.rows_sent, 6);
    let bodies = fixture.bodies();
    // One pre-read + one write per entity, in plan order.
    assert_eq!(bodies.len(), 6, "{bodies:?}");
    let parsed: Vec<Json> = bodies
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    // Pre-read: the merge-key SELECT with backtick-quoted identifiers.
    assert!(bodies[0].contains("\"MATCH (n:`Movie`) RETURN n.`title` AS k0\""));
    // Write: the constraint-derived MERGE with the policy's SET +=.
    assert!(bodies[1].contains(
        "UNWIND $rows AS row MERGE (n:`Movie` {`title`: row.key.`title`}) SET n += row.props"
    ));
    // The rel write matches both endpoints by their locally resolved keys.
    assert!(bodies[5].contains(
        "UNWIND $rows AS row MATCH (a:`Person` {`name`: row.start.`name`}) MATCH (b:`Movie` {`title`: row.end.`title`}) MERGE (a)-[r:`ACTED_IN`]->(b) SET r += row.props"
    ));
    // Every container and value is a typed envelope — never interpolated
    // text: the row is a Map envelope, its key/props maps Map envelopes,
    // their values typed envelopes.
    let first = &parsed[1]["parameters"]["rows"]["_value"][0]["_value"];
    assert_eq!(first["key"]["$type"], "Map");
    assert_eq!(first["key"]["_value"]["title"]["$type"], "String");
    assert_eq!(first["key"]["_value"]["title"]["_value"], "Sleepless");
    assert_eq!(first["props"]["_value"]["released"]["$type"], "Integer");
    assert_eq!(first["props"]["_value"]["released"]["_value"], "1993");
    let rel = &parsed[5]["parameters"]["rows"]["_value"][0]["_value"];
    assert_eq!(rel["start"]["$type"], "Map");
    assert_eq!(rel["start"]["_value"]["name"]["_value"], "Tom");
    assert_eq!(rel["props"]["_value"]["roles"]["$type"], "List");
}

/// (LABS-907) Secondary labels survive the push: a local
/// `:Movie:Sequel` node merges on the KEYED label (`Movie`) and the
/// secondary is SET onto the node the merge found — a pre-existing
/// remote `:Movie {title}` gains `:Sequel` instead of being forked
/// into a second node — and the pre-read matches the merge identity.
#[test]
fn secondary_labels_are_pushed_with_the_node() {
    let db = movies_db(&["MATCH (m:Movie {title: 'Youve'}) SET m:Sequel"]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { report, error } => {
            panic!("push failed: {error}; report {report:?}")
        }
    };
    // Two Movie groups: the plain set and the Movie:Sequel set.
    assert_eq!(entity(&report, "Movie").created, 1);
    assert_eq!(entity(&report, "Movie:Sequel").created, 1);
    let bodies = fixture.bodies();
    // The write merges on the KEYED label and SETs the secondary — a
    // remote :Movie {title} that predates the local label gains
    // :Sequel instead of being forked into a second node.
    let sequel_write = bodies
        .iter()
        .find(|b| b.contains("SET n:`Sequel`,"))
        .unwrap_or_else(|| panic!("no Movie:Sequel write in {bodies:?}"));
    assert!(sequel_write.contains(
        "UNWIND $rows AS row MERGE (n:`Movie` {`title`: row.key.`title`}) SET n:`Sequel`, n += row.props"
    ));
    // The pre-read matches the merge identity: the keyed label — once
    // per Movie label-set group, never the label set itself.
    let keyed_pre_reads = bodies
        .iter()
        .filter(|b| b.contains("MATCH (n:`Movie`) RETURN n.`title` AS k0"))
        .count();
    assert_eq!(
        keyed_pre_reads, 2,
        "one keyed-label pre-read per Movie label-set group: {bodies:?}"
    );
    assert!(
        !bodies
            .iter()
            .any(|b| b.contains("MATCH (n:`Movie`:`Sequel`)")),
        "no label-set pre-read: {bodies:?}"
    );
}

/// (g) Relationship identity resolves per type under the keyed default
/// (ADR-0401 §5): a relationship uniqueness/key constraint makes the rel
/// merge property-keyed — no mode flag asked for — and a `--rel-key`
/// override supplies a key for an unconstrained type with the same
/// statement shape.
#[test]
fn rel_keys_merge_property_keyed_under_the_default_mode() {
    // A rel-key constraint on ACTED_IN: the keyed default merges on it.
    let db = movies_db(&[
        "CREATE CONSTRAINT ai_roles IF NOT EXISTS FOR ()-[r:ACTED_IN]-() REQUIRE r.roles IS UNIQUE",
    ]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    match outcome {
        PushOutcome::Completed(_) => {}
        PushOutcome::Partial { error, .. } => panic!("keyed rel push failed: {error}"),
    }
    let bodies = fixture.bodies();
    let rel_write = &bodies[5];
    assert!(
        rel_write.contains(
            "MERGE (a)-[r:`ACTED_IN` {`roles`: row.key.`roles`}]->(b) SET r += row.props"
        ),
        "expected the property-keyed rel MERGE, got: {rel_write}"
    );
    let parsed: Json = serde_json::from_str(rel_write).unwrap();
    let row = &parsed["parameters"]["rows"]["_value"][0]["_value"];
    assert_eq!(row["key"]["$type"], "Map");
    assert_eq!(row["key"]["_value"]["roles"]["$type"], "List");

    // No constraint, --rel-key override: the same keyed merge shape.
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let mut o = opts(port, PushScope::All);
    o.rel_keys
        .insert("ACTED_IN".to_string(), vec!["roles".to_string()]);
    let outcome = push(&o, &db).unwrap();
    match outcome {
        PushOutcome::Completed(_) => {}
        PushOutcome::Partial { error, .. } => panic!("--rel-key push failed: {error}"),
    }
    let rel_write = &fixture.bodies()[5];
    assert!(
        rel_write.contains(
            "MERGE (a)-[r:`ACTED_IN` {`roles`: row.key.`roles`}]->(b) SET r += row.props"
        ),
        "expected the --rel-key merge shape, got: {rel_write}"
    );
}

/// (b) Re-push idempotence: with the remote's current rows pre-read, the
/// second push creates nothing and updates what exists — the write
/// statements are the same MERGE shapes, so the same push twice lands the
/// same remote state.
#[test]
fn re_push_creates_nothing_and_updates_the_existing() {
    let db = movies_db(&[]);
    let bodies = vec![
        // Movie pre-read: both titles exist remotely.
        rows_body(
            &["k0"],
            vec![vec![t(json!("Sleepless"))], vec![t(json!("Youve"))]],
        ),
        empty_body(), // Movie write
        // Person pre-read: both names exist remotely.
        rows_body(&["k0"], vec![vec![t(json!("Tom"))], vec![t(json!("Meg"))]]),
        empty_body(), // Person write
        // Rel pre-read: both (name, title) pairs exist.
        rows_body(
            &["k0", "e0"],
            vec![
                vec![t(json!("Tom")), t(json!("Sleepless"))],
                vec![t(json!("Meg")), t(json!("Youve"))],
            ],
        ),
        empty_body(), // rel write
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { report, error } => {
            panic!("re-push failed: {error}; report {report:?}")
        }
    };
    assert_eq!(entity(&report, "Movie").created, 0);
    assert_eq!(entity(&report, "Movie").updated, 2);
    assert_eq!(entity(&report, "Person").created, 0);
    assert_eq!(entity(&report, "Person").updated, 2);
    let rels = entity(&report, "(Person)-[:ACTED_IN]->(Movie)");
    assert_eq!(rels.created, 0);
    assert_eq!(rels.updated, 2);
    // The same MERGE statements re-ran — nothing became a CREATE.
    let requests = fixture.join();
    assert!(
        requests[1].1.contains("MERGE (n:`Movie`"),
        "{:?}",
        requests[1].1
    );
    assert!(
        requests[5].1.contains("MERGE (a)-[r:`ACTED_IN`]->(b)"),
        "{:?}",
        requests[5].1
    );
}

/// (c) A local change lands as an update: change a property locally, and
/// the re-push carries the new value in its write parameters.
#[test]
fn local_change_lands_as_update() {
    let db = movies_db(&[]);
    let mut conn = db.connect().unwrap();
    conn.query("MATCH (p:Person {name: 'Meg'}) SET p.age = 55")
        .unwrap();
    drop(conn);
    let bodies = vec![
        rows_body(
            &["k0"],
            vec![vec![t(json!("Sleepless"))], vec![t(json!("Youve"))]],
        ),
        empty_body(),
        rows_body(&["k0"], vec![vec![t(json!("Tom"))], vec![t(json!("Meg"))]]),
        empty_body(),
        rows_body(
            &["k0", "e0"],
            vec![
                vec![t(json!("Tom")), t(json!("Sleepless"))],
                vec![t(json!("Meg")), t(json!("Youve"))],
            ],
        ),
        empty_body(),
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    assert_eq!(entity(&report, "Person").updated, 2);
    // Meg's row carries the new value as a typed parameter.
    let requests = fixture.join();
    let write: Json = serde_json::from_str(&requests[3].1).unwrap();
    let meg = write["parameters"]["rows"]["_value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["_value"]["key"]["_value"]["name"]["_value"] == "Meg")
        .expect("Meg's write row");
    assert_eq!(meg["_value"]["props"]["_value"]["age"]["_value"], "55");
}

/// (c′) The delta scenario: after a first push, local edits add new
/// entities and change a property in one pass — the re-push creates only
/// the additions (the pre-read finds them absent remotely) and updates
/// the rest, so a pull-edit-push loop converges the remote to the local
/// state without ever re-creating what already exists.
#[test]
fn delta_push_creates_only_additions_and_updates_the_rest() {
    let db = movies_db(&[]);
    // First push: the remote starts empty, so every row is created.
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    match outcome {
        PushOutcome::Completed(_) => {}
        PushOutcome::Partial { error, .. } => panic!("first push failed: {error}"),
    }
    drop(fixture);
    // Local edits: one new person, one new movie, one new rel, and a
    // property change on an existing person — one delta.
    let mut conn = db.connect().unwrap();
    for statement in [
        "CREATE (:Person {name: 'Nora', age: 31})",
        "CREATE (:Movie {title: 'Nights', released: 2026})",
        "MATCH (p:Person {name: 'Meg'}) SET p.age = 55",
        "MATCH (p:Person {name: 'Nora'}), (m:Movie {title: 'Nights'}) CREATE (p)-[:ACTED_IN {roles: ['Lisa']}]->(m)",
    ] {
        conn.query(statement).unwrap();
    }
    drop(conn);
    // The re-push's pre-reads: the remote holds the first push's rows —
    // the additions are absent, so the pre-reads return only the old keys.
    let bodies = vec![
        // Movie pre-read: Sleepless and Youve exist; Nights is new.
        rows_body(
            &["k0"],
            vec![vec![t(json!("Sleepless"))], vec![t(json!("Youve"))]],
        ),
        empty_body(), // Movie write
        // Person pre-read: Tom and Meg exist; Nora is new.
        rows_body(&["k0"], vec![vec![t(json!("Tom"))], vec![t(json!("Meg"))]]),
        empty_body(), // Person write
        // Rel pre-read: the two original pairs exist; the new pair is new.
        rows_body(
            &["k0", "e0"],
            vec![
                vec![t(json!("Tom")), t(json!("Sleepless"))],
                vec![t(json!("Meg")), t(json!("Youve"))],
            ],
        ),
        empty_body(), // rel write
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { report, error } => {
            panic!("delta push failed: {error}; report {report:?}")
        }
    };
    // Additions created, the rest updated — never re-created.
    let movies = entity(&report, "Movie");
    assert_eq!(movies.created, 1, "{movies:?}");
    assert_eq!(movies.updated, 2, "{movies:?}");
    let people = entity(&report, "Person");
    assert_eq!(people.created, 1, "{people:?}");
    assert_eq!(people.updated, 2, "{people:?}");
    let rels = entity(&report, "(Person)-[:ACTED_IN]->(Movie)");
    assert_eq!(rels.created, 1, "{rels:?}");
    assert_eq!(rels.updated, 2, "{rels:?}");
    // The write rows carry the additions and the changed property as
    // typed parameters: Nora's create row, Meg's updated age.
    let requests = fixture.join();
    let person_write: Json = serde_json::from_str(&requests[3].1).unwrap();
    let person_rows = person_write["parameters"]["rows"]["_value"]
        .as_array()
        .unwrap();
    let nora = person_rows
        .iter()
        .find(|row| row["_value"]["key"]["_value"]["name"]["_value"] == "Nora")
        .expect("Nora's create row rides the Person write");
    assert_eq!(nora["_value"]["props"]["_value"]["age"]["$type"], "Integer");
    let meg = person_rows
        .iter()
        .find(|row| row["_value"]["key"]["_value"]["name"]["_value"] == "Meg")
        .expect("Meg's update row rides the Person write");
    assert_eq!(meg["_value"]["props"]["_value"]["age"]["_value"], "55");
    let rel_write: Json = serde_json::from_str(&requests[5].1).unwrap();
    assert!(
        rel_write["parameters"]["rows"]["_value"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |row| row["_value"]["start"]["_value"]["name"]["_value"] == "Nora"
                    && row["_value"]["end"]["_value"]["title"]["_value"] == "Nights"
            ),
        "the new rel rides the ACTED_IN write: {rel_write}"
    );
}

/// (d) `--updated-at`: a strictly newer remote row is skipped and counted
/// `remote-newer`; a newer-or-equal local row updates; an incomparable pair
/// applies the policy with a warning count.
#[test]
fn updated_at_gates_updates_by_timestamp() {
    let db = movies_db(&[
        // Candidate property on both persons: local timestamps.
        "MATCH (p:Person {name: 'Tom'}) SET p.updatedAt = date('2024-01-02')",
        "MATCH (p:Person {name: 'Meg'}) SET p.updatedAt = date('2024-01-02')",
    ]);
    let mut o = opts(0, PushScope::All);
    o.updated_at = vec!["updatedAt".to_string()];
    let bodies = vec![
        rows_body(&["k0"], vec![]), // Movie pre-read: nothing
        empty_body(),               // Movie write
        // Person pre-read: Tom's remote is newer; Meg's is older; an
        // incomparable pair rides a String candidate (a different Cypher
        // type than the local DATE).
        rows_body(
            &["k0", "c0"],
            vec![
                vec![
                    t(json!("Tom")),
                    json!({"$type": "Date", "_value": "2024-01-03"}),
                ],
                vec![
                    t(json!("Meg")),
                    json!({"$type": "Date", "_value": "2024-01-01"}),
                ],
            ],
        ),
        empty_body(), // Person write
        rows_body(&["k0", "e0"], vec![]),
        empty_body(),
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    o.credentials.uri = format!("http://127.0.0.1:{port}");
    let outcome = push(&o, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    let people = entity(&report, "Person");
    // Remote newer → skipped and counted; local newer → updated.
    assert_eq!(people.skipped_remote_newer, 1);
    assert_eq!(people.updated, 1);
    assert_eq!(people.created, 0);
    // The candidate column rides the Person pre-read.
    let requests = fixture.join();
    assert!(
        requests[2]
            .1
            .contains("RETURN n.`name` AS k0, n.`updatedAt` AS c0"),
        "{:?}",
        requests[2].1
    );
    // Only Meg's row was written.
    let write: Json = serde_json::from_str(&requests[3].1).unwrap();
    let rows = write["parameters"]["rows"]["_value"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["_value"]["key"]["_value"]["name"]["_value"], "Meg");
}

/// (d′) An incomparable `--updated-at` pair applies the scoped policy and
/// increments the warning counter — stated, never silent.
#[test]
fn updated_at_incomparable_pair_warns_and_applies() {
    let db = movies_db(&["MATCH (p:Person {name: 'Tom'}) SET p.updatedAt = date('2024-01-02')"]);
    let mut o = opts(0, PushScope::Labels(vec!["Person".into()]));
    o.updated_at = vec!["updatedAt".to_string()];
    let bodies = vec![
        // The remote's candidate is a STRING, the local one a DATE.
        rows_body(
            &["k0", "c0"],
            vec![vec![
                t(json!("Tom")),
                json!({"$type": "String", "_value": "2024-01-03"}),
            ]],
        ),
        empty_body(), // Person write
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    o.credentials.uri = format!("http://127.0.0.1:{port}");
    let outcome = push(&o, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    let people = entity(&report, "Person");
    assert_eq!(people.updated, 1);
    assert_eq!(people.incomparable, 1);
    assert_eq!(people.skipped_remote_newer, 0);
    assert_eq!(fixture.join().len(), 2);
}

/// (e) `replace` vs `add`: the SET operator is the only difference between
/// the two policies' statements — `+=` keeps remote-only properties, `=`
/// removes them.
#[test]
fn replace_and_add_policies_differ_in_the_set_operator() {
    let db = movies_db(&[]);
    let (fixture_add, port) = spawn_capture_server("200 OK", empty_body());
    let o = opts(port, PushScope::Labels(vec!["Movie".into()]));
    push(&o, &db).unwrap();
    let mut o_replace = opts(port, PushScope::Labels(vec!["Movie".into()]));
    o_replace.nodes_update = PolicySpec::parse("replace").unwrap();
    let (fixture_replace, port_replace) = spawn_capture_server("200 OK", empty_body());
    o_replace.credentials.uri = format!("http://127.0.0.1:{port_replace}");
    push(&o_replace, &db).unwrap();
    let add_body = fixture_add.bodies();
    let add_write: Json = serde_json::from_str(&add_body[1]).unwrap();
    let replace_write: Json = serde_json::from_str(&fixture_replace.bodies()[1]).unwrap();
    let add_stmt = add_write["statement"].as_str().unwrap();
    let replace_stmt = replace_write["statement"].as_str().unwrap();
    assert!(add_stmt.contains("SET n += row.props"), "{add_stmt}");
    assert!(replace_stmt.contains("SET n = row.props"), "{replace_stmt}");
    assert_eq!(
        add_stmt.replace("SET n += row.props", ""),
        replace_stmt.replace("SET n = row.props", "")
    );
    // And a per-entity policy reaches only its named entity.
    let mut o_mixed = opts(port, PushScope::All);
    o_mixed.nodes_update = PolicySpec::parse("Movie=replace").unwrap();
    let (fixture_mixed, port_mixed) = spawn_capture_server("200 OK", empty_body());
    o_mixed.credentials.uri = format!("http://127.0.0.1:{port_mixed}");
    push(&o_mixed, &db).unwrap();
    let mixed_write: Json = serde_json::from_str(&fixture_mixed.bodies()[1]).unwrap();
    assert!(mixed_write["statement"]
        .as_str()
        .unwrap()
        .contains("SET n = row.props"));
}

/// (f) A label with no uniqueness constraint is skipped and counted under
/// the default; `--unkeyed-labels create` switches it to CREATE rows.
#[test]
fn unkeyed_labels_are_skipped_and_counted_then_created_on_opt_in() {
    let db = movies_db(&["CREATE (:Untyped {name: 'x'})"]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    let untyped = entity(&report, "Untyped (unkeyed)");
    assert_eq!(untyped.skipped_unkeyed, 1);
    assert_eq!(untyped.total, 1);
    assert_eq!(untyped.statement, None);
    // No request mentions the unkeyed label.
    assert!(!fixture.bodies().iter().any(|b| b.contains("Untyped")));

    // The explicit opt-in: CREATE rows, documented as non-idempotent.
    let mut o = opts(port, PushScope::Labels(vec!["Untyped".into()]));
    o.unkeyed_labels = UnkeyedAction::Create;
    let (_fixture_create, port_create) = spawn_capture_server("200 OK", empty_body());
    o.credentials.uri = format!("http://127.0.0.1:{port_create}");
    let outcome = push(&o, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    let created = entity(&report, "Untyped");
    assert_eq!(created.created, 1);
    assert_eq!(
        created.statement.as_deref(),
        Some("UNWIND $rows AS row CREATE (n:`Untyped`) SET n += row.props")
    );
    // A --merge-key override supplies a key instead.
    let mut o_key = opts(port, PushScope::Labels(vec!["Untyped".into()]));
    o_key.merge_keys = [("Untyped".to_string(), vec!["name".to_string()])].into();
    let (fixture_key, port_key) = spawn_capture_server("200 OK", empty_body());
    o_key.credentials.uri = format!("http://127.0.0.1:{port_key}");
    let outcome = push(&o_key, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    assert_eq!(entity(&report, "Untyped").merge_key, ["name".to_string()]);
    assert!(fixture_key.bodies()[1].contains("MERGE (n:`Untyped` {`name`: row.key.`name`})"));
}

/// (g) Batch slicing follows `--batch-size`: one write request per slice,
/// and the row/byte totals count them.
#[test]
fn batch_slicing_follows_batch_size() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let mut o = opts(port, PushScope::Labels(vec!["Person".into()]));
    o.batch_size = 1;
    let outcome = push(&o, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    // One pre-read, two one-row writes.
    assert_eq!(fixture.bodies().len(), 3);
    assert_eq!(report.statements_sent, 2);
    assert_eq!(report.rows_sent, 2);
    assert_eq!(entity(&report, "Person").batches, 2);
    assert!(report.bytes_sent > 0);
}

/// The `batch_size >= 1` invariant is `push()`'s own, checked before any
/// work (the CLI already rejects `--batch-size 0` at parse): a direct
/// `PushOpts` constructor with 0 gets the typed refusal, no remote
/// connection at all.
#[test]
fn push_rejects_zero_batch_size_before_any_work() {
    let db = movies_db(&[]);
    let mut o = opts(1, PushScope::Labels(vec!["Person".into()]));
    o.batch_size = 0;
    let err = match push(&o, &db) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("push accepted batch_size 0"),
    };
    assert!(err.contains("batch_size must be at least 1"), "{err}");
}

/// A dry run prints the plan without touching the remote: no request at
/// all, per-entity counts, merge keys, statement shapes, batch counts.
#[test]
fn dry_run_never_touches_the_remote() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let mut o = opts(port, PushScope::All);
    o.dry_run = true;
    let outcome = push(&o, &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { .. } => panic!("a dry run cannot fail remotely"),
    };
    assert!(report.dry_run);
    assert_eq!(report.statements_sent, 0);
    assert_eq!(fixture.bodies().len(), 0);
    let movie = entity(&report, "Movie");
    assert_eq!(movie.total, 2);
    assert_eq!(movie.merge_key, ["title".to_string()]);
    assert!(movie
        .statement
        .as_deref()
        .unwrap()
        .contains("MERGE (n:`Movie` {`title`: row.key.`title`})"));
    assert_eq!(movie.batches, 1);
    let rel = entity(&report, "(Person)-[:ACTED_IN]->(Movie)");
    assert!(rel
        .statement
        .as_deref()
        .unwrap()
        .contains("MERGE (a)-[r:`ACTED_IN`]->(b)"));
}

/// A pre-read failure fails closed with the remote's own error (ADR-0401
/// §4): nothing is written.
#[test]
fn pre_read_failure_fails_closed() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_scripted_responses_capturing_requests(
        "500 Internal Server Error",
        vec![r#"{"errors":[{"code":"Neo.DatabaseError","message":"boom"}]}"#],
    );
    let outcome = push(&opts(port, PushScope::All), &db).unwrap();
    match outcome {
        PushOutcome::Completed(report) => panic!("the push should have failed: {report:?}"),
        PushOutcome::Partial { report, error } => {
            assert!(
                error.to_string().contains("pre-read of Movie failed"),
                "{error}"
            );
            assert!(error.to_string().contains("boom"), "{error}");
            assert_eq!(report.statements_sent, 0);
            let requests = fixture.join();
            assert_eq!(requests.len(), 1);
        }
    }
}

/// A failing write batch fails the push with the offending statement's
/// context, and the partial report names exactly which batches landed
/// (ADR-0401 §6).
#[test]
fn failed_batch_reports_what_landed() {
    let db = movies_db(&[]);
    let bodies = vec![
        rows_body(&["k0"], vec![]), // Movie pre-read: nothing
        empty_body(),               // Movie batch 1: lands
        empty_body(),               // Movie batch 2: lands
        rows_body(&["k0"], vec![]), // Person pre-read: nothing
        empty_body(),               // Person batch 1: lands
        r#"{"errors":[{"code":"Neo.ClientError","message":"constraint violated"}]}"#.to_string(),
    ];
    let (fixture, port) = spawn_scripted_responses_capturing_requests("200 OK", bodies);
    let mut o = opts(port, PushScope::All);
    o.batch_size = 1;
    let outcome = push(&o, &db).unwrap();
    match outcome {
        PushOutcome::Completed(report) => panic!("the push should have failed: {report:?}"),
        PushOutcome::Partial { report, error } => {
            // The failing batch names the entity and the row range.
            assert!(
                error.to_string().contains("batch 2 of Person (rows 1..2)"),
                "{error}"
            );
            // The report lists exactly what landed: both Movie batches
            // and Person's first batch.
            assert_eq!(report.statements_sent, 3);
            assert_eq!(report.rows_sent, 3);
            assert_eq!(entity(&report, "Movie").created, 2);
            // Counters describe landed writes (LABS-626): Person's second
            // batch failed, so only its first row counts as created.
            assert_eq!(entity(&report, "Person").created, 1);
            assert_eq!(fixture.join().len(), 6);
        }
    }
}

/// `--labels` and `--types` scopes stay explicit: the induced subgraph
/// only, with out-of-scope endpoints counted dangling.
#[test]
fn labels_scope_pushes_the_induced_subgraph() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::Labels(vec!["Movie".into()])), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    assert_eq!(entity(&report, "Movie").created, 2);
    // The Person-endpoint rels are out of scope: counted in the reserved
    // dangling bucket, not written.
    assert_eq!(entity(&report, "(dangling)").skipped_dangling, 2);
    assert_eq!(fixture.bodies().len(), 2); // one pre-read, one write
}

/// The `--query` scope pushes exactly what the statements return, through
/// the pull's own shape check.
#[test]
fn query_scope_pushes_the_returned_subgraph() {
    let db = movies_db(&[]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let scope = PushScope::Query(
        "MATCH (m:Movie {title: 'Sleepless'}) RETURN m; MATCH (a)-[r:ACTED_IN]->(b) RETURN a, r, b"
            .into(),
    );
    let outcome = push(&opts(port, scope), &db).unwrap();
    let report = match outcome {
        PushOutcome::Completed(report) => report,
        PushOutcome::Partial { error, .. } => panic!("push failed: {error}"),
    };
    // The whole joined subgraph: 1 movie + 1 person + 1 rel, plus the
    // second statement's extra person and movie.
    assert_eq!(entity(&report, "Movie").total, 2);
    assert_eq!(entity(&report, "Person").total, 2);
    assert_eq!(entity(&report, "(Person)-[:ACTED_IN]->(Movie)").total, 2);
    assert_eq!(fixture.bodies().len(), 6);
}

/// Values keep their Cypher type on the way out: temporal, point, and
/// vector properties encode as their ADR-0181 tags (the pull decoder's
/// exact inverse).
#[test]
fn typed_values_push_as_their_type_tags() {
    let db = movies_db(&[
        "MATCH (m:Movie {title: 'Sleepless'}) SET m.watched = date('2023-05-06'), m.loc = point({x: 1.0, y: 2.0})",
    ]);
    let (fixture, port) = spawn_capture_server("200 OK", empty_body());
    let outcome = push(&opts(port, PushScope::Labels(vec!["Movie".into()])), &db).unwrap();
    assert!(matches!(outcome, PushOutcome::Completed(_)));
    let write: Json = serde_json::from_str(&fixture.bodies()[1]).unwrap();
    let rows = write["parameters"]["rows"]["_value"].as_array().unwrap();
    let sleepless = rows
        .iter()
        .find(|row| row["_value"]["key"]["_value"]["title"]["_value"] == "Sleepless")
        .unwrap();
    assert_eq!(
        sleepless["_value"]["props"]["_value"]["watched"]["$type"],
        "Date"
    );
    assert_eq!(
        sleepless["_value"]["props"]["_value"]["watched"]["_value"],
        "2023-05-06"
    );
    assert_eq!(
        sleepless["_value"]["props"]["_value"]["loc"]["$type"],
        "Point"
    );
    assert_eq!(
        sleepless["_value"]["props"]["_value"]["loc"]["_value"],
        "SRID=7203;POINT (1.0 2.0)"
    );
}

/// The movies data model the scenarios use, pinned as the engine's own
/// view: nodes carry labels and property maps, rels their endpoints.
#[test]
fn the_fixture_store_is_the_movies_shape() {
    let db = movies_db(&[]);
    let mut conn = db.connect().unwrap();
    let nodes = conn.query("MATCH (n) RETURN count(n)").unwrap().rows[0][0].clone();
    let rels = conn.query("MATCH ()-[r]->() RETURN count(r)").unwrap().rows[0][0].clone();
    assert_eq!(nodes, Value::Int(4));
    assert_eq!(rels, Value::Int(2));
}

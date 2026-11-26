//! Public-API integration tests for the compat callables (ADR-0699): the
//! foreign procedure names the query front-end re-implements as Cypher
//! bodies — the four that answer neo4j-graphrag's writer
//! (relatedb/relate#862), end to end through the public connection with
//! no preview opt-in (an injected definition is engine-provided, so it
//! never trips the `local-callables` gate — the gated-binary twin in
//! `tests/local_callables.rs` covers the user-`DEFINE` side), plus the
//! precedence contract (a
//! store-registered procedure is never shadowed), the unchanged
//! unknown-name error, and the `SHOW PROCEDURES` rows that
//! self-document the compat policy.

use common::{
    connection, database, err_text, floats, int, list, map, params, row, run_with_params,
    scalar_one, string, strings,
};

use crate::common;

/// The bare void calls, one entity form per row: the dynamic property
/// key stores the embedding as a list property.
#[test]
fn the_vector_property_procedures_store_the_embedding_as_a_list_property() {
    for (setup, call, expected) in [
        (
            "CREATE (n:Chunk)",
            "MATCH (n:Chunk) \
             CALL db.create.setNodeVectorProperty(n, 'embedding', [0.1, 0.2, 0.3]) \
             RETURN n.embedding AS e",
            floats(&[0.1, 0.2, 0.3]),
        ),
        (
            "CREATE (a:P)-[r:LINK]->(b:Q)",
            "MATCH ()-[r:LINK]->() \
             CALL db.create.setRelationshipVectorProperty(r, 'embedding', [0.5, 0.6]) \
             RETURN r.embedding AS e",
            floats(&[0.5, 0.6]),
        ),
    ] {
        let mut c = connection();
        c.query(setup).unwrap();
        let r = c.query(call).unwrap();
        assert_eq!(r.rows, vec![vec![expected]], "{call}");
    }
}

/// `YIELD node` maps: the body's `RETURN nodes AS node` exports under the
/// upstream column name.
#[test]
fn add_labels_yields_the_node_under_the_upstream_column() {
    let mut c = connection();
    c.query("CREATE (n:Base)").unwrap();
    let l = row(
        &mut c,
        "MATCH (n:Base) WITH n, [\"Chunk\", \"__Entity__\"] AS ls \
         CALL apoc.create.addLabels(n, ls) YIELD node \
         RETURN labels(node) AS l",
    );
    assert_eq!(l, vec![strings(&["Base", "Chunk", "__Entity__"])]);
}

/// `YIELD rel` maps, and the MERGE dedup holds: two merges of the same
/// (start, type, end) leave one relationship, with the `ON MATCH` props
/// applied on the second call (upstream `apoc.merge.relationship`
/// semantics — APOC itself generates this exact MERGE).
#[test]
fn merge_relationship_yields_the_rel_and_dedups() {
    let mut c = connection();
    c.query("CREATE (a:P {id: 1}), (b:Q {id: 2})").unwrap();
    let query = "MATCH (a:P {id: 1}), (b:Q {id: 2}) \
                 CALL apoc.merge.relationship(a, 'MENTIONS', {}, {w: 1}, b, {w: 2}) YIELD rel \
                 RETURN rel.w AS w";
    assert_eq!(
        scalar_one(&mut c, query),
        int(1),
        "ON CREATE props apply: {query}"
    );
    assert_eq!(
        scalar_one(&mut c, query),
        int(2),
        "ON MATCH props apply: {query}"
    );
    assert_eq!(
        scalar_one(&mut c, "MATCH ()-[r:MENTIONS]->() RETURN count(r) AS n"),
        int(1)
    );
}

/// The optional `onMatchProps` argument may be omitted (upstream
/// default `{}`), matching the five-argument call shape some clients use.
#[test]
fn merge_relationship_accepts_the_omitted_on_match_props() {
    let mut c = connection();
    c.query("CREATE (a:P {id: 1}), (b:Q {id: 2})").unwrap();
    assert_eq!(
        scalar_one(
            &mut c,
            "MATCH (a:P {id: 1}), (b:Q {id: 2}) \
             CALL apoc.merge.relationship(a, 'REL', {}, {w: 1}, b) YIELD rel \
             RETURN rel.w AS w",
        ),
        int(1)
    );
}

/// neo4j-graphrag's `Neo4jWriter` node upsert, verbatim
/// (`neo4j_graphrag/neo4j_queries.py`: `upsert_node_query` on the
/// dynamic-labels path), over parameters. Nothing in the statement text
/// knows the procedure is re-implemented.
#[test]
fn the_graphrag_node_upsert_query_runs_as_written() {
    let mut c = connection();
    let node_query = "\
        UNWIND $rows AS row \
        CREATE (n:__KGBuilder__ {__tmp_internal_id: row.id}) \
        SET n += row.properties \
        SET n:$(row.labels) \
        WITH n, row \
        CALL (n, row) { \
            WITH n, row WHERE row.embedding_properties IS NOT NULL \
            UNWIND keys(row.embedding_properties) as emb \
            CALL db.create.setNodeVectorProperty(n, emb, row.embedding_properties[emb]) \
            RETURN count(*) as nbEmb \
        } \
        RETURN elementId(n) as element_id";
    let rows = list(vec![map(&[
        ("id", string("n1")),
        ("properties", map(&[("name", string("Neo4j"))])),
        ("labels", strings(&["Chunk", "__Entity__"])),
        (
            "embedding_properties",
            map(&[("embedding", floats(&[0.1, 0.2]))]),
        ),
    ])]);
    let nodes = run_with_params(&mut c, node_query, params(&[("rows", rows)]));
    assert_eq!(nodes.rows.len(), 1);
    let expected = row(
        &mut c,
        "MATCH (n:Chunk) RETURN labels(n) AS l, n.name AS name, n.embedding AS e",
    );
    assert_eq!(
        expected,
        vec![
            strings(&["__KGBuilder__", "Chunk", "__Entity__"]),
            string("Neo4j"),
            floats(&[0.1, 0.2]),
        ]
    );
}

/// The relationship half of the writer, its own scenario
/// (`upsert_relationship_query`): the one-relationship-per-pair merge
/// contract across repeated upserts (the issue's one behavioural gap
/// with a plain CREATE workaround) and the relationship embedding.
#[test]
fn the_graphrag_relationship_upsert_query_runs_as_written() {
    let mut c = connection();
    c.query("CREATE (s:__KGBuilder__ {__tmp_internal_id: 's1'}), (e:__KGBuilder__ {__tmp_internal_id: 'e1'})")
        .unwrap();
    let rel_query = "\
        UNWIND $rows as row \
        MATCH (start:__KGBuilder__ {__tmp_internal_id: row.start_node_id}), \
              (end:__KGBuilder__ {__tmp_internal_id: row.end_node_id}) \
        WITH start, end, row \
        CALL apoc.merge.relationship(start, row.type, {}, row.properties, end, row.properties) YIELD rel \
        WITH rel, row \
        CALL (rel, row) { \
            WITH rel, row WHERE row.embedding_properties IS NOT NULL \
            UNWIND keys(row.embedding_properties) as emb \
            CALL db.create.setRelationshipVectorProperty(rel, emb, row.embedding_properties[emb]) \
        } \
        RETURN elementId(rel)";
    let upsert_row = || {
        list(vec![map(&[
            ("start_node_id", string("s1")),
            ("end_node_id", string("e1")),
            ("type", string("MENTIONS")),
            ("properties", map(&[("weight", int(2))])),
            (
                "embedding_properties",
                map(&[("embedding", floats(&[0.5, 0.6]))]),
            ),
        ])])
    };
    assert_eq!(
        run_with_params(&mut c, rel_query, params(&[("rows", upsert_row())]))
            .rows
            .len(),
        1
    );
    // A repeated extraction upserts the same relationship: the writer's
    // one-relationship-per-pair contract (the issue's one behavioural gap
    // with a plain CREATE workaround) holds.
    assert_eq!(
        run_with_params(&mut c, rel_query, params(&[("rows", upsert_row())]))
            .rows
            .len(),
        1
    );
    let merged = c
        .query("MATCH ()-[r:MENTIONS]->() RETURN count(r) AS n, r.weight AS w, r.embedding AS e")
        .unwrap();
    assert_eq!(merged.rows, vec![vec![int(1), int(2), floats(&[0.5, 0.6])]]);
}

/// A name the table does not carry fails exactly as before: the
/// compat registry answers four names, not a blanket apoc namespace.
#[test]
fn unknown_foreign_procedures_still_fail() {
    let text = err_text(connection().query("CALL apoc.no.such.procedure()"));
    assert!(
        text.contains("ProcedureNotFound"),
        "expected the unknown-procedure error, got {text}"
    );
}

/// Precedence (ADR-0699): a procedure the store registered is never
/// shadowed by the table entry — the call answers the store's rows.
#[test]
fn a_store_registered_procedure_wins_over_the_table_entry() {
    let db = database();
    db.register_procedure(relate::Procedure {
        name: "apoc.create.addLabels".to_string(),
        arg_names: vec!["nodes".to_string(), "labels".to_string()],
        arg_types: vec!["NODE".to_string(), "LIST<STRING>".to_string()],
        yield_names: vec!["node".to_string()],
        rows: vec![params(&[("node", int(42))])],
    })
    .unwrap();
    let mut c = db.connect().unwrap();
    assert_eq!(
        scalar_one(
            &mut c,
            "CREATE (n:Base) WITH n CALL apoc.create.addLabels(n, ['X']) YIELD node RETURN node",
        ),
        int(42)
    );
}

/// The catalog self-documents the policy: the four rows are listed as
/// WRITE procedures.
#[test]
fn show_procedures_lists_the_compat_rows() {
    let r = connection()
        .query("SHOW PROCEDURES YIELD name, mode RETURN name, mode")
        .unwrap();
    for (name, mode) in [
        ("apoc.create.addLabels", "WRITE"),
        ("apoc.merge.relationship", "WRITE"),
        ("db.create.setNodeVectorProperty", "WRITE"),
        ("db.create.setRelationshipVectorProperty", "WRITE"),
    ] {
        let row = r
            .rows
            .iter()
            .find(|row| row[0] == string(name))
            .unwrap_or_else(|| panic!("{name} is not listed"));
        assert_eq!(row[1], string(mode), "{name}");
    }
}

/// Arity comes from the one signature row through the rewrite's authored
/// arity error: the five mandatory arguments of `apoc.merge.relationship`
/// are enforced like any local callable's.
#[test]
fn compat_arity_is_enforced_like_a_builtins() {
    let text = err_text(connection().query(
        "CREATE (a), (b) CALL apoc.merge.relationship(a, 'REL', {}, b) YIELD rel RETURN rel",
    ));
    assert!(
        text.contains("expects at least 5 argument(s), got 4"),
        "expected the rewrite's authored arity error, got {text}"
    );
}

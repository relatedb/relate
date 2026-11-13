//! `db.stats.retrieve('GRAPH COUNTS')`'s `data` payload (ADR-0497 §6): one
//! entry per named constraint and per named index, derived from the same
//! live catalogs `SHOW CONSTRAINTS` and `SHOW INDEXES` render — the corpus's
//! `GraphCounts` scenarios project `data.constraints` and `data.indexes`, and
//! the graph-type-managed constraints come from the same derived records the
//! `SHOW` rows read (ADR-0211).

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_model::{ConstraintType, EnforcedLabelSite, EntityKind};

use crate::overlay::ExecGraph;

use crate::exec::catalog::{constraint_binding, string_list, ConstraintBinding};
use crate::redo::backing_constraint_by_name;
use crate::redo::IndexFacts;

/// The `type` cell of a GRAPH COUNTS constraint entry: Neo4j's space-style
/// kind name, deliberately separate from `SHOW CONSTRAINTS`'s
/// underscore-style `type` column (different surfaces, different vocabulary).
/// The corpus pins `Existence constraint`, `Node Key`, `Node label existence
/// constraint` and `Relationship endpoint label constraint`
/// (`features/correctness/GraphCounts.feature`); the remaining kinds follow
/// the same Neo4j family naming. An unparseable stored tag keeps
/// `SHOW CONSTRAINTS`'s `UNKNOWN` display rather than inventing a kind.
fn graph_counts_constraint_type(kind: Option<ConstraintType>) -> &'static str {
    match kind {
        Some(kind) => crate::exec::catalog::space_constraint_names(kind).graph_counts,
        None => "UNKNOWN",
    }
}

/// One GRAPH COUNTS constraint entry: `label`/`relationshipType` by entity,
/// `properties`, `type`, plus the two keys the graph-type-derived kinds add —
/// `enforcedLabel` and, for the endpoint-label kinds, `endpointType`.
/// `enforcedLabel` reads the derived-constraint record of the same name, the
/// one `SHOW CONSTRAINTS`'s `enforcedLabel` column reads.
fn constraint_entry(def: &relate_catalog::ConstraintDef, binding: &ConstraintBinding<'_>) -> Value {
    let (kind, entity) = (binding.kind, binding.entity);
    let enforced_label = binding
        .detail
        .and_then(|constraint| constraint.enforced_label.as_deref());
    let mut entry = BTreeMap::new();
    let (entity_key, entity_value) = match entity {
        EntityKind::Node => ("label", def.label.clone()),
        EntityKind::Relationship => ("relationshipType", def.label.clone()),
    };
    entry.insert(entity_key.to_string(), Value::String(entity_value.into()));
    entry.insert(
        "properties".to_string(),
        string_list(def.properties.clone()),
    );
    entry.insert(
        "type".to_string(),
        Value::String(graph_counts_constraint_type(kind).into()),
    );
    if let Some(enforced) = enforced_label {
        entry.insert("enforcedLabel".to_string(), Value::String(enforced.into()));
    }
    // CIP-187's endpoint-label kinds carry which end they pin — read from the
    // kind's typed enforced-label site, the same mapping the constraint
    // validator targets, so the entry cannot disagree with the check. The
    // other kinds name no end at all.
    let endpoint = match kind.and_then(|kind| kind.enforced_label_site()) {
        Some(EnforcedLabelSite::Start) => Some("START"),
        Some(EnforcedLabelSite::End) => Some("END"),
        Some(EnforcedLabelSite::Node) | None => None,
    };
    if let Some(endpoint) = endpoint {
        entry.insert("endpointType".to_string(), Value::String(endpoint.into()));
    }
    Value::Map(entry.into())
}

/// One GRAPH COUNTS index entry: the index's `labels` (node) or
/// `relationshipTypes` (relationship) plus its `properties`, resolved through
/// the `SHOW INDEXES` row's own resolution so the two surfaces cannot
/// disagree.
fn index_entry(
    idx: &relate_catalog::IndexDef,
    details: &BTreeMap<String, crate::redo::IndexDetailSpec>,
    constraints: &[relate_catalog::ConstraintDef],
) -> Value {
    let detail = details.get(&idx.name);
    let backing = backing_constraint_by_name(&idx.name, constraints);
    let facts = IndexFacts::resolve(idx, detail, backing);
    let token_key = match EntityKind::from_relationship_flag(facts.relationship) {
        EntityKind::Node => "labels",
        EntityKind::Relationship => "relationshipTypes",
    };
    let mut entry = BTreeMap::new();
    entry.insert(token_key.to_string(), string_list(facts.labels));
    entry.insert("properties".to_string(), string_list(facts.properties));
    Value::Map(entry.into())
}

/// The GRAPH COUNTS row: the requested `section` echoed back and `data`
/// carrying the `constraints` and `indexes` lists. Node/relationship counts,
/// `TOKENS`, `QUERIES` and `META` are not served — no scenario projects them,
/// and an empty list would be a fabricated answer.
pub(crate) fn graph_counts_row(
    graph: &ExecGraph<'_>,
    section: &str,
) -> Result<BTreeMap<String, Value>> {
    let show = graph.show_schema()?;
    let details = &show.data.index_details;
    let derived = &show.data.derived_constraints;
    let constraints = graph.named_constraints();
    let constraint_entries: Vec<Value> = constraints
        .iter()
        .map(|def| constraint_entry(def, &constraint_binding(def, derived)))
        .collect();
    let index_entries: Vec<Value> = graph
        .named_indexes()
        .iter()
        .map(|idx| index_entry(idx, details, &constraints))
        .collect();
    let mut data = BTreeMap::new();
    data.insert(
        "constraints".to_string(),
        Value::List(constraint_entries.into()),
    );
    data.insert("indexes".to_string(), Value::List(index_entries.into()));
    Ok(BTreeMap::from([
        ("section".to_string(), Value::String(section.into())),
        ("data".to_string(), Value::Map(data.into())),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::{
        column, exec_autocommit, exec_cypher, list_cell, string_cell, try_exec_cypher,
    };
    use crate::graph::Graph;

    /// The `data.constraints` list of a `db.stats.retrieve('GRAPH COUNTS')`
    /// call over a graph built by `setup`, plus the `section` the call echoes.
    fn constraints_of(setup: &str) -> (String, Value) {
        let graph = Graph::default();
        exec_autocommit(&graph, setup).expect("the fixture's DDL succeeds");
        let result = exec_cypher(
            &graph,
            "CALL db.stats.retrieve('GRAPH COUNTS') YIELD section, data \
             RETURN section, data.constraints AS constraints",
        );
        let section = column(&result, "section", string_cell).remove(0);
        let constraints = column(&result, "constraints", list_cell).remove(0);
        (section, constraints)
    }

    /// The entries of a `data.constraints`/`data.indexes` payload; a
    /// non-list cell is a fixture failure, never a silent skip.
    fn entries_of(value: &Value) -> &[Value] {
        let Value::List(entries) = value else {
            panic!("payload is not a list: {value:?}");
        };
        entries
    }

    /// Exactly one entry matches every `expected` cell, and that entry's key
    /// set is exactly `expected` — the one row-shape assertion both the
    /// single-entry corpus pins and the graph-type multi-entry test read.
    fn assert_entry(entries: &[Value], expected: &[(&str, Value)]) {
        let matching: Vec<&BTreeMap<String, Value>> = entries
            .iter()
            .map(|value| match value {
                Value::Map(entry) => &**entry,
                _ => panic!("entry is not a map: {value:?}"),
            })
            .filter(|entry| {
                expected
                    .iter()
                    .all(|(key, value)| entry.get(*key) == Some(value))
            })
            .collect();
        assert_eq!(matching.len(), 1, "no unique entry for {expected:?}");
        let entry = matching[0];
        assert_eq!(entry.len(), expected.len(), "{entry:?}");
    }

    /// One-element-list comparison against the corpus row the scenario pins
    /// (`features/correctness/GraphCounts.feature`).
    fn assert_single_entry(payload: &Value, expected: &[(&str, Value)]) {
        let entries = entries_of(payload);
        assert_eq!(entries.len(), 1, "one entry: {entries:?}");
        assert_entry(entries, expected);
    }

    #[test]
    fn node_existence_constraint_is_a_graph_counts_entry() {
        let (section, constraints) =
            constraints_of("CREATE CONSTRAINT FOR (n:User) REQUIRE (n.name) IS NOT NULL");
        assert_eq!(section, "GRAPH COUNTS");
        assert_single_entry(
            &constraints,
            &[
                ("label", Value::String("User".into())),
                ("properties", string_list(["name".to_string()])),
                ("type", Value::String("Existence constraint".into())),
            ],
        );
    }

    #[test]
    fn node_key_constraint_is_a_graph_counts_entry() {
        let (_section, constraints) = constraints_of(
            "CREATE CONSTRAINT FOR (n:User) REQUIRE (n.name, n.surname) IS NODE KEY",
        );
        assert_single_entry(
            &constraints,
            &[
                ("label", Value::String("User".into())),
                (
                    "properties",
                    string_list(["name".to_string(), "surname".to_string()]),
                ),
                ("type", Value::String("Node Key".into())),
            ],
        );
    }

    #[test]
    fn relationship_existence_constraint_is_a_graph_counts_entry() {
        let (_section, constraints) =
            constraints_of("CREATE CONSTRAINT FOR ()-[n:User]-() REQUIRE (n.name) IS NOT NULL");
        assert_single_entry(
            &constraints,
            &[
                ("relationshipType", Value::String("User".into())),
                ("properties", string_list(["name".to_string()])),
                ("type", Value::String("Existence constraint".into())),
            ],
        );
    }

    #[test]
    fn graph_type_constraints_carry_their_enforced_label_and_endpoint() {
        let (_section, constraints) = constraints_of(
            "CYPHER 25 ALTER CURRENT GRAPH TYPE SET {\
               (:Actor => :Person),\
               (:Actor)-[:ACTED_IN =>]->(:Movie)\
             }",
        );
        let entries = entries_of(&constraints);
        // The catalog's entry order is not a pinned surface (the corpus
        // projects the list, not a position), so select by identity.
        let entry = |expected: &[(&str, Value)]| assert_entry(entries, expected);
        assert_eq!(entries.len(), 3, "{entries:?}");
        entry(&[
            ("label", Value::String("Actor".into())),
            ("properties", string_list([])),
            (
                "type",
                Value::String("Node label existence constraint".into()),
            ),
            ("enforcedLabel", Value::String("Person".into())),
        ]);
        entry(&[
            ("relationshipType", Value::String("ACTED_IN".into())),
            ("properties", string_list([])),
            (
                "type",
                Value::String("Relationship endpoint label constraint".into()),
            ),
            ("enforcedLabel", Value::String("Actor".into())),
            ("endpointType", Value::String("START".into())),
        ]);
        entry(&[
            ("relationshipType", Value::String("ACTED_IN".into())),
            ("properties", string_list([])),
            (
                "type",
                Value::String("Relationship endpoint label constraint".into()),
            ),
            ("enforcedLabel", Value::String("Movie".into())),
            ("endpointType", Value::String("END".into())),
        ]);
    }

    /// The `data.indexes` list of a call over a graph built by `setup`.
    fn indexes_of(setup: &str) -> Value {
        let graph = Graph::default();
        exec_autocommit(&graph, setup).expect("the fixture's DDL succeeds");
        let result = exec_cypher(
            &graph,
            "CALL db.stats.retrieve('GRAPH COUNTS') YIELD data RETURN data.indexes AS indexes",
        );
        column(&result, "indexes", list_cell).remove(0)
    }

    #[test]
    fn a_node_range_index_is_a_graph_counts_entry() {
        let indexes = indexes_of("CREATE INDEX FOR (n:User) ON (n.id)");
        assert_single_entry(
            &indexes,
            &[
                ("labels", string_list(["User".to_string()])),
                ("properties", string_list(["id".to_string()])),
            ],
        );
    }

    #[test]
    fn a_relationship_range_index_is_a_graph_counts_entry() {
        let indexes = indexes_of("CREATE INDEX FOR ()-[r:REL]->() ON (r.id)");
        assert_single_entry(
            &indexes,
            &[
                ("relationshipTypes", string_list(["REL".to_string()])),
                ("properties", string_list(["id".to_string()])),
            ],
        );
    }

    #[test]
    fn an_unsupported_section_is_refused_rather_than_answered_empty() {
        let graph = Graph::default();
        let error = crate::exec::test_support::error_of(try_exec_cypher(
            &graph,
            "CALL db.stats.retrieve('QUERIES') YIELD section RETURN section",
        ));
        assert!(
            error
                .contains("InvalidArgumentValue: db.stats.retrieve cannot serve section 'QUERIES'"),
            "{error}"
        );
    }
}

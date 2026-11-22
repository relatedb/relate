use super::*;
use relate_model::CypherValue;

// ---- db.schema.visualization normalization (LABS-257 follow-up) ----

/// A relate string list for the fixtures below.
fn str_list(ss: &[&str]) -> CypherValue {
    CypherValue::List(
        ss.iter()
            .map(|s| CypherValue::Str((*s).into()))
            .collect::<Vec<_>>(),
    )
}

/// A relate schema-entry map for the fixtures below.
fn entry(label: &str, constraints: &[&str], indexes: &[&str]) -> CypherValue {
    CypherValue::Map(
        [
            (":LABELS".into(), str_list(&[label])),
            ("constraints".into(), str_list(constraints)),
            ("indexes".into(), str_list(indexes)),
            ("name".into(), CypherValue::Str(label.into())),
        ]
        .into_iter()
        .collect(),
    )
}

const NEO_CONSTRAINT: &str = "Constraint( id=30, name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}), ownedIndex=31, graphTypeDependence='UNDESIGNATED' )";
const CYL_CONSTRAINT: &str = "Constraint( name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}) )";

/// Neo4j's Constraint repr folds to relate's canonical form (server
/// internals id/ownedIndex/graphTypeDependence dropped), and a label-entry
/// list compares by content regardless of catalog walk order.
#[test]
fn schema_visualization_rows_normalize_to_content() {
    let neo4j = serde_json::json!([[
        {":LABELS": ["Place"], "constraints": [], "indexes": ["location", "id", "name"], "name": "Place"},
        {":LABELS": ["_Bloom_Perspective_"], "constraints": [NEO_CONSTRAINT], "indexes": [], "name": "_Bloom_Perspective_"}
    ]]);
    let cypher = vec![CypherValue::List(vec![CypherValue::Str(
        CYL_CONSTRAINT.into(),
    )])];
    assert_eq!(
        compare_rows_schema_viz(
            serde_json::json!([[NEO_CONSTRAINT]]).as_array().unwrap(),
            &cypher,
        ),
        super::Relation::Exact,
        "constraint reprs fold equal"
    );
    // label-entry list order is normalized away (Place vs Bloom order)
    let cypher_labels = vec![CypherValue::List(
        vec![
            entry(
                "_Bloom_Perspective_",
                &["Constraint( name='constraint_2876b1c0', type='NODE_PROPERTY_UNIQUENESS', schema=(:_Bloom_Perspective_ {id}) )"],
                &[],
            ),
            entry("Place", &[], &["location", "id", "name"]),
        ],
    )];
    assert_eq!(
        compare_rows_schema_viz(neo4j.as_array().unwrap(), &cypher_labels),
        super::Relation::Exact
    );
}

/// A real result list whose elements carry a "name" key but are NOT
/// schema-visualization entries must not be reordered.
#[test]
fn plain_rows_with_name_keys_keep_their_order() {
    let neo4j = serde_json::json!([{"name": "b", "n": 2}, {"name": "a", "n": 1}]);
    let cypher = vec![
        CypherValue::Map(
            [
                ("name".into(), CypherValue::Str("b".into())),
                ("n".into(), CypherValue::Int(2)),
            ]
            .into_iter()
            .collect(),
        ),
        CypherValue::Map(
            [
                ("name".into(), CypherValue::Str("a".into())),
                ("n".into(), CypherValue::Int(1)),
            ]
            .into_iter()
            .collect(),
        ),
    ];
    assert_eq!(
        compare_rows(neo4j.as_array().unwrap(), &cypher, StatementOrder::Ordered),
        super::Relation::Exact,
        "ordered comparison keeps the real row order"
    );
}

/// The `indexes`/`constraints` cell lists inside an entry map are catalog
/// order artifacts; they sort as content (while a real string list in a
/// non-entry map is untouched).
#[test]
fn schema_entry_cells_sort_by_content() {
    let neo4j = serde_json::json!([[{
        ":LABELS": ["Person"], "constraints": [],
        "indexes": ["name", "confirmedtime", "healthstatus", "id"], "name": "Person"
    }]]);
    let cypher = vec![CypherValue::List(vec![entry(
        "Person",
        &[],
        &["confirmedtime", "healthstatus", "id", "name"],
    )])];
    assert_eq!(
        compare_rows_schema_viz(neo4j.as_array().unwrap(), &cypher),
        super::Relation::Exact
    );
}

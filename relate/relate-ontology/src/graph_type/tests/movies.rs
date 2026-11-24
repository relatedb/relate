//! The movies fixture's full-mapping pin (ADR-0667).

use super::*;
use relate_parser::{PredefinedKind, TypeExpr};

/// The movies fixture (ADR-0667): `schema/movies.json`, whose derived
/// constraint count and skip reports this module pins.
const MOVIES_JSON: &str = include_str!("../../../schema/movies.json");

/// The movies fixture's full mapping (ADR-0667): the element types with
/// their declarations and label-resolved endpoints, the six named
/// constraint definitions, the seven skip reports — and the 21
/// constraints the graph type derives, the count `ALTER CURRENT GRAPH
/// TYPE SET` creates from this body: one per declared property, one per
/// `NOT NULL` declaration, one per implied label, one per labelled
/// endpoint and one per named definition.
#[test]
fn the_movies_fixture_maps_fully() {
    let doc = crate::parse_ontology_document(MOVIES_JSON.as_bytes()).expect("movies validates");
    let mapping = to_graph_type_body(&doc).expect("movies converts");

    // Node types, in document order: no aliases, the identifying
    // labels, the implied labels, the declared property names (the
    // resolved names — `email`'s display name is `e-mail`).
    assert_eq!(mapping.body.node_types.len(), 3);
    let actor = &mapping.body.node_types[0];
    assert_eq!(actor.alias, None);
    assert_eq!(actor.identifying_label, "Actor");
    assert!(actor.implied_labels.is_empty());
    assert_eq!(property_names(actor), ["id", "name", "e-mail"]);
    let movie = &mapping.body.node_types[1];
    assert_eq!(movie.alias, None);
    assert_eq!(movie.identifying_label, "Movie");
    assert_eq!(movie.implied_labels, ["Entity"]);
    assert_eq!(property_names(movie), ["id", "genre"]);
    let director = &mapping.body.node_types[2];
    assert_eq!(director.alias, None);
    assert_eq!(director.identifying_label, "Director");
    assert!(director.implied_labels.is_empty());
    assert_eq!(property_names(director), ["id"]);
    // The required declarations are NOT NULL.
    assert_eq!(actor.properties[0].value_type, string_type(false));
    assert_eq!(actor.properties[1].value_type, string_type(true));
    assert_eq!(actor.properties[2].value_type, string_type(false));

    // Edge types: both endpoints as label references.
    assert_eq!(mapping.body.edge_types.len(), 2);
    let acted_in = &mapping.body.edge_types[0];
    assert_eq!(acted_in.identifying_label, "ACTED_IN");
    assert_eq!(acted_in.source, label_ref("Actor", true));
    assert_eq!(acted_in.target, label_ref("Movie", true));
    assert_eq!(acted_in.properties[0].name, "roles");
    assert_eq!(
        acted_in.properties[0].value_type,
        TypeExpr::List {
            element: Box::new(TypeExpr::Predefined {
                kind: PredefinedKind::String,
                not_null: true,
            }),
            not_null: true,
        }
    );
    let directed = &mapping.body.edge_types[1];
    assert_eq!(directed.identifying_label, "DIRECTED");
    assert_eq!(directed.source, label_ref("Director", true));
    assert_eq!(directed.target, label_ref("Movie", true));
    assert_eq!(directed.properties[0].name, "score");
    assert_eq!(
        directed.properties[0].value_type,
        TypeExpr::Predefined {
            kind: PredefinedKind::Float,
            not_null: false,
        }
    );

    // The six named constraint definitions: the relationship's first
    // (relationships are converted before nodes), then each node's
    // flag-derived definitions and constraint objects in order.
    let named: Vec<(Option<String>, bool, Vec<String>, &str)> = mapping
        .body
        .constraints
        .iter()
        .map(|c| {
            (
                c.name.as_ref().map(|name| name.to_string()),
                c.is_edge,
                c.properties
                    .iter()
                    .map(|property| property.to_string())
                    .collect(),
                match c.kind {
                    GraphTypeConstraintVerb::Key => "KEY",
                    GraphTypeConstraintVerb::Unique => "UNIQUE",
                    _ => "other",
                },
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            (
                Some("actedin_roles_key".to_owned()),
                true,
                vec!["roles".to_owned()],
                "KEY"
            ),
            (
                Some("id_Actor_key".to_owned()),
                false,
                vec!["id".to_owned()],
                "KEY"
            ),
            (
                Some("e-mail_Actor_uniq".to_owned()),
                false,
                vec!["e-mail".to_owned()],
                "UNIQUE"
            ),
            (
                Some("id_Movie_key".to_owned()),
                false,
                vec!["id".to_owned()],
                "KEY"
            ),
            (
                Some("unique_movie_genre".to_owned()),
                false,
                vec!["genre".to_owned()],
                "UNIQUE"
            ),
            (
                Some("id_Director_key".to_owned()),
                false,
                vec!["id".to_owned()],
                "KEY"
            ),
        ]
    );

    // The seven skip reports, exactly.
    assert_eq!(
        mapping.skipped,
        [
            "aliases on DIRECTED",
            "cardinality on ACTED_IN.to",
            "description on DIRECTED",
            "description on DIRECTED.score",
            "one_of on Movie.genre",
            "optional labels on Movie",
            "reference on DIRECTED",
        ]
    );

    // The derived constraint count: what the graph-type SET pipeline
    // creates from this body — one constraint per declared property,
    // one more per NOT NULL declaration, one per implied label, one per
    // labelled endpoint, one per named definition (CIP-176/187).
    let not_null = |p: &GraphTypePropertyType| match &p.value_type {
        TypeExpr::Predefined { not_null, .. }
        | TypeExpr::List { not_null, .. }
        | TypeExpr::Vector { not_null, .. }
        | TypeExpr::PropertyValue { not_null } => *not_null,
        // The converter never emits a union; one fail-closed arm if a
        // future form ever reaches the count.
        TypeExpr::Union(_) => unreachable!("the converter emits no union types"),
    };
    let declared: usize = mapping
        .body
        .node_types
        .iter()
        .map(|node| node.properties.len())
        .sum::<usize>()
        + mapping
            .body
            .edge_types
            .iter()
            .map(|edge| edge.properties.len())
            .sum::<usize>();
    let existences: usize = mapping
        .body
        .node_types
        .iter()
        .flat_map(|node| node.properties.iter())
        .chain(
            mapping
                .body
                .edge_types
                .iter()
                .flat_map(|edge| edge.properties.iter()),
        )
        .filter(|property| not_null(property))
        .count();
    let implied: usize = mapping
        .body
        .node_types
        .iter()
        .map(|node| node.implied_labels.len())
        .sum();
    let endpoints: usize = mapping
        .body
        .edge_types
        .iter()
        .map(|edge| {
            [&edge.source, &edge.target]
                .into_iter()
                .filter(|endpoint| !matches!(endpoint, GraphTypeElementRef::Any))
                .count()
        })
        .sum();
    assert_eq!(
        declared + existences + implied + endpoints + mapping.body.constraints.len(),
        21,
        "the movies graph type derives 21 constraints"
    );
}

/// The `STRING [NOT NULL]` declaration shape the movies test expects.
fn string_type(not_null: bool) -> TypeExpr {
    TypeExpr::Predefined {
        kind: PredefinedKind::String,
        not_null,
    }
}

/// One element spec's declared property names, in declaration order.
fn property_names(element: &GraphTypeNodeTypeSpec) -> Vec<String> {
    element
        .properties
        .iter()
        .map(|property| property.name.to_string())
        .collect()
}

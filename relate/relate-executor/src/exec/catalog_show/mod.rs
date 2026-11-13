//! `SHOW ...` catalog command row producers (extracted `execute_catalog` arms).

use relate_core::{Error, Result, Value};
use relate_model::EntityKind;
use relate_parser::RoleModifier;

use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::catalog::{catalog_row, constraint_binding};

pub(crate) mod constraints;
mod databases;
pub(crate) mod indexes;
mod settings;
mod transactions;

pub(crate) use constraints::show_constraints_rows;
pub(crate) use databases::show_databases_rows;
pub(crate) use indexes::{show_indexes_result, show_indexes_rows};
pub(crate) use settings::show_settings_rows;
pub(crate) use transactions::{show_transactions_rows, terminate_transactions_rows};

/// The pattern variable Neo4j CE's rendered statements use — the single
/// place the `n`-for-nodes / `r`-for-relationships decision lives; both
/// the `SHOW INDEXES` and the `SHOW CONSTRAINTS` createStatement
/// renderings read it.
fn entity_variable(entity: EntityKind) -> &'static str {
    match entity {
        EntityKind::Node => "n",
        EntityKind::Relationship => "r",
    }
}

/// The store's whole-catalog DDL statements (ADR-0481 §3): one `CREATE
/// INDEX` / `CREATE CONSTRAINT` statement per named catalog entry, in the
/// order the checkpoint manifest persists the catalog
/// ([`Graph::named_indexes`] then [`Graph::named_constraints`] — the
/// name-keyed catalog's order; the catalog keeps no creation sequence).
/// Every statement is the `SHOW INDEXES` / `SHOW CONSTRAINTS`
/// `createStatement` rendering — the dialect's full form, since a schema
/// mirror must recreate, not hide.
/// An entry whose renderer returns `None` — exactly the rows where the
/// SHOW column is NULL (a legacy detail-less index row, a
/// graph-type-managed dependent constraint, a kind with no standalone
/// CREATE form) — is absent: its schema is already carried by the graph
/// type's canonical text or the manifest, never a fabricated statement.
pub fn catalog_create_statements(graph: &Graph) -> Vec<String> {
    let snapshot = graph.schema_snapshot();
    let data = &snapshot.show.data;
    let current = data.current_graph_type.to_body();
    snapshot
        .indexes
        .iter()
        // A backing index's entry is absent because `index_ddl_statement`
        // suppresses it (the constraint's own statement recreates it).
        .filter_map(|idx| {
            indexes::index_ddl_statement(
                idx,
                &data.index_details,
                &data.index_options,
                &snapshot.constraints,
            )
        })
        .chain(snapshot.constraints.iter().filter_map(|def| {
            // The schema mirror's constraint statement (ADR-0481 §3) is
            // the SHOW row's own binding — a mirror must recreate, not
            // hide: `constraint_statement_for` classifies and renders
            // exactly as the `SHOW CONSTRAINTS` row does, and `None` (a
            // graph-type-managed dependent constraint, a kind with no
            // standalone CREATE form) omits the entry rather than
            // fabricate a statement, its schema already carried by the
            // graph type's canonical text.
            constraints::constraint_statement_for(def, &current, &data.derived_constraints)
        }))
        .collect()
}

pub(crate) fn show_roles_rows(c: &relate_parser::ShowRolesClause) -> Vec<Row> {
    let roles = [
        ("admin", Some("neo4j")),
        ("architect", None),
        ("publisher", None),
        ("editor", None),
        ("reader", None),
        ("PUBLIC", None),
    ];
    roles
        .into_iter()
        .filter(|(_, member)| !matches!(c.modifier, RoleModifier::Populated) || member.is_some())
        .map(|(role, member)| {
            let mut row = catalog_row([("role", Value::String(role.into()))]);
            if c.with_users {
                row.insert(
                    "member".into(),
                    member
                        .map(|m| Value::String(m.into()))
                        .unwrap_or(Value::Null),
                );
            }
            row
        })
        .collect()
}

pub(crate) fn show_graph_type_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowGraphTypeClause,
) -> Result<Vec<Row>> {
    Ok({
        let state = graph.show_schema()?;
        // The stored graph-type table is String-keyed (persisted names);
        // the query's name reads through the text bridge.
        let body = c
            .name
            .with_str(|text| state.data.graph_types.get(text))
            .ok_or_else(|| Error::semantic("GraphTypeNotFound"))?;
        vec![catalog_row([
            ("name", Value::String(c.name.to_string().into())),
            (
                "body",
                Value::String(
                    crate::graph_type::canonicalize_graph_type_body(&body.to_body())
                        .map_err(crate::graph_type::persisted_derive_error)?
                        .into(),
                ),
            ),
        ])]
    })
}

pub(crate) fn show_current_graph_type_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowCurrentGraphTypeClause,
) -> Result<Vec<Row>> {
    let state = graph.show_schema()?;
    let extras = graph
        .named_constraints()
        .into_iter()
        .filter(|constraint| {
            !state
                .data
                .derived_constraints
                .contains_key(constraint.name.as_str())
        })
        .map(|constraint| {
            // The one def→model crossing, through the one binding: a stored
            // kind tag that does not parse is a corrupt catalog row, not a
            // row to drop.
            let binding = constraint_binding(&constraint, &state.data.derived_constraints);
            let kind = binding.kind.ok_or_else(|| {
                Error::internal(format!(
                    "constraint `{}` carries the unrecognized kind tag `{}`",
                    constraint.name, constraint.kind
                ))
            })?;
            Ok(constraint.to_model(kind))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(if let Some(as_graph) = c.as_graph {
        let (nodes, rels) = crate::graph_type_visualize::render_graph_type_as_graph(
            &state.data.current_graph_type.to_body(),
            &extras,
            as_graph,
        )?;
        vec![catalog_row([
            (
                "nodes",
                Value::List(
                    nodes
                        .iter()
                        .map(relate_core::cypher_to_value)
                        .collect::<Vec<_>>()
                        .into(),
                ),
            ),
            (
                "relationships",
                Value::List(
                    rels.iter()
                        .map(relate_core::cypher_to_value)
                        .collect::<Vec<_>>()
                        .into(),
                ),
            ),
        ])]
    } else {
        vec![catalog_row([(
            "specification",
            Value::String(
                crate::graph_type::current_graph_type_specification(
                    &state.data.current_graph_type.to_body(),
                    &extras,
                )
                .map_err(crate::graph_type::persisted_derive_error)?
                .into(),
            ),
        )])]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    /// Runs a SHOW DATABASES statement through the real catalog path
    /// (the shared [`show_catalog_rows`](crate::exec::test_support)
    /// harness) against a theater opted into the `ontology` preview
    /// feature, so it carries all three fixed namespaces.
    fn show_databases(query: &str) -> Vec<Row> {
        // ADR-0593: the pinned three-row shape is the `ontology`-on
        // listing — the fixture's graph opts in exactly the way the
        // show-catalog fixtures do. The flag-off two-row listing (CE's
        // own) is pinned by the test below and the engine's preview-gate
        // suite.
        let mut graph = Graph::default();
        graph.configure_preview(relate_core::PreviewFeatures::parse("ontology").unwrap());
        crate::exec::test_support::show_catalog_rows(&graph, query)
    }

    fn row_named<'a>(rows: &'a [Row], name: &str) -> &'a Row {
        rows.iter()
            .find(|row| row.get("name") == Some(&Value::String(name.into())))
            .unwrap_or_else(|| panic!("no row for {name}"))
    }

    /// Pins the ADR-0142 Browser/Desktop-compat values *and* types
    /// on every row: strings for access/address/role/requestedStatus/
    /// currentStatus/statusMessage, booleans for writer/default/home.
    /// `access` is the one namespace-dependent value (ADR-0116 §4:
    /// `system_ontologies` is read-only).
    fn assert_browser_compat_row(row: &Row, is_default: bool, expected_access: &str) {
        assert!(
            matches!(row.get("access"), Some(Value::String(s)) if &**s == expected_access),
            "access: {:?}",
            row.get("access")
        );
        assert!(
            matches!(row.get("address"), Some(Value::String(s)) if &**s == "localhost:7687"),
            "address: {:?}",
            row.get("address")
        );
        assert!(
            matches!(row.get("role"), Some(Value::String(s)) if &**s == "primary"),
            "role: {:?}",
            row.get("role")
        );
        assert!(
            matches!(row.get("requestedStatus"), Some(Value::String(s)) if &**s == "online"),
            "requestedStatus: {:?}",
            row.get("requestedStatus")
        );
        assert!(
            matches!(row.get("currentStatus"), Some(Value::String(s)) if &**s == "online"),
            "currentStatus: {:?}",
            row.get("currentStatus")
        );
        assert!(
            matches!(row.get("statusMessage"), Some(Value::String(s)) if s.is_empty()),
            "statusMessage: {:?}",
            row.get("statusMessage")
        );
        assert_eq!(row.get("writer"), Some(&Value::Bool(true)));
        assert_eq!(row.get("default"), Some(&Value::Bool(is_default)));
        assert_eq!(row.get("home"), Some(&Value::Bool(is_default)));
    }

    /// The `ontology`-on listing carries exactly the three fixed
    /// namespaces (ADR-0116 §1): `neo4j` (default/home), `system` and
    /// `system_ontologies` (read-only, visible under the opt-in,
    /// ADR-0593); every row must carry the Browser-compat values.
    fn assert_fixed_namespace_rows(rows: &[Row]) {
        assert_eq!(rows.len(), 3);
        assert_browser_compat_row(row_named(rows, "neo4j"), true, "read-write");
        assert_browser_compat_row(row_named(rows, "system"), false, "read-write");
        assert_browser_compat_row(row_named(rows, "system_ontologies"), false, "read-only");
    }

    #[test]
    fn bare_show_databases_rows_carry_browser_compat_values() {
        assert_fixed_namespace_rows(&show_databases("SHOW DATABASES"));
    }

    #[test]
    fn show_databases_yield_exposes_browser_compat_values() {
        // Front validation must still accept a YIELD of these columns
        // (values changed, not the column set).
        relate_front::parse_and_lower(
            "SHOW DATABASES YIELD name, access, address, role, writer, \
             requestedStatus, currentStatus, statusMessage, default, home \
             RETURN name",
        )
        .expect("front validates the YIELD list");
        assert_fixed_namespace_rows(&show_databases(
            "SHOW DATABASES YIELD name, access, address, role, writer, \
             requestedStatus, currentStatus, statusMessage, default, home",
        ));
    }

    #[test]
    fn show_databases_yield_star_exposes_browser_compat_values() {
        assert_fixed_namespace_rows(&show_databases("SHOW DATABASES YIELD *"));
    }

    /// ADR-0593: the flag-off listing is CE's exact two rows — the
    /// `system_ontologies` row returns with the opt-in (the fixture
    /// above).
    #[test]
    fn show_databases_without_the_opt_in_lists_the_two_ce_rows() {
        let graph = Graph::default();
        let rows = crate::exec::test_support::show_catalog_rows(&graph, "SHOW DATABASES");
        assert_eq!(rows.len(), 2);
        assert_browser_compat_row(row_named(&rows, "neo4j"), true, "read-write");
        assert_browser_compat_row(row_named(&rows, "system"), false, "read-write");
    }

    #[test]
    fn show_default_database_row_marks_neo4j_default_and_home() {
        let rows = show_databases("SHOW DEFAULT DATABASE");
        assert_eq!(rows.len(), 1);
        assert_browser_compat_row(row_named(&rows, "neo4j"), true, "read-write");
    }

    /// Pins the load-bearing YIELD-* invariant: `show_databases_rows`
    /// inserts keys in exactly
    /// `relate_front::show_columns::full_database_columns()` order —
    /// the front crate's own composition of its DEFAULT ++ NON_DEFAULT
    /// tables; the shared key-order pin (`assert_row_keys_match`, no
    /// setup needed here) carries the invariant.
    #[test]
    fn show_databases_row_key_order_matches_front_column_tables() {
        crate::exec::test_support::assert_row_keys_match(
            None,
            "SHOW DATABASES YIELD *",
            &relate_front::show_columns::full_database_columns(),
        );
    }

    /// ADR-0481 §3: the whole-catalog DDL statements are exactly the SHOW
    /// `createStatement` column's renderings of the same committed state —
    /// one renderer, two callers, byte-identical output — and the pinned
    /// renderings are the exact text the `schema.cypher` mirror carries.
    #[test]
    fn catalog_create_statements_are_the_show_create_statement_renders() {
        use crate::exec::test_support::{column, exec_autocommit, exec_cypher, string_cell};

        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX person_name FOR (n:Person) ON (n.name)").unwrap();
        exec_autocommit(&g, "CREATE TEXT INDEX book_title FOR (n:Book) ON (n.title)").unwrap();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT person_email FOR (n:Person) REQUIRE n.email IS NOT NULL",
        )
        .unwrap();

        let ddl = super::catalog_create_statements(&g);
        assert_eq!(ddl.len(), 3, "{ddl:?}");

        // The SHOW columns, read through the SHOW row producers. Catalog
        // order is the name-keyed maps' order and the SHOW rows come from
        // the same maps, so the two orders agree without sorting. The two
        // default lookup rows are synthesized, not catalog entries, so the
        // mirror omits them and so does this comparison (every catalog row
        // is non-LOOKUP).
        let show_indexes = exec_cypher(
            &g,
            "SHOW INDEXES YIELD name, type, createStatement \
             WHERE type <> 'LOOKUP' RETURN createStatement",
        );
        let index_statements = column(&show_indexes, "createStatement", string_cell);
        let show_constraints = exec_cypher(&g, "SHOW CONSTRAINTS YIELD createStatement");
        let constraint_statements = column(&show_constraints, "createStatement", string_cell);
        assert_eq!(&ddl[..index_statements.len()], &index_statements[..]);
        assert_eq!(&ddl[index_statements.len()..], &constraint_statements[..]);

        // The pinned renderings, in the catalog's name order.
        assert_eq!(
            ddl,
            vec![
                "CREATE TEXT INDEX `book_title` FOR (n:`Book`) ON (n.`title`)",
                "CREATE RANGE INDEX `person_name` FOR (n:`Person`) ON (n.`name`)",
                "CREATE CONSTRAINT `person_email` FOR (n:`Person`) REQUIRE (n.`email`) IS NOT NULL",
            ]
        );
    }
}

//! `SHOW ...` catalog command row producers (extracted `execute_catalog` arms).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use relate_core::{Error, Result, Value};
use relate_model::EntityKind;
use relate_parser::RoleModifier;

use crate::ctx::QueryCtx;
use crate::eval::eval;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::catalog::{catalog_row, project_catalog};

pub(crate) mod constraints;
mod databases;
pub(crate) mod indexes;

pub(crate) use constraints::show_constraints_rows;
pub(crate) use databases::{
    show_databases_rows, show_transactions_rows, terminate_transactions_rows,
};
pub(crate) use indexes::{show_indexes_result, show_indexes_rows};

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
/// `createStatement` rendering with no `CYPHER 5` preamble — the
/// dialect's full form, since a schema mirror must recreate, not hide.
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
            constraints::constraint_ddl_statement(def, &current, &data.derived_constraints)
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

/// One row of the [`SETTINGS`] table. The catalog is a fixed snapshot of
/// the settings Neo4j 2026.09 declares (name, configured/default/startup
/// value, the two flags, the valid-value text and the description), sorted
/// by name -- `SHOW SETTINGS` reports the catalog in that order, and
/// `ORDER BY name LIMIT 1` therefore answers the alphabetically first
/// declared setting.
struct Setting {
    name: &'static str,
    value: &'static str,
    is_dynamic: bool,
    default_value: &'static str,
    startup_value: &'static str,
    valid_values: &'static str,
    is_explicitly_set: bool,
    is_deprecated: bool,
    description: &'static str,
}

/// The SHOW SETTINGS catalog, live-verified against
/// `neo4j:2026.09-enterprise`. Descriptions are Neo4j's; the multi-paragraph
/// ones are abbreviated to their first sentence (no scenario asserts them).
///
/// `db.query.default_language` is the server-wide default for newly created
/// databases, and it is unset here, so the row reports the setting's own
/// default (`CYPHER_5`) — the value Neo4j's acceptance fixture pins. This is
/// a different fact from a database's configured language: every fixed
/// namespace carries its own `default_language` (seeded to `25`, ADR-0116
/// §8), which `SHOW DATABASES` reports as `CYPHER 25` and
/// `ALTER DATABASE … SET DEFAULT LANGUAGE` rewrites. The two statements
/// therefore agree rather than contradict: the setting governs new
/// databases, the catalog column reports the existing one.
const SETTINGS: &[Setting] = &[
    Setting {
        name: "browser.allow_outgoing_connections",
        value: "true",
        is_dynamic: false,
        default_value: "true",
        startup_value: "true",
        valid_values: "A boolean.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Configure the policy for outgoing Neo4j Browser connections.",
    },
    Setting {
        name: "db.checkpoint",
        value: "PERIODIC",
        is_dynamic: false,
        default_value: "PERIODIC",
        startup_value: "PERIODIC",
        valid_values: "One of [PERIODIC, CONTINUOUS, VOLUME, VOLUMETRIC].",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Configures the general policy for when checkpoints should occur.",
    },
    Setting {
        name: "db.checkpoint.interval.time",
        value: "15m",
        is_dynamic: false,
        default_value: "15m",
        startup_value: "15m",
        valid_values: "A duration (Valid units are: `ns`, `μs`, `ms`, `s`, `m`, `h` and `d`; default unit is `s`).",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Configures the time interval between checkpoints.",
    },
    Setting {
        name: "db.checkpoint.interval.tx",
        value: "100000",
        is_dynamic: false,
        default_value: "100000",
        startup_value: "100000",
        valid_values: "An integer that is minimum `1`.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Configures the transaction interval between checkpoints.",
    },
    Setting {
        name: "db.checkpoint.interval.volume",
        value: "250.00MiB",
        is_dynamic: false,
        default_value: "250.00MiB",
        startup_value: "250.00MiB",
        valid_values: "A byte size that is minimum `1.00KiB`.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Configures the volume of transaction logs between checkpoints.",
    },
    Setting {
        name: "db.format",
        value: "block",
        is_dynamic: true,
        default_value: "block",
        startup_value: "block",
        valid_values: "A string.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Database format. This is the format that will be used for new databases.",
    },
    Setting {
        name: "db.query.default_language",
        value: "CYPHER_5",
        is_dynamic: false,
        default_value: "CYPHER_5",
        startup_value: "CYPHER_5",
        valid_values: "One of [CYPHER_5, CYPHER_25].",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "The default language of a database determines which language is used to evaluate queries that do not explicitly select a language. This setting determines the default language used for new (and initial) databases where not specified as part of CREATE or ALTER database.",
    },
    Setting {
        name: "dbms.cluster.network.supported_compression_algos",
        value: "",
        is_dynamic: false,
        default_value: "",
        startup_value: "",
        valid_values: "A comma-separated list where each element is a string.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Network compression algorithms that this instance allows in negotiation as a comma-separated list.",
    },
    Setting {
        name: "dbms.routing.load_balancing.plugin",
        value: "server_policies",
        is_dynamic: false,
        default_value: "server_policies",
        startup_value: "server_policies",
        valid_values: "A string.",
        is_explicitly_set: false,
        is_deprecated: true,
        description: "The load balancing plugin to use.",
    },
    Setting {
        name: "dbms.routing_ttl",
        value: "5m",
        is_dynamic: false,
        default_value: "5m",
        startup_value: "5m",
        valid_values: "A duration that is minimum `1s`.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "How long callers should cache the response of the routing procedure `dbms.routing.getRoutingTable()`.",
    },
    Setting {
        name: "initial.server.mode_constraint",
        value: "NONE",
        is_dynamic: false,
        default_value: "NONE",
        startup_value: "NONE",
        valid_values: "One of [PRIMARY, SECONDARY, NONE].",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Determines whether the server is configured to host primary databases only, secondary databases only, or both.",
    },
    Setting {
        name: "server.bolt.advertised_address",
        value: "localhost:7687",
        is_dynamic: false,
        default_value: ":7687",
        startup_value: ":7687",
        valid_values: "A socket address in the format of `hostname:port`, `hostname`, or `:port`.",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "Advertised address for this connector.",
    },
    Setting {
        name: "server.bolt.connection_keep_alive_for_requests",
        value: "ALL",
        is_dynamic: false,
        default_value: "ALL",
        startup_value: "ALL",
        valid_values: "One of [ALL, STREAMING, OFF].",
        is_explicitly_set: false,
        is_deprecated: false,
        description: "The type of messages to enable keep-alive messages for `ALL`, `STREAMING`, or `OFF`.",
    },
    Setting {
        name: "server.bolt.enabled",
        value: "true",
        is_dynamic: false,
        default_value: "true",
        startup_value: "true",
        valid_values: "A boolean.",
        is_explicitly_set: true,
        is_deprecated: false,
        description: "Enable the Bolt connector.",
    },
    Setting {
        name: "server.db.query_cache_size",
        value: "1000",
        is_dynamic: false,
        default_value: "1000",
        startup_value: "1000",
        valid_values: "An integer that is minimum `0`.",
        is_explicitly_set: false,
        is_deprecated: true,
        description: "The number of cached queries per database.",
    },
];

/// One catalog setting rendered as its SHOW row, in the `YIELD *` column
/// order. The one table both the bare form and the names-filtered form
/// select from.
fn setting_row(setting: &Setting) -> Row {
    catalog_row([
        ("name", Value::String(setting.name.into())),
        ("value", Value::String(setting.value.into())),
        ("isDynamic", Value::Bool(setting.is_dynamic)),
        ("defaultValue", Value::String(setting.default_value.into())),
        ("description", Value::String(setting.description.into())),
        ("startupValue", Value::String(setting.startup_value.into())),
        ("isExplicitlySet", Value::Bool(setting.is_explicitly_set)),
        ("validValues", Value::String(setting.valid_values.into())),
        ("isDeprecated", Value::Bool(setting.is_deprecated)),
    ])
}

/// The setting-name selection a SHOW SETTINGS argument denotes: every
/// setting (an empty list — no filter) or exactly the named ones (an empty
/// `Names` — a null expression, or a list of only nulls — selects nothing).
enum SettingFilter {
    All,
    Names(Vec<Arc<str>>),
}

impl SettingFilter {
    /// Whether the setting named `name` belongs to the selection. The
    /// selection reads the typed catalog's own name, never a `Value` it
    /// would have to re-match.
    fn admits(&self, name: &str) -> bool {
        match self {
            SettingFilter::All => true,
            SettingFilter::Names(names) => names.iter().any(|wanted| &**wanted == name),
        }
    }
}

/// The value→selection normalization of a SHOW SETTINGS name expression: a
/// STRING or a LIST<STRING>, with a null selecting nothing (CIP-253), a null
/// list entry treated as a non-existing setting, and an empty list meaning
/// "no filter". Every other shape is the data-exception type failure Neo4j
/// raises (`TypeError`), never a silently empty selection.
fn setting_filter(value: Value, cypher5: bool) -> Result<SettingFilter> {
    if value.is_null() && cypher5 {
        return Err(Error::invalid_argument_type(
            "setting name must be STRING or LIST<STRING>",
        ));
    }
    match value {
        Value::Null => Ok(SettingFilter::Names(Vec::new())),
        Value::String(name) => Ok(SettingFilter::Names(vec![name])),
        Value::List(values) if values.is_empty() => Ok(SettingFilter::All),
        Value::List(values) => {
            let mut names = Vec::with_capacity(values.len());
            for value in values.iter() {
                match value {
                    Value::String(name) => names.push(name.clone()),
                    Value::Null => {}
                    _ => return Err(Error::type_error("Expected a string")),
                }
            }
            Ok(SettingFilter::Names(names))
        }
        _ => Err(Error::type_error("Expected a string or a list of strings")),
    }
}

pub(crate) fn show_settings_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowSettingsClause,
    rows: Vec<Row>,
    clause: &relate_parser::Clause,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    // The catalog is built once per statement; a composed SHOW SETTINGS may
    // see many input rows beneath it.
    let catalog: Vec<(&'static str, Row)> = SETTINGS
        .iter()
        .map(|setting| (setting.name, setting_row(setting)))
        .collect();
    let Some(names_expr) = &c.names else {
        return project_catalog(
            graph,
            rows,
            catalog.into_iter().map(|(_, row)| row).collect(),
            clause,
            params,
            qctx.cypher_version,
        );
    };
    let lowered = relate_front::lower_expr(names_expr)?;
    // Each input row selects its own names; the compositions concatenate and
    // the clause's paging/WHERE run once over that flattened cross join — a
    // per-row `LIMIT` would repeat for every input row.
    super::catalog::project_catalog_by_row(
        graph,
        rows,
        clause,
        params,
        qctx.cypher_version,
        |row| {
            let value = eval(graph, row, &lowered, params, qctx.cypher_version)?;
            let filter = setting_filter(value, qctx.is_cypher5())?;
            Ok(catalog
                .iter()
                .filter(|(name, _)| filter.admits(name))
                .map(|(_, row)| row.clone())
                .collect())
        },
    )
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
    let derived_names: HashSet<_> = state.data.derived_constraints.keys().cloned().collect();
    let extras = graph
        .named_constraints()
        .into_iter()
        .filter(|constraint| !derived_names.contains(&constraint.name))
        .filter_map(|constraint| {
            use std::str::FromStr;
            Some(relate_model::Constraint {
                name: constraint.name,
                label: constraint.label,
                properties: constraint.properties,
                constraint_type: relate_model::ConstraintType::from_str(&constraint.kind).ok()?,
                property_type: constraint.property_type,
                entity_type: EntityKind::from_relationship_flag(constraint.relationship)
                    .display_name()
                    .into(),
                enforced_label: None,
            })
        })
        .collect::<Vec<_>>();
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
    /// harness) against the default theater, which carries exactly the
    /// `neo4j` and `system` namespaces.
    fn show_databases(query: &str) -> Vec<Row> {
        let graph = Graph::default();
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

    /// The default theater carries exactly the three fixed namespaces
    /// (ADR-0116 §1): `neo4j` (default/home), `system` and
    /// `system_ontologies` (read-only); every row must carry the
    /// Browser-compat values.
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

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::graph::Graph;

    fn show(query: &str) -> Vec<Row> {
        crate::exec::test_support::show_catalog_rows(&Graph::default(), query)
    }

    /// The catalog is Neo4j 2026.09's own snapshot: the alphabetically first
    /// declared setting is `browser.allow_outgoing_connections` (the harness
    /// pins `ORDER BY name LIMIT 1`), the metadata columns carry real values,
    /// and the deprecated entries keep their flags.
    #[test]
    fn settings_catalog_carries_neo4j_metadata() {
        let rows = show("SHOW SETTINGS YIELD name, value, isDynamic, defaultValue, startupValue, description ORDER BY name LIMIT 1");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("name"),
            Some(&Value::String("browser.allow_outgoing_connections".into()))
        );
        assert_eq!(rows[0].get("value"), Some(&Value::String("true".into())));
        assert_eq!(rows[0].get("isDynamic"), Some(&Value::Bool(false)));
        assert_eq!(
            rows[0].get("defaultValue"),
            Some(&Value::String("true".into()))
        );
        assert_eq!(
            rows[0].get("startupValue"),
            Some(&Value::String("true".into()))
        );

        let dynamic = show("SHOW SETTINGS YIELD name, isDynamic WHERE isDynamic RETURN name");
        assert_eq!(
            dynamic
                .iter()
                .map(|row| row.get("name").cloned())
                .collect::<Vec<_>>(),
            vec![Some(Value::String("db.format".into()))]
        );

        let deprecated =
            show("SHOW SETTINGS YIELD name, isDeprecated WHERE name = 'server.db.query_cache_size' RETURN isDeprecated");
        assert_eq!(deprecated[0].get("isDeprecated"), Some(&Value::Bool(true)));
    }

    /// A name expression that is neither a STRING nor a LIST<STRING> is the
    /// data-exception type failure, never a silently empty selection; a null
    /// (or all-null list) selects nothing.
    #[test]
    fn setting_name_expressions_reject_wrong_types() {
        let graph = Graph::default();
        for query in ["SHOW SETTINGS 123", "SHOW SETTINGS ['db.format', 123]"] {
            let err = crate::exec::test_support::error_of(
                crate::exec::test_support::try_exec_cypher(&graph, query),
            );
            assert!(err.contains("type error"), "{query}: {err}");
        }
        assert!(crate::exec::test_support::show_catalog_rows(
            &graph,
            "SHOW SETTINGS [null] YIELD name"
        )
        .is_empty());
    }

    /// Pins the `YIELD *` column set and order: `setting_row` inserts keys in
    /// exactly `full_setting_columns()` order, so the producer's hand-spelled
    /// pair list can never drift from the front table it renders.
    #[test]
    fn show_settings_row_key_order_matches_front_column_table() {
        crate::exec::test_support::assert_row_keys_match(
            None,
            "SHOW SETTINGS YIELD *",
            &relate_front::show_columns::full_setting_columns(),
        );
    }

    /// The name expression selects per input row, but the clause's paging
    /// applies once to the flattened cross join: the two input rows select
    /// two names then one, and `ORDER BY name LIMIT 1` must return the
    /// alphabetically first of all three — a per-input-row `LIMIT` would
    /// return two rows (`db.format` from the first row, the browser setting
    /// from the second).
    #[test]
    fn composed_show_settings_pages_the_flattened_result_once() {
        use crate::exec::test_support::{column, exec_cypher, string_cell};

        let graph = Graph::default();
        let result = exec_cypher(
            &graph,
            "UNWIND [['db.format', 'dbms.connector.bolt.listen_address'], \
             ['browser.allow_outgoing_connections']] AS names \
             SHOW SETTINGS names YIELD name ORDER BY name LIMIT 1 RETURN name",
        );
        assert_eq!(
            column(&result, "name", string_cell),
            vec!["browser.allow_outgoing_connections".to_string()]
        );
    }
}

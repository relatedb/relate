//! `SHOW SETTINGS` row producer: the fixed Neo4j settings catalog, its row
//! rendering, and the name-filter normalization.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Error, Result, Value};

use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use crate::exec::catalog::{catalog_row, project_catalog};

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
fn setting_filter(value: Value) -> Result<SettingFilter> {
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
        );
    };
    let lowered = relate_front::lower_expr(names_expr)?;
    // Each input row selects its own names; the compositions concatenate and
    // the clause's paging/WHERE run once over that flattened cross join — a
    // per-row `LIMIT` would repeat for every input row.
    crate::exec::catalog::project_catalog_by_row(graph, rows, clause, params, |row| {
        let value = eval(graph, row, &lowered, params)?;
        let filter = setting_filter(value)?;
        Ok(catalog
            .iter()
            .filter(|(name, _)| filter.admits(name))
            .map(|(_, row)| row.clone())
            .collect())
    })
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

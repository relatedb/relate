//! `SHOW DATABASES` row producer.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_parser::{DatabaseFilter, SchemaName};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::catalog::catalog_row;

/// Which fixed namespaces `SHOW DATABASES` may emit: every one, exactly
/// one, or none (a null name parameter, or a name no fixed namespace
/// carries — `Namespace::parse` already matches case-insensitively).
#[derive(Clone, Copy)]
enum NamespaceFilter {
    All,
    Only(relate_core::Namespace),
    NoMatch,
}

impl NamespaceFilter {
    /// The filter for a database name: `Only` when it names a fixed
    /// namespace, `NoMatch` otherwise.
    fn named(name: &str) -> Self {
        match relate_core::Namespace::parse(name) {
            Some(ns) => NamespaceFilter::Only(ns),
            None => NamespaceFilter::NoMatch,
        }
    }

    /// Whether a fixed namespace passes the filter.
    fn admits(self, ns: relate_core::Namespace) -> bool {
        match self {
            NamespaceFilter::All => true,
            NamespaceFilter::Only(wanted) => wanted == ns,
            NamespaceFilter::NoMatch => false,
        }
    }
}

pub(crate) fn show_databases_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowDatabasesClause,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    Ok({
        let state = graph.show_schema()?;
        // The filter resolves to the typed model at the boundary; an absent
        // parameter fails with `ParameterMissing` (ADR-0308).
        let wanted = match &c.filter {
            DatabaseFilter::All => NamespaceFilter::All,
            DatabaseFilter::Default | DatabaseFilter::Home => {
                NamespaceFilter::Only(relate_core::Namespace::Neo4j)
            }
            DatabaseFilter::Named(SchemaName::Literal(n)) => NamespaceFilter::named(n),
            DatabaseFilter::Named(SchemaName::Parameter(p)) => match params.get(p) {
                None => return Err(Error::parameters_missing([p.as_str()])),
                Some(Value::String(name)) => NamespaceFilter::named(name),
                Some(Value::Null) => NamespaceFilter::NoMatch,
                _ => return Err(Error::invalid_argument_type("database name must be STRING")),
            },
        };
        // ADR-0116 §1/§6: the fixed namespaces, an unknown name returning
        // no rows; the stored `defaultLanguage` comes from the store-level
        // databases catalog (ADR-0116 §8/§10; `CYPHER 25` default).
        // ADR-0593: a namespace invisible under the store's preview set
        // (today `system_ontologies` behind `ontology`) is omitted — the
        // flag-off default listing is CE's exact two rows — and the named
        // filter finds nothing, exactly as for an unknown name. The
        // durable catalog entry is untouched: the opt-in is per-open, so
        // the row returns when the feature is enabled.
        relate_core::Namespace::ALL
            .iter()
            .filter(|ns| wanted.admits(**ns))
            .filter(|ns| ns.visible_under(graph.preview()))
            .map(|ns| {
                let name = ns.as_str();
                // `DatabasesCatalog::default`/`sanitize` guarantee an
                // entry for every fixed namespace, so indexing cannot miss.
                let language = state.databases[ns].default_language;
                // ADR-0142: the topology/status columns carry fixed
                // Browser-compatible constants, not live status — Relate
                // is a single-node embedded engine, so every namespace is
                // always online/primary. `default`/`home` are true only for
                // `neo4j` (ADR-0116). Column order matches relate-front's
                // DEFAULT_DATABASE_COLUMNS so `YIELD *` exposes the Neo4j
                // ordering. ADR-0142: `address` is the constant
                // `localhost:7687` — Neo4j Desktop builds the instance id
                // `name@address` and rejects an empty address; the value is
                // a display placeholder correct for the default Bolt bind.
                let is_neo4j = matches!(ns, relate_core::Namespace::Neo4j);
                catalog_row([
                    ("name", Value::String(name.into())),
                    (
                        "type",
                        Value::String(
                            if matches!(ns, relate_core::Namespace::System) {
                                "system"
                            } else {
                                "standard"
                            }
                            .into(),
                        ),
                    ),
                    ("aliases", Value::List(Vec::new().into())),
                    (
                        "access",
                        Value::String(
                            if ns.is_read_only() {
                                "read-only"
                            } else {
                                "read-write"
                            }
                            .into(),
                        ),
                    ),
                    ("address", Value::String("localhost:7687".into())),
                    ("role", Value::String("primary".into())),
                    ("writer", Value::Bool(true)),
                    ("requestedStatus", Value::String("online".into())),
                    ("currentStatus", Value::String("online".into())),
                    ("statusMessage", Value::String("".into())),
                    ("default", Value::Bool(is_neo4j)),
                    ("home", Value::Bool(is_neo4j)),
                    ("constituents", Value::List(Vec::new().into())),
                    (
                        "creationTime",
                        Value::Temporal(relate_core::Temporal::DateTime {
                            utc: chrono::Utc::now().naive_utc() - chrono::Duration::hours(1),
                            offset: 0,
                            zone: None,
                        }),
                    ),
                    (
                        "lastStartTime",
                        Value::Temporal(relate_core::Temporal::DateTime {
                            utc: chrono::Utc::now().naive_utc() - chrono::Duration::minutes(30),
                            offset: 0,
                            zone: None,
                        }),
                    ),
                    ("lastStopTime", Value::Null),
                    ("store", Value::String("relate-native-1.0".into())),
                    (
                        "defaultLanguage",
                        Value::String(format!("CYPHER {language}").into()),
                    ),
                ])
            })
            .collect()
    })
}

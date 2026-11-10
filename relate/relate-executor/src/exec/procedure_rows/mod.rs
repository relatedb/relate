//! Built-in procedure row producers: the graph-dependent rows behind
//! `builtin_procedure_rows` (`dbms.components`, `dbms.clientConfig`,
//! `db.schema.*`, …). Argument names/types and yield columns come from the
//! single canonical signature registry (`crate::procedure_registry`); only
//! the rows are produced here. The generic `CALL` machinery and the
//! fulltext/vector query executors live in `exec::procedures`.

use std::collections::{BTreeMap, BTreeSet};

use relate_core::{Error, Result, Value};

use crate::graph::Procedure;
use crate::overlay::ExecGraph;
use crate::procedure_registry::ProcSignature;

mod graph_counts;
mod schema;
mod stats;
#[cfg(test)]
mod test_helpers;

use schema::{schema_node_type_rows, schema_rel_type_rows, schema_visualization_rows};

/// Relate engine version, single-sourced from the workspace version
/// (`version.workspace = true`), reported by `dbms.components` row 3
/// (`Relate`) (ADR-0142).
const RELATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version reported by the `dbms.components` row 1 (`Neo4j Kernel`)
/// (ADR-0152): a deliberately-bumped **policy constant**, not derived from
/// the workspace version. It tracks the Neo4j calver month the Browser
/// compat surface was verified against and carries the `-relate`
/// product suffix (Aura's `5.27-aura` convention) so it stays
/// distinguishable from any real Neo4j release. Bump it deliberately, with
/// a re-check of the browser.neo4j.io bundle gates — the sdk minimum-version
/// gate (`>= 4.4.0`, coerced loose semver) reads only this row's
/// `versions[0]` and throws on anything lower, blocking the Database
/// information sidebar (LABS-150). Must always be non-empty.
const NEO4J_KERNEL_VERSION: &str = "2026.10-relate";

/// The executable signature of a built-in name, `None` when the registry does
/// not list it as callable (a catalog-only `SHOW PROCEDURES` row). `CALL`
/// resolves a name to it once and reads it for the call's metadata and its
/// void-terminal fact, so no other path re-spells "executable".
pub(crate) fn executable_signature(name: &str) -> Option<&'static ProcSignature> {
    crate::procedure_registry::builtin_procedure_signature(name)
        .filter(|signature| signature.executable)
}

/// The metadata a signature declares — argument names/types and YIELD columns
/// from the canonical registry — as the row-less `Procedure` a `CALL` binds
/// its arguments against; the rows are [`builtin_procedure_rows`]'s business.
pub(crate) fn procedure_metadata(signature: &ProcSignature) -> Procedure {
    Procedure {
        name: signature.name.into(),
        arg_names: signature
            .args
            .iter()
            .map(|arg| arg.name.to_string())
            .collect(),
        arg_types: signature
            .args
            .iter()
            .map(|arg| arg.type_str.to_string())
            .collect(),
        yield_names: signature
            .returns
            .iter()
            .map(|ret| ret.name.to_string())
            .collect(),
        rows: Vec::new(),
    }
}

/// The rows of a built-in procedure under one `CALL`'s bound arguments.
/// `signature` is the canonical registry entry the call resolved its name
/// to: argument names/types, yield columns and the void fact all come from
/// it — only the rows are produced here, and they are a function of `graph`
/// and, where the procedure's contract says so, of `args`
/// (`db.schema.visualization` with a null `options` map selects nothing;
/// `db.stats.retrieve` serves the section it is asked for).
/// `principal` is the connection's authenticated principal (ADR-0273): only
/// `dbms.showCurrentUser` reads it; `None` (a surface with no notion of
/// identity, or `--auth none`) yields the documented placeholder.
/// The row producer's own contract errors (a `db.stats.retrieve` section the
/// engine cannot serve) come back as the `Err` arm.
pub(crate) fn builtin_procedure_rows(
    graph: &ExecGraph<'_>,
    signature: &'static ProcSignature,
    principal: Option<&str>,
    args: &BTreeMap<String, Value>,
) -> Result<Vec<BTreeMap<String, Value>>> {
    // A void procedure — the signature declares no return columns
    // (`tx.setMetaData`, `db.createLabel`, `db.awaitIndexes`) — is a unit
    // operation with no rows.
    if signature.is_void() {
        return Ok(Vec::new());
    }
    // The registry row owns which producer answers it, so a row-declaring
    // signature without one is a table error rather than a silent zero-row
    // answer, and the producer table cannot drift into a second name-keyed
    // list.
    let producer = signature.rows.ok_or_else(|| {
        Error::internal(format!(
            "builtin procedure '{}' declares return columns but has no row producer",
            signature.name
        ))
    })?;
    producer.produce(graph, principal, args)
}

impl crate::procedure_registry::RowProducer {
    /// Produce this row's rows under one `CALL`'s bound arguments.
    fn produce(
        self,
        graph: &ExecGraph<'_>,
        principal: Option<&str>,
        args: &BTreeMap<String, Value>,
    ) -> Result<Vec<BTreeMap<String, Value>>> {
        match self {
            Self::TxGetMetaData => Ok(tx_get_metadata_rows()),
            Self::DbmsComponents => Ok(dbms_components_rows()),
            Self::DbPing => Ok(ping_rows()),
            Self::DbLabels => Ok(label_rows(graph)),
            Self::DbRelationshipTypes => Ok(relationship_type_rows(graph)),
            Self::DbPropertyKeys => Ok(property_key_rows(graph)),
            Self::DbInfo => Ok(db_info_rows(graph)),
            Self::DbmsInfo => Ok(dbms_info_rows()),
            Self::DbmsShowCurrentUser => Ok(dbms_show_current_user_rows(principal)),
            Self::DbmsConfig => Ok(dbms_config_rows()),
            Self::DbmsLicenseAgreementDetails => Ok(dbms_license_agreement_rows()),
            Self::DbStatsRetrieve => stats::stats_retrieve_rows(
                graph,
                args.get("section")
                    .expect("db.stats.retrieve binds its required section before its rows")
                    .as_string(),
            ),
            Self::SchemaNodeTypeProperties => Ok(schema_node_type_rows(graph)),
            Self::SchemaRelTypeProperties => Ok(schema_rel_type_rows(graph)),
            Self::SchemaVisualization => Ok(schema_visualization_rows(graph, args.get("options"))),
        }
    }
}

/// Row for `tx.getMetaData` (CIP-230): the fixed `source: 'cip230'`
/// metadata map.
fn tx_get_metadata_rows() -> Vec<BTreeMap<String, Value>> {
    vec![BTreeMap::from([(
        "metadata".into(),
        Value::Map(BTreeMap::from([("source".into(), Value::String("cip230".into()))]).into()),
    )])]
}

/// Shared producer for the single-column distinct-string procedures
/// (`db.labels`, `db.relationshipTypes`, `db.propertyKeys`): a `BTreeSet`
/// dedupes and sorts in one pass, then each value maps to a one-column row.
fn distinct_string_rows(
    values: impl IntoIterator<Item = String>,
    column: &str,
) -> Vec<BTreeMap<String, Value>> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|value| BTreeMap::from([(column.into(), Value::String(value.into()))]))
        .collect()
}

/// Row for `db.ping` (Neo4j Browser's connectivity probe): reaching this
/// code means the database answered, so the row is a constant `true`.
fn ping_rows() -> Vec<BTreeMap<String, Value>> {
    vec![BTreeMap::from([("success".into(), Value::Bool(true))])]
}

/// Rows for `db.labels`: every distinct stored node label, sorted.
fn label_rows(graph: &ExecGraph<'_>) -> Vec<BTreeMap<String, Value>> {
    let labels = graph
        .all_node_ids()
        .into_iter()
        .flat_map(|id| graph.node_labels(id).unwrap_or_default());
    distinct_string_rows(labels, "label")
}

/// Rows for `db.relationshipTypes`: every distinct stored relationship
/// type, sorted.
fn relationship_type_rows(graph: &ExecGraph<'_>) -> Vec<BTreeMap<String, Value>> {
    let types = graph
        .all_rel_ids()
        .into_iter()
        .filter_map(|id| graph.rel_type(id));
    distinct_string_rows(types, "relationshipType")
}

/// Rows for `db.propertyKeys`: every distinct property key on stored nodes
/// or relationships, sorted.
fn property_key_rows(graph: &ExecGraph<'_>) -> Vec<BTreeMap<String, Value>> {
    let node_keys = graph
        .all_node_ids()
        .into_iter()
        .flat_map(|id| graph.node_prop_keys(id));
    let rel_keys = graph
        .all_rel_ids()
        .into_iter()
        .flat_map(|id| graph.rel_prop_keys(id));
    distinct_string_rows(node_keys.chain(rel_keys), "propertyKey")
}

/// Row for `db.info`: the database the statement targets is the graph's
/// namespace (ADR-0116 §7).
fn db_info_rows(graph: &ExecGraph<'_>) -> Vec<BTreeMap<String, Value>> {
    let namespace = graph.namespace().as_str();
    vec![BTreeMap::from([
        ("id".into(), Value::String(namespace.into())),
        ("name".into(), Value::String(namespace.into())),
        ("creationDate".into(), Value::Null),
    ])]
}

/// Row for `dbms.info` (ADR-0142: Desktop-compat procedure,
/// `updateDBMSInfo`). The procedure arm only sees the graph, not the
/// session's current database, and Relate is a single embedded
/// database — so `name` is the constant `'system'` (the always-present
/// namespace, ADR-0116) standing in for "the current database name".
/// `creationDate` is a documented placeholder: the engine tracks no
/// creation time.
fn dbms_info_rows() -> Vec<BTreeMap<String, Value>> {
    vec![BTreeMap::from([
        ("id".into(), Value::String("relate".into())),
        ("name".into(), Value::String("system".into())),
        (
            "creationDate".into(),
            Value::String("1970-01-01T00:00:00Z".into()),
        ),
    ])]
}

/// Row for `dbms.showCurrentUser` (ADR-0143: Browser/Desktop-compat
/// procedure, LABS-148). Session context beyond the principal is not
/// plumbed, so the other columns are fixed documented placeholders.
/// ADR-0273: `username` is the connection's authenticated principal when
/// the surface set one (Bolt after a validated HELLO), else the documented
/// placeholder `'relate'`; `roles: ['admin']` conveys the engine's
/// actual authorization model (there is none) and `flags: []` is honest
/// (Relate has no user flags).
fn dbms_show_current_user_rows(principal: Option<&str>) -> Vec<BTreeMap<String, Value>> {
    vec![BTreeMap::from([
        (
            "username".into(),
            Value::String(principal.unwrap_or("relate").into()),
        ),
        (
            "roles".into(),
            Value::List(vec![Value::String("admin".into())].into()),
        ),
        ("flags".into(), Value::List(Vec::new().into())),
    ])]
}

/// Row for `dbms.licenseAgreementDetails` (ADR-0143): honest constant —
/// Relate is Apache-2.0 (ADR-0055): no license agreement to accept and
/// no trial, so `status: 'yes'` with null off-trial counters.
fn dbms_license_agreement_rows() -> Vec<BTreeMap<String, Value>> {
    vec![BTreeMap::from([
        ("status".into(), Value::String("yes".into())),
        ("daysLeftOnTrial".into(), Value::Null),
        ("totalTrialDays".into(), Value::Null),
    ])]
}

/// Rows for `dbms.components` (ADR-0142, ADR-0152): three rows, order
/// load-bearing —
/// row 1 is `Neo4j Kernel` because Neo4j Desktop finds the version row *by
/// name* while the public web Browser reads only the first record's
/// `versions[0]`; row 2 feeds Desktop's `cypherVersions`; row 3 keeps
/// Relate's identification row. Row 1 reports the ADR-0152 policy
/// constant `NEO4J_KERNEL_VERSION`; row 3 reports the honest workspace
/// version. Every row reports edition `community`
/// (Relate has no enterprise tier).
fn dbms_components_rows() -> Vec<BTreeMap<String, Value>> {
    const COMPONENTS: &[(&str, &[&str])] = &[
        ("Neo4j Kernel", &[NEO4J_KERNEL_VERSION]),
        ("Cypher", &["5", "25"]),
        ("Relate", &[RELATE_VERSION]),
    ];
    COMPONENTS
        .iter()
        .map(|(name, versions)| {
            BTreeMap::from([
                ("name".into(), Value::String((*name).into())),
                (
                    "versions".into(),
                    Value::List(
                        versions
                            .iter()
                            .map(|version| Value::String((*version).into()))
                            .collect::<Vec<_>>()
                            .into(),
                    ),
                ),
                ("edition".into(), Value::String("community".into())),
            ])
        })
        .collect()
}

/// Rows for `dbms.clientConfig`/`dbms.listConfig` (ADR-0143): one row per
/// Neo4j 5.x+ default clientConfig allowlist key, in the Browser's lookup
/// order. On every row `dynamic: false`, `defaultValue`/`startupValue`
/// equal `value`, `explicitlySet: false`, `validValues: ''`. All rows are
/// documented placeholders except `server.metrics.prefix` and
/// `client.allow_telemetry`, which are honest (Relate exposes no
/// Neo4j-style metrics namespace, and engine telemetry is opt-in).
fn dbms_config_rows() -> Vec<BTreeMap<String, Value>> {
    const CONFIG: &[(&str, &str, &str)] = &[
        (
            "browser.allow_outgoing_connections",
            "true",
            "Whether the Browser may open outgoing connections to other servers.",
        ),
        (
            "browser.credential_timeout",
            "0",
            "Seconds before the Browser discards retained credentials (0 = never).",
        ),
        (
            "browser.post_connect_cmd",
            "",
            "Command the Browser runs after connecting.",
        ),
        (
            "browser.remote_content_hostname_whitelist",
            "*",
            "Hostnames the Browser may load remote content from.",
        ),
        (
            "browser.retain_connection_credentials",
            "false",
            "Whether the Browser retains connection credentials.",
        ),
        (
            "browser.retain_editor_history",
            "true",
            "Whether the Browser retains editor history.",
        ),
        (
            "client.allow_telemetry",
            "false",
            "Whether client telemetry collection is allowed.",
        ),
        (
            "dbms.security.auth_enabled",
            "true",
            "Whether the server requires authentication.",
        ),
        (
            "server.metrics.prefix",
            "relate",
            "Prefix for reported metrics names.",
        ),
    ];
    CONFIG
        .iter()
        .map(|(name, value, description)| {
            BTreeMap::from([
                ("name".into(), Value::String((*name).into())),
                ("description".into(), Value::String((*description).into())),
                ("value".into(), Value::String((*value).into())),
                ("dynamic".into(), Value::Bool(false)),
                ("defaultValue".into(), Value::String((*value).into())),
                ("startupValue".into(), Value::String((*value).into())),
                ("explicitlySet".into(), Value::Bool(false)),
                ("validValues".into(), Value::String("".into())),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{rows, rows_with_principal, string};
    use super::*;
    use crate::graph::Graph;

    /// A void call still binds and type-checks its arguments per driving
    /// row: a wrong-typed argument is `InvalidArgumentType`, never silently
    /// accepted by the unit fast path.
    #[test]
    fn a_void_call_type_checks_its_arguments() {
        let graph = Graph::default();
        for query in [
            "CALL db.awaitIndexes('not-an-int')",
            "CALL tx.setMetaData(1)",
        ] {
            let error = crate::exec::test_support::error_of(
                crate::exec::test_support::try_exec_cypher(&graph, query),
            );
            assert!(error.contains("InvalidArgumentType"), "{query}: {error}");
        }
    }

    /// A NULL section passes the CALL type check (null is admitted for a
    /// required STRING), so the producer must reject it with a typed error —
    /// never a panic, which would unwind the query worker.
    #[test]
    fn a_null_stats_section_is_a_typed_rejection() {
        let graph = Graph::default();
        let error = crate::exec::test_support::error_of(
            crate::exec::test_support::try_exec_cypher(&graph, "CALL db.stats.retrieve(null)"),
        );
        assert!(
            error.contains("InvalidArgumentValue: db.stats.retrieve") && error.contains("NULL"),
            "{error}"
        );
    }

    /// ADR-0497 §6: the two procedures the corpus has no row for are gone —
    /// a call is the ordinary unknown-procedure error, and `SHOW INDEXES` /
    /// `SHOW CONSTRAINTS` are the surfaces.
    #[test]
    fn removed_catalog_procedures_are_unknown() {
        let graph = Graph::default();
        for name in ["db.indexes", "db.constraints"] {
            let error = crate::exec::test_support::error_of(
                crate::exec::test_support::try_exec_cypher(&graph, &format!("CALL {name}()")),
            );
            assert!(error.contains("ProcedureNotFound: "), "{name}: {error}");
        }
    }

    /// The producer table and the registry agree: every executable signature
    /// that declares return columns either has a diverted call path or a
    /// producer arm, so a new row-declaring procedure cannot silently answer
    /// the drift error. A signature whose producer needs real arguments is
    /// exercised with an empty binding, which must not be the drift error.
    #[test]
    fn every_row_declaring_signature_has_a_producer_or_a_diverted_path() {
        let graph = Graph::default();
        let view = crate::overlay::ExecGraph::read(&graph);
        for signature in crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES {
            if !signature.executable || signature.returns.is_empty() {
                continue;
            }
            if crate::exec::procedures::is_diverted_call(signature.name) {
                continue;
            }
            // Bind every declared parameter so a producer that reads its
            // arguments is exercised rather than panicking on the CALL
            // contract's missing binding; a typed contract error is fine —
            // only the drift error fails the pin.
            let args: BTreeMap<String, Value> = signature
                .args
                .iter()
                .map(|arg| (arg.name.to_string(), Value::String("GRAPH COUNTS".into())))
                .collect();
            match builtin_procedure_rows(&view, signature, None, &args) {
                Ok(_) => {}
                Err(error) => assert!(
                    !error.to_string().contains("has no row producer"),
                    "{} declares return columns but fell through the producer table: {error}",
                    signature.name
                ),
            }
        }
    }

    /// The metadata boundary: resolving a call reads the canonical signature
    /// and runs no row producer, so no producer ever sees a synthetic empty
    /// argument binding. The same name still answers its rows through
    /// `builtin_procedure_rows`, under a call's own arguments.
    #[test]
    fn metadata_comes_from_the_signature_without_running_a_producer() {
        fn metadata(name: &str) -> Procedure {
            procedure_metadata(executable_signature(name).expect("a registered builtin"))
        }

        let graph = Graph::default();
        crate::exec::test_support::exec_autocommit(&graph, "CREATE (:User {name: 'a'})")
            .expect("the fixture's CREATE succeeds");

        // `db.labels` answers a row for this graph; its metadata never does.
        let labels = metadata("db.labels");
        assert_eq!(labels.yield_names, ["label"]);
        assert!(labels.rows.is_empty());
        assert_eq!(rows(&graph, "db.labels").len(), 1);

        // `db.stats.retrieve`'s required argument and its yields come from
        // the signature; nothing is produced without the call's section.
        let retrieve = metadata("db.stats.retrieve");
        assert_eq!(retrieve.arg_names, ["section"]);
        assert_eq!(retrieve.arg_types, ["STRING"]);
        assert_eq!(retrieve.yield_names, ["section", "data"]);
        assert!(retrieve.rows.is_empty());

        // A void procedure declares no yields, so it has no row producer.
        let await_indexes = metadata("db.awaitIndexes");
        assert!(await_indexes.yield_names.is_empty() && await_indexes.rows.is_empty());

        // A catalog-only row is listed but not callable: the registry carries
        // it, `executable_signature` refuses it. Both halves are asserted,
        // because `None` alone cannot tell a catalog-only row from a name the
        // registry does not carry at all.
        let catalog_only =
            crate::procedure_registry::builtin_procedure_signature("db.clearQueryCaches")
                .expect("the catalog-only row is registered");
        assert!(catalog_only.listed && !catalog_only.executable);
        assert!(executable_signature("db.clearQueryCaches").is_none());
    }

    /// A row-declaring signature without a producer arm is table/producer
    /// drift, never a zero-row answer. The vector and fulltext query
    /// procedures are the registry's remaining row-declaring entries without
    /// an arm here — `exec_call` diverts them to their own executors — so a
    /// direct call pins the guard.
    #[test]
    fn a_diverted_row_declaring_signature_is_an_internal_error_on_the_generic_path() {
        let graph = Graph::default();
        let view = ExecGraph::read(&graph);
        let signature = executable_signature("db.index.vector.queryNodes")
            .expect("db.index.vector.queryNodes is registered");
        let error = builtin_procedure_rows(&view, signature, None, &BTreeMap::new())
            .expect_err("a diverted procedure has no generic row producer");
        let message = error.to_string();
        assert!(
            message.contains("dedicated executor") || message.contains("no row producer"),
            "{message}"
        );
    }

    /// A terminal void call is a unit statement (CIP-159): the clause engine
    /// reads the void fact from the signature — never by producing the rows
    /// and discarding them — so the terminal `db.awaitIndexes()` result
    /// carries no rows, while a row-declaring terminal call keeps its row.
    #[test]
    fn a_terminal_void_builtin_call_answers_no_rows() {
        let graph = Graph::default();
        let void = crate::exec::test_support::exec_cypher(&graph, "CALL db.awaitIndexes()");
        assert!(void.rows.is_empty());

        let rows_afield = crate::exec::test_support::exec_cypher(&graph, "CALL db.ping()");
        assert_eq!(rows_afield.rows.len(), 1);
    }

    #[test]
    fn dbms_components_returns_kernel_cypher_relate_rows_with_edition() {
        // ADR-0142: three rows, order load-bearing — Neo4j Desktop finds the
        // version row by name (`Neo4j Kernel`), the public web Browser reads
        // only the first record's `versions[0]`, so row 1 is both first and
        // named `Neo4j Kernel`.
        let graph = Graph::default();
        let rows = rows(&graph, "dbms.components");

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["name"], string("Neo4j Kernel"));
        assert_eq!(
            rows[0]["versions"],
            Value::List(vec![string(NEO4J_KERNEL_VERSION)].into())
        );
        assert_eq!(rows[0]["edition"], string("community"));
        assert_eq!(rows[1]["name"], string("Cypher"));
        assert_eq!(
            rows[1]["versions"],
            Value::List(vec![string("5"), string("25")].into())
        );
        assert_eq!(rows[1]["edition"], string("community"));
        assert_eq!(rows[2]["name"], string("Relate"));
        assert_eq!(
            rows[2]["versions"],
            Value::List(vec![string(RELATE_VERSION)].into())
        );
        assert_eq!(rows[2]["edition"], string("community"));
    }

    #[test]
    fn db_ping_reports_success() {
        // The connectivity probe answers one constant row; the signature
        // declares the BOOLEAN `success` output.
        let rows = rows(&Graph::default(), "db.ping");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["success"], Value::Bool(true));
        let signature = crate::procedure_registry::builtin_procedure_signature("db.ping")
            .expect("db.ping is registered");
        assert_eq!(signature.returns[0].name, "success");
        assert_eq!(signature.returns[0].type_str, "BOOLEAN");
        assert!(signature.works_on_system);
    }

    #[test]
    fn db_info_reports_the_graphs_namespace() {
        // Default graph is the `neo4j` namespace.
        let graph = Graph::default();
        let neo4j_rows = rows(&graph, "db.info");
        assert_eq!(neo4j_rows.len(), 1);
        assert_eq!(neo4j_rows[0]["id"], string("neo4j"));
        assert_eq!(neo4j_rows[0]["name"], string("neo4j"));

        // A graph tagged with the system_ontologies namespace reports it
        // (ADR-0116 §7; the facade's ADR-0229 gate now rejects every
        // statement routed there before it can reach this, but the
        // graph-level identity rule is unchanged).
        let mut ontologies = Graph::default();
        ontologies.set_namespace(relate_core::Namespace::SystemOntologies);
        let ontologies_rows = rows(&ontologies, "db.info");
        assert_eq!(ontologies_rows.len(), 1);
        assert_eq!(ontologies_rows[0]["id"], string("system_ontologies"));
        assert_eq!(ontologies_rows[0]["name"], string("system_ontologies"));
    }

    #[test]
    fn dbms_info_returns_the_desktop_compat_row() {
        // ADR-0142: one row; `name` is the constant `system` (the procedure
        // arm cannot see the session's current database) and `creationDate`
        // is a documented placeholder.
        let graph = Graph::default();
        let rows = rows(&graph, "dbms.info");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], string("relate"));
        assert_eq!(rows[0]["name"], string("system"));
        assert_eq!(rows[0]["creationDate"], string("1970-01-01T00:00:00Z"));
    }

    #[test]
    fn dbms_info_signature_declares_id_name_creation_date_outputs() {
        let signature = crate::procedure_registry::builtin_procedure_signature("dbms.info")
            .expect("dbms.info is registered");
        assert!(signature.executable);
        let outputs: Vec<&str> = signature.returns.iter().map(|ret| ret.name).collect();
        assert_eq!(outputs, ["id", "name", "creationDate"]);
    }

    #[test]
    fn dbms_components_signature_declares_edition_output() {
        let signature = crate::procedure_registry::builtin_procedure_signature("dbms.components")
            .expect("dbms.components is registered");
        let outputs: Vec<&str> = signature.returns.iter().map(|ret| ret.name).collect();
        assert_eq!(outputs, ["name", "versions", "edition"]);
    }

    #[test]
    fn dbms_show_current_user_returns_the_placeholder_row_without_a_principal() {
        // ADR-0143/0273: with no connection principal (every non-Bolt
        // surface, and Bolt under `--auth none`), one fixed
        // documented-placeholder row.
        let graph = Graph::default();
        let rows = rows(&graph, "dbms.showCurrentUser");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["username"], string("relate"));
        assert_eq!(rows[0]["roles"], Value::List(vec![string("admin")].into()));
        assert_eq!(rows[0]["flags"], Value::List(Vec::new().into()));
    }

    #[test]
    fn dbms_show_current_user_reports_the_connection_principal() {
        // ADR-0273: a set principal becomes `username`; roles/flags keep the
        // ADR-0143 values.
        let graph = Graph::default();
        let rows = rows_with_principal(&graph, "dbms.showCurrentUser", Some("alice"));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["username"], string("alice"));
        assert_eq!(rows[0]["roles"], Value::List(vec![string("admin")].into()));
        assert_eq!(rows[0]["flags"], Value::List(Vec::new().into()));
    }

    #[test]
    fn dbms_config_rows_match_the_adr0143_allowlist() {
        // ADR-0143: nine rows, one per Neo4j 5.x+ allowlist key; identical
        // for dbms.clientConfig and dbms.listConfig.
        let graph = Graph::default();
        let client_rows = rows(&graph, "dbms.clientConfig");
        let list_rows = rows(&graph, "dbms.listConfig");
        assert_eq!(client_rows, list_rows);
        assert_eq!(client_rows.len(), 9);

        let expected: &[(&str, &str)] = &[
            ("browser.allow_outgoing_connections", "true"),
            ("browser.credential_timeout", "0"),
            ("browser.post_connect_cmd", ""),
            ("browser.remote_content_hostname_whitelist", "*"),
            ("browser.retain_connection_credentials", "false"),
            ("browser.retain_editor_history", "true"),
            ("client.allow_telemetry", "false"),
            ("dbms.security.auth_enabled", "true"),
            ("server.metrics.prefix", "relate"),
        ];
        for (row, (name, value)) in client_rows.iter().zip(expected) {
            assert_eq!(row["name"], string(name));
            assert_eq!(row["value"], string(value));
            assert_eq!(row["dynamic"], Value::Bool(false));
            assert_eq!(row["defaultValue"], string(value));
            assert_eq!(row["startupValue"], string(value));
            assert_eq!(row["explicitlySet"], Value::Bool(false));
            assert_eq!(row["validValues"], string(""));
            // Every row carries a non-empty fixed description string.
            assert!(matches!(&row["description"], Value::String(d) if !d.is_empty()));
        }
    }

    #[test]
    fn dbms_license_agreement_details_returns_the_honest_row() {
        // ADR-0143: honest constant — Apache-2.0 (ADR-0055), no agreement
        // to accept and no trial.
        let graph = Graph::default();
        let rows = rows(&graph, "dbms.licenseAgreementDetails");

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["status"], string("yes"));
        assert_eq!(rows[0]["daysLeftOnTrial"], Value::Null);
        assert_eq!(rows[0]["totalTrialDays"], Value::Null);
    }
}

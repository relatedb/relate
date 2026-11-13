//! Catalog command execution: row shaping for `SHOW ...` and dispatch into
//! `catalog_show` / `catalog_schema` / `graph_type_schema`.

use std::collections::BTreeMap;

use std::str::FromStr;

use relate_core::{Error, Result, Value};
use relate_model::identifier::quote_identifier_always;
use relate_model::EntityKind;

use crate::eval::filter_rows_where;
use crate::graph::QueryStats;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::ddl_options::KEY_INDEX_CONFIG;
use super::scope::visible_keys;
use super::{catalog_schema, catalog_show, graph_type_schema, QueryResult};
use crate::ctx::QueryCtx;

/// The engine's one user (ADR-0116's fixed namespace model has no user
/// catalog): `SHOW USERS`, `SHOW CURRENT USER` and the `EXECUTABLE BY`
/// resolution all name this user.
const CURRENT_USER: &str = "neo4j";

/// The `EXECUTABLE [BY <user>]` resolution: a request for the engine's own
/// user (or `CURRENT USER`) sees the whole listed catalog, any other user
/// name sees nothing. An absent selector — including the bare `EXECUTABLE`
/// form — is not a privilege filter.
fn executable_by_user(executable: Option<&relate_parser::ExecutableBy>) -> bool {
    use relate_parser::ExecutableBy;
    match executable {
        None | Some(ExecutableBy::Current) => true,
        Some(ExecutableBy::User(user)) => user == CURRENT_USER,
    }
}

/// The def→typed-binding resolution both the `SHOW CONSTRAINTS` row
/// producer, the ADR-0481 `schema.cypher` mirror and the GRAPH COUNTS
/// section's entry producer (ADR-0497 §6) read: the stored kind tag parsed
/// once (`None` an unparseable tag), the entity kind from the relationship
/// flag, and the derived-constraint record of the same name (`None` when the
/// graph type manages no such record) — one binding, never a second spelling
/// of the parse/flag/join.
pub(crate) fn constraint_binding<'a>(
    def: &relate_catalog::ConstraintDef,
    derived: &'a BTreeMap<String, relate_model::Constraint>,
) -> ConstraintBinding<'a> {
    ConstraintBinding {
        kind: relate_model::ConstraintType::from_str(&def.kind).ok(),
        entity: EntityKind::from_relationship_flag(def.relationship),
        detail: derived.get(&def.name),
    }
}

/// One constraint's typed binding: the stored kind tag parsed (`None` an
/// unparseable tag), the entity kind, and the derived-constraint record of
/// the same name. Named fields, so the three adjacent facts cannot
/// transpose at a destructuring site.
pub(crate) struct ConstraintBinding<'a> {
    pub kind: Option<relate_model::ConstraintType>,
    pub entity: EntityKind,
    pub detail: Option<&'a relate_model::Constraint>,
}

/// The Neo4j constraint-kind renderings for the two space-style surfaces:
/// `db.schema.visualization`'s upper space style and `db.stats.retrieve`
/// GRAPH COUNTS' title style. One table keyed by the model's vocabulary, so
/// a new constraint kind forces every surface's spelling decision at once.
pub(crate) struct SpaceConstraintNames {
    pub visualization: &'static str,
    pub graph_counts: &'static str,
}

/// The two space-style kind names of one constraint kind.
pub(crate) fn space_constraint_names(kind: relate_model::ConstraintType) -> SpaceConstraintNames {
    use relate_model::ConstraintType;
    match kind {
        ConstraintType::Unique => SpaceConstraintNames {
            visualization: "NODE PROPERTY UNIQUENESS",
            graph_counts: "Uniqueness constraint",
        },
        ConstraintType::Key => SpaceConstraintNames {
            visualization: "NODE KEY",
            graph_counts: "Node Key",
        },
        ConstraintType::Exists => SpaceConstraintNames {
            visualization: "NODE PROPERTY EXISTENCE",
            graph_counts: "Existence constraint",
        },
        ConstraintType::PropertyType => SpaceConstraintNames {
            visualization: "NODE PROPERTY TYPE",
            graph_counts: "Property type constraint",
        },
        ConstraintType::LabelExistence | ConstraintType::NodeLabelExistence => {
            SpaceConstraintNames {
                visualization: "NODE LABEL EXISTENCE",
                graph_counts: "Node label existence constraint",
            }
        }
        ConstraintType::SourceLabel => SpaceConstraintNames {
            visualization: "RELATIONSHIP SOURCE LABEL",
            graph_counts: "Relationship endpoint label constraint",
        },
        ConstraintType::SourceLabelExistence => SpaceConstraintNames {
            visualization: "RELATIONSHIP SOURCE LABEL EXISTENCE",
            graph_counts: "Relationship endpoint label constraint",
        },
        ConstraintType::TargetLabel => SpaceConstraintNames {
            visualization: "RELATIONSHIP TARGET LABEL",
            graph_counts: "Relationship endpoint label constraint",
        },
        ConstraintType::TargetLabelExistence => SpaceConstraintNames {
            visualization: "RELATIONSHIP TARGET LABEL EXISTENCE",
            graph_counts: "Relationship endpoint label constraint",
        },
    }
}

pub(crate) fn imported_catalog(
    rows: Vec<indexmap::IndexMap<String, relate_model::CypherValue>>,
) -> Vec<Row> {
    rows.into_iter()
        .map(|r| {
            r.into_iter()
                .map(|(k, v)| (k, relate_core::cypher_to_value(&v)))
                .collect()
        })
        .collect()
}

/// The `SHOW FUNCTIONS` / `SHOW PROCEDURES` visibility policy: the imported
/// catalogue rows when the `EXECUTABLE BY` filter admits the current user, the
/// empty result otherwise.
fn visible_catalog(
    visible: bool,
    rows: impl FnOnce() -> Vec<indexmap::IndexMap<String, relate_model::CypherValue>>,
) -> Vec<Row> {
    if visible {
        imported_catalog(rows())
    } else {
        Vec::new()
    }
}

/// The catalog descriptor of a catalog clause; a non-catalog clause reaching
/// the catalog projection is a caller bug, not a user-facing error.
fn require_catalog_info(
    clause: &relate_parser::Clause,
) -> Result<relate_front::show_columns::CatalogInfo<'_>> {
    relate_front::show_columns::catalog_info(clause)
        .ok_or_else(|| Error::internal("catalog projection called for a non-catalog clause"))
}

/// The two marker keys the projection's renamed-YIELD shadowing carries into
/// the restore; no Cypher identifier can spell them.
const SOURCE_CELLS: &str = "\u{0}show.sources";
const SOURCE_PREVIOUS: &str = "\u{0}show.previous";

/// Compose the cross join of `input` rows with `catalog` rows under the
/// clause's projection — prefill, YIELD columns, and the renamed-YIELD
/// source-name shadowing — without the clause-level `ORDER BY`/`SKIP`/
/// `LIMIT`, the trailing WHERE, or the shadow restore. Those belong in
/// [`finish_catalog_rows`], which runs over the *flattened* result: paging
/// applies to the whole cross join, never once per input row.
fn compose_catalog_rows(
    info: &relate_front::show_columns::CatalogInfo<'_>,
    input: impl IntoIterator<Item = Row>,
    mut catalog: Vec<Row>,
) -> Vec<Row> {
    // The function/procedure/setting catalog rows are built from metadata
    // tables that may omit keys; pre-fill every full column with Null so
    // `YIELD *` + WHERE sees a uniform shape.
    if info.prefill {
        for row in &mut catalog {
            for column in &info.full_columns {
                row.entry(column.clone()).or_insert(Value::Null);
            }
        }
    }
    // The projection rule is selected once; `None` is the `YIELD *` raw-row
    // passthrough, `Some` the (output, source) column lists. The cross join
    // skeleton then exists once. The two arms' collision policies are the
    // two projection contracts, stated once each: `YIELD *` merges the raw
    // catalog row into the outer row and keeps an outer variable that
    // collides with a column (the passthrough must not clobber the row it
    // is joining), while a projected column list names the composed
    // columns outright, so its `insert` wins (a column the catalog row
    // lacks composes as Null, like a missing projected cell).
    let projection = info.projection();
    let fill: Option<&[(String, String)]> = match &projection {
        relate_front::show_columns::CatalogProjection::Star => None,
        relate_front::show_columns::CatalogProjection::Columns(pairs) => Some(pairs),
    };
    // A renamed YIELD keeps *both* names in scope for the clause's own
    // ORDER BY and WHERE (`YIELD name AS constraintName ORDER BY name` is
    // valid Neo4j). The source cells sit at the row's top level while the
    // clause's stages run -- the SHOW column wins over an outer variable of
    // the same name -- and are restored/removed afterwards, so a later
    // `RETURN *` never observes them. The two marker keys carry what the
    // restore needs; no Cypher identifier can spell them.
    //
    // Coupling invariant, with the `Some(pairs)` write site below as its
    // only producer: `SOURCE_CELLS` and `SOURCE_PREVIOUS` are inserted
    // together, each always a map, and only when there is something to
    // restore. The restore therefore reads an absent `SOURCE_CELLS` as
    // "nothing was shadowed", and every other shape -- either marker
    // present without its map -- as a broken write-site invariant, never
    // as a state to fall back from.
    let mut output = Vec::new();
    for outer in input {
        for raw in &catalog {
            let mut row = outer.clone();
            match fill {
                None => {
                    for (k, v) in raw {
                        row.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
                Some(pairs) => {
                    // A renamed YIELD keeps both names in scope, but a
                    // projected (output) column always wins: in
                    // `YIELD name AS options, options AS name` the
                    // clause's own `size(options)` reads the *renamed*
                    // string column, not the raw map.
                    let outputs: std::collections::HashSet<&str> =
                        pairs.iter().map(|(out, _)| out.as_str()).collect();
                    let mut sources = BTreeMap::new();
                    for (out, src) in pairs {
                        let value = raw.get(src).cloned().unwrap_or(Value::Null);
                        // The shadow keeps its own copy; the projected cell
                        // takes the original, so the clone happens only when
                        // this column is actually shadowed.
                        if src != out && !outputs.contains(src.as_str()) {
                            sources.insert(src.clone(), value.clone());
                        }
                        row.insert(out.clone(), value);
                    }
                    if !sources.is_empty() {
                        let mut previous = BTreeMap::new();
                        for (key, value) in &sources {
                            if let Some(old) = row.get(key) {
                                previous.insert(key.clone(), old.clone());
                            }
                            row.insert(key.clone(), value.clone());
                        }
                        row.insert(SOURCE_CELLS.into(), Value::Map(sources.into()));
                        row.insert(SOURCE_PREVIOUS.into(), Value::Map(previous.into()));
                    }
                }
            }
            output.push(row);
        }
    }
    output
}

/// The stage half of the catalog projection, applied once over the composed
/// cross join: the clause's own `ORDER BY`/`SKIP`/`LIMIT` in written order,
/// then the trailing WHERE, then the source-name restore. Paging before the
/// WHERE is the SHOW order (`ORDER BY … SKIP … LIMIT … WHERE …`); paging
/// once per input row would change both composed SHOW cardinality and which
/// row a subsequent WHERE observes.
fn finish_catalog_rows(
    graph: &ExecGraph<'_>,
    info: &relate_front::show_columns::CatalogInfo<'_>,
    params: &BTreeMap<String, Value>,
    mut output: Vec<Row>,
) -> Result<Vec<Row>> {
    // The predicate lowers before the paging stages run, so an invalid WHERE
    // expression reports ahead of any paging-expression error (ADR-0335).
    let predicate = info.where_expr.map(relate_front::lower_expr).transpose()?;
    super::order::apply_yield_page_stages(graph, params, info.stages, &mut output)?;
    if let Some(predicate) = predicate.as_ref() {
        // ADR-0335: the predicate lowers at run time, so an undefined
        // variable surfaces its typed error here instead of silently
        // filtering every row; a null value still filters.
        output = filter_rows_where(graph, params, predicate, output)?;
    }
    // Undo the source-name shadowing (see the marker coupling invariant
    // above): restore an outer variable the SHOW column shadowed, remove a
    // source name it introduced. Only a genuinely absent marker means
    // "nothing to restore"; skipping a present-but-wrong-shape row would
    // silently drop the restore and leak the internal marker key into the
    // output, so a broken write-site invariant fails loud for both markers
    // alike.
    let marker_invariant_broken = |marker: &str, found: Option<&Value>| {
        Error::internal(format!(
            "SHOW source-name restore: {marker} did not hold the map the write \
             site inserts (found {found:?}); {SOURCE_CELLS} and {SOURCE_PREVIOUS} \
             are always written together"
        ))
    };
    for row in &mut output {
        let sources = match row.shift_remove(SOURCE_CELLS) {
            None => continue,
            Some(Value::Map(sources)) => sources,
            Some(other) => {
                return Err(marker_invariant_broken(SOURCE_CELLS, Some(&other)));
            }
        };
        // `SOURCE_PREVIOUS` is written by the same site, so an absent or
        // non-map value here is the same broken invariant.
        let previous = match row.shift_remove(SOURCE_PREVIOUS) {
            Some(Value::Map(previous)) => previous,
            other => return Err(marker_invariant_broken(SOURCE_PREVIOUS, other.as_ref())),
        };
        for key in sources.keys() {
            match previous.get(key) {
                Some(value) => {
                    row.insert(key.clone(), value.clone());
                }
                None => {
                    row.shift_remove(key);
                }
            }
        }
    }
    Ok(output)
}

/// Project a catalog clause over `input` rows against one catalog — the
/// single-catalog form (`SHOW FUNCTIONS`/`PROCEDURES`/`ROLES`/…). The
/// composition runs for the whole cross join, then the clause's stages run
/// once over it.
pub(crate) fn project_catalog(
    graph: &ExecGraph<'_>,
    input: Vec<Row>,
    catalog: Vec<Row>,
    clause: &relate_parser::Clause,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Row>> {
    let info = require_catalog_info(clause)?;
    let output = compose_catalog_rows(&info, input, catalog);
    finish_catalog_rows(graph, &info, params, output)
}

/// The per-input-row selection form of [`project_catalog`] (`SHOW SETTINGS`'
/// name expression): `select` yields each input row's catalog rows, the
/// composed results concatenate, and the paging/WHERE/restore stages run
/// ONCE over the flattened cross join — never once per input row, which
/// would repeat the clause's `LIMIT` for every input row.
pub(crate) fn project_catalog_by_row<F>(
    graph: &ExecGraph<'_>,
    input: Vec<Row>,
    clause: &relate_parser::Clause,
    params: &BTreeMap<String, Value>,
    mut select: F,
) -> Result<Vec<Row>>
where
    F: FnMut(&Row) -> Result<Vec<Row>>,
{
    let info = require_catalog_info(clause)?;
    let mut output = Vec::new();
    for row in input {
        let catalog = select(&row)?;
        output.extend(compose_catalog_rows(&info, std::iter::once(row), catalog));
    }
    finish_catalog_rows(graph, &info, params, output)
}

/// Statement result of a catalog clause: `Some` of the projected catalog
/// rows for row-producing commands (the SHOW family and `TERMINATE
/// TRANSACTIONS`), `None` for schema/admin DDL, which updates the catalog
/// and finishes with an empty result — zero rows, zero columns — matching
/// Neo4j's `CREATE CONSTRAINT` expectation. The row-producing
/// classification is the canonical `relate-front` predicate, so this
/// module never re-enumerates the command set.
pub(crate) fn catalog_result(rows: &[Row], clause: &relate_parser::Clause) -> Option<QueryResult> {
    let info = relate_front::show_columns::catalog_info(clause)?;
    // The projection: `YIELD *` keeps the first row's key order when a row
    // exists (falling back to the command's full column table for a
    // zero-row result, so the header survives), and both other modes come
    // from the one column projection.
    let columns = match info.projection() {
        relate_front::show_columns::CatalogProjection::Star => rows
            .first()
            .map(visible_keys)
            .unwrap_or_else(|| info.full_columns.clone()),
        relate_front::show_columns::CatalogProjection::Columns(pairs) => {
            pairs.into_iter().map(|(output, _)| output).collect()
        }
    };
    let data = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    Some(QueryResult::intermediate(
        columns,
        data,
        QueryStats::default(),
    ))
}

pub(crate) fn catalog_row(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Row {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

pub(crate) fn string_list(values: impl IntoIterator<Item = String>) -> Value {
    Value::List(
        values
            .into_iter()
            .map(|s| Value::String(s.into()))
            .collect::<Vec<_>>()
            .into(),
    )
}

/// The SHOW cell of an optional string column — the string, or the
/// column's NULL: the one `Option`→cell policy every catalog row's
/// optional string column reads (the `createStatement`,
/// `owningConstraint`, `propertyType`, `enforcedLabel` columns), never
/// a second inline `.map(...).unwrap_or(Value::Null)` spelling beside
/// it.
pub(crate) fn optional_string_cell(value: Option<&str>) -> Value {
    value
        .map(|s| Value::String(s.into()))
        .unwrap_or(Value::Null)
}

/// The SHOW `options` cell of a schema rule: the stored options map with the
/// always-present `indexConfig` entry (Neo4j's shape; a kind that stores no
/// config reports `{indexConfig: {}}`). One construction for both row
/// producers — the index row and the index-backed constraint row.
pub(crate) fn options_cell(options: &BTreeMap<String, Value>) -> Value {
    let mut cell = options.clone();
    cell.entry(KEY_INDEX_CONFIG.to_string())
        .or_insert_with(|| Value::Map(BTreeMap::new().into()));
    Value::Map(cell.into())
}

/// A schema rule row's `id` cell. Neo4j reports NULL for a rule that is not
/// yet committed (a rule created inside the open transaction) and the
/// engine's stable placeholder id for a committed one; `SHOW INDEXES` and
/// `SHOW CONSTRAINTS` must answer alike, so both row producers read this one
/// policy.
pub(crate) fn schema_id_cell(committed: bool) -> Value {
    if committed {
        Value::Int(0)
    } else {
        Value::Null
    }
}

/// Render an identifier in the always-backtick-quoted form SHOW-rendered
/// Cypher text and the CIP-180 canonical graph-type body use: the model's
/// one forced-quoting constructor,
/// [`quote_identifier_always`](relate_model::identifier::quote_identifier_always)
/// (`Person` → `` `Person` ``), so the escape rule (embedded backticks
/// doubled) is never re-spelled here.
pub(crate) fn backtick(identifier: &str) -> String {
    quote_identifier_always(identifier)
}

pub(crate) fn schema_name_value(
    name: &relate_parser::SchemaName,
    params: &BTreeMap<String, Value>,
) -> Result<String> {
    match name {
        relate_parser::SchemaName::Literal(name) => Ok(name.clone()),
        relate_parser::SchemaName::Parameter(name) => match params.get(name) {
            Some(Value::String(value)) => Ok(String::from(&**value)),
            // ADR-0308: an absent parameter is ParameterMissing, not a type.
            None => Err(Error::parameters_missing([name.as_str()])),
            Some(_) => Err(Error::type_error("schema name must be STRING")),
        },
    }
}

pub(crate) fn execute_catalog(
    graph: &ExecGraph<'_>,
    clause: &relate_parser::Clause,
    rows: Vec<Row>,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    use relate_parser::{Clause as C, FunctionScope};
    let catalog = match clause {
        C::ShowFunctions(c) => visible_catalog(
            !matches!(c.scope, FunctionScope::UserDefined)
                && executable_by_user(c.executable.as_ref()),
            || crate::show_catalog::function_catalog_rows(graph.preview()),
        ),
        C::ShowProcedures(c) => visible_catalog(executable_by_user(c.executable.as_ref()), || {
            crate::show_catalog::procedure_catalog_rows(graph.preview())
        }),
        C::ShowRoles(c) => catalog_show::show_roles_rows(c),
        C::ShowServers(_) => vec![catalog_row([(
            "name",
            Value::String("relate-server-1".into()),
        )])],
        C::ShowUsers(_) => vec![catalog_row([("user", Value::String(CURRENT_USER.into()))])],
        C::ShowSettings(c) => {
            return catalog_show::show_settings_rows(graph, c, rows, clause, params);
        }
        C::ShowTransactions(c) => {
            catalog_show::show_transactions_rows(graph, c, &rows, params, qctx)?
        }
        C::TerminateTransactions(c) => {
            catalog_show::terminate_transactions_rows(graph, c, &rows, params)?
        }
        C::ShowDatabases(c) => catalog_show::show_databases_rows(graph, c, params)?,
        C::ShowIndexes(c) => catalog_show::show_indexes_rows(graph, c)?,
        C::ShowConstraints(c) => catalog_show::show_constraints_rows(graph, c)?,
        C::ShowGraphType(c) => catalog_show::show_graph_type_rows(graph, c)?,
        C::ShowCurrentGraphType(c) => catalog_show::show_current_graph_type_rows(graph, c)?,
        C::CreateIndex(c) => return catalog_schema::catalog_create_index(graph, c, rows, params),
        C::DropIndex(c) => return catalog_schema::catalog_drop_index(graph, c, rows, params),
        C::CreateConstraint(c) => {
            return catalog_schema::catalog_create_constraint(graph, c, rows, params)
        }
        C::DropConstraint(c) => {
            return catalog_schema::catalog_drop_constraint(graph, c, rows, params)
        }
        C::AlterDatabase(c) => {
            return catalog_schema::catalog_alter_database(graph, c, rows, params)
        }
        C::CreateGraphType(c) => {
            return graph_type_schema::catalog_create_graph_type(graph, c, rows, params)
        }
        C::AlterCurrentGraphType(c) => {
            return graph_type_schema::alter_current_graph_type(graph, c, rows, params)
        }
        _ => return Err(Error::semantic("unsupported catalog command")),
    };
    project_catalog(graph, rows, catalog, clause, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_catalog_preserves_temporal_and_map_list_values() {
        let date = chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let mut attrs = indexmap::IndexMap::new();
        attrs.insert("nested".to_string(), relate_model::CypherValue::Int(7));
        let mut row = indexmap::IndexMap::new();
        row.insert("created".to_string(), relate_model::CypherValue::Date(date));
        row.insert("attrs".to_string(), relate_model::CypherValue::Map(attrs));
        row.insert(
            "tags".to_string(),
            relate_model::CypherValue::List(vec![
                relate_model::CypherValue::Str("a".to_string()),
                relate_model::CypherValue::Int(2),
            ]),
        );

        let rows = imported_catalog(vec![row]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert!(matches!(
            row.get("created"),
            Some(Value::Temporal(relate_core::temporal::Temporal::Date(d))) if *d == date
        ));
        assert!(
            matches!(row.get("attrs"), Some(Value::Map(m)) if m.get("nested") == Some(&Value::Int(7)))
        );
        match row.get("tags") {
            Some(Value::List(items)) => {
                assert_eq!(items[0], Value::String("a".into()));
                assert_eq!(items[1], Value::Int(2));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }
}

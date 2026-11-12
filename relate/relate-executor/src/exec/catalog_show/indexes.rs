//! `SHOW INDEXES` row producer and its rendering helpers.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_model::EntityKind;

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::super::QueryResult;
use super::entity_variable;
use crate::exec::catalog::{backtick, catalog_row, optional_string_cell, string_list};
use crate::redo::{backing_constraint_by_name, IndexDetailSpec, IndexFacts, IndexKindSpec};

// `SHOW INDEXES` rendering helpers over the shared
// `relate_model::EntityKind`: they bind the index pattern shapes to
// the entity decision instead of re-branching on a bare
// `relationship: bool` at each use site; the pattern variable itself is
// the shared `super::entity_variable` decision. The `entityType` column
// uses `EntityKind::display_name` directly.

/// The `FOR` pattern for a label/type-token index, quoting and
/// `|`-joining the labels itself: `(n:`A`|`B`)` / `()-[r:`T`]-()`.
fn entity_pattern(entity: EntityKind, labels: &[String]) -> String {
    let variable = entity_variable(entity);
    let tokens = labels
        .iter()
        .map(|label| backtick(label))
        .collect::<Vec<_>>()
        .join("|");
    match entity {
        EntityKind::Node => format!("({variable}:{tokens})"),
        EntityKind::Relationship => format!("()-[{variable}:{tokens}]-()"),
    }
}

/// The LOOKUP pattern and EACH expression:
/// `FOR (n) ON EACH labels(n)` / `FOR ()-[r]-() ON EACH type(r)`.
fn entity_lookup_pattern_each(entity: EntityKind) -> (String, String) {
    let variable = entity_variable(entity);
    match entity {
        EntityKind::Node => (format!("({variable})"), format!("labels({variable})")),
        EntityKind::Relationship => (format!("()-[{variable}]-()"), format!("type({variable})")),
    }
}

/// The `OPTIONS {indexConfig: {...}}` suffix of a createStatement.
/// `None` when the stored options cannot be faithfully rendered — a
/// present `indexConfig` that is not a map, or an entry with no scalar
/// spelling (a nested map) — never an inlined fake `null` literal. No
/// `indexConfig` renders as the empty string.
fn render_index_options(stored_options: &BTreeMap<String, Value>) -> Option<String> {
    let config = match stored_options.get(crate::exec::ddl_options::KEY_INDEX_CONFIG) {
        None => return Some(String::new()),
        // An empty config renders no OPTIONS suffix (Neo4j's own shape:
        // `options` carries `{}}}}` but `createStatement` omits an
        // empty `indexConfig`).
        Some(Value::Map(config)) if config.is_empty() => return Some(String::new()),
        Some(Value::Map(config)) => config,
        Some(_) => return None,
    };
    let mut entries = Vec::with_capacity(config.len());
    for (key, value) in config.iter() {
        let rendered = scalar_option_text(value)?;
        entries.push(format!("{}: {rendered}", backtick(key)));
    }
    Some(format!(
        " OPTIONS {{indexConfig: {{{}}}}}",
        entries.join(",")
    ))
}

/// One `indexConfig` entry's rendered text: a string quotes, a float keeps
/// Neo4j's shortest `.0` form, and a list of scalars renders `[a, b]`
/// (the POINT index's spatial bounds). Anything else — a nested map — has
/// no faithful rendering (`None`).
fn scalar_option_text(value: &Value) -> Option<String> {
    match value {
        // The one scalar spelling — `Value::to_cypher_literal` escapes a
        // string's quotes and keeps the float's `.0` form, so the quoted
        // DDL text is valid Cypher whatever the stored option value holds.
        Value::String(_) | Value::Int(_) | Value::Bool(_) | Value::Float(_) => {
            Some(value.to_cypher_literal())
        }
        Value::List(values) => values
            .iter()
            .map(scalar_option_text)
            .collect::<Option<Vec<_>>>()
            .map(|items| format!("[{}]", items.join(", "))),
        _ => None,
    }
}

/// The `createStatement` column for one index row: Neo4j CE's rendered
/// CREATE statement, or `None` when it cannot be faithfully rendered
/// (an empty property list or a non-scalar `indexConfig` entry has no
/// faithful rendering). The caller maps `None` to the column's NULL. The
/// kind is the renderer's own `detail.kind` — this renderer is reached
/// only when the detail join hits, so [`IndexKindSpec::resolve`]'s
/// binding equals it by construction; no threaded second channel.
fn index_create_statement(
    detail: &IndexDetailSpec,
    name: &str,
    entity: EntityKind,
    stored_options: &BTreeMap<String, Value>,
) -> Option<String> {
    let kind = detail.kind;
    let variable = entity_variable(entity);
    let name = backtick(name);
    // The EACH form ([`is_lookup`]'s one LOOKUP decision): Neo4j CE's
    // `FOR (n) ON EACH labels(n)` / `FOR ()-[r]-() ON EACH type(r)` —
    // the generic path below would render the broken `FOR (n:) ON
    // (n.``)`.
    if kind.is_lookup() {
        let (pattern, each) = entity_lookup_pattern_each(entity);
        return Some(format!(
            "CREATE LOOKUP INDEX {name} FOR {pattern} ON EACH {each}"
        ));
    }
    // A property-less index has no faithful ON clause (`ON (n.``)`) —
    // fail closed rather than render a broken statement.
    if detail.properties.is_empty() {
        return None;
    }
    let pattern = entity_pattern(entity, &detail.labels);
    let prop_refs = |props: &[String]| {
        props
            .iter()
            .map(|property| format!("{variable}.{}", backtick(property)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let on_list = prop_refs(&detail.properties);
    let with_text = if detail.with_properties.is_empty() {
        String::new()
    } else {
        format!(" WITH [{}]", prop_refs(&detail.with_properties))
    };
    let options_text = render_index_options(stored_options)?;
    // FULLTEXT is the one kind whose CREATE form takes the `ON EACH [ … ]`
    // list (CIP-38); every other property kind uses `ON ( … )`.
    let on_clause = if matches!(kind, IndexKindSpec::Fulltext) {
        format!(" ON EACH [{on_list}]")
    } else {
        format!(" ON ({on_list})")
    };
    Some(format!(
        "CREATE {} INDEX {name} FOR {pattern}{on_clause}{with_text}{options_text}",
        kind.display_name(),
    ))
}

/// The one spelling of an index's stored-options join (ADR-0481 §3:
/// the SHOW row and the DDL mirror must resolve it identically) — the
/// named entry, or the empty map when the index carries none. Borrowed,
/// never cloned: the empty default is a local, not allocated per row.
fn stored_options<'a>(
    index_options: &'a BTreeMap<String, BTreeMap<String, Value>>,
    name: &str,
) -> &'a BTreeMap<String, Value> {
    static EMPTY: BTreeMap<String, Value> = BTreeMap::new();
    index_options.get(name).unwrap_or(&EMPTY)
}

/// The entity kind and `createStatement` for one index — the one binding
/// the `SHOW INDEXES` row's columns and the ADR-0481 `schema.cypher`
/// mirror share: both callers resolve the row's [`IndexFacts`] once and
/// render through the one [`index_create_statement`] renderer (one
/// renderer, two callers — never a second copy). `None` is the SHOW
/// column's NULL.
fn index_statement_binding(
    facts: &IndexFacts,
    detail: Option<&IndexDetailSpec>,
    name: &str,
    stored_options: &BTreeMap<String, Value>,
) -> (EntityKind, Option<String>) {
    let entity = EntityKind::from_relationship_flag(facts.relationship);
    let statement = detail.and_then(|d| index_create_statement(d, name, entity, stored_options));
    (entity, statement)
}

/// The index's CREATE statement for the checkpoint's schema mirror
/// (ADR-0481 §3), resolved from the committed SHOW metadata exactly as
/// the SHOW row resolves it — a schema mirror must recreate, not hide, so
/// the DDL renders the dialect's full form. `None` is the SHOW column's
/// NULL (a legacy detail-less catalog row, or a form with no faithful
/// CREATE rendering); the mirror omits the entry rather than fabricate a
/// statement.
pub(super) fn index_ddl_statement(
    idx: &relate_catalog::IndexDef,
    details: &BTreeMap<String, IndexDetailSpec>,
    index_options: &BTreeMap<String, BTreeMap<String, Value>>,
    constraints: &[relate_catalog::ConstraintDef],
) -> Option<String> {
    // A constraint's backing index has no standalone CREATE form: the
    // constraint's own statement recreates it (ADR-0339), so emitting both
    // would run the same CREATE CONSTRAINT twice. The suppression lives
    // here, where the backing join is already resolved, so callers hold no
    // second copy of the predicate.
    if backing_constraint_by_name(&idx.name, constraints).is_some() {
        return None;
    }
    let detail = details.get(&idx.name);
    let facts = IndexFacts::resolve(idx, detail, None);
    index_statement_binding(
        &facts,
        detail,
        &idx.name,
        stored_options(index_options, &idx.name),
    )
    .1
}

/// One `SHOW INDEXES` row: the `labelsOrTypes` and `properties` lists and
/// the typed kind resolved once through [`IndexFacts`], and the Neo4j CE
/// column set assembled over them. `backing` is the row's index-backed
/// constraint of the same name, resolved once by the caller. The
/// 16-column vocabulary and its order are `relate-front`'s
/// `show_columns::DEFAULT_INDEX_COLUMNS` ++
/// `NON_DEFAULT_INDEX_COLUMNS` — the pinning test at the bottom of this
/// module fails the moment the pair list drifts from that table.
fn index_row(
    idx: relate_catalog::IndexDef,
    detail: Option<&IndexDetailSpec>,
    facts: &IndexFacts,
    backing: Option<&relate_catalog::ConstraintDef>,
    committed: bool,
    ctx: IndexRowContext<'_>,
) -> Row {
    let options = stored_options(ctx.index_options, &idx.name);
    let (entity, index_statement) = index_statement_binding(facts, detail, &idx.name, options);
    // A constraint's backing index shows the owning constraint's CREATE
    // CONSTRAINT (Neo4j parity); every standalone index shows its own.
    let create_statement = match backing {
        Some(def) => super::constraints::constraint_statement_for(def, ctx.current, ctx.derived),
        None => index_statement,
    };
    // The options column always carries an `indexConfig` entry in Neo4j's
    // shape; the stored map omits it for kinds that carry no config. The
    // createStatement renderer still decides the OPTIONS suffix from the
    // stored map (an empty config renders no suffix).
    let options_value = crate::exec::catalog::options_cell(options);
    let kind = facts.kind;
    let mut properties = facts.properties.clone();
    properties.extend(facts.with_properties.clone());
    let owning = optional_string_cell(backing.map(|def| def.name.as_str()));
    let (labels_value, properties_value) = if kind.is_lookup() {
        (Value::Null, Value::Null)
    } else {
        (string_list(facts.labels.clone()), string_list(properties))
    };
    let create_statement = optional_string_cell(create_statement.as_deref());
    catalog_row([
        ("id", crate::exec::catalog::schema_id_cell(committed)),
        ("name", Value::String(idx.name.as_str().into())),
        ("state", Value::String("ONLINE".into())),
        ("populationPercent", Value::Float(100.0)),
        ("type", Value::String(kind.display_name().into())),
        ("entityType", Value::String(entity.display_name().into())),
        ("labelsOrTypes", labels_value),
        ("properties", properties_value),
        ("indexProvider", Value::String(kind.provider_name().into())),
        ("owningConstraint", owning),
        ("lastRead", Value::Null),
        ("readCount", Value::Int(0)),
        (
            "trackedSince",
            Value::Temporal(relate_core::Temporal::DateTime {
                utc: chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1),
                offset: 0,
                zone: None,
            }),
        ),
        ("options", options_value),
        ("failureMessage", Value::String("".into())),
        ("createStatement", create_statement),
    ])
}

/// The shared per-row inputs of [`index_row`]: the stored options map, the
/// current graph type (for a backing constraint's classification), and its
/// derived records — bundled so the row producer stays within the
/// argument-count budget.
#[derive(Clone, Copy)]
struct IndexRowContext<'a> {
    index_options: &'a BTreeMap<String, BTreeMap<String, Value>>,
    current: &'a relate_parser::GraphTypeBody,
    derived: &'a BTreeMap<String, relate_model::Constraint>,
}

/// The planner-IR `Clause::ShowIndexes` result (a fixed four-column
/// name/label/property/type table over the named indexes), kept beside the
/// parser-clause `SHOW INDEXES` row producer above. The `type` cell reads
/// the one [`IndexFacts`] resolution — the same binding the parser-clause
/// row producer reads, never a second hard-coded spelling.
/// The rows come in [`ExecGraph::named_indexes`]'s order — name order on
/// both its paths (the committed catalog's name-keyed map, the overlay
/// fold's name-keyed map) — so no second ordering spelling here.
pub(crate) fn show_indexes_result(graph: &ExecGraph<'_>) -> Result<QueryResult> {
    let details = &graph.show_schema()?.data.index_details;
    let rows = graph
        .named_indexes()
        .into_iter()
        .map(|d| {
            let kind = IndexFacts::resolve(&d, details.get(&d.name), None).kind;
            vec![
                Value::String(d.name.into()),
                Value::String(d.label.into()),
                Value::String(d.property.into()),
                Value::String(kind.display_name().into()),
            ]
        })
        .collect();
    Ok(QueryResult::intermediate(
        vec![
            "name".into(),
            "label".into(),
            "property".into(),
            "type".into(),
        ],
        rows,
        graph.stats(),
    ))
}

/// The two lookup indexes every store carries without a DDL statement:
/// the engine-generated, stable names live Neo4j 2026.10.1 reports
/// (`SHOW INDEXES` on a fresh database), relationship first, node second.
/// They ride no stored catalog — SHOW synthesizes their rows, so this
/// table is their one definition.
///
/// shortcut: a synthesized default cannot be `DROP`ped (there is no
/// catalog row to remove); seed them into the catalog if a surface ever
/// needs `DROP INDEX index_460996c0` without `IF EXISTS`.
const DEFAULT_LOOKUP_INDEXES: [(&str, bool); 2] =
    [("index_1b9dcc97", true), ("index_460996c0", false)];

/// One default lookup index's `SHOW INDEXES` row, built through the same
/// [`index_row`] as a stored index: a LOOKUP index carries no
/// label/property tokens, so its detail is the entity kind alone.
fn default_lookup_row(name: &str, relationship: bool, ctx: IndexRowContext<'_>) -> Row {
    let detail = IndexDetailSpec {
        kind: IndexKindSpec::Lookup,
        relationship,
        labels: Vec::new(),
        properties: Vec::new(),
        with_properties: Vec::new(),
    };
    let idx = relate_catalog::IndexDef {
        name: name.to_string(),
        label: String::new(),
        property: String::new(),
    };
    let facts = IndexFacts::resolve(&idx, Some(&detail), None);
    index_row(idx, Some(&detail), &facts, None, true, ctx)
}

pub(crate) fn show_indexes_rows(
    graph: &ExecGraph<'_>,
    c: &relate_parser::ShowIndexesClause,
) -> Result<Vec<Row>> {
    let show = graph.show_schema()?;
    let details = &show.data.index_details;
    let current = show.data.current_graph_type.to_body();
    let ctx = IndexRowContext {
        index_options: &show.data.index_options,
        current: &current,
        derived: &show.data.derived_constraints,
    };
    let constraints = graph.named_constraints();
    // One kind filter, one predicate, both streams: the defaults are
    // LOOKUP rows, so a lookup-only SHOW renders exactly the two rows
    // live does — and the defaults lead by construction.
    let wanted = c.kind_filter.map(IndexKindSpec::from);
    let passes = |kind: IndexKindSpec| wanted.is_none_or(|w| w == kind);
    let default_rows = DEFAULT_LOOKUP_INDEXES
        .iter()
        .filter(|_| passes(IndexKindSpec::Lookup))
        .map(|&(name, relationship)| default_lookup_row(name, relationship, ctx));
    let named_rows = graph
        .named_indexes()
        .into_iter()
        .filter_map(|idx| {
            // Resolve the row's facts once, through the one [`IndexFacts`]
            // precedence; the kind filter and the row's columns read from
            // this single binding (the createStatement renderer reads its
            // own `detail.kind`, equal by construction).
            let detail = details.get(&idx.name);
            let backing = backing_constraint_by_name(&idx.name, &constraints);
            let facts = IndexFacts::resolve(&idx, detail, backing);
            passes(facts.kind).then_some((idx, detail, facts, backing))
        })
        .map(|(idx, detail, facts, backing)| {
            // The committed catalog alone decides the schema id: a rule the
            // open transaction created is uncommitted (NULL), exactly as the
            // constraint row reports its twin.
            let committed = graph.base().named_index(&idx.name).is_some();
            index_row(idx, detail, &facts, backing, committed, ctx)
        });
    let rows: Vec<Row> = default_rows.chain(named_rows).collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use crate::exec::catalog_show::indexes::scalar_option_text;
    use crate::exec::test_support::{
        assert_row_keys_match, column, exec_autocommit, exec_cypher, string_cell,
    };
    use crate::graph::Graph;
    use crate::redo::IndexKindSpec;

    /// A stored string option value with an embedded single quote renders
    /// through the canonical Cypher literal (quote escaped), so the quoted
    /// `createStatement` text stays valid Cypher rather than truncating at
    /// the first quote — the value is user-settable via `OPTIONS $cfg`.
    #[test]
    fn a_string_option_value_renders_escaped() {
        assert_eq!(
            scalar_option_text(&relate_core::Value::String("a'b".into())),
            Some(r"'a\'b'".to_string())
        );
    }

    /// Pins the load-bearing YIELD-* invariant: `index_row` inserts keys
    /// in exactly `relate_front::show_columns::full_index_columns()`
    /// order — the front crate's own composition of its DEFAULT ++
    /// NON_DEFAULT tables; the shared key-order pin
    /// (`assert_row_keys_match`) carries the invariant.
    #[test]
    fn index_row_keys_match_front_column_tables() {
        assert_row_keys_match(
            Some("CREATE INDEX person_name FOR (n:Person) ON (n.name)"),
            "SHOW INDEXES YIELD *",
            &relate_front::show_columns::full_index_columns(),
        );
    }

    /// The `createStatement` cells of one kind-filtered SHOW (the default
    /// lookup rows every store carries are not part of these fixtures).
    fn create_statements(g: &Graph, show: &str) -> Vec<String> {
        column(&exec_cypher(g, show), "createStatement", string_cell)
    }

    /// A constraint's backing index shows the owning constraint's CREATE
    /// CONSTRAINT (live Neo4j 2026.09), never a standalone CREATE RANGE
    /// INDEX.
    #[test]
    fn backing_index_create_statement_is_the_owning_constraint() {
        let g = Graph::default();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT c1 FOR (n:Label) REQUIRE (n.prop) IS UNIQUE",
        )
        .unwrap();
        assert_eq!(
            create_statements(&g, "SHOW RANGE INDEXES YIELD createStatement"),
            vec!["CREATE CONSTRAINT `c1` FOR (n:`Label`) REQUIRE (n.`prop`) IS UNIQUE"]
        );
    }

    /// FULLTEXT's CREATE form is `ON EACH [ … ]`, and the recorded default
    /// config rides the OPTIONS suffix (live Neo4j 2026.09).
    #[test]
    fn fulltext_create_statement_uses_on_each_and_default_options() {
        let g = Graph::default();
        exec_autocommit(
            &g,
            "CREATE FULLTEXT INDEX f1 FOR (n:Label) ON EACH [n.prop]",
        )
        .unwrap();
        assert_eq!(
            create_statements(&g, "SHOW FULLTEXT INDEXES YIELD createStatement"),
            vec![
                "CREATE FULLTEXT INDEX `f1` FOR (n:`Label`) ON EACH [n.`prop`] \
                 OPTIONS {indexConfig: {`fulltext.analyzer`: 'standard-no-stop-words',\
                 `fulltext.eventually_consistent`: false}}"
            ]
        );
    }

    /// A POINT index records Neo4j's spatial bounds; the createStatement
    /// spells the float lists in Neo4j's `[1000000.0, …]` form.
    #[test]
    fn point_create_statement_carries_the_spatial_bounds() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE POINT INDEX p1 FOR (n:Label) ON (n.prop)").unwrap();
        let statements = create_statements(&g, "SHOW POINT INDEXES YIELD createStatement");
        assert!(
            statements[0].starts_with(
                "CREATE POINT INDEX `p1` FOR (n:`Label`) ON (n.`prop`) OPTIONS {indexConfig: {"
            ),
            "{}",
            statements[0]
        );
        assert!(
            statements[0].contains("`spatial.cartesian-3d.max`: [1000000.0, 1000000.0, 1000000.0]"),
            "{}",
            statements[0]
        );
        assert!(
            statements[0].contains("`spatial.wgs-84.min`: [-180.0, -90.0]"),
            "{}",
            statements[0]
        );
    }

    /// The `options` column always carries `indexConfig`, even for a kind
    /// that stores no config (Neo4j renders `{indexConfig: {}}`).
    #[test]
    fn options_column_carries_the_empty_index_config() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX r1 FOR (n:Label) ON (n.prop)").unwrap();
        let options = column(
            &exec_cypher(&g, "SHOW INDEXES YIELD options"),
            "options",
            |value| match value {
                relate_core::Value::Map(map) => Some(map.clone()),
                _ => None,
            },
        );
        assert!(
            options[0]
                .get(crate::exec::ddl_options::KEY_INDEX_CONFIG)
                .is_some(),
            "{:?}",
            options[0]
        );
    }

    /// A rule created inside an open transaction is not committed yet, so
    /// `SHOW INDEXES` reports a NULL schema id — the same answer
    /// `SHOW CONSTRAINTS` gives for its twin. A committed index keeps the
    /// engine's stable placeholder.
    #[test]
    fn index_schema_id_mirrors_the_constraint_probe() {
        let g = Graph::default();
        let scratch = crate::exec::test_support::write_scratch(&g);
        crate::exec::test_support::exec_write(
            &g,
            &scratch,
            "CREATE INDEX i1 FOR (n:Label) ON (n.prop)",
        )
        .unwrap();
        let uncommitted = crate::exec::test_support::exec_write(
            &g,
            &scratch,
            "SHOW INDEXES YIELD name, id WHERE name = 'i1' RETURN id",
        )
        .unwrap();
        assert_eq!(
            column(&uncommitted, "id", |value| Some(value.clone())),
            vec![relate_core::Value::Null]
        );
        crate::exec::test_support::commit_scratch(&g, scratch);
        let committed = exec_cypher(
            &g,
            "SHOW INDEXES YIELD name, id WHERE name = 'i1' RETURN id",
        );
        assert_eq!(
            column(&committed, "id", |value| Some(value.clone())),
            vec![relate_core::Value::Int(0)]
        );
    }

    /// The provider names are Neo4j 2026.09's own (`fulltext-2.0`,
    /// `vector-2026.08`); the rest predate the pin.
    #[test]
    fn provider_names_match_live_neo4j() {
        assert_eq!(IndexKindSpec::Fulltext.provider_name(), "fulltext-2.0");
        assert_eq!(IndexKindSpec::Vector.provider_name(), "vector-2026.08");
        assert_eq!(IndexKindSpec::Range.provider_name(), "range-1.0");
        assert_eq!(IndexKindSpec::Point.provider_name(), "point-1.0");
        assert_eq!(IndexKindSpec::Text.provider_name(), "text-3.0");
        assert_eq!(IndexKindSpec::Lookup.provider_name(), "token-lookup-1.0");
    }
}

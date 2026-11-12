//! Schema-DDL query notifications: Neo4j's `00NA0`-`00NA2` family.
//!
//! The notifications ride the plan tree's statement-scoped list (ADR-0363
//! decision 3), so a plain DDL execution still reports none; `EXPLAIN`
//! derives them from the parsed clause and the catalog state. Nothing is
//! executed here — each detector consumes the verdict of the execution gate
//! it describes (the index create gate and the constraint create gate both
//! hand the rule that made the statement a no-op out), so an `EXPLAIN` of a
//! no-op `IF NOT EXISTS` statement reports exactly the note its execution
//! would, and a statement the gate rejects raises none.
//!
//! The descriptions are Neo4j's own GQLSTATUS text. They are advisory (the
//! spec harness enforces the code), so the renderers below spell the
//! notification form — variable `e`, unquoted simple names — rather than
//! reusing the SHOW `createStatement` renderer, whose backticked, `n`/`r`
//! form is a different contract.
//!
//! The one detector that needs a catalog snapshot (the `IF NOT EXISTS`
//! `CREATE INDEX` note) takes it once, and only when it will use it: a
//! failed read is the statement's own error, never a silent "the rule does
//! not exist". The constraint note reads no snapshot of its own — it
//! consumes the execution gate's verdict.

use std::collections::BTreeMap;
use std::str::FromStr;

use relate_catalog::ConstraintDef;
use relate_core::{Result, Value};
use relate_model::wire::{Notification, NotificationKind};
use relate_model::{ConstraintType, EntityKind};
use relate_names::SmallName;
use relate_parser::{OptionsForm, SchemaName};

use crate::exec::catalog_schema::{constraint_create_gate, constraint_def};
use crate::exec::ddl::{
    default_index_name, drop_target_exists, index_conflict_gate, ExistingRule, Gate, IndexFacts,
    SchemaTarget,
};
use crate::graph_type::standalone_verb;
use crate::overlay::ExecGraph;
use crate::redo::{IndexDetailSpec, IndexFacts as IndexRowFacts, IndexKindSpec};

use super::catalog::schema_name_value;

/// The schema-DDL notifications one IR clause raises at `EXPLAIN` time: the
/// catalog passthrough's parsed clause and the IR's own compact legacy
/// `CreateIndex`/`DropIndex` shapes all dispatch here, so the statement
/// walker never re-assembles a clause shape across the crate boundary. A
/// non-DDL clause raises none.
pub fn ddl_notifications(
    graph: &ExecGraph<'_>,
    clause: &relate_ir::Clause,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Notification>> {
    match clause {
        relate_ir::Clause::Catalog(command) => catalog_clause_notifications(graph, command, params),
        relate_ir::Clause::CreateIndex {
            name,
            if_not_exists,
            label,
            property,
        } => {
            // Text renders: the notification layer owns String text.
            let name_text = name.as_ref().map(|n| n.to_string());
            legacy_create_index_notifications(
                graph,
                name_text.as_deref(),
                *if_not_exists,
                label,
                property,
            )
        }
        relate_ir::Clause::DropIndex { if_exists, target } => {
            Ok(legacy_drop_index_notifications(graph, *if_exists, target))
        }
        _ => Ok(Vec::new()),
    }
}

/// One parsed catalog clause's notes. A non-DDL catalog clause (a `SHOW`, an
/// `ALTER DATABASE`) raises none.
fn catalog_clause_notifications(
    graph: &ExecGraph<'_>,
    clause: &relate_parser::Clause,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Notification>> {
    match clause {
        relate_parser::Clause::CreateIndex(c) => match clause_name(c.name.as_ref(), params) {
            ClauseName::Unresolvable => Ok(Vec::new()),
            ClauseName::Unnamed => create_index_notifications(graph, c, None),
            ClauseName::Named(name) => create_index_notifications(graph, c, Some(&name)),
        },
        relate_parser::Clause::CreateConstraint(c) => match clause_name(c.name.as_ref(), params) {
            ClauseName::Unresolvable => Ok(Vec::new()),
            ClauseName::Unnamed => create_constraint_notifications(graph, c, None, params),
            ClauseName::Named(name) => {
                create_constraint_notifications(graph, c, Some(&name), params)
            }
        },
        relate_parser::Clause::DropIndex(c) => match clause_name(Some(&c.name), params) {
            ClauseName::Named(name) => Ok(drop_notifications(
                graph,
                DropNoun::Index,
                &name,
                c.if_exists,
            )),
            // `DROP` always names its target, so `Unnamed` cannot occur; the
            // unresolvable-parameter policy is no notes.
            ClauseName::Unnamed | ClauseName::Unresolvable => Ok(Vec::new()),
        },
        relate_parser::Clause::DropConstraint(c) => match clause_name(Some(&c.name), params) {
            ClauseName::Named(name) => Ok(drop_notifications(
                graph,
                DropNoun::Constraint,
                &name,
                c.if_exists,
            )),
            ClauseName::Unnamed | ClauseName::Unresolvable => Ok(Vec::new()),
        },
        _ => Ok(Vec::new()),
    }
}

/// A schema clause's declared name, resolved once for every detector. The
/// `Unresolvable` state's one policy is **no notes**: the note is advisory,
/// and the statement itself fails with `ParameterMissing` at execution — an
/// `EXPLAIN` must never describe a command that cannot run.
enum ClauseName {
    /// The clause names none; the engine generates one (`CREATE` only).
    Unnamed,
    /// The declared name, from a literal or a resolved parameter.
    Named(String),
    /// A parameter name did not resolve.
    Unresolvable,
}

fn clause_name(name: Option<&SchemaName>, params: &BTreeMap<String, Value>) -> ClauseName {
    match name {
        None => ClauseName::Unnamed,
        Some(name) => match schema_name_value(name, params) {
            Ok(resolved) => ClauseName::Named(resolved),
            Err(_) => ClauseName::Unresolvable,
        },
    }
}

/// The IR's compact `CreateIndex` clause (the single-label, single-property,
/// option-less RANGE shape the front-end keeps in the IR): the same `00NA0`
/// note as the catalog passthrough's rich form, through the one shared tail.
fn legacy_create_index_notifications(
    graph: &ExecGraph<'_>,
    name: Option<&str>,
    if_not_exists: bool,
    label: &SmallName,
    property: &SmallName,
) -> Result<Vec<Notification>> {
    if !if_not_exists {
        return Ok(Vec::new());
    }
    // Text renders: the notification layer owns String text.
    let resolved = name
        .map(str::to_string)
        .unwrap_or_else(|| default_index_name(&label.to_string(), &property.to_string()));
    let facts = IndexFacts::legacy_range(
        resolved,
        label.to_string(),
        property.to_string(),
        if_not_exists,
    );
    already_exists_index_note(graph, &facts, name, None)
}

/// The legacy IR `DropIndex` clause: the `00NA1` note when the target is
/// missing under `IF EXISTS`, through the one DROP-target existence rule.
fn legacy_drop_index_notifications(
    graph: &ExecGraph<'_>,
    if_exists: bool,
    target: &relate_ir::DropIndexTarget,
) -> Vec<Notification> {
    if !if_exists {
        return Vec::new();
    }
    let (exists, command, name) = match target {
        relate_ir::DropIndexTarget::Name(name) => (
            drop_target_exists(graph, SchemaTarget::IndexName(name)).is_some(),
            format!("DROP INDEX {name} IF EXISTS"),
            Some(name.as_str()),
        ),
        relate_ir::DropIndexTarget::Schema { label, property } => {
            // Text renders: the stored-index lookup and the notification
            // text both speak String.
            let label = label.to_string();
            let property = property.to_string();
            (
                drop_target_exists(
                    graph,
                    SchemaTarget::IndexSchema {
                        label: &label,
                        property: &property,
                    },
                )
                .is_some(),
                format!("DROP INDEX ON :{label}({property}) IF EXISTS"),
                None,
            )
        }
    };
    if exists {
        return Vec::new();
    }
    vec![does_not_exist_note(&command, name)]
}

/// The noun a `DROP … IF EXISTS` note names: both kinds share the note text.
#[derive(Clone, Copy)]
enum DropNoun {
    Index,
    Constraint,
}

impl DropNoun {
    fn noun(self) -> &'static str {
        match self {
            DropNoun::Index => "INDEX",
            DropNoun::Constraint => "CONSTRAINT",
        }
    }
}

/// `DROP INDEX/CONSTRAINT … IF EXISTS` on a missing name is a no-op with a
/// `00NA1` note. A parameter name resolves from the statement's parameters
/// exactly as the execution gate resolves it.
fn drop_notifications(
    graph: &ExecGraph<'_>,
    noun: DropNoun,
    name: &str,
    if_exists: bool,
) -> Vec<Notification> {
    if !if_exists {
        return Vec::new();
    }
    let target = match noun {
        DropNoun::Index => SchemaTarget::IndexName(name),
        DropNoun::Constraint => SchemaTarget::ConstraintName(name),
    };
    if drop_target_exists(graph, target).is_some() {
        return Vec::new();
    }
    vec![does_not_exist_note(
        &format!("DROP {} {name} IF EXISTS", noun.noun()),
        Some(name),
    )]
}

/// The `CREATE INDEX` notes: the `IF NOT EXISTS` no-op (`00NA0`) and the
/// missing-dimensions note (`00NA2`, which fires whether or not the index
/// already exists).
fn create_index_notifications(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateIndexClause,
    name: Option<&str>,
) -> Result<Vec<Notification>> {
    let mut notifications = Vec::new();
    if c.kind == relate_parser::IndexKind::Vector && !vector_dimensions_given(c) {
        notifications.push(Notification::new(
            NotificationKind::SchemaVectorIndexDimensionsNotSpecified,
            "note: successful completion - vector index dimensions not specified. \
             When creating a vector index, `vector.dimensions` should be specified. \
             Omitting it is allowed, but specifying dimensions ensures that only \
             vectors of that size are indexed and makes dimension mismatches fail \
             clearly at query time. For example, set `OPTIONS { indexConfig: { \
             `vector.dimensions`: 1536 } }` when creating the index.",
        ));
    }
    if !c.if_not_exists {
        return Ok(notifications);
    }
    let facts = IndexFacts::from_clause(
        c,
        name.map(str::to_string)
            .unwrap_or_else(|| IndexFacts::generated_name(c)),
    );
    notifications.extend(already_exists_index_note(
        graph,
        &facts,
        name,
        literal_options_text(c.options.literal()),
    )?);
    Ok(notifications)
}

/// The `00NA0` note for an `IF NOT EXISTS` create the gate made a no-op —
/// the one tail both the rich catalog clause and the IR's compact legacy
/// clause share. The note reads the create gate's own evidence (the same
/// rule that made the statement a no-op names it), so the two cannot
/// disagree, and the effective name resolves inside the gate.
fn already_exists_index_note(
    graph: &ExecGraph<'_>,
    facts: &IndexFacts,
    declared_name: Option<&str>,
    options: Option<String>,
) -> Result<Vec<Notification>> {
    // The gate's error is the statement's own (a failed catalog read, a
    // rejected name collision): `EXPLAIN` fails with it rather than
    // reporting a note for a statement that cannot run. Only a skip carries
    // evidence for a note.
    let gate = index_conflict_gate(graph, facts)?;
    let Gate::Skip(ExistingRule::Index(rule)) = gate else {
        return Ok(Vec::new());
    };
    let Some(descriptor) = index_descriptor_text(&rule.index, rule.detail.as_ref()) else {
        return Ok(Vec::new());
    };
    let Some(tail) = index_descriptor_tail(
        IndexKindSpec::from(facts.kind),
        EntityKind::from_relationship_flag(facts.relationship),
        &facts.labels,
        &facts.properties,
        &facts.with_properties,
    ) else {
        return Ok(Vec::new());
    };
    let command = create_index_command(
        IndexKindSpec::from(facts.kind),
        declared_name,
        facts.if_not_exists,
        &tail,
        options,
    );
    Ok(vec![already_exists_note(&command, &descriptor)])
}

/// Whether the clause's literal `OPTIONS` carries `vector.dimensions`. An
/// `OPTIONS $param` map is unknown at plan time, so it counts as given
/// (no note) — the executed statement still records the real config.
fn vector_dimensions_given(c: &relate_parser::CreateIndexClause) -> bool {
    match &c.options {
        OptionsForm::Absent => false,
        // Unknown at plan time; the executed statement records the real
        // config, so it counts as given (no note).
        OptionsForm::Parameter(_) => true,
        OptionsForm::Literal(entries) => entries
            .iter()
            .filter(|(key, _)| key == super::ddl_options::KEY_INDEX_CONFIG)
            .any(|(_, value)| {
                let relate_parser::Expr::Map(pairs) = value else {
                    return false;
                };
                pairs.iter().any(|pair| pair.key == "vector.dimensions")
            }),
    }
}

/// One existing index's descriptor in the notification's own spelling,
/// resolved through the one [`IndexFacts`] precedence — so a detail-less row
/// is classified exactly like its SHOW row. `None` when the index's shape
/// has no faithful rendering (a property-less non-LOOKUP index fails closed
/// rather than fabricate a LOOKUP statement).
fn index_descriptor_text(
    index: &relate_catalog::IndexDef,
    detail: Option<&IndexDetailSpec>,
) -> Option<String> {
    let facts = IndexRowFacts::resolve(index, detail, None);
    let tail = index_descriptor_tail(
        facts.kind,
        EntityKind::from_relationship_flag(facts.relationship),
        &facts.labels,
        &facts.properties,
        &facts.with_properties,
    )?;
    Some(format!(
        "{} INDEX {} {tail}",
        facts.kind.display_name(),
        index.name
    ))
}

/// The `FOR …` tail shared by a `CREATE … INDEX` command and an existing
/// index's descriptor. `kind` and `entity` are the typed facts (a LOOKUP
/// index is `is_lookup`, never "property list empty"; the entity kind picks
/// the node/relationship pattern), and a property-less non-LOOKUP index
/// returns `None`: it has no faithful `ON` clause.
fn index_descriptor_tail(
    kind: IndexKindSpec,
    entity: EntityKind,
    labels: &[String],
    properties: &[String],
    with_properties: &[String],
) -> Option<String> {
    if kind.is_lookup() {
        let (pattern, each) = match entity {
            EntityKind::Node => ("(e)", "labels(e)"),
            EntityKind::Relationship => ("()-[e]-()", "type(e)"),
        };
        return Some(format!("FOR {pattern} ON EACH {each}"));
    }
    if properties.is_empty() {
        return None;
    }
    let tokens = labels.join("|");
    let pattern = match entity {
        EntityKind::Node if tokens.is_empty() => "(e)".to_string(),
        EntityKind::Node => format!("(e:{tokens})"),
        EntityKind::Relationship if tokens.is_empty() => "()-[e]-()".to_string(),
        EntityKind::Relationship => format!("()-[e:{tokens}]-()"),
    };
    let refs = properties
        .iter()
        .map(|property| format!("e.{property}"))
        .collect::<Vec<_>>();
    let mut text = format!("FOR {pattern} ON ({})", refs.join(", "));
    if !with_properties.is_empty() {
        let with = with_properties
            .iter()
            .map(|property| format!("e.{property}"))
            .collect::<Vec<_>>();
        text.push_str(&format!(" WITH [{}]", with.join(", ")));
    }
    Some(text)
}

/// The `CREATE … INDEX …` command text the note quotes: the kind and name,
/// the `IF NOT EXISTS` marker exactly where the source statement writes it,
/// the shared `FOR …` tail, and any literal `OPTIONS` suffix.
fn create_index_command(
    kind: IndexKindSpec,
    name: Option<&str>,
    if_not_exists: bool,
    tail: &str,
    options: Option<String>,
) -> String {
    let mut command = format!("CREATE {} INDEX ", kind.display_name());
    if let Some(name) = name {
        command.push_str(name);
        command.push(' ');
    }
    if if_not_exists {
        command.push_str("IF NOT EXISTS ");
    }
    command.push_str(tail);
    if let Some(options) = options {
        command.push_str(&options);
    }
    command
}

/// The literal `OPTIONS {…}` suffix in the statement's own spelling. `None`
/// for a parameter map (unknown at plan time) or an empty map.
fn literal_options_text(options: Option<&[(String, relate_parser::Expr)]>) -> Option<String> {
    let options = options?;
    if options.is_empty() {
        return None;
    }
    let mut entries = Vec::with_capacity(options.len());
    for (key, value) in options {
        let relate_parser::Expr::Map(pairs) = value else {
            return None;
        };
        let inner = pairs
            .iter()
            .map(|pair| {
                simple_expr_text(&pair.value).map(|text| format!("`{}`: {}", pair.key, text))
            })
            .collect::<Option<Vec<_>>>()?;
        entries.push(format!("`{key}`: {{{}}}", inner.join(", ")));
    }
    Some(format!(" OPTIONS {{{}}}", entries.join(", ")))
}

/// A scalar expression's literal text; `None` for anything that is not a
/// literal at plan time.
fn simple_expr_text(expr: &relate_parser::Expr) -> Option<String> {
    match expr {
        relate_parser::Expr::Literal(relate_parser::LiteralValue::Integer(value)) => {
            Some(value.to_string())
        }
        relate_parser::Expr::Literal(relate_parser::LiteralValue::Float(value)) => {
            // The one float spelling (`Value::to_cypher_literal` → core's
            // canonical `float_literal`), never a second `format!`.
            Some(relate_core::Value::Float(*value).to_cypher_literal())
        }
        relate_parser::Expr::Literal(relate_parser::LiteralValue::Bool(value)) => {
            Some(value.to_string())
        }
        // The one scalar spelling (`Value::to_cypher_literal`): a string's
        // quotes are escaped, so the quoted command text stays valid Cypher
        // whatever the user's OPTIONS literal holds.
        relate_parser::Expr::Literal(relate_parser::LiteralValue::String(value)) => {
            Some(relate_core::Value::String(value.as_str().into()).to_cypher_literal())
        }
        _ => None,
    }
}

/// The `CREATE CONSTRAINT` note: an `IF NOT EXISTS` create whose name or
/// schema already exists (`00NA0`). The note reads the execution gate's own
/// verdict, so it follows the gate's precedence exactly — including the
/// rejections that make the statement unrunnable (a graph-type-managed
/// label, a conflict), which `EXPLAIN` reports as the statement's own error
/// instead of describing a command that cannot run.
fn create_constraint_notifications(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateConstraintClause,
    name: Option<&str>,
    params: &BTreeMap<String, Value>,
) -> Result<Vec<Notification>> {
    if !c.if_not_exists {
        return Ok(Vec::new());
    }
    // The same definition the execution handler builds (kind tag, property
    // type, name), so the gate sees one shape.
    let (def, constraint_type) = constraint_def(c, name.map(str::to_string))?;
    // The EXPLAIN layer carries no dialect version; the gate's OPTIONS
    // validation can only turn the statement into an error (the statement's
    // own, which propagates), so the version cannot change the skip
    // evidence. Only a skip carries evidence for a note.
    let gate = constraint_create_gate(graph, c, &def, constraint_type, params)?;
    let Gate::Skip(rule) = gate else {
        return Ok(Vec::new());
    };
    let ExistingRule::Constraint(existing) = rule else {
        // A serving-pair index has no constraint descriptor to name.
        return Ok(Vec::new());
    };
    let Some(descriptor) = constraint_descriptor_text(&existing) else {
        return Ok(Vec::new());
    };
    let Some(command) = constraint_command_text(&def, constraint_type, name) else {
        return Ok(Vec::new());
    };
    Ok(vec![already_exists_note(&command, &descriptor)])
}

/// The `FOR <pattern> REQUIRE (<properties>)` tail shared by a
/// `CREATE CONSTRAINT` command and an existing constraint's descriptor — the
/// constraint counterpart of [`index_descriptor_tail`], so the two renderings
/// cannot drift.
fn constraint_tail(entity: EntityKind, label: &str, properties: &[String]) -> String {
    let pattern = match entity {
        EntityKind::Node => format!("(e:{label})"),
        EntityKind::Relationship => format!("()-[e:{label}]-()"),
    };
    if properties.is_empty() {
        return format!("FOR {pattern}");
    }
    let refs = properties
        .iter()
        .map(|property| format!("e.{property}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("FOR {pattern} REQUIRE ({refs})")
}

/// The `CREATE CONSTRAINT …` command text the note quotes, from the same
/// definition the gate saw.
fn constraint_command_text(
    def: &ConstraintDef,
    kind: ConstraintType,
    name: Option<&str>,
) -> Option<String> {
    let verb = standalone_verb(kind, def.property_type.as_deref())?;
    let entity = EntityKind::from_relationship_flag(def.relationship);
    let mut command = String::from("CREATE CONSTRAINT ");
    if let Some(name) = name {
        command.push_str(name);
        command.push(' ');
    }
    command.push_str("IF NOT EXISTS ");
    command.push_str(&constraint_tail(entity, &def.label, &def.properties));
    command.push(' ');
    command.push_str(&verb);
    Some(command)
}

/// An existing constraint's descriptor text (the command text without
/// `CREATE`/`IF NOT EXISTS`), or `None` when the stored kind tag does not
/// parse or has no standalone verb — the caller skips the note rather than
/// emit `REQUIRE ` with nothing after it.
fn constraint_descriptor_text(def: &relate_catalog::ConstraintDef) -> Option<String> {
    let kind = ConstraintType::from_str(&def.kind).ok()?;
    let verb = standalone_verb(kind, def.property_type.as_deref())?;
    let entity = EntityKind::from_relationship_flag(def.relationship);
    Some(format!(
        "CONSTRAINT {} {} {verb}",
        def.name,
        constraint_tail(entity, &def.label, &def.properties)
    ))
}

/// The `00NA0` note: the command and the existing rule's descriptor. One
/// spelling of Neo4j's asserted text, shared by the index and constraint
/// notes.
fn already_exists_note(command: &str, descriptor: &str) -> Notification {
    Notification::new(
        NotificationKind::SchemaIndexOrConstraintAlreadyExists,
        format!(
            "note: successful completion - index or constraint already exists. \
             The command '{command}' has no effect. The index or constraint specified by \
             '{descriptor}' already exists."
        ),
    )
}

/// The `00NA1` note: the command and, when the statement named one, the
/// missing catalog name.
fn does_not_exist_note(command: &str, name: Option<&str>) -> Notification {
    let subject = match name {
        Some(name) => format!("`{name}` "),
        None => String::new(),
    };
    Notification::new(
        NotificationKind::SchemaIndexOrConstraintDoesNotExist,
        format!(
            "note: successful completion - index or constraint does not exist. \
             The command '{command}' has no effect. The specified index or constraint \
             {subject}does not exist."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::exec_autocommit;
    use crate::graph::Graph;

    fn notes(graph: &Graph, query: &str) -> Vec<Notification> {
        let text = query.strip_prefix("EXPLAIN ").unwrap_or(query);
        let statement = relate_parser::parse(text).expect("parses");
        let view = ExecGraph::read(graph);
        statement
            .clauses
            .iter()
            .flat_map(|clause| {
                catalog_clause_notifications(&view, clause, &BTreeMap::new())
                    .expect("notification facts read")
            })
            .collect()
    }

    fn assert_single_note(
        graph: &Graph,
        query: &str,
        kind: NotificationKind,
        code: &str,
    ) -> Notification {
        let found = notes(graph, query);
        assert_eq!(found.len(), 1, "expected one note for `{query}`: {found:?}");
        assert_eq!(found[0].kind(), kind);
        assert_eq!(found[0].gql_status(), code);
        found.into_iter().next().expect("checked non-empty")
    }

    #[test]
    fn duplicate_if_not_exists_index_raises_the_already_exists_note() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX i1 FOR (n:L) ON (n.p)").unwrap();
        assert_single_note(
            &g,
            "EXPLAIN CREATE INDEX i1 IF NOT EXISTS FOR (n:L) ON (n.p)",
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            "00NA0",
        );
    }

    #[test]
    fn equivalent_schema_notifications_name_the_existing_rule() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX i1 FOR (n:L) ON (n.p)").unwrap();
        let note = assert_single_note(
            &g,
            "EXPLAIN CREATE INDEX i2 IF NOT EXISTS FOR (n:L) ON (n.p)",
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            "00NA0",
        );
        assert!(note.description().contains("RANGE INDEX i1"));

        exec_autocommit(&g, "CREATE CONSTRAINT c1 FOR (n:M) REQUIRE (n.p) IS UNIQUE").unwrap();
        let note = assert_single_note(
            &g,
            "EXPLAIN CREATE CONSTRAINT c2 IF NOT EXISTS FOR (n:M) REQUIRE (n.p) IS UNIQUE",
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            "00NA0",
        );
        assert!(note.description().contains("CONSTRAINT c1"));
    }

    #[test]
    fn vector_without_dimensions_raises_the_dimensions_note() {
        let g = Graph::default();
        assert_single_note(
            &g,
            "EXPLAIN CREATE VECTOR INDEX v1 FOR (n:L) ON (n.p)",
            NotificationKind::SchemaVectorIndexDimensionsNotSpecified,
            "00NA2",
        );
    }

    #[test]
    fn drop_if_exists_missing_raises_the_does_not_exist_note() {
        let g = Graph::default();
        assert_single_note(
            &g,
            "EXPLAIN DROP INDEX missing IF EXISTS",
            NotificationKind::SchemaIndexOrConstraintDoesNotExist,
            "00NA1",
        );
    }

    #[test]
    fn plain_create_raises_no_note() {
        let g = Graph::default();
        assert!(notes(&g, "EXPLAIN CREATE INDEX i1 FOR (n:L) ON (n.p)").is_empty());
    }

    /// A parameter-supplied name resolves from the statement's parameters,
    /// exactly as the execution gate resolves it.
    #[test]
    fn parameter_names_resolve_for_the_note() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX i1 FOR (n:L) ON (n.p)").unwrap();
        let statement = relate_parser::parse("CREATE INDEX $name IF NOT EXISTS FOR (n:L) ON (n.p)")
            .expect("parses");
        let view = ExecGraph::read(&g);
        let params = BTreeMap::from([("name".to_string(), Value::String("i1".into()))]);
        let found = statement
            .clauses
            .iter()
            .flat_map(|clause| {
                catalog_clause_notifications(&view, clause, &params)
                    .expect("notification facts read")
            })
            .collect::<Vec<_>>();
        assert_eq!(found[0].gql_status(), "00NA0");
        assert!(
            found[0]
                .description()
                .contains("CREATE RANGE INDEX i1 IF NOT EXISTS"),
            "{}",
            found[0].description()
        );
    }

    /// An unresolvable parameter name yields no notes at all — the one policy
    /// for every schema-DDL site. Before, the schema-equivalence probe still
    /// fired and the command text fabricated a `$parameter` placeholder.
    #[test]
    fn unresolvable_parameter_name_raises_no_note() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX i1 FOR (n:L) ON (n.p)").unwrap();
        assert!(notes(
            &g,
            "EXPLAIN CREATE INDEX $missing IF NOT EXISTS FOR (n:L) ON (n.p)"
        )
        .is_empty());
    }

    /// The create gate's skip and the note's rule are the same evidence: a
    /// name that exists with a *different* schema still names that index.
    #[test]
    fn name_collision_note_names_the_occupied_index() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE INDEX i1 FOR (n:L) ON (n.p)").unwrap();
        let note = assert_single_note(
            &g,
            "EXPLAIN CREATE INDEX i1 IF NOT EXISTS FOR (n:Other) ON (n.q)",
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            "00NA0",
        );
        assert!(
            note.description().contains("RANGE INDEX i1"),
            "{}",
            note.description()
        );
        assert!(
            note.description()
                .contains("CREATE RANGE INDEX i1 IF NOT EXISTS FOR (e:Other) ON (e.q)"),
            "{}",
            note.description()
        );
    }

    /// A constraint kind with no standalone `REQUIRE` verb (label existence)
    /// skips the note instead of emitting `REQUIRE ` with an empty verb.
    #[test]
    fn constraint_without_a_standalone_verb_skips_the_note() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE CONSTRAINT q FOR (n:P) REQUIRE n:Q").unwrap();
        assert!(notes(
            &g,
            "EXPLAIN CREATE CONSTRAINT q2 IF NOT EXISTS FOR (n:P) REQUIRE n:Q"
        )
        .is_empty());
    }

    /// RB2: an unnamed `CREATE INDEX … IF NOT EXISTS` resolves the same
    /// generated name the execution gate does, so an occupied generated name
    /// is reported by the note (before, the name probe was skipped and the
    /// note was silently absent while execution was a no-op).
    #[test]
    fn unnamed_if_not_exists_index_probes_the_generated_name() {
        let g = Graph::default();
        // Create the unnamed index for real, then read the name the engine
        // generated — the EXPLAIN probe must derive the same one.
        exec_autocommit(&g, "CREATE INDEX FOR (n:L) ON (n.p)").unwrap();
        let name = g
            .named_indexes()
            .into_iter()
            .next()
            .expect("the unnamed create landed")
            .name;
        let note = assert_single_note(
            &g,
            "EXPLAIN CREATE INDEX IF NOT EXISTS FOR (n:L) ON (n.p)",
            NotificationKind::SchemaIndexOrConstraintAlreadyExists,
            "00NA0",
        );
        assert!(
            note.description().contains(&format!("RANGE INDEX {name}")),
            "{}",
            note.description()
        );
        // The executed statement is the same no-op.
        exec_autocommit(&g, "CREATE INDEX IF NOT EXISTS FOR (n:L) ON (n.p)").unwrap();
        assert_eq!(g.named_indexes().len(), 1);
    }

    /// RB1: a graph-type-managed label makes the `CREATE CONSTRAINT` gate
    /// reject a *different* dependency-managed schema, so `EXPLAIN` fails
    /// with that same error instead of describing a command that cannot run.
    /// An `IF NOT EXISTS` create whose schema the graph type already derives
    /// is by contrast a no-op (Neo4j's live corpus).
    #[test]
    fn graph_type_managed_constraint_explain_propagates_the_gate_error() {
        let g = Graph::default();
        exec_autocommit(&g, "ALTER CURRENT GRAPH TYPE SET { (:L => {p :: STRING}) }").unwrap();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT x IF NOT EXISTS FOR (n:L) REQUIRE n.p IS :: STRING",
        )
        .expect("an equivalent derived constraint satisfies IF NOT EXISTS");
        let error = match exec_autocommit(
            &g,
            "CREATE CONSTRAINT x IF NOT EXISTS FOR (n:L) REQUIRE n.q IS :: STRING",
        ) {
            Ok(_) => panic!("the gate must reject a dependency-managed constraint"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("IncompatibleGraphTypeDependence"),
            "{error}"
        );
        let statement = relate_parser::parse(
            "CREATE CONSTRAINT x IF NOT EXISTS FOR (n:L) REQUIRE n.q IS :: STRING",
        )
        .expect("parses");
        let clause = statement.clauses.first().expect("one clause");
        let view = ExecGraph::read(&g);
        let error = catalog_clause_notifications(&view, clause, &BTreeMap::new())
            .expect_err("the gate's rejection propagates out of the note derivation");
        assert!(
            error
                .to_string()
                .contains("IncompatibleGraphTypeDependence"),
            "{error}"
        );
    }

    /// B3: a property-less non-LOOKUP index has no faithful `ON` clause, so
    /// the tail fails closed instead of fabricating a LOOKUP statement.
    #[test]
    fn property_less_non_lookup_index_has_no_descriptor() {
        assert_eq!(
            index_descriptor_tail(IndexKindSpec::Point, EntityKind::Node, &[], &[], &[]),
            None
        );
        assert_eq!(
            index_descriptor_tail(IndexKindSpec::Lookup, EntityKind::Node, &[], &[], &[]),
            Some("FOR (e) ON EACH labels(e)".to_string())
        );
    }
}

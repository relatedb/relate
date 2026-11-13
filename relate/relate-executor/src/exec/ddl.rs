//! Index and constraint DDL (`CREATE`/`DROP INDEX`/`CONSTRAINT`).

use std::collections::BTreeMap;

use relate_catalog::{rule_disposition, ConstraintDef, RuleDisposition};
use relate_core::{Error, Result};
use relate_ir::DropIndexTarget;
use relate_names::SmallName;
use relate_parser::IndexKind;

use crate::overlay::ExecGraph;
use crate::redo::{backing_constraint_by_name, IndexDetailSpec, IndexKindSpec, RedoOp};

pub(crate) fn default_index_name(label: &str, property: &str) -> String {
    format!("{label}_{property}")
}

/// Append one generated-name list segment: the list's length, then each
/// entry as its byte length followed by its text. Unlike a plain `_` join,
/// this is injective — `[A, B]` and `[A_B]` cannot encode the same string —
/// and prefix-decodable, which is what makes
/// [`IndexFacts::generated_name`] a faithful rendering of the schema
/// identity.
fn push_encoded_list(name: &mut String, parts: &[String]) {
    name.push('_');
    name.push_str(&parts.len().to_string());
    for part in parts {
        name.push('_');
        name.push_str(&part.len().to_string());
        name.push('_');
        name.push_str(part);
    }
}

/// Create the range backing index of an index-backed (`unique`/`key`)
/// constraint: the index shares the constraint's name and label and
/// encodes the first constrained property with the shared legacy encoder.
/// The one place this backing-index rule lives. The recorded detail
/// carries **every** constrained property — SHOW INDEXES reports the
/// composite backing index with the constraint's full property list
/// (CIP-11/46/62), and `covered_pairs` materializes a range pair for
/// each, so an equality seek on any constrained property is served. Only
/// the legacy `CreateIndex` redo stays first-property-only (the
/// single-property legacy encoding).
///
/// `relationship` is the constraint's entity kind: a relationship
/// constraint's backing detail records the flag truthfully (SHOW INDEXES
/// reports it as a relationship index), and `covered_pairs` — the
/// postings machinery being node-only today — yields no pairs for it, so
/// nothing is materialized, exactly as before this fix.
pub(crate) fn create_constraint_backing_index(
    graph: &ExecGraph<'_>,
    name: &str,
    relationship: bool,
    label: &str,
    properties: &[String],
) -> Result<()> {
    // Only index-backed (`unique`/`key`) constraints reach here, and the
    // parser guarantees those carry at least one property
    // (`parse_create_constraint` and the graph-type constraint forms all
    // require a non-empty property spec) — an empty list would encode a
    // malformed `"#RANGE#<name>"`, so the invariant is an expect, never a
    // silent default.
    let property = properties
        .first()
        .expect("index-backed constraints carry at least one property (parser guarantee)");
    // The catalog detail is what materializes the backing index's seek
    // postings (`RedoOp::CatalogIndexDetail` → `materialize_detail` — the
    // same path `exec_create_index` takes through
    // [`push_range_index_detail`]). Without it the constraint's backing
    // index is a catalog ghost: SHOW INDEXES lists it, but every equality
    // lookup on the constrained property label-scans instead of seeking —
    // the LABS-328 quadratic merge/match. The encoded `CreateIndex` redo
    // below keeps the catalog's `IndexDef` (whose property consumers split
    // back via `split_legacy_property_encoding`).
    push_range_index_detail(graph, name, relationship, label, properties);
    // The constraint's backing index is part of the constraint, so it is
    // not counted in `QueryStats::indexes_added` (ADR-0309) — the branch's
    // `create_backing_index` call, kept beside main's LABS-328 detail push.
    graph.create_backing_index(
        name.to_string(),
        label.to_string(),
        crate::redo::legacy_property_encoding(property, crate::redo::IndexKindSpec::Range, name),
    )
}

/// The one construction of the single-label range index detail redo —
/// the push that puts the index's seek postings into the floor
/// (ADR-0202) and materializes them at commit. Both the `CREATE INDEX`
/// IR path and the index-backed constraints' backing indexes record the
/// identical detail through this one place; `properties` is the full
/// property list (`covered_pairs` materializes one range pair each).
fn push_range_index_detail(
    graph: &ExecGraph<'_>,
    name: &str,
    relationship: bool,
    label: &str,
    properties: &[String],
) {
    graph.push_catalog_redo(RedoOp::CatalogIndexDetail {
        name: name.to_string(),
        detail: IndexDetailSpec {
            kind: IndexKindSpec::Range,
            relationship,
            labels: vec![label.to_string()],
            properties: properties.to_vec(),
            with_properties: vec![],
        },
        options: crate::redo::props_json(&BTreeMap::new()),
    });
}

/// The existing index an `IF NOT EXISTS` create accepted, carried out of the
/// gate so the `00NA0` note renders the same index row instead of re-probing
/// the catalog.
pub(crate) struct ExistingIndexRule {
    pub index: relate_catalog::IndexDef,
    pub detail: Option<IndexDetailSpec>,
}

/// The rule that made an `IF NOT EXISTS` create a no-op, carried out of the
/// gate so the notification detectors read the gate's own verdict instead of
/// re-probing the catalog.
pub(crate) enum ExistingRule {
    /// An index the create would duplicate (by name, legacy encoding, or
    /// schema equivalence).
    Index(ExistingIndexRule),
    /// An index-backed constraint that already owns the rule: the constraint
    /// an index create's serving pair conflicts with, or the constraint a
    /// constraint create would duplicate. Neither has an index row the
    /// `00NA0` note could render, so no note is emitted; the skip holds.
    Constraint(ConstraintDef),
}

/// The `IF NOT EXISTS` fork every existence probe shares: an existing rule
/// either satisfies the statement (the caller returns the verdict, carrying
/// the rule as evidence) or carries the probe's error.
pub(crate) fn skip_existing(if_not_exists: bool, err: Error, rule: ExistingRule) -> Result<Gate> {
    if if_not_exists {
        Ok(Gate::Skip(rule))
    } else {
        Err(err)
    }
}

/// The create gate's verdict: creation may proceed, or an existing rule
/// satisfied the statement's `IF NOT EXISTS` and the caller returns its rows
/// unchanged. One type for the index and constraint gates; the carried rule
/// is the evidence the notification detectors read.
pub(crate) enum Gate {
    /// Creation may proceed.
    Proceed,
    /// `IF NOT EXISTS` satisfied an existing rule; return the rows unchanged.
    Skip(ExistingRule),
}

/// The `CREATE INDEX` facts the conflict gate consults, independent of the
/// clause that carried them: the catalog `CREATE INDEX` clause and the
/// legacy IR fast path ([`exec_create_index`]) are the two callers, and this
/// is the one conflict policy both obey.
pub(crate) struct IndexFacts {
    pub name: String,
    pub kind: IndexKind,
    pub relationship: bool,
    pub labels: Vec<String>,
    pub properties: Vec<String>,
    pub with_properties: Vec<String>,
    pub if_not_exists: bool,
}

impl IndexFacts {
    /// The catalog clause's facts — every field derived from the clause
    /// exactly once. The caller resolves the effective name (`name`), so the
    /// conflict gate, the creation tail, and the `EXPLAIN` note all read the
    /// same spelling.
    pub(crate) fn from_clause(c: &relate_parser::CreateIndexClause, name: String) -> Self {
        Self {
            name,
            kind: c.kind,
            relationship: c.entity_type == relate_parser::SchemaEntityType::Relationship,
            // Catalog-facing String feeds: the parsed name lists render.
            labels: c.labels.iter().map(|label| label.to_string()).collect(),
            properties: c
                .properties
                .iter()
                .map(|property| property.to_string())
                .collect(),
            with_properties: c
                .with_properties
                .iter()
                .map(|property| property.to_string())
                .collect(),
            if_not_exists: c.if_not_exists,
        }
    }

    /// The name the engine generates when the clause names none — the
    /// catalog clause's `{kind}_index_…` form, stated once for the execution
    /// handler and the `EXPLAIN` note.
    ///
    /// The identity encoded is exactly [`IndexFacts::to_detail`]'s: every
    /// schema Neo4j treats as a distinct rule — the kind, the entity type (a
    /// node and a relationship index on the same label/property are
    /// different schemas), the labels and properties in their written order,
    /// and the VECTOR `WITH` list — gets a distinct name. The `_` join alone
    /// is not injective (`[A, B] + [c]` and `[A] + [B_c]` would collide), so
    /// each list is written as its length followed by length-prefixed
    /// entries, which is prefix-decodable and therefore collision-free.
    pub(crate) fn generated_name(c: &relate_parser::CreateIndexClause) -> String {
        let detail = Self::from_clause(c, String::new()).to_detail();
        let mut name = format!(
            "{}_index_{}",
            detail.kind.display_name().to_ascii_lowercase(),
            if detail.relationship { 'r' } else { 'n' },
        );
        push_encoded_list(&mut name, &detail.labels);
        push_encoded_list(&mut name, &detail.properties);
        if !detail.with_properties.is_empty() {
            name.push_str("_with");
            push_encoded_list(&mut name, &detail.with_properties);
        }
        name
    }

    /// The legacy IR fast path's facts: the node-only, single-label RANGE
    /// index on one property that path models natively.
    pub(crate) fn legacy_range(
        name: String,
        label: String,
        property: String,
        if_not_exists: bool,
    ) -> Self {
        Self {
            name,
            kind: IndexKind::Range,
            relationship: false,
            labels: vec![label],
            properties: vec![property],
            with_properties: Vec::new(),
            if_not_exists,
        }
    }

    /// The `CatalogIndexDetail` redo's index spec — the one clause→detail
    /// derivation, so the same facts are never hand-spelled a second time.
    pub(crate) fn to_detail(&self) -> IndexDetailSpec {
        IndexDetailSpec {
            kind: self.kind.into(),
            relationship: self.relationship,
            labels: self.labels.clone(),
            properties: self.properties.clone(),
            with_properties: self.with_properties.clone(),
        }
    }
}

/// The `CREATE INDEX` conflict policy: duplicate name, the legacy-range
/// equivalence probe, schema equivalence, and the serving-pair conflict. The
/// constraint gate's twin; shape validation stays in `ddl_options`. A skip
/// carries the rule that caused it, so the notification detector reads the
/// gate's own evidence (one policy, not a mirrored probe set).
pub(crate) fn index_conflict_gate(graph: &ExecGraph<'_>, facts: &IndexFacts) -> Result<Gate> {
    // The one schema read the equivalence probe and the skip evidence share.
    let details = graph.show_schema()?.data.index_details;
    let index_rule = |index: relate_catalog::IndexDef| {
        let detail = details.get(&index.name).cloned();
        ExistingRule::Index(ExistingIndexRule { index, detail })
    };
    // Index and constraint names share one namespace in Neo4j. An
    // index-backed constraint owns an index under its own name, so
    // `IF NOT EXISTS` treats it as the existing index (a no-op); every other
    // collision -- a constraint with no backing index, or no `IF NOT EXISTS`
    // -- is rejected (live-verified 2026-11-02 on neo4j:2026.09-enterprise).
    if let Some(constraint) = graph
        .named_constraints()
        .into_iter()
        .find(|constraint| constraint.name == facts.name)
    {
        let err = Error::semantic(format!(
            "There already exists a constraint called '{}'.",
            facts.name
        ));
        if !constraint.is_index_backed() {
            return Err(err);
        }
        // The backing index carries the constraint's name; the note renders
        // that row when the catalog holds it.
        return match graph.named_index(&facts.name) {
            Some(index) => skip_existing(facts.if_not_exists, err, index_rule(index)),
            None => skip_existing(
                facts.if_not_exists,
                err,
                ExistingRule::Constraint(constraint),
            ),
        };
    }
    if let Some(index) = graph.named_index(&facts.name) {
        return skip_existing(
            facts.if_not_exists,
            Error::semantic("IndexAlreadyExists"),
            index_rule(index),
        );
    }
    // The legacy catalog rows (`named_indexes`) predate `CatalogIndexDetail`:
    // a restored image carries the row with `index_details` empty
    // (`#[serde(default)]`). The probe is therefore unconditional — an
    // equivalent legacy range index rejects a duplicate create with the
    // `EquivalentSchemaRuleAlreadyExists` the pre-detail IR path raised, and
    // `IF NOT EXISTS` accepts it. The legacy table is node-only, so a
    // relationship index never matches it. A row that *does* carry a detail
    // is not legacy: its kind is known and the kind-aware probe below has
    // already judged it, so a TEXT index on the same schema is not a
    // RANGE-equivalent rule (live Neo4j allows both on one schema).
    if !facts.relationship && matches!(facts.kind, IndexKind::Range) && facts.labels.len() == 1 {
        let legacy = graph.named_indexes().into_iter().find(|index| {
            !details.contains_key(&index.name)
                && index.label == facts.labels[0]
                && crate::redo::split_legacy_property_encoding(&index.property).0
                    == facts.properties.join(",")
        });
        if let Some(index) = legacy {
            return skip_existing(
                facts.if_not_exists,
                Error::semantic("EquivalentSchemaRuleAlreadyExists"),
                index_rule(index),
            );
        }
    }
    let equivalent = graph.named_indexes().into_iter().find(|index| {
        details.get(&index.name).is_some_and(|old| {
            old.kind == crate::redo::IndexKindSpec::from(facts.kind)
                && old.relationship == facts.relationship
                && old.labels == facts.labels
                && old.properties == facts.properties
                && old.with_properties == facts.with_properties
        })
    });
    if let Some(index) = equivalent {
        return skip_existing(
            facts.if_not_exists,
            Error::semantic("EquivalentSchemaRuleAlreadyExists"),
            index_rule(index),
        );
    }
    // ADR-0372: the schema-rule conflict is narrowed to serving pairs —
    // only a RANGE index can back a constraint (ADR-0339), so only a RANGE
    // index conflicts with an index-backed constraint on the same
    // entity/label/properties. FULLTEXT/VECTOR/POINT indexes coexist with
    // every constraint: they serve search, not validation, and cannot
    // serve as a backing.
    let conflicting = graph.named_constraints().into_iter().find(|constraint| {
        crate::redo::serving_pair_conflicts(
            crate::redo::ServingPairIndex {
                kind: crate::redo::IndexKindSpec::from(facts.kind),
                relationship: facts.relationship,
                labels: &facts.labels,
                properties: &facts.properties,
            },
            crate::redo::ServingPairConstraint {
                backed: constraint.is_index_backed(),
                relationship: constraint.relationship,
                label: &constraint.label,
                properties: &constraint.properties,
            },
        )
    });
    if let Some(constraint) = conflicting {
        return skip_existing(
            facts.if_not_exists,
            Error::semantic("IndexWithNameAlreadyExists"),
            ExistingRule::Constraint(constraint),
        );
    }
    Ok(Gate::Proceed)
}

pub(crate) fn exec_create_index(
    graph: &ExecGraph<'_>,
    name: Option<&str>,
    if_not_exists: bool,
    label: &SmallName,
    property: &SmallName,
) -> Result<()> {
    // The catalog's String domain crossing, once per name: the index name
    // (a catalog identifier, never the front-end name representation) and
    // the label/property renders every catalog-facing consumer below reads.
    let label_text = label.to_string();
    let property_text = property.to_string();
    let name = name
        .map(str::to_string)
        .unwrap_or_else(|| default_index_name(&label_text, &property_text));
    // The conflict family is one policy: the same named gate the catalog
    // `CREATE INDEX` handler calls, fed this path's facts -- a node-only,
    // single-label RANGE index. Delegating (rather than carrying a second
    // ladder of rungs) is what keeps the two spellings' verdicts from
    // drifting on duplicate names, equivalent schemas, and serving pairs.
    let facts = IndexFacts::legacy_range(
        name,
        label_text.clone(),
        property_text.clone(),
        if_not_exists,
    );
    if matches!(index_conflict_gate(graph, &facts)?, Gate::Skip(_)) {
        return Ok(());
    }
    // The IR path is the single-label range index. Record the same detail the
    // catalog path records for TEXT and POINT, so the range posting is in
    // the floor (ADR-0202) and drop removes it by name.
    push_range_index_detail(
        graph,
        &facts.name,
        facts.relationship,
        &label_text,
        &facts.properties,
    );
    graph.create_named_index(facts.name.clone(), label_text, property_text)
}

/// One `DROP` target in the form its caller holds it: the IR path's name or
/// legacy schema, or a parsed clause's resolved name.
#[derive(Clone, Copy)]
pub(crate) enum SchemaTarget<'a> {
    IndexName(&'a str),
    IndexSchema { label: &'a str, property: &'a str },
    ConstraintName(&'a str),
}

/// The one DROP-target existence rule: the catalog name the target resolves
/// to, or `None`. `exec_drop_index`, the parsed-clause `00NA1` note and the
/// legacy IR note all read this, never a second probe spelling.
pub(crate) fn drop_target_exists(
    graph: &ExecGraph<'_>,
    target: SchemaTarget<'_>,
) -> Option<String> {
    match target {
        SchemaTarget::IndexName(name) => graph.named_index(name).map(|def| def.name),
        SchemaTarget::IndexSchema { label, property } => graph
            .named_index_by_schema(label, property)
            .map(|def| def.name),
        SchemaTarget::ConstraintName(name) => graph.named_constraint(name).map(|def| def.name),
    }
}

pub(crate) fn exec_drop_index(
    graph: &ExecGraph<'_>,
    if_exists: bool,
    target: &DropIndexTarget,
) -> Result<()> {
    // One existence decision, and a resolved name when it holds.
    let (resolved, missing) = match target {
        DropIndexTarget::Name(name) => (
            drop_target_exists(graph, SchemaTarget::IndexName(name)),
            format!("NoSuchIndex: `{name}`"),
        ),
        DropIndexTarget::Schema { label, property } => (
            // Text render: the catalog target and the error text both
            // speak String.
            label.with_str(|label_text| {
                property.with_str(|property_text| {
                    drop_target_exists(
                        graph,
                        SchemaTarget::IndexSchema {
                            label: label_text,
                            property: property_text,
                        },
                    )
                })
            }),
            format!("NoSuchIndex: (:{label} {{{property}}})"),
        ),
    };
    let Some(name) = resolved else {
        return if if_exists {
            Ok(())
        } else {
            Err(Error::semantic(missing))
        };
    };
    // A constraint's backing index is part of the constraint: it cannot be
    // dropped on its own (`DROP CONSTRAINT` owns its lifecycle). `IF EXISTS`
    // does not change that.
    let constraints = graph.named_constraints();
    if backing_constraint_by_name(&name, &constraints).is_some() {
        return Err(Error::execution(format!(
            "Unable to drop '{name}': the index belongs to a constraint."
        )));
    }
    graph.drop_named_index(name)
}

/// The one creation half of both constraint paths: install the named
/// constraint, push the derived theater record the statement path carries
/// (between the constraint row and its backing index), and, when the kind is
/// index-backed, its range backing index (an index already holding the name
/// is left alone). `CREATE CONSTRAINT` reaches it through the
/// `Clause::Catalog` passthrough's catalog handler;
/// [`sync_derived_constraints`] calls it directly with no derived record.
pub(crate) fn create_constraint_core(
    graph: &ExecGraph<'_>,
    def: ConstraintDef,
    derived: Option<relate_model::Constraint>,
) -> Result<()> {
    let index_backed = def.is_index_backed();
    // The backing index needs the def's fields after it moves.
    let name = def.name.clone();
    let relationship = def.relationship;
    let label = def.label.clone();
    let properties = def.properties.clone();
    graph.create_named_constraint(def)?;
    if let Some(constraint) = derived {
        graph.push_catalog_redo(RedoOp::CatalogDerivedConstraint { constraint });
    }
    // An index-backed constraint (unique/key) owns a range backing index
    // under the same effective name; an index already holding the name is
    // left alone.
    if index_backed && graph.named_index(&name).is_none() {
        create_constraint_backing_index(graph, &name, relationship, &label, &properties)?;
    }
    Ok(())
}

pub(crate) fn exec_drop_constraint(
    graph: &ExecGraph<'_>,
    name: &str,
    if_exists: bool,
) -> Result<()> {
    // The one DROP-target existence rule, not a second by-name probe.
    if drop_target_exists(graph, SchemaTarget::ConstraintName(name)).is_none() {
        if if_exists {
            return Ok(());
        }
        return Err(Error::semantic(format!("NoSuchConstraint: `{name}`")));
    }
    graph.drop_named_constraint(name.to_string())?;
    if graph.named_index(name).is_some() {
        graph.drop_backing_index(name.to_string())?;
    }
    Ok(())
}

/// Which constraints an `ALTER CURRENT GRAPH TYPE` purges before the
/// derived set is re-synced (ADR-0211). `SET` replaces the whole type, so
/// every committed named constraint goes; every other verb purges only
/// the constraints the previous graph type derived.
pub(crate) fn constraints_to_purge(
    committed: Vec<String>,
    derived_before: &BTreeMap<String, relate_model::Constraint>,
    verb: relate_parser::AlterGraphTypeVerb,
) -> Vec<String> {
    if matches!(verb, relate_parser::AlterGraphTypeVerb::Set) {
        committed
    } else {
        derived_before.keys().cloned().collect()
    }
}

/// Sync the physical catalog to a graph type's derived constraint set:
/// drop the purged constraints (backing indexes go with them, via the
/// canonical [`exec_drop_constraint`]) and create the derived ones,
/// skipping schema-equivalent survivors. The one place the
/// constraint-plus-backing-index invariant is applied to a bulk
/// re-sync, next to the single-statement DDL it mirrors.
pub(crate) fn sync_derived_constraints(
    graph: &ExecGraph<'_>,
    purge: Vec<String>,
    derived: Vec<crate::graph_type::DerivedConstraint>,
) -> Result<()> {
    for name in purge {
        exec_drop_constraint(graph, &name, true)?;
    }
    for derived_constraint in derived {
        let crate::graph_type::DerivedConstraint { constraint, entity } = derived_constraint;
        let def = ConstraintDef::from_model(&constraint, entity);
        // A committed constraint that is schema-equivalent to the derived
        // one survives the purge; the derived constraint is skipped, not
        // re-created (no `CREATE CONSTRAINT` error behaviors here). The
        // enforced label joins the equivalence (CIP-266's multi-parent
        // `(:A => :B&C)`): without it, a node element type's second implied
        // label looked schema-equivalent to its first and was silently
        // skipped. The effective view resolves a legacy row's label first
        // (ADR-0450), so an upgraded store keeps its old skip behavior.
        let existing = graph.effective_constraints()?;
        match rule_disposition(&existing, &def.schema()) {
            RuleDisposition::Equivalent(_) => continue,
            // ADR-0489: a graph-type DDL body is validated as syntax, so a
            // derived constraint clashing with an independent one is
            // Neo4j's syntax-class conflict (42001), unlike the same
            // conflict through `CREATE CONSTRAINT` (a schema error).
            RuleDisposition::Conflicts => return Err(Error::syntax("ConstraintConflict")),
            RuleDisposition::New => {}
        }
        // The cross-kind backing-index claim: a KEY and a UNIQUE on the same
        // subject cannot coexist, whether the second arrives through the same
        // graph type or an independent `CREATE CONSTRAINT`. `rule_disposition`
        // misses it because the kinds differ; `claims_backing_subject_of` is
        // the one predicate for the rule.
        if existing
            .iter()
            .any(|committed| def.claims_backing_subject_of(committed))
        {
            return Err(Error::syntax("ConstraintConflict"));
        }
        create_constraint_core(graph, def, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use relate_core::Value;

    use crate::exec::test_support::{
        commit_scratch, error_of, exec_autocommit, exec_cypher, write_scratch,
    };
    use crate::graph::Graph;
    use crate::overlay::ExecGraph;

    /// R1: a pre-`CatalogIndexDetail` store — a restored image whose
    /// `index_details` is `#[serde(default)]` empty while its
    /// `named_indexes` row survives — must reject an equivalent legacy
    /// create without `IF NOT EXISTS`, exactly as the pre-detail IR path
    /// did; `IF NOT EXISTS` still accepts it. The legacy probe is therefore
    /// unconditional in the shared gate, not folded into the
    /// `IF NOT EXISTS` branch.
    #[test]
    fn pre_detail_legacy_index_rejects_an_equivalent_create() {
        let g = Graph::default();
        // The pre-detail shape: the catalog row without a
        // `CatalogIndexDetail` redo (the row alone commits).
        let scratch = write_scratch(&g);
        {
            let view = ExecGraph::write(&g, &scratch);
            view.create_named_index("legacy".into(), "L".into(), "p".into())
                .unwrap();
        }
        commit_scratch(&g, scratch);
        assert_eq!(g.named_indexes().len(), 1);
        assert!(ExecGraph::read(&g)
            .show_schema()
            .unwrap()
            .data
            .index_details
            .is_empty());

        let error = error_of(exec_autocommit(&g, "CREATE INDEX ON :L(p)"));
        assert!(
            error.contains("EquivalentSchemaRuleAlreadyExists"),
            "{error}"
        );

        exec_autocommit(&g, "CREATE INDEX IF NOT EXISTS ON :L(p)").unwrap();
        assert_eq!(g.named_indexes().len(), 1);
    }

    /// A constraint's backing index is not droppable on its own — the
    /// constraint owns its lifecycle — and `IF EXISTS` does not change
    /// that: both spellings of the named drop raise the constraint-owned
    /// rejection.
    #[test]
    fn a_constraint_backing_index_is_not_droppable() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE CONSTRAINT c FOR (n:L) REQUIRE n.p IS UNIQUE").unwrap();

        for statement in ["DROP INDEX c", "DROP INDEX c IF EXISTS"] {
            let error = error_of(exec_autocommit(&g, statement));
            assert!(
                error.contains("the index belongs to a constraint"),
                "{statement}: {error}"
            );
        }
    }

    /// The equivalence gate's two outcomes, pinned for the `exists` kind too:
    /// a duplicate schema rule is rejected whatever the kind, and
    /// `IF NOT EXISTS` accepts the existing rule unchanged.
    #[test]
    fn equivalent_constraint_schemas_are_rejected_and_if_not_exists_accepts() {
        let g = Graph::default();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT e1 FOR (n:P) REQUIRE n.name IS NOT NULL",
        )
        .unwrap();
        let error = match exec_autocommit(
            &g,
            "CREATE CONSTRAINT e2 FOR (n:P) REQUIRE n.name IS NOT NULL",
        ) {
            Ok(_) => panic!("an equivalent exists rule must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "semantic error: EquivalentConstraintAlreadyExists"
        );
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT e3 IF NOT EXISTS FOR (n:P) REQUIRE n.name IS NOT NULL",
        )
        .unwrap();
        assert_eq!(g.named_constraints().len(), 1);
        exec_autocommit(&g, "CREATE CONSTRAINT u1 FOR (n:Q) REQUIRE n.id IS UNIQUE").unwrap();
        let error =
            match exec_autocommit(&g, "CREATE CONSTRAINT u2 FOR (n:Q) REQUIRE n.id IS UNIQUE") {
                Ok(_) => panic!("a duplicate unique rule must be rejected"),
                Err(error) => error,
            };
        assert_eq!(
            error.to_string(),
            "semantic error: EquivalentConstraintAlreadyExists"
        );
    }

    /// The one `CREATE CONSTRAINT` path: the front-end lowers the clause into
    /// the `Clause::Catalog` passthrough (the IR has no constraint clause),
    /// and the catalog handler creates it. Pins the path the removed dead IR
    /// family could otherwise suggest exists.
    #[test]
    fn create_constraint_lowers_to_the_catalog_passthrough() {
        let query =
            relate_front::parse_and_lower("CREATE CONSTRAINT c FOR (n:P) REQUIRE n.id IS UNIQUE")
                .expect("lowers");
        let clauses = &query.parts[0].clauses;
        assert!(matches!(
            clauses.as_slice(),
            [relate_ir::Clause::Catalog(command)]
                if matches!(
                    command.as_ref(),
                    relate_parser::Clause::CreateConstraint(_)
                )
        ));
        let g = Graph::default();
        exec_autocommit(&g, "CREATE CONSTRAINT c FOR (n:P) REQUIRE n.id IS UNIQUE").unwrap();
        assert!(g.named_constraints().iter().any(|def| def.name == "c"));
    }

    /// LABS-328: an index-backed constraint's backing range index is
    /// materialized (present in the schema's `index_details`), not a
    /// catalog ghost. Before the fix, `CREATE CONSTRAINT … REQUIRE n.id IS
    /// UNIQUE` recorded only the catalog `IndexDef` — SHOW INDEXES listed
    /// the backing range index, but no `CatalogIndexDetail` ever reached
    /// `materialize_detail`, so the seek postings never existed and every
    /// equality lookup on the constrained property label-scanned instead
    /// of seeking (the quadratic merge/match: 5k MERGE lookups took 5.5s;
    /// with postings, 0.047s).
    #[test]
    fn constraint_backing_index_is_materialized_not_a_catalog_ghost() {
        let g = Graph::default();
        exec_autocommit(
            &g,
            "UNWIND range(1, 100) AS id CREATE (p:Person {name: 'p'+id, id: id})",
        )
        .unwrap();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT bulk_person_id FOR (p:Person) REQUIRE p.id IS UNIQUE",
        )
        .unwrap();
        let view = ExecGraph::read(&g);
        let details = view.show_schema().unwrap().data.index_details;
        let backing = details.values().any(|detail| {
            detail.kind == crate::redo::IndexKindSpec::Range
                && !detail.relationship
                && detail.labels == ["Person"]
                && detail.properties == ["id"]
        });
        assert!(
            backing,
            "the constraint's backing range index must carry a materialized index detail"
        );
        // And the materialized postings serve equality: the lookup finds
        // the constrained node.
        let found = exec_cypher(&g, "MATCH (p:Person {id: 42}) RETURN p.id AS id");
        assert_eq!(
            found.rows.first().map(|row| row.first().cloned()),
            Some(Some(Value::Int(42))),
            "the seek path still answers correctly"
        );
    }
    /// ADR-0372: the schema-rule conflict is narrowed to serving pairs —
    /// only a RANGE index conflicts with an index-backed constraint on the
    /// same label/property. A property-type/existence constraint coexists
    /// with a FULLTEXT index in both creation orders (the Stack Overflow
    /// notebook case, LABS-275): before, the constraint-first order failed
    /// with `IndexWithNameAlreadyExists` and the declared schema was less
    /// expressive than the data's real one.
    /// CIP-266: two label-existence rules on one label with different
    /// enforced labels are two schema rules (a multi-parent element type),
    /// so both create cleanly; a repeat of one enforced label is the same
    /// rule and is rejected. The catalog row carries the enforced label, so
    /// the identity survives the round trip through the redo op.
    #[test]
    fn enforced_label_joins_the_constraint_identity() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE CONSTRAINT q FOR (n:P) REQUIRE n:Q").unwrap();
        exec_autocommit(&g, "CREATE CONSTRAINT r FOR (n:P) REQUIRE n:R").unwrap();
        let names: Vec<String> = g
            .named_constraints()
            .iter()
            .map(|def| def.name.clone())
            .collect();
        assert_eq!(names, vec!["q", "r"]);
        assert_eq!(
            g.named_constraints()[0].enforced_label.as_deref(),
            Some("Q")
        );
        let error = match exec_autocommit(&g, "CREATE CONSTRAINT q_again FOR (n:P) REQUIRE n:Q") {
            Ok(_) => panic!("a repeat of one enforced label is the same schema rule"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "semantic error: EquivalentConstraintAlreadyExists"
        );
    }

    #[test]
    fn fulltext_index_and_existence_constraint_coexist_both_orders() {
        assert_both_orders(
            "CREATE CONSTRAINT q_title_exists FOR (q:Question) REQUIRE q.title IS NOT NULL",
            "CREATE FULLTEXT INDEX ft1 FOR (q:Question) ON EACH [q.title]",
            true,
        );
    }

    /// The ADR-0372 both-orders verdict, the one canonical scaffold: run
    /// the pair in each order on a fresh graph and assert the expectation
    /// inside, naming the order and the pair that broke it. `coexists`
    /// false also asserts the conflict carries an AlreadyExists-class
    /// error (the ADR's vocabulary), so the collapsing of the second
    /// statement's `Result` to its outcome still pins the error class.
    fn assert_both_orders(first: &str, second: &str, coexists: bool) {
        for (order, (f, s)) in [(first, second), (second, first)].into_iter().enumerate() {
            let g = Graph::default();
            exec_autocommit(&g, f).unwrap();
            // One match on the outcome: the Ok/Err arm carries both the
            // coexistence verdict and (for conflicts) the error class, so
            // no arm re-derives what the other proved.
            match exec_autocommit(&g, s) {
                Ok(_) => assert!(
                    coexists,
                    "order {order} (`{f}` then `{s}`): expected a conflict"
                ),
                Err(e) => {
                    assert!(
                        !coexists,
                        "order {order} (`{f}` then `{s}`): expected coexistence"
                    );
                    assert!(
                        e.to_string().contains("AlreadyExists"),
                        "order {order}: the conflict must be an AlreadyExists-class error, got {e}"
                    );
                }
            }
        }
    }

    /// ADR-0372: the serving pair still conflicts in both orders — a RANGE
    /// index and an index-backed (uniqueness) constraint on the same
    /// label/property never coexist, because the constraint's backing
    /// (ADR-0339) would duplicate the range index.
    #[test]
    fn range_index_and_backed_constraint_conflict_both_orders() {
        assert_both_orders(
            "CREATE INDEX r1 FOR (q:Question) ON (q.title)",
            "CREATE CONSTRAINT q_title_unique FOR (q:Question) REQUIRE q.title IS UNIQUE",
            false,
        );
    }

    /// ADR-0372 (the blocker pin): a RANGE index coexists with a
    /// NON-backed constraint (existence) in both orders — the serving pair
    /// needs both halves (kind RANGE AND constraint backed); a
    /// property-type/existence constraint is not backed, so even a RANGE
    /// index coexists with it. Before the canonical-helper fix, the catalog
    /// direction gated on the kind alone and wrongly rejected this pair.
    #[test]
    fn range_index_and_existence_constraint_coexist_both_orders() {
        assert_both_orders(
            "CREATE INDEX r1 FOR (q:Question) ON (q.title)",
            "CREATE CONSTRAINT q_title_exists FOR (q:Question) REQUIRE q.title IS NOT NULL",
            true,
        );
    }

    /// ADR-0372: a FULLTEXT index coexists with an index-backed (uniqueness)
    /// constraint too — non-serving kinds never conflict — and SHOW INDEXES
    /// lists both: the full-text index and the constraint's own backing
    /// range index (`owningConstraint` set).
    #[test]
    fn fulltext_index_and_backed_constraint_coexist() {
        let g = Graph::default();
        exec_autocommit(
            &g,
            "CREATE FULLTEXT INDEX ft1 FOR (q:Question) ON EACH [q.title]",
        )
        .unwrap();
        exec_autocommit(
            &g,
            "CREATE CONSTRAINT q_title_unique FOR (q:Question) REQUIRE q.title IS UNIQUE",
        )
        .unwrap();
        let view = ExecGraph::read(&g);
        let details = view.show_schema().unwrap().data.index_details;
        let fulltext = details.values().any(|detail| {
            detail.kind == crate::redo::IndexKindSpec::Fulltext
                && detail.labels == ["Question"]
                && detail.properties == ["title"]
        });
        let backing = details.values().any(|detail| {
            detail.kind == crate::redo::IndexKindSpec::Range
                && detail.labels == ["Question"]
                && detail.properties == ["title"]
        });
        assert!(fulltext, "the FULLTEXT index must coexist");
        assert!(backing, "the constraint's backing range index must exist");
    }
}

#[cfg(test)]
mod index_shapes {
    use crate::exec::test_support::{exec_autocommit, exec_cypher};
    use crate::graph::Graph;

    /// A TEXT index and a RANGE index on the same label/property are two
    /// distinct rules (live Neo4j allows both); only a same-kind schema is
    /// the equivalent-rule conflict. The legacy probe must not treat a
    /// detailed (kind-known) row as a legacy row.
    #[test]
    fn a_range_index_may_share_a_text_index_schema() {
        let g = Graph::default();
        exec_autocommit(&g, "CREATE TEXT INDEX t FOR (n:L) ON (n.p)").unwrap();
        exec_autocommit(&g, "CREATE RANGE INDEX r FOR (n:L) ON (n.p)").unwrap();
        exec_autocommit(&g, "CREATE RANGE INDEX IF NOT EXISTS r2 FOR (n:L) ON (n.p)").unwrap();
        let rows = exec_cypher(&g, "SHOW INDEXES YIELD name, type RETURN name, type");
        // The two default lookup rows every store carries plus the pair.
        assert_eq!(rows.rows.len(), 4, "{:?}", rows.rows);
    }

    /// An unnamed index's generated name encodes the whole schema identity
    /// (`IndexFacts::to_detail`), so every schema Neo4j treats as distinct —
    /// the `WITH` list and its order, the entity type, the LOOKUP entity
    /// slot — never collapses into one `IF NOT EXISTS` no-op.
    #[test]
    fn generated_names_distinguish_the_whole_schema_identity() {
        let g = Graph::default();
        // The VECTOR `WITH` list, and its order.
        exec_autocommit(&g, "CREATE VECTOR INDEX FOR (n:L) ON (n.p) WITH [n.a, n.b]").unwrap();
        exec_autocommit(
            &g,
            "CREATE VECTOR INDEX IF NOT EXISTS FOR (n:L) ON (n.p) WITH [n.b, n.a]",
        )
        .unwrap();
        exec_autocommit(&g, "CREATE VECTOR INDEX IF NOT EXISTS FOR (n:L) ON (n.p)").unwrap();
        // A node and a relationship index on the same label/property are
        // different schemas.
        exec_autocommit(&g, "CREATE INDEX IF NOT EXISTS FOR (n:R) ON (n.p)").unwrap();
        exec_autocommit(&g, "CREATE INDEX IF NOT EXISTS FOR ()-[r:R]-() ON (r.p)").unwrap();
        // Two LOOKUP indexes differ by entity type alone.
        exec_autocommit(
            &g,
            "CREATE LOOKUP INDEX IF NOT EXISTS FOR (n) ON EACH labels(n)",
        )
        .unwrap();
        exec_autocommit(
            &g,
            "CREATE LOOKUP INDEX IF NOT EXISTS FOR ()-[r]-() ON EACH type(r)",
        )
        .unwrap();
        let rows = exec_cypher(&g, "SHOW INDEXES YIELD name RETURN name");
        // Seven created rows plus the two default lookup rows.
        assert_eq!(rows.rows.len(), 9, "{:?}", rows.rows);
    }
}

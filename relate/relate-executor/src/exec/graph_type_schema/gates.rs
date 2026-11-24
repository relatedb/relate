//! The admission gates the `ALTER CURRENT GRAPH TYPE` prepare pipeline
//! runs before the first catalog write (the parent module keeps the
//! entry points and the pipeline itself): the uncovered-independent and
//! added-equivalent gates, the same-command replacement gate and the
//! named-drop classification, plus the rule-level delta and
//! `db.ontology.apply`'s rename gate. Every gate that reads the live
//! constraints or the derived rules' comparison shape takes them as
//! parameters — the one derivation and one mapping the statement's
//! prepare step made under its single snapshot.

use std::str::FromStr;

use relate_catalog::{rule_disposition, ConstraintDef, RuleDisposition};
use relate_core::{Error, Result};
use relate_error::NestedErrorClass;
use relate_model::ConstraintType;

use crate::exec::ddl::{drop_target_exists, SchemaTarget};
use crate::overlay::ExecGraph;

use super::GraphTypeDelta;

/// `db.ontology.apply`'s equivalent-constraint gate (ADR-0667): an
/// index-backed constraint the new derivation wants whose exact rule a
/// live constraint already holds under a *different* name cannot be
/// created by the statement that retires the old one (both would claim
/// the same backing index). Detected up front, before the first catalog
/// redo, with the remediation named instead of a mid-apply schema
/// failure. A rule held under the wanted name itself is the no-op the
/// delta already counts as neither created nor dropped.
///
/// Takes the live constraints and the derived rules' comparison shape as
/// parameters — the one derivation each the statement's prepare step
/// already made under its single snapshot, the sibling gates' contract
/// of never re-taking the snapshot or re-deriving what a prior step
/// computed.
pub(super) fn reject_renamed_equivalent_constraints(
    live: &[ConstraintDef],
    defs: &[ConstraintDef],
) -> Result<()> {
    for def in defs {
        if !def.is_index_backed() {
            continue;
        }
        if let Some(existing) = live
            .iter()
            .find(|existing| existing.name != def.name && existing.schema_equivalent(def))
        {
            return Err(Error::procedure_call(format!(
                "The ontology defines a constraint '{}' that is equivalent to the existing \
                 constraint '{}' (same type, entity and properties, different name). A \
                 constraint cannot be replaced by an equivalent one under a different name \
                 in a single transaction: DROP CONSTRAINT `{}` first, then run \
                 db.ontology.apply again.",
                def.name, existing.name, existing.name
            )));
        }
    }
    Ok(())
}

/// The rule-level delta of one graph-type SET against the live
/// constraints (ADR-0667): the derived rules the live set does not
/// already hold, and the live rules the new derivation does not
/// reproduce — matched through the canonical six-field rule identity, so
/// a rule that survives under its own or a new generated name counts as
/// neither created nor dropped. Both sides arrive already derived: the
/// live list and the derived rules' comparison shape are the statement's
/// one derivations, shared with the gates and the apply-path rename
/// gate.
pub(super) fn graph_type_delta(
    existing: &[ConstraintDef],
    defs: &[ConstraintDef],
) -> GraphTypeDelta {
    GraphTypeDelta {
        created: only_in(defs, existing),
        dropped: only_in(existing, defs),
    }
}

/// How many rules `from` holds that no rule in `than` reproduces — the
/// set difference both delta sides count through the one identity
/// predicate, with the two lists swapped.
fn only_in(from: &[ConstraintDef], than: &[ConstraintDef]) -> usize {
    from.iter()
        .filter(|def| !than.iter().any(|other| other.schema_equivalent(def)))
        .count()
}

/// Classifies every name an `ALTER CURRENT GRAPH TYPE DROP` body lists and
/// adds the independent catalog constraints it names to the purge set. The
/// body's own constraint elements are [`apply_drop`]'s to remove; a name the
/// graph type derives on its own (any dependent kind) cannot be dropped
/// individually (Neo4j 22N68); an independent `CREATE CONSTRAINT` rule may be
/// dropped together with the element type that shares its label
/// (live corpus: `AlterCurrentGraphTypeDrop`).
///
/// [`apply_drop`]: crate::graph_type::apply_drop
pub(super) fn add_named_drop_targets(
    graph: &ExecGraph<'_>,
    state: &crate::graph::ShowSchema,
    current: &relate_parser::GraphTypeBody,
    body: &relate_parser::GraphTypeBody,
    purge: &mut Vec<String>,
) -> Result<()> {
    for name in &body.drop_constraint_names {
        if current
            .constraints
            .iter()
            .any(|constraint| constraint.name.as_ref() == Some(name))
        {
            continue;
        }
        if let Some(derived) = state.data.derived_constraints.get(&name.to_string()) {
            if crate::graph_type::classify_constraint(current, derived)
                == relate_model::ConstraintClassification::Dependent
            {
                return Err(Error::syntax(
                    "DependentConstraintManagedIndividually: dependent constraints cannot be \
                     dropped individually",
                ));
            }
            continue;
        }
        if name
            .with_str(|text| drop_target_exists(graph, SchemaTarget::ConstraintName(text)))
            .is_none()
        {
            return Err(Error::syntax("SchemaError: constraint does not exist"));
        }
        if !purge.iter().any(|existing| name == existing.as_str()) {
            purge.push(name.to_string());
        }
    }
    Ok(())
}

/// The kind tag of a *stored* constraint, parsed fail-closed. The engine
/// writes every tag itself, so an unparseable one is a corrupted or foreign
/// catalog row: skipping it would let that constraint escape the gate reading
/// it (fail open here, exactly the silent drop
/// [`index_backed_declarations`] refuses for an unresolved target).
fn stored_constraint_kind(constraint: &ConstraintDef) -> Result<ConstraintType> {
    ConstraintType::from_str(&constraint.kind).map_err(|reason| {
        Error::internal(format!(
            "stored constraint '{}' carries an unparseable kind tag: {reason}",
            constraint.name
        ))
    })
}

/// Rejects a graph type that introduces an element type on a label (or
/// relationship type) an *independent* property existence / property type
/// constraint occupies, unless the element type's derived constraints cover
/// that rule exactly -- in that case Neo4j lets the rule switch from
/// independent to dependent (live corpus:
/// `GraphTypeAndConstraintCommandCombinations`).
///
/// Every key in `introduced` is absent from the previous body by construction
/// ([`introduced_element_types`] is a set difference), so a matching stored
/// rule was independent under that body: the introduced-set membership alone
/// proves the rule is not graph-type-managed yet.
///
/// Reads the live constraints and the derived rules' comparison shape as
/// parameters — the one derivation and one mapping the statement's prepare
/// step already made under its single snapshot, the sibling gates' contract
/// of never re-taking the snapshot or re-deriving what a prior step
/// computed.
pub(super) fn reject_uncovered_independent_constraints(
    live: &[ConstraintDef],
    introduced: &std::collections::HashSet<ElementTypeKey>,
    defs: &[ConstraintDef],
) -> Result<()> {
    if introduced.is_empty() {
        return Ok(());
    }
    for constraint in live {
        let kind = stored_constraint_kind(constraint)?;
        if !matches!(kind, ConstraintType::Exists | ConstraintType::PropertyType) {
            continue;
        }
        let entity = relate_model::EntityKind::from_relationship_flag(constraint.relationship);
        if !introduced.contains(&(entity, constraint.label.clone())) {
            continue;
        }
        let covered = defs
            .iter()
            .any(|def| def.schema().matches(constraint.schema_view()));
        if !covered {
            return Err(Error::syntax(format!(
                "IndependentConstraintOnLabel: the independent constraint '{}' is using the \
                 same {} `{}` as an element type",
                constraint.schema_text(),
                if entity.is_relationship() {
                    "relationship type"
                } else {
                    "label"
                },
                constraint.label
            )));
        }
    }
    Ok(())
}

/// Rejects an `ADD` declaration that is schema-equivalent to a committed
/// constraint on a subject the statement does not (re)declare as an element
/// type (Neo4j 22N65: an equivalent constraint already exists). When the
/// statement also declares that element type, the inline and standalone rules
/// are one rule and the re-statement is accepted.
///
/// Reads the live constraints as a parameter — the one derivation the
/// caller made under the statement's single snapshot, the sibling gates'
/// contract of never re-taking the snapshot between views.
pub(super) fn reject_added_equivalent_constraints(
    live: &[ConstraintDef],
    statement: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> Result<()> {
    for derived in crate::graph_type::derive_constraints_typed(statement)? {
        if crate::graph_type::label_identifies_type(
            after,
            derived.entity.is_relationship(),
            &derived.constraint.label,
        ) {
            continue;
        }
        let def = ConstraintDef::from_model(&derived.constraint, derived.entity);
        if let RuleDisposition::Equivalent(existing) = rule_disposition(live.iter(), &def.schema())
        {
            return Err(Error::syntax(format!(
                "{}: an equivalent constraint already exists: '{}'",
                NestedErrorClass::EquivalentConstraintAlreadyExists.as_str(),
                existing.name
            )));
        }
    }
    Ok(())
}

/// One index-backed constraint declaration resolved to its comparison
/// shape — the declaration-to-schema bridge: every rule-identity
/// question a declaration is asked goes through its
/// [`ConstraintSchemaView`](relate_catalog::ConstraintSchemaView) and
/// the catalog's own predicates, never a parser-side copy. `kind` stays
/// the typed catalog tag, so a future verb cannot silently collapse
/// into "unique" through a wildcard arm.
struct IndexBackedDecl {
    relationship: bool,
    label: String,
    properties: Vec<String>,
    kind: relate_model::ConstraintType,
    name: Option<String>,
}

impl IndexBackedDecl {
    /// The declaration's name-free comparison shape. An index-backed
    /// declaration carries neither a property type nor an enforced
    /// label — the kinds that carry either are never index-backed — so
    /// the two members read `None` and the view's equality is the
    /// subject-plus-kind identity.
    fn schema_view(&self) -> relate_catalog::ConstraintSchemaView<'_> {
        relate_catalog::ConstraintSchemaView {
            relationship: self.relationship,
            label: &self.label,
            properties: &self.properties,
            kind: self.kind.as_str(),
            property_type: None,
            enforced_label: None,
        }
    }

    /// The same `(entity, label, properties)` subject — one backing
    /// index: the catalog's
    /// [`ConstraintSchemaView::same_subject`](relate_catalog::ConstraintSchemaView::same_subject).
    fn same_subject(&self, other: &Self) -> bool {
        self.schema_view().same_subject(other.schema_view())
    }

    /// The same rule restated: the full six-field schema identity plus
    /// the name.
    fn same_rule(&self, other: &Self) -> bool {
        self.schema_view() == other.schema_view() && self.name == other.name
    }
}

/// Every KEY/UNIQUE declaration resolved to its `(entity, label, properties)`
/// subject. The target label is resolved through the shared
/// [`resolved_constraint_label`](crate::graph_type::resolved_constraint_label),
/// so an unresolved alias is a loud internal invariant violation -- dropping
/// the declaration here would let a same-command replacement escape
/// detection.
fn index_backed_declarations(body: &relate_parser::GraphTypeBody) -> Result<Vec<IndexBackedDecl>> {
    let (node_aliases, edge_aliases) = crate::graph_type::alias_tables(body);
    body.constraints
        .iter()
        .filter(|c| crate::graph_type::constraint_type_of(&c.kind).is_index_backed())
        .map(|c| {
            let table = if c.is_edge {
                &edge_aliases
            } else {
                &node_aliases
            };
            Ok(IndexBackedDecl {
                relationship: c.is_edge,
                label: crate::graph_type::resolved_constraint_label(c, table)?,
                properties: c
                    .properties
                    .iter()
                    .map(|property| property.to_string())
                    .collect(),
                kind: crate::graph_type::constraint_type_of(&c.kind),
                name: c.name.as_ref().map(|name| name.to_string()),
            })
        })
        .collect()
}

/// Neo4j refuses to drop and re-create a *similar* index-backed constraint in
/// one command (the same subject under a new name or another kind), because
/// both would be backed by the same index (live corpus:
/// `AlterCurrentGraphTypeSetPart2`, 50N11 with cause 22N66).
pub(super) fn reject_same_command_constraint_replacement(
    before: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> Result<()> {
    let old = index_backed_declarations(before)?;
    let new = index_backed_declarations(after)?;
    for created in &new {
        // The first dropped declaration sharing the subject: a replacement
        // unless it is the same rule. A declaration survives only as the
        // *same* rule; a same-name declaration of another kind still drops
        // the old rule.
        let Some(dropped) = old
            .iter()
            .find(|dropped| created.same_subject(dropped) && !created.same_rule(dropped))
        else {
            continue;
        };
        if !new.iter().any(|candidate| candidate.same_rule(dropped)) {
            return Err(Error::semantic(format!(
                "Trying to create constraint '{}' in same transaction as dropping '{}'. \
                 This is not supported because they are both backed by similar indexes. \
                 Please drop constraint in a separate transaction before creating the new \
                 one.",
                created.name.as_deref().unwrap_or("constraint_(generated)"),
                dropped.name.as_deref().unwrap_or("constraint_(generated)")
            )));
        }
    }
    Ok(())
}

/// The element-type identity: `(entity kind, identifying label)`. Text
/// render: the label is String.
type ElementTypeKey = (relate_model::EntityKind, String);

/// The element-type identity set a body declares: one key per node and edge
/// type.
fn element_type_keys(
    body: &relate_parser::GraphTypeBody,
) -> std::collections::HashSet<ElementTypeKey> {
    body.node_types
        .iter()
        .map(|node| {
            (
                relate_model::EntityKind::Node,
                node.identifying_label.to_string(),
            )
        })
        .chain(body.edge_types.iter().map(|edge| {
            (
                relate_model::EntityKind::Relationship,
                edge.identifying_label.to_string(),
            )
        }))
        .collect()
}

/// The element-type identities the new body introduces relative to the
/// previous one -- the subjects an `ALTER` newly makes an element type.
pub(super) fn introduced_element_types(
    before: &relate_parser::GraphTypeBody,
    after: &relate_parser::GraphTypeBody,
) -> std::collections::HashSet<ElementTypeKey> {
    element_type_keys(after)
        .difference(&element_type_keys(before))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constraint_body(query: &str) -> relate_parser::GraphTypeBody {
        let stmt = relate_parser::parse(query).expect("should parse");
        match stmt.clauses.into_iter().next().expect("one clause") {
            relate_parser::Clause::AlterCurrentGraphType(agt) => match agt.target {
                relate_parser::AlterCurrentGraphTypeTarget::Body(body) => body,
                relate_parser::AlterCurrentGraphTypeTarget::Named(name) => {
                    panic!("expected an inline body, got named type {name}")
                }
            },
            other => panic!("expected ALTER CURRENT GRAPH TYPE, got {other:?}"),
        }
    }

    #[test]
    fn an_unresolved_key_declaration_fails_loud() {
        // A deserialized body can carry an alias reference the current body
        // no longer declares. Skipping the declaration would let a
        // same-command replacement escape detection, so the unresolved
        // target is an internal invariant violation, never a silent drop.
        let body = constraint_body(
            "ALTER CURRENT GRAPH TYPE SET { CONSTRAINT FOR (n) REQUIRE n.prop IS KEY }",
        );
        let err = index_backed_declarations(&body)
            .err()
            .expect("an unresolved target must fail loud");
        assert!(
            err.to_string()
                .contains("constraint target resolves to no declared element type"),
            "{err}"
        );
    }

    #[test]
    fn an_unparseable_stored_constraint_kind_fails_loud() {
        // The stored tag is engine-written, so an unknown one is a corrupted
        // catalog row. Skipping it would let the constraint escape the
        // independent-constraint gate (fail open) -- the same silent-drop
        // hazard the unresolved-target test above pins.
        let constraint = ConstraintDef {
            name: "constraint_typo".into(),
            relationship: false,
            label: "Person".into(),
            properties: vec!["id".into()],
            kind: "uniqeu".into(),
            property_type: None,
            enforced_label: None,
        };
        let err = stored_constraint_kind(&constraint)
            .expect_err("an unparseable stored kind must fail loud");
        assert!(err.to_string().contains("unknown constraint type"), "{err}");
        assert!(err.to_string().contains("constraint_typo"), "{err}");
    }
}

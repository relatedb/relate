//! Re-keying a graph type's derived constraints when a checkpoint image or
//! WAL record was written before a naming or derivation change.
//!
//! The derived index is a pure function of the persisted graph type, while
//! the generated names are internal bookkeeping that has changed across
//! releases ([`crate::graph_type::name`]). Re-deriving on the way in keeps
//! the graph type, its index, and the catalog agreeing instead of rejecting
//! an older store at its identity checks or leaving stale rows for the next
//! `ALTER CURRENT GRAPH TYPE` to trip over.

use std::collections::BTreeMap;

use relate_catalog::{rule_disposition, Catalog, ConstraintDef, IndexDef, RuleDisposition};
use relate_core::{Error, Result};

use crate::redo;

use super::theater::TheaterData;
use super::Graph;

/// Re-derive the current graph type's constraint index into `data` and carry
/// the catalog rows (and their backing indexes' details) that belong to the
/// previous derivation over to it.
///
/// The derived index is a pure function of the persisted spec, while the
/// generated names are internal bookkeeping that has changed across releases
/// ([`crate::graph_type::name`]). A checkpoint or WAL record written before
/// such a change therefore names rows the current binary would not generate;
/// re-deriving on the way in keeps the graph type, its index, and the catalog
/// agreeing, instead of rejecting the store at its identity checks or leaving
/// stale rows for the next `ALTER CURRENT GRAPH TYPE` to trip over.
pub(super) fn rederive_derived_constraints(
    data: &mut TheaterData,
    catalog: &mut Catalog,
) -> Result<()> {
    // No current graph type owns the index (a legacy store's carried
    // enforced-label records live there too): nothing to re-derive.
    if data.current_graph_type == Default::default() {
        return Ok(());
    }
    let derived = crate::graph_type::derived_constraints_map(&data.current_graph_type.to_body())
        .map_err(crate::graph_type::persisted_derive_error)?;
    // Nothing to carry: an image with no derived records is either a store
    // without a graph type or one whose identity checks own the verdict, and
    // installing rows here would launder it.
    if data.derived_constraints.is_empty() || data.derived_constraints == derived {
        return Ok(());
    }
    let previous = std::mem::take(&mut data.derived_constraints);
    // Local image state only, no graph locks: the caller hands in the image
    // and a catalog copy. A persisted image is the authoritative ownership
    // set, so a record the current derivation does not produce is a
    // user-declared one and stays.
    let migration = plan_derived_migration(&previous, &derived, UnmatchedRecords::Keep);
    migration.apply_to_theater(data);
    let installed = migration.apply_to_catalog(catalog)?;
    migration.install_theater_details(data, &installed);
    Ok(())
}

impl Graph {
    /// Install a `SetCurrentGraphType` record: replace the theater's
    /// current graph type and derived index with the persisted body, then
    /// re-key the catalog rows (and their index details) a previous
    /// derivation under a changed naming or derivation had left behind.
    pub(super) fn apply_current_graph_type(
        &self,
        body: &crate::graph_type::GraphTypeSpec,
    ) -> Result<()> {
        let (previous, derived) = {
            let mut theater = self.theater.lock().unwrap();
            let previous = theater.derived_constraints.clone();
            super::theater::set_current_graph_type(&mut theater, body)?;
            (previous, theater.derived_constraints.clone())
        };
        // A record written before a generated-name or derivation change
        // installs rows under names this binary no longer derives. Re-keying
        // them here is what keeps a replayed store's catalog equal to its
        // re-derived index (a stale row would otherwise survive every purge
        // and turn the next `ALTER CURRENT GRAPH TYPE` into a conflict). This
        // record replaces the set wholesale — the statement's own purge
        // already dropped every record it named — so a record the new
        // derivation does not produce goes. Each half runs under its own
        // lock: schema capture takes the catalog without waiting for the
        // theater, so no path may hold both.
        let migration = (!previous.is_empty() && previous != derived)
            .then(|| plan_derived_migration(&previous, &derived, UnmatchedRecords::Drop));
        let installed = match &migration {
            Some(migration) => {
                migration.apply_to_theater(&mut self.theater.lock().unwrap());
                migration.apply_to_catalog(&mut self.catalog.lock().unwrap())?
            }
            None => Vec::new(),
        };
        {
            let catalog = self.catalog.lock().unwrap();
            self.legacy_unenforced.lock().unwrap().retain(|name, _| {
                !catalog.constraints.get(name).is_some_and(|def| {
                    derived
                        .get(name)
                        .is_some_and(|constraint| super::matching_enforced_label(def, constraint))
                })
            });
        }
        if let Some(migration) = migration {
            migration.install_theater_details(&mut self.theater.lock().unwrap(), &installed);
            self.refresh_index_charge();
            self.recompute_used();
        }
        // ADR-0439: this arm is the constraint-key map's fourth rebuild
        // trigger. The derived set was just replaced wholesale from the
        // persisted body, and the per-op row stream does not mirror it in
        // every reachable shape: a legacy replay record may carry
        // `SetCurrentGraphType` alone, without the `CreateConstraint` rows a
        // current statement emits first, and a current statement's
        // `sync_derived_constraints` skips a derived constraint that is
        // schema-equivalent to a surviving catalog one, so that derived name
        // has no row either. Rebuild from the committed catalog + the
        // re-derived set so the map still covers both.
        self.rebuild_constraint_keys();
        Ok(())
    }
}

/// What a re-derivation moves and drops, split into the theater half and the
/// catalog half so each applies under its own lock: schema capture takes the
/// catalog without ever waiting for the theater, so no path may hold both.
pub(super) struct DerivedMigration {
    /// `(old, new)` for a rule that survives under a new generated name.
    renames: Vec<(String, String)>,
    /// Rows whose rule the current derivation no longer spells the same way
    /// (the property-type `NOT NULL` split): the stale row goes and the
    /// install pass writes the current shape.
    drops: Vec<(String, relate_model::Constraint)>,
    /// The index after the migration. A persisted image's user-declared
    /// records ride over verbatim ([`UnmatchedRecords::Keep`]), so a load
    /// never relaxes an enforcement the store already had.
    derived: BTreeMap<String, relate_model::Constraint>,
}

/// What a re-derivation does with a previous record the new derivation does
/// not produce. The two callers hold opposite authority over their map: a
/// checkpoint image persists the whole ownership set (a user-declared
/// enforced-label record lives beside the graph type's), while a
/// `SetCurrentGraphType` record replaces the set wholesale — the statement's
/// own purge already dropped every record the previous set named.
pub(super) enum UnmatchedRecords {
    /// Keep the record (a persisted image's user-declared rule).
    Keep,
    /// Drop the record and any row it owns (the previous derivation's).
    Drop,
}

pub(super) fn plan_derived_migration(
    previous: &BTreeMap<String, relate_model::Constraint>,
    derived: &BTreeMap<String, relate_model::Constraint>,
    unmatched: UnmatchedRecords,
) -> DerivedMigration {
    let mut migration = DerivedMigration {
        renames: Vec::new(),
        drops: Vec::new(),
        derived: derived.clone(),
    };
    for (old_name, record) in previous {
        // Already current: same record under the same name.
        if derived.get(old_name) == Some(record) {
            continue;
        }
        if let Some((new_name, _)) = derived.iter().find(|(_, new)| same_rule(new, record)) {
            migration.renames.push((old_name.clone(), new_name.clone()));
        } else if derived
            .iter()
            .any(|(_, new)| same_rule_before_not_null_split(new, record))
        {
            migration.drops.push((old_name.clone(), record.clone()));
        } else {
            match unmatched {
                UnmatchedRecords::Keep => {
                    migration.derived.insert(old_name.clone(), record.clone());
                }
                UnmatchedRecords::Drop => {
                    migration.drops.push((old_name.clone(), record.clone()));
                }
            }
        }
    }
    migration
}

impl DerivedMigration {
    /// Move this theater's derived index and its index details.
    pub(super) fn apply_to_theater(&self, data: &mut TheaterData) {
        for (old, new) in &self.renames {
            move_key(&mut data.index_details, old, new, |_, _| {});
            move_key(&mut data.index_options, old, new, |_, _| {});
        }
        for (old, record) in &self.drops {
            if record.constraint_type.is_index_backed() {
                data.index_details.remove(old);
                data.index_options.remove(old);
            }
        }
        data.derived_constraints = self.derived.clone();
    }

    /// Move this catalog's derived constraint rows and their backing indexes,
    /// then install the current derivation's rows where none survives. The
    /// installed rows come back so their index details can follow under the
    /// theater's own lock.
    pub(super) fn apply_to_catalog(
        &self,
        catalog: &mut Catalog,
    ) -> Result<Vec<(String, relate_model::Constraint)>> {
        for (old, new) in &self.renames {
            if catalog.constraints.contains_key(new) {
                // A partly migrated index: the row already sits under the
                // new name, so the stale duplicate goes.
                catalog.constraints.remove(old);
                catalog.indexes.remove(old);
            } else {
                move_key(&mut catalog.constraints, old, new, |def, name| {
                    def.name = name.to_string();
                });
                move_key(&mut catalog.indexes, old, new, |index, name| {
                    index.name = name.to_string();
                });
            }
        }
        for (old, record) in &self.drops {
            drop_derived_row(catalog, old, record)?;
        }
        let mut installed = Vec::new();
        for (name, record) in &self.derived {
            if !catalog.constraints.contains_key(name)
                && recover_derived_row(catalog, name, record)?
            {
                installed.push((name.clone(), record.clone()));
            }
        }
        Ok(installed)
    }

    /// Give each freshly installed derived row the backing-index detail its
    /// seek postings come from — the half `apply_to_catalog` cannot touch
    /// under the catalog lock.
    pub(super) fn install_theater_details(
        &self,
        data: &mut TheaterData,
        installed: &[(String, relate_model::Constraint)],
    ) {
        for (name, record) in installed {
            if !record.constraint_type.is_index_backed() {
                continue;
            }
            data.index_details.insert(
                name.clone(),
                redo::IndexDetailSpec {
                    kind: redo::IndexKindSpec::Range,
                    relationship: record
                        .entity_kind()
                        .is_ok_and(|kind| kind.is_relationship()),
                    labels: vec![record.label.clone()],
                    properties: record.properties.clone(),
                    with_properties: Vec::new(),
                },
            );
            data.index_options.insert(name.clone(), BTreeMap::new());
        }
    }
}

/// Move `map`'s `old` entry to `new`, letting `rename` carry the entry's own
/// name field where it has one. An entry already sitting under `new` wins:
/// the stale duplicate goes.
fn move_key<V>(
    map: &mut BTreeMap<String, V>,
    old: &str,
    new: &str,
    rename: impl FnOnce(&mut V, &str),
) {
    if map.contains_key(new) {
        map.remove(old);
        return;
    }
    if let Some(mut value) = map.remove(old) {
        rename(&mut value, new);
        map.insert(new.to_string(), value);
    }
}

/// The six-field rule identity without the generated name — the same
/// comparison [`relate_catalog::rule_disposition`] makes, over the
/// model-level record pair a migration holds.
fn same_rule(left: &relate_model::Constraint, right: &relate_model::Constraint) -> bool {
    left.entity_type == right.entity_type
        && left.label == right.label
        && left.properties == right.properties
        && left.constraint_type == right.constraint_type
        && left.property_type == right.property_type
        && left.enforced_label == right.enforced_label
}

/// The suffix a pre-split property-type record kept in its stored type: the
/// derivation moved it to a separate existence constraint, so a legacy
/// record's type is the current type with this suffix still appended.
const NOT_NULL_SUFFIX: &str = " NOT NULL";

/// The same rule as the pre-split derivation spelled it: the property-type
/// record used to keep `NOT NULL` in its stored type string and derived no
/// separate existence constraint beside it.
///
/// The legacy type's outer suffix is stripped, never its first ` NOT NULL`
/// occurrence: a nested type (`LIST<STRING NOT NULL> NOT NULL`) keeps its
/// inner `NOT NULL`, which is part of the element type, not the record's
/// mandatory flag.
fn same_rule_before_not_null_split(
    derived: &relate_model::Constraint,
    legacy: &relate_model::Constraint,
) -> bool {
    let Some(bare_type) = legacy
        .property_type
        .as_deref()
        .and_then(|property_type| property_type.strip_suffix(NOT_NULL_SUFFIX))
    else {
        return false;
    };
    let mut normalized = legacy.clone();
    normalized.property_type = Some(bare_type.to_string());
    same_rule(derived, &normalized)
}

/// Drop one stale derived row. The catalog row must be the rule the record
/// names: anything else is corruption, not a migration.
fn drop_derived_row(
    catalog: &mut Catalog,
    name: &str,
    record: &relate_model::Constraint,
) -> Result<()> {
    if let Some(def) = catalog.constraints.get(name) {
        let entity = record.entity_kind().map_err(Error::storage)?;
        if def != &ConstraintDef::from_model(record, entity) {
            return Err(Error::storage(
                "checkpoint derived constraint does not match its catalog row",
            ));
        }
        catalog.constraints.remove(name);
    }
    if record.constraint_type.is_index_backed() {
        // A derived unique/key constraint owns its backing index under the
        // same name.
        catalog.indexes.remove(name);
    }
    Ok(())
}

/// Install the catalog row a re-derived record owns, unless an equivalent
/// rule already holds the schema — the same skip `sync_derived_constraints`
/// makes at statement time, so a user-declared rule that survived keeps
/// owning the schema.
fn recover_derived_row(
    catalog: &mut Catalog,
    name: &str,
    record: &relate_model::Constraint,
) -> Result<bool> {
    let entity = record.entity_kind().map_err(Error::storage)?;
    let def = ConstraintDef::from_model(record, entity);
    match rule_disposition(catalog.constraints.values(), &def.schema()) {
        RuleDisposition::Equivalent(_) => return Ok(false),
        RuleDisposition::Conflicts => {
            return Err(Error::storage(
                "graph type derived constraint conflicts with an existing constraint",
            ))
        }
        RuleDisposition::New => {}
    }
    let index_backed = def.is_index_backed();
    let label = def.label.clone();
    let properties = def.properties.clone();
    catalog.insert_constraint(def)?;
    if !index_backed {
        return Ok(true);
    }
    let property = properties.first().ok_or_else(|| {
        Error::storage("graph type derived constraint has no property for its backing index")
    })?;
    catalog.insert_index(IndexDef {
        name: name.to_string(),
        label,
        property: redo::legacy_property_encoding(property, redo::IndexKindSpec::Range, name),
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_model::{Constraint, ConstraintType};

    fn property_type_record(name: &str, property_type: &str) -> Constraint {
        Constraint {
            name: name.to_string(),
            label: "D".to_string(),
            properties: vec!["p".to_string()],
            constraint_type: ConstraintType::PropertyType,
            property_type: Some(property_type.to_string()),
            entity_type: "NODE".to_string(),
            enforced_label: None,
        }
    }

    /// A nested `NOT NULL` belongs to the element type: the legacy record's
    /// outer suffix is what the split removed, not its first occurrence.
    #[test]
    fn a_nested_not_null_legacy_type_matches_its_split_derivation() {
        let derived = property_type_record("constraint_1", "LIST<STRING NOT NULL>");
        let legacy = property_type_record("legacy", "LIST<STRING NOT NULL> NOT NULL");
        assert!(same_rule_before_not_null_split(&derived, &legacy));
    }

    #[test]
    fn a_scalar_not_null_legacy_type_matches_its_split_derivation() {
        let derived = property_type_record("constraint_1", "BOOLEAN");
        let legacy = property_type_record("legacy", "BOOLEAN NOT NULL");
        assert!(same_rule_before_not_null_split(&derived, &legacy));
    }

    #[test]
    fn an_unrelated_legacy_type_is_not_a_split_match() {
        let derived = property_type_record("constraint_1", "BOOLEAN");
        let legacy = property_type_record("legacy", "INTEGER NOT NULL");
        assert!(!same_rule_before_not_null_split(&derived, &legacy));
    }

    /// The nested-type record must be dropped, not carried over: keeping it
    /// leaves a stale row that the install pass then conflicts with.
    #[test]
    fn a_nested_not_null_legacy_record_is_dropped_not_kept() {
        let previous = BTreeMap::from([(
            "legacy".to_string(),
            property_type_record("legacy", "LIST<STRING NOT NULL> NOT NULL"),
        )]);
        let derived = BTreeMap::from([(
            "constraint_1".to_string(),
            property_type_record("constraint_1", "LIST<STRING NOT NULL>"),
        )]);
        let migration = plan_derived_migration(&previous, &derived, UnmatchedRecords::Keep);
        assert_eq!(migration.drops.len(), 1, "the stale row is dropped");
        assert_eq!(migration.drops[0].0, "legacy");
        assert!(!migration.derived.contains_key("legacy"));
    }
}

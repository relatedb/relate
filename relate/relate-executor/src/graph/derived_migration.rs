//! Reconciling a graph type's derived constraint set with the catalog.
//!
//! One machinery, two jobs: installing the rows a fresh derived set owns
//! (a bare `SetCurrentGraphType` record — the committed schema's open
//! tail, ADR-0649, or a legacy WAL shape — carries no `CreateConstraint`
//! rows), and re-keying the rows a previous derivation under a changed
//! naming or derivation had left behind. The plan decides the drop and
//! install moves against the catalog up front; the apply halves write
//! them, the rename arm keeping its own target-present choice
//! (`move_key`'s idempotency).

use std::collections::{BTreeMap, BTreeSet};

use relate_catalog::{derived_install_disposition, Catalog, ConstraintDef, RuleDisposition};
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
    let derived = crate::graph_type::persisted_derived_map(&data.current_graph_type)?;
    // Nothing to carry: an image with no derived records is either a store
    // without a graph type or one whose identity checks own the verdict, and
    // installing rows here would launder it.
    if data.derived_constraints.is_empty() || data.derived_constraints == derived {
        return Ok(());
    }
    // Local image state only, no graph locks: the caller hands in the image
    // and a catalog copy. A persisted image is the authoritative ownership
    // set, so a record the current derivation does not produce is a
    // user-declared one and stays. The plan reads the image's own map and
    // spec; the theater half replaces them, so a rejected plan leaves the
    // image untouched (the spec rides the plan as a clone, never a take).
    let migration = plan_derived_migration(
        &data.derived_constraints,
        &derived,
        catalog,
        UnmatchedRecords::Keep,
        &data.current_graph_type,
    )?;
    migration.apply_to_catalog(catalog)?;
    migration.apply_to_theater(data);
    Ok(())
}

impl Graph {
    /// Install a `SetCurrentGraphType` record: reconcile the catalog with
    /// the record's derived set — re-keying the rows (and their index
    /// details) a previous derivation under a changed naming or derivation
    /// had left behind, and installing the rows a fresh derived set owns
    /// but no preceding `CreateConstraint` record carried (a bare record:
    /// the legacy WAL shape, or the committed schema's open tail,
    /// ADR-0649) — then replace the theater's current graph type and
    /// derived index. The plan decides everything before anything is
    /// written, so a rejected record leaves both the theater and the
    /// catalog untouched.
    pub(super) fn apply_current_graph_type(
        &self,
        body: &crate::graph_type::GraphTypeSpec,
    ) -> Result<()> {
        // The record's derived set, derived through the persisted-bytes
        // entry — the plan reads it, and the theater half below installs
        // it (the replay path's only theater writer).
        let derived = crate::graph_type::persisted_derived_map(body)?;
        let previous = self.theater.lock().unwrap().derived_constraints.clone();
        // A record written before a generated-name or derivation change
        // installs rows under names this binary no longer derives. Re-keying
        // them here is what keeps a replayed store's catalog equal to its
        // re-derived index (a stale row would otherwise survive every purge
        // and turn the next `ALTER CURRENT GRAPH TYPE` into a conflict). This
        // record replaces the set wholesale — the statement's own purge
        // already dropped every record it named — so a record the new
        // derivation does not produce goes.
        //
        // The plan also installs for a fresh set, not only a re-keying: a
        // bare record — the legacy WAL shape, or the committed schema's
        // open tail (ADR-0649), which carries no `CreateConstraint` rows —
        // would otherwise leave the theater's derived records with no
        // catalog row, and the checkpoint's identity checks refuse exactly
        // that for an enforced-label record. The catalog half runs under
        // one guard — the plan reads the catalog, the apply writes it, the
        // legacy retain follows — then the theater half under its own:
        // schema capture takes the catalog without waiting for the
        // theater, so no path may hold both.
        let migration = {
            let mut catalog = self.catalog.lock().unwrap();
            let migration = plan_derived_migration(
                &previous,
                &derived,
                &catalog,
                UnmatchedRecords::Drop,
                body,
            )?;
            migration.apply_to_catalog(&mut catalog)?;
            self.legacy_unenforced.lock().unwrap().retain(|name, _| {
                !catalog.constraints.get(name).is_some_and(|def| {
                    derived
                        .get(name)
                        .is_some_and(|constraint| super::matching_enforced_label(def, constraint))
                })
            });
            migration
        };
        migration.apply_to_theater(&mut self.theater.lock().unwrap());
        // The recount runs only when the pass does anything: an unchanged
        // record re-applied (the committed schema's idempotent open tail)
        // must not pay the O(graph) structure walk per open.
        if migration.changed() {
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

/// One planned install: the row's definition and, for the index-backed
/// kinds, its backing pair — both resolved at plan time, so every
/// fallible decision happens before a lock is taken or a row moved and
/// the apply halves are pure writes.
struct PlannedInstall {
    def: ConstraintDef,
    backing: Option<(relate_catalog::IndexDef, redo::IndexDetailSpec)>,
}

/// What a re-derivation does to the catalog and the theater's derived
/// index, decided up front against the catalog and split into the theater
/// half and the catalog half so each applies under its own lock: schema
/// capture takes the catalog without ever waiting for the theater, so no
/// path may hold both.
struct DerivedMigration {
    /// `(old, new)` for a rule that survives under a new generated name.
    renames: Vec<(String, String)>,
    /// The definitions of the stale rows whose rule the current derivation
    /// no longer spells the same way (the property-type `NOT NULL` split):
    /// the row goes and the install pass writes the current shape.
    drops: Vec<ConstraintDef>,
    /// The definitions — and resolved backing pairs — of the fresh rows
    /// the record's derived set owns: neither row nor equivalent rule
    /// already held, decided at plan time against the catalog as the
    /// renames and drops leave it, written by the apply passes.
    installs: Vec<PlannedInstall>,
    /// The record's spec — the theater half installs it as the current
    /// graph type beside the plan's index. Cloned from the caller's
    /// image on the re-derive path (a rejected plan must leave the image
    /// untouched, so the field never moves out of it).
    spec: crate::graph_type::GraphTypeSpec,
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
enum UnmatchedRecords {
    /// Keep the record (a persisted image's user-declared rule).
    Keep,
    /// Drop the record and any row it owns (the previous derivation's).
    Drop,
}

/// Decide the migration's drop and install moves up front: the
/// re-keyings and drops a previous derivation left behind, and the fresh
/// rows the record's derived set owns that neither a catalog row nor an
/// equivalent rule already holds (the rename arm's move-vs-drop choice
/// stays with the apply — `move_key`'s own target-present idempotency).
/// The catalog is read-only here — the apply passes do the writing, so a
/// rejected plan leaves the store untouched.
fn plan_derived_migration(
    previous: &BTreeMap<String, relate_model::Constraint>,
    derived: &BTreeMap<String, relate_model::Constraint>,
    catalog: &Catalog,
    unmatched: UnmatchedRecords,
    spec: &crate::graph_type::GraphTypeSpec,
) -> Result<DerivedMigration> {
    let mut migration = DerivedMigration {
        renames: Vec::new(),
        drops: Vec::new(),
        installs: Vec::new(),
        spec: spec.clone(),
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
            migration.drops.push(record_def(record)?);
        } else {
            match unmatched {
                UnmatchedRecords::Keep => {
                    migration.derived.insert(old_name.clone(), record.clone());
                }
                UnmatchedRecords::Drop => {
                    migration.drops.push(record_def(record)?);
                }
            }
        }
    }
    // The install decisions read the catalog as the renames and drops
    // leave it: a dropped name frees its row for the fresh record that
    // replaces it (the `NOT NULL` split spells the new rule under the
    // stale one's name), and the dropped row itself is neither an
    // equivalent rule nor a conflict. Only the freshly derived set
    // installs — a kept rider's missing row is the image inconsistency
    // the checkpoint's identity checks refuse, never one to heal here.
    let retired: BTreeSet<&str> = migration
        .drops
        .iter()
        .map(|def| def.name.as_str())
        .collect();
    let held = HeldRows::of(catalog, &retired);
    for record in derived.values() {
        // The decision sees the rows the plan already decided to install,
        // so a derived set conflicting with itself refuses here instead
        // of installing both halves of the conflict.
        if let Some(install) = planned_install(&held, &migration.installs, record)? {
            migration.installs.push(install);
        }
    }
    Ok(migration)
}

/// The catalog definition one derived record describes — the module's
/// one model→def crossing, made once at plan time so no apply half
/// re-derives it.
fn record_def(record: &relate_model::Constraint) -> Result<ConstraintDef> {
    let entity = record.entity_kind().map_err(Error::storage)?;
    Ok(ConstraintDef::from_model(record, entity))
}

/// The definition a fresh re-derived record owns a catalog row for —
/// the install decision's one home: the row's definition with its
/// resolved backing pair. `None` when the record's row pair is already
/// held ([`HeldRows::holds_pair_of`]) or an equivalent rule already holds the
/// schema (the same skip `sync_derived_constraints` makes at statement
/// time), an error when a conflicting rule — or an install this plan
/// already decided, or the cross-kind backing claim — refuses it. Run
/// at plan time; the apply passes write what this returns, and only
/// that.
fn planned_install(
    held: &HeldRows<'_>,
    planned: &[PlannedInstall],
    record: &relate_model::Constraint,
) -> Result<Option<PlannedInstall>> {
    let def = record_def(record)?;
    if held.holds_pair_of(record, &def) {
        return Ok(None);
    }
    // The live rules the disposition asks over: everything the plan does
    // not retire, minus the row under this record's own name — a held
    // name is the pair's own half to heal (an identical row would read
    // as its own equivalent and skip the heal) — plus every install the
    // plan already decided, so the set cannot conflict with itself.
    // Every install decision — the heal included — runs the one
    // disposition, so the cross-kind backing claim is never skipped.
    let live = held
        .constraints
        .values()
        .copied()
        .filter(|row| row.name != def.name)
        .chain(planned.iter().map(|install| &install.def));
    match derived_install_disposition(live, &def) {
        RuleDisposition::Equivalent(_) => Ok(None),
        RuleDisposition::Conflicts => Err(Error::storage(
            "graph type derived constraint conflicts with an existing constraint",
        )),
        RuleDisposition::New => {
            let backing = redo::constraint_backing_pair(&def)?;
            Ok(Some(PlannedInstall { def, backing }))
        }
    }
}

/// The catalog rows a plan reads its install decisions against: every
/// constraint row and backing-index name the plan does not retire. One
/// view, built once per plan, so a retired name is absent from both
/// halves by construction and no decision re-spells the retirement.
struct HeldRows<'a> {
    constraints: BTreeMap<&'a str, &'a ConstraintDef>,
    indexes: BTreeSet<&'a str>,
}

impl<'a> HeldRows<'a> {
    fn of(catalog: &'a Catalog, retired: &BTreeSet<&str>) -> Self {
        Self {
            constraints: catalog
                .constraints
                .iter()
                .filter(|(name, _)| !retired.contains(name.as_str()))
                .map(|(name, row)| (name.as_str(), row))
                .collect(),
            indexes: catalog
                .indexes
                .keys()
                .filter(|name| !retired.contains(name.as_str()))
                .map(String::as_str)
                .collect(),
        }
    }

    /// Whether the record's own row pair is already held — the install
    /// decision's held test. A row the plan retires frees both slots for its
    /// own replacement (the `NOT NULL` split); a pair-keyed retirement (a
    /// committed file index on the same pair, ADR-0649 §6) drops the backing
    /// row while the constraint row survives, and the held-row identity
    /// rides ADR-0450's legacy resolution
    /// ([`super::resolve_enforced_label`]): a field-less row under the
    /// record's name resolves its enforced label from the record — its
    /// current-shape twin — before the rows are compared, so an upgraded
    /// store's legacy twin counts as the record's own row instead of
    /// mismatching into the catalog's duplicate gate. A row carrying a
    /// different label or rule is the duplicate gate's to refuse at the
    /// write, never a silent skip.
    fn holds_pair_of(&self, record: &relate_model::Constraint, def: &ConstraintDef) -> bool {
        let row_held = self.constraints.get(def.name.as_str()).is_some_and(|held| {
            let mut resolved = (*held).clone();
            super::resolve_enforced_label(&mut resolved, record.enforced_label.as_deref());
            resolved == *def
        });
        let backing_held = !def.is_index_backed() || self.indexes.contains(def.name.as_str());
        row_held && backing_held
    }
}

impl DerivedMigration {
    /// Whether the pass does anything at all — the one fact the recount
    /// tail reads, so an unchanged record re-applied (the committed
    /// schema's idempotent open tail) skips the O(graph) structure walk.
    fn changed(&self) -> bool {
        !(self.renames.is_empty() && self.drops.is_empty() && self.installs.is_empty())
    }

    /// Write the theater half — the replay path's one theater writer:
    /// install the plan's spec as the current graph type, move the
    /// re-keyed rows' details, remove the retired rows' details, give
    /// each planned install its resolved backing detail, then set the
    /// plan's derived index. No fallible step — the plan resolved
    /// everything.
    fn apply_to_theater(&self, data: &mut TheaterData) {
        data.current_graph_type = self.spec.clone();
        for (old, new) in &self.renames {
            move_key(&mut data.index_details, old, new, |_, _| {});
            move_key(&mut data.index_options, old, new, |_, _| {});
        }
        for def in &self.drops {
            if def.is_index_backed() {
                data.index_details.remove(&def.name);
                data.index_options.remove(&def.name);
            }
        }
        for install in &self.installs {
            if let Some((_, detail)) = &install.backing {
                let name = install.def.name.clone();
                data.index_details.insert(name.clone(), detail.clone());
                data.index_options.insert(name, BTreeMap::new());
            }
        }
        data.derived_constraints = self.derived.clone();
    }

    /// Write the catalog half: move the re-keyed rows and their backing
    /// indexes (`move_key`'s own target-present choice is the idempotency
    /// rule), remove the retired rows, then write the planned installs and
    /// their resolved backing rows. Every move is plan-decided; the only
    /// failures left are the catalog's own duplicate gates, failing closed
    /// on a state the plan could not see.
    fn apply_to_catalog(&self, catalog: &mut Catalog) -> Result<()> {
        for (old, new) in &self.renames {
            move_key(&mut catalog.constraints, old, new, |def, name| {
                def.name = name.to_string();
            });
            move_key(&mut catalog.indexes, old, new, |index, name| {
                index.name = name.to_string();
            });
        }
        for def in &self.drops {
            drop_derived_row(catalog, def)?;
        }
        for install in &self.installs {
            catalog.insert_constraint(install.def.clone())?;
            if let Some((row, _)) = &install.backing {
                catalog.insert_index(row.clone())?;
            }
        }
        Ok(())
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

/// Drop one stale derived row. The catalog row must be the rule the plan
/// retired: anything else is corruption, not a migration. The backing
/// index goes with an index-backed row, under the same name.
fn drop_derived_row(catalog: &mut Catalog, def: &ConstraintDef) -> Result<()> {
    if let Some(existing) = catalog.constraints.get(&def.name) {
        if existing != def {
            return Err(Error::storage(
                "checkpoint derived constraint does not match its catalog row",
            ));
        }
        catalog.constraints.remove(&def.name);
    }
    if def.is_index_backed() {
        catalog.indexes.remove(&def.name);
    }
    Ok(())
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

    /// One index-backed record — the shape whose install owns a backing
    /// index row beside its constraint row.
    fn key_record(name: &str) -> Constraint {
        Constraint {
            name: name.to_string(),
            label: "Person".to_string(),
            properties: vec!["title".to_string()],
            constraint_type: ConstraintType::Key,
            property_type: None,
            entity_type: "NODE".to_string(),
            enforced_label: None,
        }
    }

    /// The body a vault's committed ontology converts to (ADR-0649 §2): a
    /// node type whose `title` is the key, and a relationship whose labelled
    /// endpoints derive enforced-label records. Parsed through the same
    /// canonical body entry the persisted-spec restore uses.
    fn committed_schema_body() -> relate_parser::GraphTypeBody {
        relate_parser::parse_graph_type_body(
            "(:Person => {title :: STRING IS KEY}), (:Person=>)-[:KNOWS=>]->(:Person=>)",
        )
        .expect("parses")
    }

    /// A bare `SetCurrentGraphType` record — no `CreateConstraint` rows, the
    /// committed schema's open-tail shape (ADR-0649 §6) — installs the
    /// derived set's catalog rows on a store that held none, so the
    /// checkpoint write's identity checks pass: an enforced-label record
    /// with no catalog row is the refusal this pins (REL8-2022).
    #[test]
    fn a_bare_graph_type_record_installs_its_derived_catalog_rows() {
        let g = Graph::default();
        let spec = crate::graph_type::GraphTypeSpec::from_body(&committed_schema_body())
            .expect("shape-validated body");
        g.apply_redo(
            vec![crate::redo::RedoOp::SetCurrentGraphType { body: spec }],
            1,
        )
        .expect("the bare record applies");

        // The catalog holds exactly the derived rows — the key constraint,
        // the declared property type, and the two endpoint records — and
        // the index-backed kind owns its backing index row.
        let mut rows: Vec<(String, String, Vec<String>)> = g
            .named_constraints()
            .into_iter()
            .map(|def| (def.kind, def.label, def.properties))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (
                    "key".to_string(),
                    "Person".to_string(),
                    vec!["title".to_string()]
                ),
                (
                    "propertyType".to_string(),
                    "Person".to_string(),
                    vec!["title".to_string()]
                ),
                ("sourceLabel".to_string(), "KNOWS".to_string(), Vec::new()),
                ("targetLabel".to_string(), "KNOWS".to_string(), Vec::new()),
            ],
            "the derived set is the catalog's constraint rows"
        );
        let key = g
            .named_constraints()
            .into_iter()
            .find(|def| def.kind == "key")
            .expect("the derived key constraint is a catalog row");
        assert!(
            g.named_indexes().iter().any(|index| index.name == key.name),
            "the index-backed kind owns its backing index row"
        );

        // The checkpoint write's own gate: the image this state produces
        // validates (the enforced-label records find their catalog rows).
        g.checkpoint_theater_for_write()
            .expect("the theater image validates for the checkpoint write");
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
        let migration = plan_derived_migration(
            &previous,
            &derived,
            &Catalog::default(),
            UnmatchedRecords::Keep,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .expect("the split match plans");
        assert_eq!(migration.drops.len(), 1, "the stale row is dropped");
        assert_eq!(migration.drops[0].name, "legacy");
        assert!(!migration.derived.contains_key("legacy"));
    }

    /// A fresh record spelling its rule under a retired row's name installs
    /// over it: the install decisions read the catalog as the drops leave
    /// it, so the stale row is neither a presence nor a conflict for its
    /// own replacement.
    #[test]
    fn a_fresh_record_installs_over_the_row_it_retires() {
        let stale = property_type_record("constraint_1", "LIST<STRING NOT NULL> NOT NULL");
        let fresh = property_type_record("constraint_1", "LIST<STRING NOT NULL>");
        let previous = BTreeMap::from([("constraint_1".to_string(), stale)]);
        let derived = BTreeMap::from([("constraint_1".to_string(), fresh)]);
        let mut catalog = Catalog::default();
        catalog
            .insert_constraint(record_def(&previous["constraint_1"]).expect("typed record"))
            .expect("the stale row seeds the catalog");
        let migration = plan_derived_migration(
            &previous,
            &derived,
            &catalog,
            UnmatchedRecords::Drop,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .expect("the same-name plan builds");
        assert_eq!(migration.drops.len(), 1, "the stale row is retired");
        assert_eq!(
            migration.installs.len(),
            1,
            "the fresh same-named record installs over the retired row"
        );
        assert_eq!(migration.installs[0].def.name, "constraint_1");
    }

    /// A kept rider never installs: the install decision ranges over the
    /// freshly derived set only, so a rider whose catalog row is missing
    /// stays missing — the inconsistency the checkpoint's identity checks
    /// refuse, never one this pass heals.
    #[test]
    fn a_kept_rider_never_installs() {
        let rider = property_type_record("user_rule", "BOOLEAN");
        let fresh = property_type_record("constraint_1", "BOOLEAN NOT NULL");
        let previous = BTreeMap::from([("user_rule".to_string(), rider)]);
        let derived = BTreeMap::from([("constraint_1".to_string(), fresh)]);
        let migration = plan_derived_migration(
            &previous,
            &derived,
            &Catalog::default(),
            UnmatchedRecords::Keep,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .expect("the rider plan builds");
        assert!(
            migration.derived.contains_key("user_rule"),
            "the rider rides over verbatim"
        );
        assert!(
            !migration
                .installs
                .iter()
                .any(|install| install.def.name == "user_rule"),
            "the rider's missing row is not healed by an install"
        );
    }

    /// A held constraint row whose backing index row is missing heals at
    /// the next plan: a pair-keyed retirement (a committed file index on
    /// the same pair, ADR-0649 §6) drops the backing half while the
    /// constraint row survives, and the install decision treats the pair
    /// as held only when both halves are (REL8-2022's rust-review round).
    #[test]
    fn a_held_row_with_a_missing_backing_index_heals() {
        let key = key_record("constraint_k");
        let derived = BTreeMap::from([("constraint_k".to_string(), key.clone())]);
        let mut catalog = Catalog::default();
        // The half-held state: the constraint row, no backing index row.
        catalog
            .insert_constraint(record_def(&key).expect("typed record"))
            .expect("the constraint row seeds the catalog");
        let migration = plan_derived_migration(
            &BTreeMap::new(),
            &derived,
            &catalog,
            UnmatchedRecords::Drop,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .expect("the heal plan builds");
        assert_eq!(
            migration.installs.len(),
            1,
            "the missing backing half re-plans its install"
        );
        migration
            .apply_to_catalog(&mut catalog)
            .expect("the heal applies");
        assert!(
            catalog.indexes.contains_key("constraint_k"),
            "the backing index row is healed beside its held constraint row"
        );
        let mut theater = TheaterData::default();
        migration.apply_to_theater(&mut theater);
        assert!(
            theater.index_details.contains_key("constraint_k"),
            "the healed row's seek-postings detail is installed"
        );
    }

    /// An upgraded store's legacy row — field-less per ADR-0450 — under
    /// the derived name still counts as the record's own: the identity
    /// check resolves the missing enforced label from the record (its
    /// current-shape twin), so a re-applied derived constraint skips
    /// instead of mismatching, installing beside the legacy row, and
    /// hitting the catalog's duplicate gate at every open.
    #[test]
    fn a_fieldless_legacy_row_under_the_name_is_the_records_own() {
        let record = Constraint {
            name: "constraint_s".to_string(),
            label: "KNOWS".to_string(),
            properties: Vec::new(),
            constraint_type: ConstraintType::SourceLabel,
            property_type: None,
            entity_type: "RELATIONSHIP".to_string(),
            enforced_label: Some("Person".to_string()),
        };
        // The catalog's legacy row: the twin's identity, minus the field.
        let mut legacy_row = record_def(&record).expect("typed record");
        legacy_row.enforced_label = None;
        let mut catalog = Catalog::default();
        catalog
            .insert_constraint(legacy_row)
            .expect("the legacy row seeds the catalog");
        let derived = BTreeMap::from([("constraint_s".to_string(), record)]);
        let migration = plan_derived_migration(
            &BTreeMap::new(),
            &derived,
            &catalog,
            UnmatchedRecords::Drop,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .expect("the legacy-twin plan builds");
        assert!(
            migration.installs.is_empty(),
            "the resolved twin is the record's own row: no install, no duplicate-gate error"
        );
    }

    /// A derived set conflicting with itself refuses at the plan: each
    /// install decision sees the rows the plan already decided, so a KEY
    /// and a UNIQUE on one subject cannot both install — the mutual
    /// blindness the rust-review round flagged.
    #[test]
    fn a_derived_set_conflicting_with_itself_refuses() {
        let key = key_record("constraint_k");
        let mut unique = key_record("constraint_u");
        unique.constraint_type = ConstraintType::Unique;
        let derived = BTreeMap::from([
            ("constraint_k".to_string(), key),
            ("constraint_u".to_string(), unique),
        ]);
        let err = plan_derived_migration(
            &BTreeMap::new(),
            &derived,
            &Catalog::default(),
            UnmatchedRecords::Drop,
            &crate::graph_type::GraphTypeSpec::default(),
        )
        .map(|_| ())
        .expect_err("the self-conflicting set refuses at the plan");
        assert!(
            err.to_string()
                .contains("graph type derived constraint conflicts with an existing constraint"),
            "{err}"
        );
    }
}

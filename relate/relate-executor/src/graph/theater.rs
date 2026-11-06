//! Committed SHOW metadata captured with a checkpoint, plus the read-admission
//! schema snapshot derived from it.

use std::collections::BTreeMap;
use std::sync::Mutex;

use relate_catalog::Catalog;
#[cfg(test)]
use relate_catalog::ConstraintDef;
use relate_core::{Error, Namespace, Result};

use crate::redo::{self, RedoOp};

use super::{matching_enforced_label, requires_enforced_label, Graph};

pub use relate_engine_api::{
    map_put_database, CheckpointTheater, DatabaseEntry, SchemaSnapshot, ShowSchema, TheaterData,
};

/// invariants): the three fixed
/// namespaces' settings (default language, composite flag) live in ONE
/// table shared by every namespace graph of the store through the
/// `StoreServices` bundle. `ALTER DATABASE` writes it and `SHOW DATABASES`
/// reads it from any namespace session, so the two can never diverge the
/// way per-graph theater copies did. The table is keyed by `Namespace`, so
/// it holds exactly the three fixed entries by construction and every
/// lookup is total; the on-disk checkpoint theater image keeps serializing
/// it as strings under its historical `databases` key, with the boundary
/// conversion in [`Self::sanitize`] and [`Graph::checkpoint_theater`].
#[derive(Debug)]
pub(crate) struct DatabasesCatalog {
    entries: Mutex<BTreeMap<Namespace, DatabaseEntry>>,
}

impl DatabasesCatalog {
    /// ADR-0116 §1/§8: a fixed namespace's default entry — `CYPHER 25`,
    /// non-composite — until an `ALTER DATABASE ... SET DEFAULT LANGUAGE`
    /// redo says otherwise. `Default` and [`Self::sanitize`] seed every
    /// fixed namespace with it, and the typed key plus filtered inserts
    /// keep exactly the three entries present.
    pub(crate) const DEFAULT_DATABASE_ENTRY: DatabaseEntry = DatabaseEntry {
        default_language: 25,
        composite: false,
    };

    /// The seeded table: exactly the three fixed namespaces, each at its
    /// default entry.
    fn seeded() -> BTreeMap<Namespace, DatabaseEntry> {
        Namespace::ALL
            .iter()
            .map(|ns| (*ns, Self::DEFAULT_DATABASE_ENTRY))
            .collect()
    }

    /// The committed table at one capture point (read admission or
    /// checkpoint cut).
    pub(crate) fn snapshot(&self) -> BTreeMap<Namespace, DatabaseEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// ADR-0116 §8: record a namespace's default language (committed redo
    /// path; see `map_put_database` for the matching rule).
    pub(crate) fn put_database(&self, name: &str, language: u32, composite: bool) {
        map_put_database(&mut self.entries.lock().unwrap(), name, language, composite);
    }

    /// ADR-0116 §8: the boundary conversion for a checkpoint image's
    /// string-keyed table. Entries for non-fixed names (from an older
    /// store's `CREATE DATABASE` theater) are dropped so the next
    /// checkpoint no longer carries them; names match case-insensitively,
    /// and a fixed namespace missing from the image gets its default
    /// (`CYPHER 25`).
    pub(crate) fn sanitize(
        map: &BTreeMap<String, DatabaseEntry>,
    ) -> BTreeMap<Namespace, DatabaseEntry> {
        // One matching rule (`Namespace::parse`, as in `map_put_database`):
        // fold the image's entries over the seeded defaults, keeping only
        // the fixed namespaces.
        let mut out = Self::seeded();
        for (name, entry) in map {
            if let Some(ns) = Namespace::parse(name) {
                out.insert(ns, *entry);
            }
        }
        out
    }

    /// Install a checkpoint image's (already sanitized) table as the
    /// store-level catalog.
    pub(crate) fn install(&self, map: BTreeMap<Namespace, DatabaseEntry>) {
        *self.entries.lock().unwrap() = map;
    }
}

impl Default for DatabasesCatalog {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Self::seeded()),
        }
    }
}

/// Apply speculative catalog operations to this private admitted copy.
pub(crate) fn apply_show_overlay<'o>(
    show: &mut ShowSchema,
    ops: impl IntoIterator<Item = &'o RedoOp>,
) -> Result<()> {
    for op in ops {
        match op {
            RedoOp::CatalogIndexDetail {
                name,
                detail,
                options,
            } => {
                let options = redo::props_from_json(options)?;
                show.data.index_details.insert(name.clone(), detail.clone());
                show.data.index_options.insert(name.clone(), options);
            }
            RedoOp::DropIndex { name } => {
                show.data.index_details.remove(name);
                show.data.index_options.remove(name);
            }
            RedoOp::PutDatabase {
                name,
                language,
                composite,
            } => {
                map_put_database(&mut show.databases, name, *language, *composite);
            }
            RedoOp::DropDatabase { .. } => {}
            RedoOp::PutGraphType { name, body } => {
                show.data.graph_types.insert(name.clone(), body.clone());
            }
            RedoOp::SetCurrentGraphType { body } => {
                set_current_graph_type(&mut show.data, body)?;
            }
            RedoOp::CatalogDerivedConstraint { constraint } => {
                show.data
                    .derived_constraints
                    .insert(constraint.name.clone(), constraint.clone());
            }
            RedoOp::DropConstraint { name } => {
                show.data.derived_constraints.remove(name);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Install `spec` as the current graph type and re-derive its constraint index.
pub(crate) fn set_current_graph_type(
    data: &mut TheaterData,
    spec: &crate::graph_type::GraphTypeSpec,
) -> Result<()> {
    let derived = crate::graph_type::derived_constraints_map(&spec.to_body())
        .map_err(crate::graph_type::persisted_derive_error)?;
    data.current_graph_type = spec.clone();
    data.derived_constraints = derived;
    Ok(())
}

impl Graph {
    /// Capture the committed checkpoint metadata at the caller's checkpoint cut.
    #[doc(hidden)]
    pub fn checkpoint_theater(&self) -> CheckpointTheater {
        CheckpointTheater {
            // The on-disk image keeps the historical string keys; the
            // in-memory table is `Namespace`-keyed.
            databases: self
                .databases()
                .snapshot()
                .into_iter()
                .map(|(ns, entry)| (ns.as_str().to_string(), entry))
                .collect(),
            data: self.theater.lock().unwrap().clone(),
            legacy_unknown_constraints: self
                .legacy_unenforced
                .lock()
                .unwrap()
                .clone()
                .into_iter()
                .collect(),
        }
    }

    /// Validate a typed checkpoint image against its named catalog before
    /// installing it; a missing enforced label must never relax a constraint.
    ///
    /// The graph type's derived-constraint index is a pure function of the
    /// persisted spec, so the image is re-derived before the identity checks
    /// run and the catalog's derived rows are re-keyed to match
    /// ([`super::derived_migration::rederive_derived_constraints`]): a store
    /// written before a naming or derivation change keeps loading instead of
    /// failing on names its own binary no longer generates. The migration
    /// runs on a copy of the catalog, so a rejected image leaves the graph
    /// untouched.
    #[doc(hidden)]
    pub fn restore_checkpoint_theater(&self, image: CheckpointTheater) -> Result<()> {
        let mut image = image;
        {
            let mut catalog = self.catalog.lock().unwrap();
            let mut migrated = catalog.clone();
            super::derived_migration::rederive_derived_constraints(&mut image.data, &mut migrated)?;
            Self::validate_checkpoint_theater_image_in(&image, &migrated)?;
            *catalog = migrated;
        }
        self.install_checkpoint_theater_image(image)
    }

    /// CIP-176 spec string for the JSON snapshot codec (ADR-0050). A
    /// deserialized default value the canonical renderer cannot render is
    /// the storage-corruption error family the checkpoint path uses for
    /// the same persisted bytes (typed, never a panic).
    pub fn graph_type_spec(&self) -> Result<Option<String>> {
        let model = self.theater.lock().unwrap().current_graph_type.clone();
        if model == Default::default() {
            return Ok(None);
        }
        crate::graph_type::canonicalize_graph_type_body(&model.to_body())
            .map_err(crate::graph_type::persisted_derive_error)
            .map(Some)
    }

    /// Restore a Relate GraphData `graph_type` spec after snapshot load.
    pub fn restore_graph_type_spec(&self, spec: Option<&str>) -> Result<()> {
        let Some(raw) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let inner = raw
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(raw);
        let body = relate_parser::parse_graph_type_body(inner)
            .map_err(|e| Error::storage(format!("invalid graph_type spec: {e}")))?;
        // Install without re-validation, exactly as WAL replay and
        // db.ontology.apply do (ADR-0388): the store's preview set is not
        // resolved yet at load time (configure_preview runs after), so a
        // gated construct a legitimately gated-opened store wrote must
        // restore unchanged. Poisoned bytes still fail typed here: the
        // derivation below renders every default it stores (the family's
        // persisted message, round 8's rust Medium fix).
        let spec = crate::graph_type::GraphTypeSpec::from_body(&body)
            .map_err(crate::graph_type::persisted_validation_error)?;
        let mut theater = self.theater.lock().unwrap();
        set_current_graph_type(&mut theater, &spec)?;
        Ok(())
    }

    /// Produce a validated typed image, preserving exact legacy-unknown
    /// definitions alongside all known committed theater metadata.
    #[doc(hidden)]
    pub fn checkpoint_theater_for_write(&self) -> Result<CheckpointTheater> {
        let image = self.checkpoint_theater();
        self.validate_checkpoint_theater_image(&image)?;
        Ok(image)
    }

    /// Record historical missing enforcement only after loading an untyped source.
    #[doc(hidden)]
    pub fn mark_legacy_unenforced(&self) {
        let image = self.checkpoint_theater();
        let catalog = self.catalog.lock().unwrap();
        let mut legacy = self.legacy_unenforced.lock().unwrap();
        for def in catalog
            .constraints
            .values()
            .filter(|d| requires_enforced_label(d))
        {
            if !image
                .data
                .derived_constraints
                .get(&def.name)
                .is_some_and(|c| matching_enforced_label(def, c))
            {
                legacy.insert(def.name.clone(), def.clone());
            }
        }
    }

    /// Install an already-validated theater image (the typed image's checks,
    /// or the legacy path's per-entry checks).
    fn install_checkpoint_theater_image(&self, image: CheckpointTheater) -> Result<()> {
        *self.legacy_unenforced.lock().unwrap() = image
            .legacy_unknown_constraints
            .iter()
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect();
        // ADR-0116 §8: an older store's image may carry `CREATE
        // DATABASE` theater rows; the boundary conversion keeps only the
        // fixed namespaces so the next checkpoint image no longer
        // contains them.
        let databases = DatabasesCatalog::sanitize(&image.databases);
        // ADR-0276: the databases table is store-level — install it
        // into the shared catalog every namespace graph reads.
        self.databases().install(databases);
        // Checkpoint rows feed only data labels and types; the DDL names live
        // in this theater image. Record them through the same catalog sets so
        // a name that existed only in an index or constraint survives a
        // restart exactly as it did before one (ADR-0363 decision 5).
        for detail in image.data.index_details.values() {
            self.record_schema_names(detail.relationship, detail.labels.iter().cloned());
        }
        for constraint in image.data.derived_constraints.values() {
            if let Some(enforced) = &constraint.enforced_label {
                self.record_schema_names(false, std::iter::once(enforced.clone()));
            }
        }
        *self.theater.lock().unwrap() = image.data;
        self.refresh_index_charge();
        Ok(())
    }

    fn validate_checkpoint_theater_image(&self, image: &CheckpointTheater) -> Result<()> {
        Self::validate_checkpoint_theater_image_in(image, &self.catalog.lock().unwrap())
    }

    /// The image checks over an explicit catalog view: the restore path
    /// validates against the catalog as the re-derivation leaves it, without
    /// committing the migration before the image is accepted.
    fn validate_checkpoint_theater_image_in(
        image: &CheckpointTheater,
        catalog: &Catalog,
    ) -> Result<()> {
        Self::validate_legacy_unknown(image, catalog)?;
        Self::validate_enforced_label_completeness(image, catalog)?;
        Self::validate_graph_type_derivation(image)?;
        Self::validate_derived_identity(image, catalog)?;
        Self::validate_index_details(image, catalog)
    }

    /// Legacy-unknown markers name rows the catalog still carries and the
    /// derived index does not shadow.
    fn validate_legacy_unknown(image: &CheckpointTheater, catalog: &Catalog) -> Result<()> {
        for (name, def) in &image.legacy_unknown_constraints {
            if name != &def.name
                || !requires_enforced_label(def)
                || catalog.constraints.get(name) != Some(def)
                || image.data.derived_constraints.contains_key(name)
            {
                return Err(Error::storage(
                    "checkpoint legacy-unknown constraint does not match its named definition",
                ));
            }
        }
        Ok(())
    }

    /// Every enforced-label constraint has a matching derived record or a
    /// recorded legacy marker; a complete image may not leave one dangling.
    fn validate_enforced_label_completeness(
        image: &CheckpointTheater,
        catalog: &Catalog,
    ) -> Result<()> {
        for def in catalog
            .constraints
            .values()
            .filter(|d| requires_enforced_label(d))
        {
            // One probe, flat arms: a matching derived record, or a recorded
            // legacy marker for an absent record; anything else is a missing
            // match.
            match image.data.derived_constraints.get(&def.name) {
                Some(constraint) if matching_enforced_label(def, constraint) => {}
                None if image.legacy_unknown_constraints.get(&def.name) == Some(def) => {}
                _ => {
                    return Err(Error::storage(
                        "checkpoint named label constraint is missing matching enforced label",
                    ))
                }
            }
        }
        Ok(())
    }

    /// The persisted graph type derives exactly the constraints the theater
    /// lists.
    fn validate_graph_type_derivation(image: &CheckpointTheater) -> Result<()> {
        for expected in
            crate::graph_type::derive_constraints(&image.data.current_graph_type.to_body())
                .map_err(crate::graph_type::persisted_derive_error)?
        {
            if image.data.derived_constraints.get(&expected.name) != Some(&expected) {
                return Err(Error::storage(
                    "checkpoint graph type is missing a derived constraint",
                ));
            }
        }
        Ok(())
    }

    /// Every derived record's key, kind, entity side, and enforced label are
    /// coherent with the catalog.
    fn validate_derived_identity(image: &CheckpointTheater, catalog: &Catalog) -> Result<()> {
        for (name, constraint) in &image.data.derived_constraints {
            // One typed boundary for both identity facts: the entry's key is
            // its record's name, and `entity_type` is one of the two rendered
            // kinds. Any other spelling is corruption — the same
            // model classifier the WAL preflight uses.
            let kind = match constraint.entity_kind() {
                Ok(kind) if name == &constraint.name => kind,
                _ => {
                    return Err(Error::storage(
                        "invalid checkpoint derived constraint identity",
                    ))
                }
            };
            let label_kind = constraint.constraint_type.is_enforced_label_kind();
            let has_label = super::has_enforced_label(constraint);
            let relationship = kind.is_relationship();
            let wrong_entity = constraint
                .constraint_type
                .enforced_label_site()
                .is_some_and(|site| site.is_relationship() != relationship);
            if wrong_entity {
                return Err(Error::storage(
                    "checkpoint enforced-label entity type mismatch",
                ));
            }
            if label_kind && !has_label {
                return Err(Error::storage(
                    "checkpoint constraint is missing its enforced label",
                ));
            }
            if constraint.enforced_label.is_some() {
                let matches = catalog.constraints.values().any(|def| {
                    (def.name == *name || !catalog.constraints.contains_key(name))
                        && matching_enforced_label(def, constraint)
                });
                if !matches || !has_label {
                    return Err(Error::storage(
                        "invalid checkpoint enforced-label constraint",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every index detail/option names a live index (the same
    /// `(image, &Catalog)` shape as its four siblings).
    fn validate_index_details(image: &CheckpointTheater, catalog: &Catalog) -> Result<()> {
        for name in image
            .data
            .index_details
            .keys()
            .chain(image.data.index_options.keys())
        {
            if !catalog.indexes.contains_key(name) {
                return Err(Error::storage("checkpoint index detail has no named index"));
            }
        }
        Ok(())
    }

    /// Recover the legacy GraphData enforced-label field when no typed theater
    /// image exists. Base definitions must already be installed.
    #[doc(hidden)]
    pub fn restore_legacy_enforced_constraints(
        &self,
        constraints: Vec<relate_model::Constraint>,
    ) -> Result<()> {
        let mut image = self.checkpoint_theater();
        for constraint in constraints {
            // Legacy GraphData may omit the theater entirely: validate the
            // supplied entries one by one (typed identity plus a matching
            // catalog definition) instead of imposing the typed image's
            // completeness guarantee.
            self.validate_legacy_enforced_entry(&constraint)?;
            image
                .data
                .derived_constraints
                .insert(constraint.name.clone(), constraint);
        }
        self.install_checkpoint_theater_image(image)
    }

    /// The legacy entry gate: a typed entity kind, an enforced-label kind,
    /// a non-empty label, and a catalog definition it matches.
    fn validate_legacy_enforced_entry(&self, constraint: &relate_model::Constraint) -> Result<()> {
        constraint.entity_kind().map_err(Error::storage)?;
        let catalog = self.catalog.lock().unwrap();
        let matches = catalog
            .constraints
            .values()
            .any(|def| matching_enforced_label(def, constraint));
        if !super::has_enforced_label(constraint) || !matches {
            return Err(Error::storage(
                "invalid checkpoint enforced-label constraint",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redo::{IndexDetailSpec, IndexKindSpec};

    fn range_index_detail() -> IndexDetailSpec {
        IndexDetailSpec {
            kind: IndexKindSpec::Range,
            relationship: false,
            labels: vec!["N".into()],
            properties: vec!["p".into()],
            with_properties: Vec::new(),
        }
    }

    fn admitted_show() -> ShowSchema {
        ShowSchema::capture(
            DatabasesCatalog::default().snapshot(),
            &TheaterData::default(),
        )
    }

    #[test]
    fn apply_overlay_propagates_corrupt_index_options() {
        let mut show = admitted_show();
        let ops = vec![RedoOp::CatalogIndexDetail {
            name: "idx".into(),
            detail: range_index_detail(),
            options: serde_json::json!("corrupted"),
        }];
        // A decode failure is an error, never a silently dropped index entry
        // (matching `Graph::apply_redo`).
        assert!(apply_show_overlay(&mut show, &ops).is_err());
        assert!(show.data.index_details.is_empty());
        assert!(show.data.index_options.is_empty());
    }

    #[test]
    fn apply_overlay_installs_valid_index_options() {
        let mut show = admitted_show();
        let ops = vec![RedoOp::CatalogIndexDetail {
            name: "idx".into(),
            detail: range_index_detail(),
            options: serde_json::json!({}),
        }];
        apply_show_overlay(&mut show, &ops).unwrap();
        assert!(show.data.index_details.contains_key("idx"));
        assert!(show.data.index_options.contains_key("idx"));
    }

    /// ADR-0116 §1/§8/§10: the default store-level catalog carries exactly
    /// the three fixed namespaces.
    #[test]
    fn default_catalog_has_exactly_the_fixed_namespaces() {
        let catalog = DatabasesCatalog::default();
        let snapshot = catalog.snapshot();
        let names: Vec<Namespace> = snapshot.keys().copied().collect();
        assert_eq!(names, Namespace::ALL);
    }

    /// ADR-0116 §8: `PutDatabase` redo for a non-fixed name (an older
    /// store's `CREATE DATABASE` theater) is dropped at the replay
    /// boundary; fixed names match case-insensitively and store under the
    /// typed key, and a `DropDatabase` redo is a no-op because the fixed
    /// namespaces can never be dropped. The speculative overlay and the
    /// committed store-level catalog apply the same rule.
    #[test]
    fn database_ops_keep_only_the_fixed_namespaces() {
        let ops = [
            RedoOp::PutDatabase {
                name: "analytics".into(),
                language: 5,
                composite: false,
            },
            RedoOp::PutDatabase {
                name: "Neo4j".into(),
                language: 5,
                composite: false,
            },
            RedoOp::DropDatabase {
                name: "system".into(),
            },
        ];
        let mut show = admitted_show();
        apply_show_overlay(&mut show, &ops).unwrap();
        let names: Vec<Namespace> = show.databases.keys().copied().collect();
        assert_eq!(names, Namespace::ALL);
        assert_eq!(
            show.databases[&Namespace::Neo4j],
            DatabaseEntry {
                default_language: 5,
                composite: false,
            }
        );

        let catalog = DatabasesCatalog::default();
        for op in &ops {
            match op {
                RedoOp::PutDatabase {
                    name,
                    language,
                    composite,
                } => catalog.put_database(name, *language, *composite),
                RedoOp::DropDatabase { .. } => {}
                _ => unreachable!(),
            }
        }
        assert_eq!(catalog.snapshot(), show.databases);
    }

    /// ADR-0116 §8: the boundary conversion for a checkpoint image drops
    /// non-fixed entries and fills any missing fixed namespace with its
    /// default.
    #[test]
    fn sanitize_databases_drops_non_fixed_and_fills_defaults() {
        let mut map = BTreeMap::new();
        map.insert(
            "analytics".to_string(),
            DatabaseEntry {
                default_language: 5,
                composite: false,
            },
        );
        map.insert(
            "NEO4J".to_string(),
            DatabaseEntry {
                default_language: 5,
                composite: true,
            },
        );
        let fixed = DatabasesCatalog::sanitize(&map);
        let names: Vec<Namespace> = fixed.keys().copied().collect();
        assert_eq!(names, Namespace::ALL);
        assert_eq!(
            fixed[&Namespace::Neo4j],
            DatabaseEntry {
                default_language: 5,
                composite: true,
            }
        );
        assert_eq!(
            fixed[&Namespace::System],
            DatabasesCatalog::DEFAULT_DATABASE_ENTRY
        );
        assert_eq!(
            fixed[&Namespace::SystemOntologies],
            DatabasesCatalog::DEFAULT_DATABASE_ENTRY
        );
    }

    /// The derived-constraint index is a pure function of the persisted
    /// graph type, but its generated names are internal bookkeeping that has
    /// changed across releases (and the property-type `NOT NULL` split
    /// reshaped one rule). A checkpoint image written before such a change
    /// must re-derive on restore instead of failing its identity checks.
    #[test]
    fn restore_rederives_a_pre_generated_name_theater() {
        use relate_model::{ConstraintType, EntityKind};

        use crate::exec::test_support::exec_autocommit;

        let source = Graph::default();
        exec_autocommit(
            &source,
            "ALTER CURRENT GRAPH TYPE SET { (:Person => {name :: STRING NOT NULL}) }",
        )
        .unwrap();
        let mut image = source.checkpoint_theater();
        // Age the image: the readable name the pre-split derivation
        // generated, with `NOT NULL` still spelled into the property type
        // (the separate existence constraint did not exist yet).
        let legacy = relate_model::Constraint {
            name: "constraint_propertyType_Person_name".into(),
            label: "Person".into(),
            properties: vec!["name".into()],
            constraint_type: ConstraintType::PropertyType,
            property_type: Some("STRING NOT NULL".into()),
            entity_type: "NODE".into(),
            enforced_label: None,
        };
        image.data.derived_constraints =
            std::iter::once((legacy.name.clone(), legacy.clone())).collect::<BTreeMap<_, _>>();

        let graph = Graph::default();
        graph
            .install_named_constraint(ConstraintDef::from_model(&legacy, EntityKind::Node))
            .unwrap();
        graph
            .restore_checkpoint_theater(image)
            .expect("a pre-generated-name image re-derives and restores");

        let theater = graph.theater.lock().unwrap();
        let expected =
            crate::graph_type::derived_constraints_map(&theater.current_graph_type.to_body())
                .unwrap();
        assert_eq!(theater.derived_constraints, expected);
        drop(theater);
        let mut rows = graph.named_constraints();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter().all(|def| def.name.starts_with("constraint_")),
            "{rows:?}"
        );
        let property_type = rows
            .iter()
            .find(|def| def.kind == "propertyType")
            .expect("the property-type row survives the re-derivation");
        assert_eq!(property_type.property_type.as_deref(), Some("STRING"));
    }

    /// A WAL record written by an older binary installs its readable-named
    /// rows and then replaces the graph type. Replaying it must not leave
    /// the catalog behind the re-derived index: the stale `"<type> NOT
    /// NULL"` row would survive every purge and turn the next non-`SET`
    /// ALTER into a conflict.
    #[test]
    fn replaying_a_pre_generated_name_record_keeps_the_catalog_current() {
        use relate_model::{ConstraintType, EntityKind};

        use crate::exec::test_support::exec_autocommit;

        let graph = Graph::default();
        exec_autocommit(
            &graph,
            "ALTER CURRENT GRAPH TYPE SET { (:Person => {name :: STRING NOT NULL}) }",
        )
        .unwrap();
        let body = graph.checkpoint_theater().data.current_graph_type.clone();
        let committed: Vec<String> = graph
            .named_constraints()
            .into_iter()
            .map(|def| def.name)
            .collect();
        assert_eq!(committed.len(), 2, "{committed:?}");

        // The pre-PR record: purge the committed set, install the
        // readable-name rows, then carry the new body.
        let legacy = relate_model::Constraint {
            name: "constraint_propertyType_Person_name".into(),
            label: "Person".into(),
            properties: vec!["name".into()],
            constraint_type: ConstraintType::PropertyType,
            property_type: Some("STRING NOT NULL".into()),
            entity_type: "NODE".into(),
            enforced_label: None,
        };
        let mut ops: Vec<RedoOp> = committed
            .into_iter()
            .map(|name| RedoOp::DropConstraint { name })
            .collect();
        ops.push(RedoOp::create_constraint(ConstraintDef::from_model(
            &legacy,
            EntityKind::Node,
        )));
        ops.push(RedoOp::CatalogDerivedConstraint { constraint: legacy });
        ops.push(RedoOp::SetCurrentGraphType { body });
        graph.apply_redo(ops, graph.commit_ts() + 1).unwrap();

        let mut rows = graph.named_constraints();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter().all(|def| def.name.starts_with("constraint_")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|def| def.property_type.as_deref() == Some("STRING")),
            "{rows:?}"
        );

        // The first non-SET ALTER after the upgrade re-syncs the derived
        // set, and a repeated one stays a no-op.
        let alter = "ALTER CURRENT GRAPH TYPE ALTER { (:Person => {name :: STRING NOT NULL, nickname :: STRING}) }";
        exec_autocommit(&graph, alter).expect("the upgraded store's ALTER re-syncs cleanly");
        let after = graph.named_constraints().len();
        exec_autocommit(&graph, alter).expect("a repeated ALTER stays idempotent");
        assert_eq!(graph.named_constraints().len(), after);
    }
}

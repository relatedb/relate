//! The graph's named-schema surface: named indexes and constraints, and the
//! schema snapshot. Split out of `graph/mod.rs` (LABS-677).

use relate_catalog::{ConstraintDef, IndexDef};

use super::*;

impl Graph {
    pub fn named_indexes(&self) -> Vec<IndexDef> {
        self.catalog
            .lock()
            .unwrap()
            .indexes
            .values()
            .cloned()
            .collect()
    }

    /// Capture named indexes, constraints and their SHOW metadata at one
    /// admission boundary. Embedded callers hold the publication read guard
    /// across this call and `pin_now`; direct callers must serialize with
    /// their own graph writers in the same way.
    pub fn schema_snapshot(&self) -> SchemaSnapshot {
        self.schema_snapshot_with_hook(|| {})
    }

    pub(super) fn schema_snapshot_with_hook(&self, after_catalog: impl FnOnce()) -> SchemaSnapshot {
        let (indexes, constraints) = {
            let catalog = self.catalog.lock().unwrap();
            (
                catalog.indexes.values().cloned().collect(),
                catalog.constraints.values().cloned().collect(),
            )
        };
        after_catalog();
        // Publication serialization makes these non-overlapping captures
        // (catalog above, then databases + theater in capture_show_schema)
        // atomic to embedded readers, without ever nesting catalog/theater
        // locks.
        let show = self.capture_show_schema();
        SchemaSnapshot {
            indexes,
            constraints,
            show,
        }
    }

    /// The committed SHOW metadata at this instant: the store-level
    /// databases table plus this graph's theater, under the theater lock.
    pub(crate) fn capture_show_schema(&self) -> ShowSchema {
        ShowSchema::capture(self.databases().snapshot(), &self.theater.lock().unwrap())
    }

    pub fn named_constraints(&self) -> Vec<ConstraintDef> {
        self.catalog
            .lock()
            .unwrap()
            .constraints
            .values()
            .cloned()
            .collect()
    }

    /// Install one catalog constraint row. Restore paths only: this mutates
    /// the catalog WITHOUT the maintained constraint-key map (ADR-0439), so
    /// the caller must finish with `rebuild_indexes` / `finish_restore`,
    /// which builds the map from the catalog ∪ the theater's derived set.
    /// Calling it on a live graph would leave a uniqueness constraint the
    /// admission probe cannot see.
    pub fn install_named_constraint(&self, def: ConstraintDef) -> Result<()> {
        self.catalog.lock().unwrap().insert_constraint(def)
    }

    pub fn named_index(&self, name: &str) -> Option<IndexDef> {
        self.catalog.lock().unwrap().indexes.get(name).cloned()
    }

    /// By-name constraint probe, the constraint side of [`Self::named_index`].
    pub fn has_constraint(&self, name: &str) -> bool {
        self.catalog.lock().unwrap().constraints.contains_key(name)
    }

    pub fn named_index_by_schema(&self, label: &str, property: &str) -> Option<IndexDef> {
        self.catalog
            .lock()
            .unwrap()
            .index_by_schema(label, property)
            .cloned()
    }

    pub fn install_named_index(&self, def: IndexDef) -> Result<()> {
        self.catalog.lock().unwrap().insert_index(def)
    }
}

//! Catalog image a snapshot carries, shared by every query engine.
//! Derivation and admission behavior stay with the engine that owns the live graph.

use std::collections::BTreeMap;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::{Namespace, Value};
use serde::{Deserialize, Serialize};

use crate::redo::IndexDetailSpec;

/// The five committed per-graph SHOW-metadata maps that make up a checkpoint
/// theater. Composed by the live graph state (a plain `TheaterData`), the
/// serialized checkpoint image (`CheckpointTheater`'s flattened `data`) and
/// the read-admission snapshot (`ShowSchema`'s `data`). The databases table
/// is NOT here: it is the store-level databases table (ADR-0276).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TheaterData {
    pub index_details: BTreeMap<String, IndexDetailSpec>,
    pub index_options: BTreeMap<String, BTreeMap<String, Value>>,
    pub graph_types: BTreeMap<String, crate::graph_type::GraphTypeSpec>,
    pub current_graph_type: crate::graph_type::GraphTypeSpec,
    pub derived_constraints: BTreeMap<String, relate_model::Constraint>,
}

/// One namespace's entry in the store-level databases catalog (ADR-0116
/// §8, ADR-0276): the settings `SHOW DATABASES` reports and `ALTER
/// DATABASE ... SET DEFAULT LANGUAGE` rewrites. Serde keeps the
/// historical two-element array shape (`[language, composite]`) so
/// existing checkpoint images keep loading.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(u32, bool)", into = "(u32, bool)")]
pub struct DatabaseEntry {
    /// Default Cypher language version for the namespace (`5` or `25`).
    pub default_language: u32,
    /// Whether the namespace is a composite database.
    pub composite: bool,
}

impl From<(u32, bool)> for DatabaseEntry {
    fn from((default_language, composite): (u32, bool)) -> Self {
        Self {
            default_language,
            composite,
        }
    }
}

impl From<DatabaseEntry> for (u32, bool) {
    fn from(entry: DatabaseEntry) -> Self {
        (entry.default_language, entry.composite)
    }
}

/// ADR-0116 §8: record a namespace's default language in `map`. The redo
/// name is the serde/replay boundary: it matches the fixed namespaces
/// case-insensitively and is stored under the typed key; entries for any
/// other name (written by earlier `CREATE DATABASE` calls or an older
/// binary) are dropped here.
pub fn map_put_database(
    map: &mut BTreeMap<Namespace, DatabaseEntry>,
    name: &str,
    language: u32,
    composite: bool,
) {
    if let Some(ns) = Namespace::parse(name) {
        map.insert(
            ns,
            DatabaseEntry {
                default_language: language,
                composite,
            },
        );
    }
}


/// Typed committed SHOW metadata stored with a checkpoint's named catalog.
/// Missing images in older snapshots use the historical empty theater.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointTheater {
    /// Store-level databases table at the checkpoint cut (ADR-0276; ADR-0116
    /// §8 invariants).
    /// Declared first so the serialized key order matches the historical
    /// flattened layout, where `databases` led `TheaterData`.
    pub databases: BTreeMap<String, DatabaseEntry>,
    #[serde(flatten)]
    pub data: TheaterData,
    /// Exact catalog definitions imported without known label enforcement.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub legacy_unknown_constraints: BTreeMap<String, ConstraintDef>,
}

impl CheckpointTheater {
    /// Enforced label of a named constraint, if present.
    #[doc(hidden)]
    pub fn enforced_label(&self, name: &str) -> Option<&str> {
        self.data
            .derived_constraints
            .get(name)
            .and_then(|constraint| constraint.enforced_label.as_deref())
    }

    /// Catalog detail of a named index, if present.
    #[doc(hidden)]
    pub fn index_detail(&self, name: &str) -> Option<&IndexDetailSpec> {
        self.data.index_details.get(name)
    }
}

/// Catalog facts captured alongside an entity pin at read admission. The
/// caller serializes capture with commit publication; execution needs no
/// publication lock and never consults newer named schema.
pub struct SchemaSnapshot {
    pub indexes: Vec<IndexDef>,
    pub constraints: Vec<ConstraintDef>,
    pub show: ShowSchema,
}

/// The read-admission SHOW metadata: the store-level databases table plus
/// the graph's committed theater maps, captured at one admission point.
/// Composes [`TheaterData`] the way [`CheckpointTheater`] does, so the five
/// maps exist once. Speculative catalog redo is applied by the query
/// engine that owns derivation; committed state changes only when that
/// engine applies redo.
#[derive(Debug, Clone)]
pub struct ShowSchema {
    pub databases: BTreeMap<Namespace, DatabaseEntry>,
    pub data: TheaterData,
}

impl ShowSchema {
    /// Capture the committed SHOW metadata at one admission point: the
    /// store-level databases table plus this graph's theater maps.
    pub fn capture(
        databases: BTreeMap<Namespace, DatabaseEntry>,
        theater: &TheaterData,
    ) -> Self {
        Self {
            databases,
            data: theater.clone(),
        }
    }
}


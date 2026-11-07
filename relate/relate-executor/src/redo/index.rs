//! The index-DDL payload family: the typed index kind, the serving-pair
//! predicates, the legacy property encoding, and the detail spec its
//! `CatalogIndexDetail` op carries. A private sibling re-exported at the
//! `redo` root (the one-public-path pattern), so callers keep using
//! `crate::redo::…`.

use std::collections::BTreeMap;

use relate_catalog::{ConstraintDef, IndexDef};
use relate_core::Value;
use serde::{Deserialize, Serialize};

use super::RedoOp;

/// The DDL index kind carried by [`IndexDetailSpec`]: a
/// serde-serializable mirror of `relate_parser::IndexKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexKindSpec {
    Range,
    Point,
    Text,
    Fulltext,
    Lookup,
    Vector,
}

impl IndexKindSpec {
    /// Every variant; derived inverses (and their tests) iterate this so
    /// the variant list is maintained in exactly one place.
    pub const ALL: [IndexKindSpec; 6] = [
        IndexKindSpec::Range,
        IndexKindSpec::Point,
        IndexKindSpec::Text,
        IndexKindSpec::Fulltext,
        IndexKindSpec::Lookup,
        IndexKindSpec::Vector,
    ];

    /// The upper-case display name Neo4j's `SHOW INDEXES` `type` column
    /// uses (same strings as `relate_parser::IndexKind::display_name`).
    /// The `SHOW INDEXES` `type` column spelling — the parser's own display
    /// name, so the six names live once in the crate that owns the dialect.
    pub fn display_name(&self) -> &'static str {
        relate_parser::IndexKind::from(*self).display_name()
    }

    /// Parse the display name back into the kind — the inverse of
    /// [`IndexKindSpec::display_name`], used to recover the kind of
    /// legacy catalog rows that predate `CatalogIndexDetail` from their
    /// `{props}#{KIND}#{name}` property encoding. Derived from the one
    /// string table in `display_name` so the two directions cannot drift.
    pub fn from_display_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.display_name() == name)
    }

    /// The Neo4j `SHOW INDEXES` `indexProvider` column value for this
    /// kind. Exhaustive on purpose: a new index kind fails to compile
    /// here instead of silently falling back to `range-1.0`.
    pub fn provider_name(&self) -> &'static str {
        match self {
            IndexKindSpec::Range => "range-1.0",
            IndexKindSpec::Point => "point-1.0",
            IndexKindSpec::Text => "text-3.0",
            IndexKindSpec::Fulltext => "fulltext-2.0",
            IndexKindSpec::Lookup => "token-lookup-1.0",
            IndexKindSpec::Vector => "vector-2026.08",
        }
    }

    /// The DDL default `indexConfig` this kind records when the statement
    /// names none — the one mutator the DDL layer applies. `None` for the
    /// kinds whose default config is the empty map: RANGE, TEXT and LOOKUP
    /// record none, and VECTOR's metric-dependent defaults are applied by
    /// `vector_config::apply_defaults` through the vector OPTIONS pipeline
    /// (`catalog_schema::create_config`), which is why this table omits it.
    /// The table lives beside [`IndexKindSpec::display_name`] and
    /// [`IndexKindSpec::provider_name`] so a new kind states its default
    /// here, never in a `_`-glued match one layer above.
    pub(crate) fn default_config(&self) -> Option<fn(&mut BTreeMap<String, Value>)> {
        match self {
            IndexKindSpec::Point => Some(apply_point_defaults),
            IndexKindSpec::Fulltext => Some(apply_fulltext_defaults),
            IndexKindSpec::Range
            | IndexKindSpec::Text
            | IndexKindSpec::Lookup
            | IndexKindSpec::Vector => None,
        }
    }

    /// The one LOOKUP discrimination: a LOOKUP index carries no
    /// label/property tokens. Every LOOKUP reader — the row's null-ed
    /// `labelsOrTypes`/`properties` cells, the createStatement's EACH form,
    /// the notification descriptor's tail — asks this, never its own
    /// `matches!`.
    pub fn is_lookup(&self) -> bool {
        matches!(self, IndexKindSpec::Lookup)
    }

    /// The kind of one index row, resolved once — the detail's kind when the
    /// detail join hits, else the legacy `{props}#{KIND}#{name}` encoding's
    /// kind for a detail-less legacy WAL/checkpoint row (`catalog_create_index`
    /// writes both redo ops in one statement today), else RANGE. The one
    /// resolution every kind reader consults — the kind filter, the row's
    /// type/provider columns and LOOKUP nulling, the createStatement's own
    /// `detail.kind`, and the planner-IR diagnostic table's `type` cell.
    /// The RANGE default covers exactly the encoding's two no-kind shapes:
    /// the bare pre-encoding form (`name`, written before kinds existed —
    /// RANGE by history, the only kind that era stored) and an unparseable
    /// kind segment (`name#NOTAKIND#idx`, pinned to no kind by the redo
    /// decoder's own tests). The second cannot occur in a store this engine
    /// wrote — [`legacy_property_encoding`] only ever embeds a valid
    /// [`IndexKindSpec`] — so the default never fabricates a kind over a
    /// reachable row.
    pub fn resolve(detail: Option<&IndexDetailSpec>, legacy_property: &str) -> Self {
        detail
            .map(|detail| detail.kind)
            .or_else(|| split_legacy_property_encoding(legacy_property).1)
            .unwrap_or(IndexKindSpec::Range)
    }
}

/// The spatial bounds Neo4j records for a `POINT` index that names none:
/// one key per coordinate reference system, each a `[min…]`/`[max…]` list.
fn apply_point_defaults(config: &mut BTreeMap<String, Value>) {
    const DEFAULTS: &[(&str, &[f64])] = &[
        ("spatial.cartesian.min", &[-1000000.0, -1000000.0]),
        ("spatial.cartesian.max", &[1000000.0, 1000000.0]),
        (
            "spatial.cartesian-3d.min",
            &[-1000000.0, -1000000.0, -1000000.0],
        ),
        (
            "spatial.cartesian-3d.max",
            &[1000000.0, 1000000.0, 1000000.0],
        ),
        ("spatial.wgs-84.min", &[-180.0, -90.0]),
        ("spatial.wgs-84.max", &[180.0, 90.0]),
        ("spatial.wgs-84-3d.min", &[-180.0, -90.0, -1000000.0]),
        ("spatial.wgs-84-3d.max", &[180.0, 90.0, 1000000.0]),
    ];
    for (key, values) in DEFAULTS {
        let coordinates = values
            .iter()
            .map(|value| Value::Float(*value))
            .collect::<Vec<_>>();
        config
            .entry((*key).to_string())
            .or_insert_with(|| Value::List(coordinates.into()));
    }
}

/// The fulltext config Neo4j records when the statement names none.
fn apply_fulltext_defaults(config: &mut BTreeMap<String, Value>) {
    config
        .entry("fulltext.analyzer".to_string())
        .or_insert_with(|| Value::String("standard-no-stop-words".into()));
    config
        .entry("fulltext.eventually_consistent".to_string())
        .or_insert(Value::Bool(false));
}

impl From<IndexKindSpec> for relate_parser::IndexKind {
    fn from(kind: IndexKindSpec) -> Self {
        match kind {
            IndexKindSpec::Range => relate_parser::IndexKind::Range,
            IndexKindSpec::Point => relate_parser::IndexKind::Point,
            IndexKindSpec::Text => relate_parser::IndexKind::Text,
            IndexKindSpec::Fulltext => relate_parser::IndexKind::Fulltext,
            IndexKindSpec::Lookup => relate_parser::IndexKind::Lookup,
            IndexKindSpec::Vector => relate_parser::IndexKind::Vector,
        }
    }
}

impl From<relate_parser::IndexKind> for IndexKindSpec {
    fn from(kind: relate_parser::IndexKind) -> Self {
        match kind {
            relate_parser::IndexKind::Range => IndexKindSpec::Range,
            relate_parser::IndexKind::Point => IndexKindSpec::Point,
            relate_parser::IndexKind::Text => IndexKindSpec::Text,
            relate_parser::IndexKind::Fulltext => IndexKindSpec::Fulltext,
            relate_parser::IndexKind::Lookup => IndexKindSpec::Lookup,
            relate_parser::IndexKind::Vector => IndexKindSpec::Vector,
        }
    }
}

/// The index side of an ADR-0372 serving-pair check: the rule's kind,
/// entity kind, labels, and properties, as the creating statement spells
/// them (or as the materialized `index_details` record them).
#[derive(Clone, Copy)]
pub struct ServingPairIndex<'a> {
    pub kind: IndexKindSpec,
    pub relationship: bool,
    pub labels: &'a [String],
    pub properties: &'a [String],
}

/// The constraint side of an ADR-0372 serving-pair check.
#[derive(Clone, Copy)]
pub struct ServingPairConstraint<'a> {
    pub backed: bool,
    pub relationship: bool,
    pub label: &'a str,
    pub properties: &'a [String],
}

/// ADR-0372's serving-pair conflict, the one canonical form, owning the
/// whole match: the pair's typed fields in, the verdict out. A schema rule
/// pair conflicts only when the index kind is RANGE — the only kind that
/// can serve a constraint's backing (ADR-0339) — and the constraint is
/// index-backed (uniqueness/key), on the same label and properties.
/// Property-type/existence constraints and FULLTEXT/VECTOR/POINT indexes
/// coexist with everything. All three creation directions consult this; a
/// change to the rule lands here once.
pub fn serving_pair_conflicts(
    index: ServingPairIndex<'_>,
    constraint: ServingPairConstraint<'_>,
) -> bool {
    index.kind == IndexKindSpec::Range
        && index.relationship == constraint.relationship
        && index.labels.len() == 1
        && constraint.label == index.labels[0]
        && constraint.properties == index.properties
        && constraint.backed
}

/// Decode a legacy `IndexDef.property` string: split the value `catalog_create_index`
/// (and the constraint-backed index
/// path) into its props head and index kind. A bare property name (no
/// `#`) is its own head with no kind. The single decoder for this
/// format — every reader of `IndexDef.property` goes through here.
pub fn split_legacy_property_encoding(property: &str) -> (&str, Option<IndexKindSpec>) {
    match property.split_once('#') {
        None => (property, None),
        Some((head, rest)) => (
            head,
            rest.split('#')
                .next()
                .and_then(IndexKindSpec::from_display_name),
        ),
    }
}

/// Encode an `IndexDef.property` in the legacy `{property}#{KIND}#{name}`
/// format — the encoder half of [`split_legacy_property_encoding`], which
/// decodes the result back to `(property, Some(kind))`.
pub fn legacy_property_encoding(property: &str, kind: IndexKindSpec, name: &str) -> String {
    format!("{property}#{}#{name}", kind.display_name())
}

/// Decode a legacy `IndexDef.property` string into its property-name
/// list: the comma-join head of [`split_legacy_property_encoding`]
/// split back into names (a bare single property name is its own
/// one-element list). The list half of the format's decoder pair —
/// every reader that wants the names (not the raw head) decodes through
/// here, never an inline comma split one layer above the canonical
/// decoder.
pub fn legacy_property_names(property: &str) -> Vec<String> {
    split_legacy_property_encoding(property)
        .0
        .split(',')
        .map(str::to_string)
        .collect()
}

/// One index's typed facts as every reader resolves them, through the fixed
/// precedence the `SHOW INDEXES` row and the `00NA0` note share: the index's
/// [`IndexDetailSpec`], then the owning constraint (a constraint-backed
/// index), then the legacy [`IndexDef`] encoding. No reader re-spells the
/// chain, so a new detail field lands in every rendering at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexFacts {
    pub kind: IndexKindSpec,
    pub relationship: bool,
    pub labels: Vec<String>,
    pub properties: Vec<String>,
    /// CIP-235 `WITH [...]` single-stage-filtering properties (VECTOR).
    pub with_properties: Vec<String>,
}

impl IndexFacts {
    /// Resolve one index's facts. `backing` is the index's index-backed
    /// constraint of the same name, when the caller has already joined it
    /// (the SHOW row); `None` elsewhere. A detail-less, constraint-less row
    /// is the legacy encoding: a node index, its label from the
    /// [`IndexDef`] and its properties from the encoding's head.
    pub fn resolve(
        index: &IndexDef,
        detail: Option<&IndexDetailSpec>,
        backing: Option<&ConstraintDef>,
    ) -> Self {
        let kind = IndexKindSpec::resolve(detail, &index.property);
        let relationship = detail.is_some_and(|detail| detail.relationship)
            || backing.is_some_and(|constraint| constraint.relationship);
        if let Some(detail) = detail {
            return Self {
                kind,
                relationship,
                labels: detail.labels.clone(),
                properties: detail.properties.clone(),
                with_properties: detail.with_properties.clone(),
            };
        }
        if let Some(constraint) = backing {
            return Self {
                kind,
                relationship,
                labels: vec![constraint.label.clone()],
                properties: constraint.properties.clone(),
                with_properties: Vec::new(),
            };
        }
        Self {
            kind,
            relationship,
            labels: vec![index.label.clone()],
            properties: legacy_property_names(&index.property),
            with_properties: Vec::new(),
        }
    }
}

/// Resolve every named index's [`IndexFacts`] with the detail and
/// backing-constraint joins done once, in the caller's order, for the
/// statement-scoped index readers; the SHOW row resolves its own rows
/// through the same [`backing_constraint_by_name`] join this shares.
pub(crate) fn resolve_index_facts(
    indexes: &[relate_catalog::IndexDef],
    details: &std::collections::BTreeMap<String, IndexDetailSpec>,
    constraints: &[relate_catalog::ConstraintDef],
) -> Vec<IndexFacts> {
    indexes
        .iter()
        .map(|index| {
            let detail = details.get(&index.name);
            let backing = backing_constraint_by_name(&index.name, constraints);
            IndexFacts::resolve(index, detail, backing)
        })
        .collect()
}

/// The index-backed constraint of the given name, when one exists — the
/// one owner-constraint join every index reader resolves through (moved
/// from `exec::catalog` beside `IndexFacts`, its every consumer: the
/// SHOW row, the graph-counts row, and [`resolve_index_facts`]).
pub(crate) fn backing_constraint_by_name<'a>(
    name: &str,
    constraints: &'a [relate_catalog::ConstraintDef],
) -> Option<&'a relate_catalog::ConstraintDef> {
    constraints
        .iter()
        .find(|def| def.name == name && def.is_index_backed())
}

/// Typed payload of [`RedoOp::CatalogIndexDetail`]: the catalog facts of
/// a `CREATE INDEX`, captured once at DDL-execution time from the parsed
/// clause. `apply_redo` stores it verbatim -- it never formats a CREATE
/// INDEX string and re-parses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDetailSpec {
    pub kind: IndexKindSpec,
    /// True for a relationship (`FOR ()-[r:T]-()`) index.
    pub relationship: bool,
    /// Node labels, or (for a relationship index) relationship types;
    /// more than one entry only for the `A|B` disjunctive form, empty for
    /// a LOOKUP index.
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub properties: Vec<String>,
    /// CIP-235 `WITH [...]` single-stage-filtering properties (VECTOR).
    #[serde(default)]
    pub with_properties: Vec<String>,
}

impl IndexDetailSpec {
    /// Node `(label, property)` pairs this detail materializes and charges.
    /// Relationship indexes, multi-label indexes, and kinds other than
    /// single-property TEXT or POINT, or RANGE, contribute nothing.
    pub fn covered_pairs(&self) -> Vec<(&str, &str, IndexKindSpec)> {
        if self.relationship || self.labels.len() != 1 || self.properties.is_empty() {
            return Vec::new();
        }
        let label = self.labels[0].as_str();
        match self.kind {
            IndexKindSpec::Text | IndexKindSpec::Point if self.properties.len() == 1 => {
                vec![(label, self.properties[0].as_str(), self.kind)]
            }
            IndexKindSpec::Range => self
                .properties
                .iter()
                .map(|property| (label, property.as_str(), IndexKindSpec::Range))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The node `(label, property)` pairs this index serves an equality seek
    /// on. Derived from [`Self::covered_pairs`] so the entity, label-count
    /// and kind gates live once: only a single-label node RANGE index
    /// contributes, and it contributes **every** covered property — the
    /// executor materializes and seeks each component of a composite index,
    /// so leading-only coverage would under-report it. A relationship index
    /// never yields a node pair, and a property name is never split on a
    /// comma.
    pub(crate) fn range_seek_pairs(&self) -> Vec<(&str, &str)> {
        self.covered_pairs()
            .into_iter()
            .filter_map(|(label, property, kind)| {
                (kind == IndexKindSpec::Range).then_some((label, property))
            })
            .collect()
    }
}

/// Apply the catalog index ops in `ops` to `details`. This is the speculative
/// projection `estimate_redo_delta` uses; committed replay in `apply_redo`
/// stores the same two ops.
pub fn project_index_details(details: &mut BTreeMap<String, IndexDetailSpec>, ops: &[RedoOp]) {
    for op in ops {
        match op {
            RedoOp::CatalogIndexDetail { name, detail, .. } => {
                details.insert(name.clone(), detail.clone());
            }
            RedoOp::DropIndex { name } => {
                details.remove(name);
            }
            _ => {}
        }
    }
}

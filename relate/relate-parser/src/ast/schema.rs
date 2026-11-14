//! Index and constraint DDL clause nodes.

use super::*;

/// The DDL index kind (CIP-11 RANGE/POINT/TEXT, CIP-38 FULLTEXT, the
/// pre-existing VECTOR, and the label/type-token LOOKUP index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    Range,
    Point,
    Text,
    Fulltext,
    Lookup,
    Vector,
}

impl IndexKind {
    /// The canonical lowercase tag persisted in
    /// `relate_storage::Index::index_type` -- also the tag the engine
    /// switches on to decide which (if any) in-memory acceleration
    /// structure to build for a given index (`Engine::rebuild_indexes`).
    /// Unchanged for `Point`/`Vector` from before CIP-11 so existing
    /// vector/point index acceleration keeps working untouched; `Range`
    /// replaces the old ad hoc `"secondary"` tag (still accepted as a
    /// legacy alias when reading it back).
    pub fn storage_tag(&self) -> &'static str {
        match self {
            IndexKind::Range => "range",
            IndexKind::Point => "point",
            IndexKind::Text => "text",
            IndexKind::Fulltext => "fulltext",
            IndexKind::Lookup => "lookup",
            IndexKind::Vector => "vector",
        }
    }

    /// The upper-case display name Neo4j's `SHOW INDEXES` `type` column
    /// uses.
    pub fn display_name(&self) -> &'static str {
        match self {
            IndexKind::Range => "RANGE",
            IndexKind::Point => "POINT",
            IndexKind::Text => "TEXT",
            IndexKind::Fulltext => "FULLTEXT",
            IndexKind::Lookup => "LOOKUP",
            IndexKind::Vector => "VECTOR",
        }
    }
}

/// Which kind of graph element a CREATE INDEX / CREATE CONSTRAINT `FOR`
/// pattern targets (CIP-11's relationship-pattern generalization: `FOR
/// (n:Label)` vs. `FOR ()-[r:TYPE]-()` in all four direction spellings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaEntityType {
    Node,
    Relationship,
}

impl SchemaEntityType {
    /// The upper-case display value Neo4j's `SHOW INDEXES`/`SHOW
    /// CONSTRAINTS` `entityType` column uses.
    pub fn as_str(&self) -> &'static str {
        match self {
            SchemaEntityType::Node => "NODE",
            SchemaEntityType::Relationship => "RELATIONSHIP",
        }
    }
}

/// The name of an index or constraint in CREATE INDEX / CREATE CONSTRAINT /
/// DROP INDEX / DROP CONSTRAINT: either a literal identifier or a `$param`
/// reference resolved against the query's parameter map at execution time
/// (CIP-134).
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaName {
    Literal(String),
    Parameter(String),
}

/// The optional `OPTIONS` tail of a schema DDL command (ADR-0489):
/// the three states the old `options` + `options_param` pair
/// encoded in a comment only.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionsForm {
    /// No `OPTIONS` clause at all.
    Absent,
    /// `OPTIONS { key: <value expression>, ... }`: lowered and evaluated at
    /// execution time.
    Literal(Vec<(String, Expr)>),
    /// `OPTIONS $param`: the parameter must evaluate to a MAP at execution
    /// time, whose entries become the command's literal options.
    Parameter(String),
}

impl OptionsForm {
    /// The literal map's entries — `None` for the two forms that carry no
    /// literal map, so no caller can mistake an absent clause or an
    /// unresolved `$param` for an empty map.
    pub fn literal(&self) -> Option<&[(String, Expr)]> {
        match self {
            Self::Literal(entries) => Some(entries),
            Self::Absent | Self::Parameter(_) => None,
        }
    }

    /// The unresolved parameter's name, if this is that form.
    pub fn parameter(&self) -> Option<&str> {
        match self {
            Self::Parameter(name) => Some(name),
            Self::Absent | Self::Literal(_) => None,
        }
    }

    /// The literal map's entries for in-place rewriting — `None` for the two
    /// forms that carry no literal map, the mutable twin of [`Self::literal`].
    pub fn literal_mut(&mut self) -> Option<&mut [(String, Expr)]> {
        match self {
            Self::Literal(entries) => Some(entries),
            Self::Absent | Self::Parameter(_) => None,
        }
    }
}

/// CREATE INDEX clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndexClause {
    pub name: Option<SchemaName>,
    pub entity_type: SchemaEntityType,
    /// Node labels, or (for a relationship index) relationship types, the
    /// index applies to. More than one entry only for the `A|B` disjunctive
    /// form (CIP-38/CIP-235); empty for a label-less/type-less LOOKUP index
    /// (`FOR (n)` / `FOR ()-[r]-()`).
    pub labels: Vec<SmallName>,
    pub properties: Vec<SmallName>,
    pub if_not_exists: bool,
    pub kind: IndexKind,
    /// True when the property list was written as `ON EACH [...]` (CIP-38
    /// FULLTEXT) rather than a plain `ON (...)` / paren-less `ON ...` list.
    pub on_each: bool,
    /// `WITH [var.prop1, var.prop2, ...]` (CIP-235): additional properties
    /// available for single-stage filtering on a VECTOR index, following
    /// the `ON` vector property. Always empty for every other index kind
    /// and for a vector index that doesn't use the clause.
    pub with_properties: Vec<SmallName>,
    /// The `OPTIONS` tail: absent, a literal map, or a whole-map parameter
    /// (resolved at execution time).
    pub options: OptionsForm,
}

/// DROP INDEX clause.
#[derive(Debug, Clone, PartialEq)]
pub struct DropIndexClause {
    pub name: SchemaName,
    pub if_exists: bool,
    /// Legacy `DROP INDEX ON :Label(property)` target.
    pub legacy_target: Option<(SmallName, SmallName)>,
}

/// CREATE CONSTRAINT clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateConstraintClause {
    pub name: Option<SchemaName>,
    pub entity_type: SchemaEntityType,
    pub label: SmallName,
    pub variable: SmallName,
    pub properties: Vec<SmallName>,
    pub constraint_type: String, // "unique", "exists", "key", "propertyType"
    pub if_not_exists: bool,
    /// The parsed `<value type>` for a `REQUIRE (n.prop) IS :: <type>` /
    /// `IS TYPED <type>` property type constraint (CIP-85), `None` for
    /// every other `constraint_type`.
    pub property_type: Option<TypeExpr>,
    /// The `OPTIONS` tail: absent, a literal map, or a whole-map parameter
    /// (resolved at execution time).
    pub options: OptionsForm,
    /// The target label for a `REQUIRE <alias>:<Label>` label
    /// existence constraint (`constraint_type` one of `"labelExistence"`/
    /// `"sourceLabelExistence"`/`"targetLabelExistence"`), `None` for
    /// every other `constraint_type`.
    pub enforced_label: Option<SmallName>,
}

/// DROP CONSTRAINT clause.
#[derive(Debug, Clone, PartialEq)]
pub struct DropConstraintClause {
    pub name: SchemaName,
    pub if_exists: bool,
}

//! The persisted (serde) graph-type model.
//!
//! Parsing happens once at DDL-execution time ([`GraphTypeSpec::from_body`])
//! and redo replay converts back with [`GraphTypeSpec::to_body`] -- an
//! infallible, mechanical conversion, never a re-parse of Cypher text. Split
//! out of `graph_type/mod.rs` (LABS-818's file-size rule) so the semantic
//! layer and the model each stay under the 1k-line ceiling.

use relate_core::Value;
use relate_error::EngineError;
use relate_names::SmallName;
use relate_parser::{
    Expr, GraphTypeBody, GraphTypeConstraintSpec, GraphTypeConstraintVerb, GraphTypeEdgeTypeSpec,
    GraphTypeElementRef, GraphTypeEntityQualifier, GraphTypeInlineConstraint, GraphTypeInlineVerb,
    GraphTypeNodeTypeSpec, GraphTypePropertyType, LiteralValue, OptionsForm, PredefinedKind,
    TypeExpr, VectorCoordKind,
};
use serde::{Deserialize, Serialize};

use super::default_value::DefaultValue;
use super::syn;

// ---------------------------------------------------------------------
// Persisted graph-type model (serde)
// ---------------------------------------------------------------------

/// The typed, serde-serializable form of a parsed
/// [`GraphTypeBody`] (CIP-176/178/180). Graph-type redo ops
/// (`RedoOp::PutGraphType` / `RedoOp::SetCurrentGraphType`) carry this
/// model and `TheaterData` stores it: parsing happens once at
/// DDL-execution time ([`GraphTypeSpec::from_body`]), and redo replay
/// converts back with [`GraphTypeSpec::to_body`] -- an infallible,
/// mechanical conversion, never a re-parse of Cypher text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GraphTypeSpec {
    #[serde(default)]
    pub node_types: Vec<NodeTypeModel>,
    #[serde(default)]
    pub edge_types: Vec<EdgeTypeModel>,
    #[serde(default)]
    pub constraints: Vec<GraphTypeConstraintModel>,
    #[serde(default)]
    pub drop_constraint_names: Vec<String>,
}

impl GraphTypeSpec {
    /// Captures a parsed [`GraphTypeBody`] into its persisted model. A
    /// property default value whose expression is not one of the
    /// CIP-262 allowed shapes fails here (the same shape the body
    /// validator rejects) rather than being captured lossily -- the
    /// capture stays fail-closed even for a caller that skipped
    /// validation. An unresolved `OPTIONS $param` on a constraint fails
    /// here too (its caller resolves parameters first), so the persisted
    /// model only ever carries literal option values.
    pub fn from_body(body: &GraphTypeBody) -> Result<Self, EngineError> {
        Ok(Self {
            node_types: body
                .node_types
                .iter()
                .map(NodeTypeModel::capture)
                .collect::<Result<_, EngineError>>()?,
            edge_types: body
                .edge_types
                .iter()
                .map(EdgeTypeModel::capture)
                .collect::<Result<_, EngineError>>()?,
            constraints: body
                .constraints
                .iter()
                .map(GraphTypeConstraintModel::capture)
                .collect::<Result<_, EngineError>>()?,
            // Storage boundary: the persisted model keeps String names
            // (serde payloads, ADR-0509's staying-String rule).
            drop_constraint_names: body
                .drop_constraint_names
                .iter()
                .map(|name| name.to_string())
                .collect(),
        })
    }

    /// Rebuilds the parser model for the semantic layer
    /// (`validate_graph_type_body`, `derive_constraints`,
    /// `canonicalize_graph_type_body`, ...). Infallible.
    pub fn to_body(&self) -> GraphTypeBody {
        GraphTypeBody {
            node_types: self
                .node_types
                .iter()
                .map(GraphTypeNodeTypeSpec::from)
                .collect(),
            edge_types: self
                .edge_types
                .iter()
                .map(GraphTypeEdgeTypeSpec::from)
                .collect(),
            constraints: self
                .constraints
                .iter()
                .map(GraphTypeConstraintSpec::from)
                .collect(),
            drop_constraint_names: self
                .drop_constraint_names
                .iter()
                .map(|name| SmallName::from_name(name.as_str()))
                .collect(),
        }
    }
}

/// Persisted form of [`GraphTypeNodeTypeSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTypeModel {
    pub alias: Option<String>,
    pub identifying_label: String,
    #[serde(default)]
    pub implied_labels: Vec<String>,
    #[serde(default)]
    pub properties: Vec<PropertyTypeModel>,
}

impl NodeTypeModel {
    /// Captures one parsed node type specification, fail-closed on a
    /// property default value whose expression is not a CIP-262 allowed
    /// shape (the same shape the body validator rejects).
    pub fn capture(s: &GraphTypeNodeTypeSpec) -> Result<Self, EngineError> {
        Ok(Self {
            alias: s.alias.as_ref().map(|a| a.to_string()),
            identifying_label: s.identifying_label.to_string(),
            implied_labels: s.implied_labels.iter().map(|l| l.to_string()).collect(),
            properties: s
                .properties
                .iter()
                .map(PropertyTypeModel::capture)
                .collect::<Result<_, EngineError>>()?,
        })
    }
}

impl From<&NodeTypeModel> for GraphTypeNodeTypeSpec {
    fn from(m: &NodeTypeModel) -> Self {
        Self {
            alias: m.alias.as_ref().map(|a| SmallName::from_name(a.as_str())),
            identifying_label: SmallName::from_name(m.identifying_label.as_str()),
            implied_labels: m
                .implied_labels
                .iter()
                .map(|l| SmallName::from_name(l.as_str()))
                .collect(),
            properties: m
                .properties
                .iter()
                .map(GraphTypePropertyType::from)
                .collect(),
        }
    }
}

/// Persisted form of [`GraphTypeEdgeTypeSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeTypeModel {
    pub alias: Option<String>,
    pub identifying_label: String,
    #[serde(default)]
    pub properties: Vec<PropertyTypeModel>,
    pub source: ElementRefModel,
    pub target: ElementRefModel,
}

impl EdgeTypeModel {
    /// Captures one parsed edge type specification, fail-closed on a
    /// property default value whose expression is not a CIP-262 allowed
    /// shape (the same shape the body validator rejects).
    pub fn capture(s: &GraphTypeEdgeTypeSpec) -> Result<Self, EngineError> {
        Ok(Self {
            alias: s.alias.as_ref().map(|a| a.to_string()),
            identifying_label: s.identifying_label.to_string(),
            properties: s
                .properties
                .iter()
                .map(PropertyTypeModel::capture)
                .collect::<Result<_, EngineError>>()?,
            source: ElementRefModel::from(&s.source),
            target: ElementRefModel::from(&s.target),
        })
    }
}

impl From<&EdgeTypeModel> for GraphTypeEdgeTypeSpec {
    fn from(m: &EdgeTypeModel) -> Self {
        Self {
            alias: m.alias.as_ref().map(|a| SmallName::from_name(a.as_str())),
            identifying_label: SmallName::from_name(m.identifying_label.as_str()),
            properties: m
                .properties
                .iter()
                .map(GraphTypePropertyType::from)
                .collect(),
            source: GraphTypeElementRef::from(&m.source),
            target: GraphTypeElementRef::from(&m.target),
        }
    }
}

/// Persisted form of [`GraphTypePropertyType`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyTypeModel {
    pub name: String,
    pub value_type: ValueTypeModel,
    /// The property's `DEFAULT ON COMMIT` value (CIP-262), in its
    /// restricted, serde-shaped form.
    #[serde(default)]
    pub default_value: Option<DefaultValue>,
    pub inline_constraint: Option<InlineConstraintModel>,
}

impl PropertyTypeModel {
    /// Captures one parsed property type, fail-closed on a default value
    /// expression that is not a CIP-262 allowed shape.
    pub fn capture(s: &GraphTypePropertyType) -> Result<Self, EngineError> {
        Ok(Self {
            name: s.name.to_string(),
            value_type: ValueTypeModel::from(&s.value_type),
            default_value: s
                .default_value
                .as_ref()
                .map(DefaultValue::from_expr)
                .transpose()?,
            inline_constraint: s
                .inline_constraint
                .as_ref()
                .map(InlineConstraintModel::from),
        })
    }
}

impl From<&PropertyTypeModel> for GraphTypePropertyType {
    fn from(m: &PropertyTypeModel) -> Self {
        Self {
            name: SmallName::from_name(m.name.as_str()),
            value_type: TypeExpr::from(&m.value_type),
            default_value: m.default_value.as_ref().map(DefaultValue::to_expr),
            inline_constraint: m
                .inline_constraint
                .as_ref()
                .map(GraphTypeInlineConstraint::from),
        }
    }
}

/// Persisted form of [`GraphTypeInlineConstraint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineConstraintModel {
    pub verb: InlineVerbModel,
    pub qualifier: Option<EntityQualifierModel>,
}

impl From<&GraphTypeInlineConstraint> for InlineConstraintModel {
    fn from(s: &GraphTypeInlineConstraint) -> Self {
        Self {
            verb: InlineVerbModel::from(s.verb),
            qualifier: s.qualifier.map(EntityQualifierModel::from),
        }
    }
}

impl From<&InlineConstraintModel> for GraphTypeInlineConstraint {
    fn from(m: &InlineConstraintModel) -> Self {
        Self {
            verb: GraphTypeInlineVerb::from(m.verb),
            qualifier: m.qualifier.map(GraphTypeEntityQualifier::from),
        }
    }
}

/// Persisted form of [`GraphTypeInlineVerb`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineVerbModel {
    Key,
    Unique,
}

impl From<GraphTypeInlineVerb> for InlineVerbModel {
    fn from(v: GraphTypeInlineVerb) -> Self {
        match v {
            GraphTypeInlineVerb::Key => Self::Key,
            GraphTypeInlineVerb::Unique => Self::Unique,
        }
    }
}

impl From<InlineVerbModel> for GraphTypeInlineVerb {
    fn from(m: InlineVerbModel) -> Self {
        match m {
            InlineVerbModel::Key => Self::Key,
            InlineVerbModel::Unique => Self::Unique,
        }
    }
}

/// Persisted form of [`GraphTypeEntityQualifier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityQualifierModel {
    Node,
    Rel,
}

impl From<GraphTypeEntityQualifier> for EntityQualifierModel {
    fn from(q: GraphTypeEntityQualifier) -> Self {
        match q {
            GraphTypeEntityQualifier::Node => Self::Node,
            GraphTypeEntityQualifier::Rel => Self::Rel,
        }
    }
}

impl From<EntityQualifierModel> for GraphTypeEntityQualifier {
    fn from(m: EntityQualifierModel) -> Self {
        match m {
            EntityQualifierModel::Node => Self::Node,
            EntityQualifierModel::Rel => Self::Rel,
        }
    }
}

/// Persisted form of [`GraphTypeElementRef`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ElementRefModel {
    Any,
    AliasRef(String),
    InSitu {
        alias: Option<String>,
        label: String,
        implies: bool,
    },
}

impl From<&GraphTypeElementRef> for ElementRefModel {
    fn from(r: &GraphTypeElementRef) -> Self {
        match r {
            GraphTypeElementRef::Any => Self::Any,
            GraphTypeElementRef::AliasRef(a) => Self::AliasRef(a.to_string()),
            GraphTypeElementRef::InSitu {
                alias,
                label,
                implies,
            } => Self::InSitu {
                alias: alias.as_ref().map(|a| a.to_string()),
                label: label.to_string(),
                implies: *implies,
            },
        }
    }
}

impl From<&ElementRefModel> for GraphTypeElementRef {
    fn from(m: &ElementRefModel) -> Self {
        match m {
            ElementRefModel::Any => Self::Any,
            ElementRefModel::AliasRef(a) => Self::AliasRef(SmallName::from_name(a.as_str())),
            ElementRefModel::InSitu {
                alias,
                label,
                implies,
            } => Self::InSitu {
                alias: alias.as_ref().map(|a| SmallName::from_name(a.as_str())),
                label: SmallName::from_name(label.as_str()),
                implies: *implies,
            },
        }
    }
}

/// Persisted form of [`TypeExpr`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValueTypeModel {
    Predefined {
        kind: PredefinedTypeModel,
        not_null: bool,
    },
    List {
        element: Box<ValueTypeModel>,
        not_null: bool,
    },
    PropertyValue {
        not_null: bool,
    },
    Union(Vec<ValueTypeModel>),
    Vector {
        coord: Option<VectorCoordModel>,
        dimension: Option<i64>,
        not_null: bool,
    },
}

impl From<&TypeExpr> for ValueTypeModel {
    fn from(t: &TypeExpr) -> Self {
        match t {
            TypeExpr::Predefined { kind, not_null } => Self::Predefined {
                kind: PredefinedTypeModel::from(*kind),
                not_null: *not_null,
            },
            TypeExpr::List { element, not_null } => Self::List {
                element: Box::new(ValueTypeModel::from(element.as_ref())),
                not_null: *not_null,
            },
            TypeExpr::PropertyValue { not_null } => Self::PropertyValue {
                not_null: *not_null,
            },
            TypeExpr::Union(parts) => Self::Union(parts.iter().map(ValueTypeModel::from).collect()),
            TypeExpr::Vector {
                coord,
                dimension,
                not_null,
            } => Self::Vector {
                coord: coord.map(VectorCoordModel::from),
                dimension: *dimension,
                not_null: *not_null,
            },
        }
    }
}

impl From<&ValueTypeModel> for TypeExpr {
    fn from(m: &ValueTypeModel) -> Self {
        match m {
            ValueTypeModel::Predefined { kind, not_null } => Self::Predefined {
                kind: PredefinedKind::from(*kind),
                not_null: *not_null,
            },
            ValueTypeModel::List { element, not_null } => Self::List {
                element: Box::new(TypeExpr::from(element.as_ref())),
                not_null: *not_null,
            },
            ValueTypeModel::PropertyValue { not_null } => Self::PropertyValue {
                not_null: *not_null,
            },
            ValueTypeModel::Union(parts) => Self::Union(parts.iter().map(TypeExpr::from).collect()),
            ValueTypeModel::Vector {
                coord,
                dimension,
                not_null,
            } => Self::Vector {
                coord: coord.map(VectorCoordKind::from),
                dimension: *dimension,
                not_null: *not_null,
            },
        }
    }
}

/// Persisted form of [`PredefinedKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredefinedTypeModel {
    Nothing,
    Null,
    Any,
    Boolean,
    String,
    Integer,
    Float,
    Date,
    Duration,
    Point,
    LocalTime,
    ZonedTime,
    LocalDateTime,
    ZonedDateTime,
    Node,
    Relationship,
    Path,
    Map,
    /// CIP-246 UUID, carried since CIP-262: the property-default showcase
    /// is a UUID property defaulting to `uuid()`, and the model's
    /// historical narrowing to STRING broke the SHOW/DROP roundtrip for
    /// it (a stored `UUID` re-rendered as `STRING` never matched a DROP's
    /// restated `UUID`). Old persisted images deserialize their
    /// `String`s unchanged.
    Uuid,
}

impl From<PredefinedKind> for PredefinedTypeModel {
    fn from(k: PredefinedKind) -> Self {
        match k {
            PredefinedKind::Nothing => Self::Nothing,
            PredefinedKind::Null => Self::Null,
            PredefinedKind::Any => Self::Any,
            PredefinedKind::Boolean => Self::Boolean,
            PredefinedKind::String => Self::String,
            PredefinedKind::Integer => Self::Integer,
            PredefinedKind::Float => Self::Float,
            PredefinedKind::Date => Self::Date,
            PredefinedKind::Duration => Self::Duration,
            PredefinedKind::Point => Self::Point,
            PredefinedKind::LocalTime => Self::LocalTime,
            PredefinedKind::ZonedTime => Self::ZonedTime,
            PredefinedKind::LocalDateTime => Self::LocalDateTime,
            PredefinedKind::ZonedDateTime => Self::ZonedDateTime,
            PredefinedKind::Node => Self::Node,
            PredefinedKind::Relationship => Self::Relationship,
            PredefinedKind::Path => Self::Path,
            PredefinedKind::Map => Self::Map,
            PredefinedKind::Uuid => Self::Uuid,
        }
    }
}

impl From<PredefinedTypeModel> for PredefinedKind {
    fn from(m: PredefinedTypeModel) -> Self {
        match m {
            PredefinedTypeModel::Nothing => Self::Nothing,
            PredefinedTypeModel::Null => Self::Null,
            PredefinedTypeModel::Any => Self::Any,
            PredefinedTypeModel::Boolean => Self::Boolean,
            PredefinedTypeModel::String => Self::String,
            PredefinedTypeModel::Integer => Self::Integer,
            PredefinedTypeModel::Float => Self::Float,
            PredefinedTypeModel::Date => Self::Date,
            PredefinedTypeModel::Duration => Self::Duration,
            PredefinedTypeModel::Point => Self::Point,
            PredefinedTypeModel::LocalTime => Self::LocalTime,
            PredefinedTypeModel::ZonedTime => Self::ZonedTime,
            PredefinedTypeModel::LocalDateTime => Self::LocalDateTime,
            PredefinedTypeModel::ZonedDateTime => Self::ZonedDateTime,
            PredefinedTypeModel::Node => Self::Node,
            PredefinedTypeModel::Relationship => Self::Relationship,
            PredefinedTypeModel::Path => Self::Path,
            PredefinedTypeModel::Map => Self::Map,
            PredefinedTypeModel::Uuid => Self::Uuid,
        }
    }
}

/// Persisted form of [`VectorCoordKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VectorCoordModel {
    Int8,
    Int16,
    Int32,
    Int64,
    Float16,
    BFloat16,
    Float32,
    Float64,
}

impl From<VectorCoordKind> for VectorCoordModel {
    fn from(k: VectorCoordKind) -> Self {
        match k {
            VectorCoordKind::Int8 => Self::Int8,
            VectorCoordKind::Int16 => Self::Int16,
            VectorCoordKind::Int32 => Self::Int32,
            VectorCoordKind::Int64 => Self::Int64,
            VectorCoordKind::Float16 => Self::Float16,
            VectorCoordKind::BFloat16 => Self::BFloat16,
            VectorCoordKind::Float32 => Self::Float32,
            VectorCoordKind::Float64 => Self::Float64,
        }
    }
}

impl From<VectorCoordModel> for VectorCoordKind {
    fn from(m: VectorCoordModel) -> Self {
        match m {
            VectorCoordModel::Int8 => Self::Int8,
            VectorCoordModel::Int16 => Self::Int16,
            VectorCoordModel::Int32 => Self::Int32,
            VectorCoordModel::Int64 => Self::Int64,
            VectorCoordModel::Float16 => Self::Float16,
            VectorCoordModel::BFloat16 => Self::BFloat16,
            VectorCoordModel::Float32 => Self::Float32,
            VectorCoordModel::Float64 => Self::Float64,
        }
    }
}

/// Persisted form of [`GraphTypeConstraintSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphTypeConstraintModel {
    pub name: Option<String>,
    pub target: ElementRefModel,
    pub is_edge: bool,
    #[serde(default)]
    pub properties: Vec<String>,
    pub kind: ConstraintVerbModel,
    pub property_type: Option<ValueTypeModel>,
    /// The constraint's literal `OPTIONS` entries. A whole-map `OPTIONS
    /// $param` is resolved before capture, so the persisted model keeps the
    /// literal vocabulary only (a value outside it is
    /// [`OptionValueModel::NonLiteral`]).
    #[serde(default)]
    pub options: Vec<(String, OptionValueModel)>,
}

impl GraphTypeConstraintModel {
    /// Captures one parsed constraint spec, fail-closed on an unresolved
    /// `OPTIONS $param`: the persisted model carries literal options only,
    /// so a missed resolution is an error, never a silently dropped map.
    fn capture(s: &GraphTypeConstraintSpec) -> Result<Self, EngineError> {
        let options = match &s.options {
            OptionsForm::Absent => Vec::new(),
            OptionsForm::Literal(entries) => entries
                .iter()
                .map(|(key, value)| (key.clone(), OptionValueModel::from(value)))
                .collect(),
            OptionsForm::Parameter(name) => {
                return Err(syn(format!(
                    "unresolved OPTIONS ${name}: graph-type constraint options must be resolved before capture"
                )))
            }
        };
        Ok(Self {
            name: s.name.as_ref().map(|n| n.to_string()),
            target: ElementRefModel::from(&s.target),
            is_edge: s.is_edge,
            properties: s.properties.iter().map(|p| p.to_string()).collect(),
            kind: ConstraintVerbModel::from(&s.kind),
            property_type: s.property_type.as_ref().map(ValueTypeModel::from),
            options,
        })
    }
}

impl From<&GraphTypeConstraintModel> for GraphTypeConstraintSpec {
    fn from(m: &GraphTypeConstraintModel) -> Self {
        Self {
            name: m.name.as_ref().map(|n| SmallName::from_name(n.as_str())),
            target: GraphTypeElementRef::from(&m.target),
            is_edge: m.is_edge,
            properties: m
                .properties
                .iter()
                .map(|p| SmallName::from_name(p.as_str()))
                .collect(),
            kind: GraphTypeConstraintVerb::from(m.kind),
            property_type: m.property_type.as_ref().map(TypeExpr::from),
            options: if m.options.is_empty() {
                OptionsForm::Absent
            } else {
                OptionsForm::Literal(
                    m.options
                        .iter()
                        .map(|(k, v)| (k.clone(), Expr::from(v)))
                        .collect(),
                )
            },
        }
    }
}

/// Persisted form of [`GraphTypeConstraintVerb`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstraintVerbModel {
    Key,
    Unique,
    Exists,
    PropertyType,
}

impl From<&GraphTypeConstraintVerb> for ConstraintVerbModel {
    fn from(v: &GraphTypeConstraintVerb) -> Self {
        match v {
            GraphTypeConstraintVerb::Key => Self::Key,
            GraphTypeConstraintVerb::Unique => Self::Unique,
            GraphTypeConstraintVerb::Exists => Self::Exists,
            GraphTypeConstraintVerb::PropertyType => Self::PropertyType,
        }
    }
}

impl From<ConstraintVerbModel> for GraphTypeConstraintVerb {
    fn from(m: ConstraintVerbModel) -> Self {
        match m {
            ConstraintVerbModel::Key => Self::Key,
            ConstraintVerbModel::Unique => Self::Unique,
            ConstraintVerbModel::Exists => Self::Exists,
            ConstraintVerbModel::PropertyType => Self::PropertyType,
        }
    }
}

/// A graph-type constraint `OPTIONS` value. The canonical renderer
/// (`canonical_option_value`) maps the five literal kinds to text and
/// every other expression to `?`; the model mirrors exactly that
/// distinction, so [`GraphTypeSpec::to_body`] reproduces the same
/// canonical rendering as the parsed source body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OptionValueModel {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    /// A non-literal OPTIONS expression: its canonical rendering is `?`,
    /// byte-identical to what the source expression canonicalized to.
    NonLiteral,
}

impl OptionValueModel {
    /// The one resolution-time `Value` → model conversion for a whole-map
    /// `OPTIONS $param`: the five persisted literal kinds, and `NonLiteral`
    /// for every other value — the model's own vocabulary, so the resolver
    /// never re-spells the match (`Expr::from(&OptionValueModel::from_value(v))`
    /// renders the canonical `?`).
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(i) => Self::Integer(*i),
            Value::Float(f) => Self::Float(*f),
            Value::String(s) => Self::String(s.to_string()),
            _ => Self::NonLiteral,
        }
    }
}

impl From<&Expr> for OptionValueModel {
    fn from(e: &Expr) -> Self {
        match e {
            Expr::Literal(LiteralValue::Null) => Self::Null,
            Expr::Literal(LiteralValue::Bool(b)) => Self::Bool(*b),
            Expr::Literal(LiteralValue::Integer(i)) => Self::Integer(*i),
            Expr::Literal(LiteralValue::Float(f)) => Self::Float(*f),
            Expr::Literal(LiteralValue::String(s)) => Self::String(s.clone()),
            _ => Self::NonLiteral,
        }
    }
}

impl From<&OptionValueModel> for Expr {
    fn from(v: &OptionValueModel) -> Self {
        match v {
            OptionValueModel::Null => Expr::Literal(LiteralValue::Null),
            OptionValueModel::Bool(b) => Expr::Literal(LiteralValue::Bool(*b)),
            OptionValueModel::Integer(i) => Expr::Literal(LiteralValue::Integer(*i)),
            OptionValueModel::Float(f) => Expr::Literal(LiteralValue::Float(*f)),
            OptionValueModel::String(s) => Expr::Literal(LiteralValue::String(s.clone())),
            OptionValueModel::NonLiteral => Expr::OptionValuePlaceholder,
        }
    }
}

//! The token/property compiler half of the ontology→graph-type mapping:
//! what each main `Neo4jType` token compiles to in a CIP-176 property
//! type (ADR-0364 §5.2's table), and an owner's typed property
//! declarations. Split from the converter's body so the token table and
//! the declaration compiler are addressable beside the constraint
//! compiler in [`super::constraints`].

use relate_names::SmallName;
use relate_parser::{GraphTypePropertyType, PredefinedKind, TypeExpr, VectorCoordKind};

use super::{OntologyError, Owner};
use crate::model::{Neo4jType, Property};

/// What one main `Neo4jType` token compiles to in a CIP-176 property type.
///
/// The match producing it is exhaustive over [`Neo4jType`]: a token added
/// to main's closed enum must be classified here before the crate
/// compiles, so no token can be silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenMapping {
    /// `ANY` and `LIST<ANY>`: the document says "unconstrained", so no
    /// declaration is compiled (ADR-0228). An absent `type` is handled
    /// separately and means the same.
    DeclaresNothing,
    /// A predefined CIP-176 leaf of the same name.
    Predefined(PredefinedKind),
    /// `LIST<X>`: CIP-176 property types are closed, so the element is
    /// always `NOT NULL`.
    List(PredefinedKind),
    /// `VECTOR<X>`: the coordinate kind, with the property's `dimension`.
    Vector(VectorCoordKind),
    /// A token with no plain CIP-176 counterpart. The converter fails
    /// loudly for it (ADR-0364 §5.2): widening would silently change the
    /// declared constraint, and declaring nothing would assert less than
    /// the document.
    Unsupported,
}

impl Neo4jType {
    /// The ONE token classification, read everywhere the mapping decides
    /// between a declaration, "declares nothing", and a loud failure.
    pub(super) fn mapping(self) -> TokenMapping {
        use Neo4jType as T;
        match self {
            T::Any | T::ListAny => TokenMapping::DeclaresNothing,
            T::Boolean => TokenMapping::Predefined(PredefinedKind::Boolean),
            T::ListBoolean => TokenMapping::List(PredefinedKind::Boolean),
            T::Date => TokenMapping::Predefined(PredefinedKind::Date),
            T::ListDate => TokenMapping::List(PredefinedKind::Date),
            T::Duration => TokenMapping::Predefined(PredefinedKind::Duration),
            T::ListDuration => TokenMapping::List(PredefinedKind::Duration),
            T::Float32 | T::ListFloat32 => TokenMapping::Unsupported,
            T::Float => TokenMapping::Predefined(PredefinedKind::Float),
            T::ListFloat => TokenMapping::List(PredefinedKind::Float),
            T::Integer8
            | T::ListInteger8
            | T::Integer16
            | T::ListInteger16
            | T::Integer32
            | T::ListInteger32 => TokenMapping::Unsupported,
            T::Integer => TokenMapping::Predefined(PredefinedKind::Integer),
            T::ListInteger => TokenMapping::List(PredefinedKind::Integer),
            T::LocalDatetime => TokenMapping::Predefined(PredefinedKind::LocalDateTime),
            T::ListLocalDatetime => TokenMapping::List(PredefinedKind::LocalDateTime),
            T::LocalTime => TokenMapping::Predefined(PredefinedKind::LocalTime),
            T::ListLocalTime => TokenMapping::List(PredefinedKind::LocalTime),
            T::Point => TokenMapping::Predefined(PredefinedKind::Point),
            T::ListPoint => TokenMapping::List(PredefinedKind::Point),
            T::String => TokenMapping::Predefined(PredefinedKind::String),
            T::ListString => TokenMapping::List(PredefinedKind::String),
            T::VectorFloat => TokenMapping::Vector(VectorCoordKind::Float64),
            T::VectorFloat32 => TokenMapping::Vector(VectorCoordKind::Float32),
            T::VectorInteger => TokenMapping::Vector(VectorCoordKind::Int64),
            T::VectorInteger32 => TokenMapping::Vector(VectorCoordKind::Int32),
            T::VectorInteger16 => TokenMapping::Vector(VectorCoordKind::Int16),
            T::VectorInteger8 => TokenMapping::Vector(VectorCoordKind::Int8),
            T::ZonedDatetime => TokenMapping::Predefined(PredefinedKind::ZonedDateTime),
            T::ListZonedDatetime => TokenMapping::List(PredefinedKind::ZonedDateTime),
            T::ZonedTime => TokenMapping::Predefined(PredefinedKind::ZonedTime),
            T::ListZonedTime => TokenMapping::List(PredefinedKind::ZonedTime),
            T::Uuid => TokenMapping::Predefined(PredefinedKind::Uuid),
        }
    }

    /// Whether this token declares no CIP-176 property type — such a
    /// property gets no declaration, and its `must_exist: true` becomes a
    /// standalone existence constraint. `Unsupported` is not "nothing":
    /// its failure is raised before this classification is consulted.
    pub(super) fn declares_nothing(self) -> bool {
        self.mapping() == TokenMapping::DeclaresNothing
    }
}

/// Whether `property` declares no CIP-176 property type — no token at
/// all, or a token that declares nothing — the ONE decision the two
/// halves share: `property_decl` compiles no declaration for it, and
/// the constraint compiler's `must_exist` shorthand becomes a
/// standalone existence constraint for it.
pub(super) fn declares_nothing(property: &Property) -> bool {
    property
        .property_type
        .is_none_or(Neo4jType::declares_nothing)
}

/// An owner's typed property declarations, in document order. A property
/// that declares nothing contributes no declaration (and may still
/// contribute an existence constraint in [`super::constraints`]).
pub(super) fn property_decls(
    owner: &Owner<'_>,
) -> Result<Vec<GraphTypePropertyType>, OntologyError> {
    let mut declarations = Vec::with_capacity(owner.properties().len());
    for property in owner.properties() {
        if let Some(declaration) = property_decl(owner, property)? {
            declarations.push(declaration);
        }
    }
    Ok(declarations)
}

/// A property's type declaration: the CIP-176 value type of the declared
/// token, `NOT NULL` when `must_exist: true`. Returns `None` exactly when
/// the property declares nothing — no token at all, or `ANY`/`LIST<ANY>`.
///
/// # Errors
///
/// [`OntologyError::Mapping`] for an empty-named property that declares a
/// type, a token with no plain CIP-176 counterpart, or a non-positive
/// vector dimension (ADR-0364 §5.4).
fn property_decl(
    owner: &Owner<'_>,
    property: &Property,
) -> Result<Option<GraphTypePropertyType>, OntologyError> {
    let not_null = property.must_exist == Some(true);
    let Some(ty) = property.property_type else {
        return Ok(None);
    };
    if property.id.is_empty() {
        return Err(owner.mapping(format!(
            "property '' declares type '{}': a graph-type property name is non-empty",
            ty.as_str()
        )));
    }
    let value_type = match ty.mapping() {
        TokenMapping::DeclaresNothing => return Ok(None),
        TokenMapping::Unsupported => {
            return Err(owner.mapping(format!(
                "property '{}' has type '{}', which has no CIP-176 property type",
                property.id,
                ty.as_str()
            )));
        }
        TokenMapping::Predefined(kind) => TypeExpr::Predefined { kind, not_null },
        TokenMapping::List(kind) => TypeExpr::List {
            // CIP-176 property types are closed: a list's inner type is
            // mandatory NOT NULL, so the spec's `LIST<X>` maps to
            // `LIST<X NOT NULL>`.
            element: Box::new(TypeExpr::Predefined {
                kind,
                not_null: true,
            }),
            not_null,
        },
        TokenMapping::Vector(coord) => {
            if let Some(dimension) = property.dimension.filter(|dimension| *dimension <= 0) {
                return Err(owner.mapping(format!(
                    "property '{}' has vector dimension {dimension}: a graph-type vector \
                     needs a positive dimension",
                    property.id
                )));
            }
            TypeExpr::Vector {
                coord: Some(coord),
                dimension: property.dimension,
                not_null,
            }
        }
    };
    Ok(Some(GraphTypePropertyType {
        name: SmallName::from_name(property.id.as_str()),
        value_type,
        default_value: None,
        inline_constraint: None,
    }))
}

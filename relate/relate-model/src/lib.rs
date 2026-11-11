//! Relate data model types.
//!
//! Defines the core value type system used throughout Relate,
//! including the `CypherValue` enum that replaces Go's `interface{}`.

pub mod canonical_key;
pub mod identifier;
pub mod name_list;
pub mod point;
pub mod schema;
pub mod serde_value;
pub mod value;
mod vector;
pub mod wire;

pub use indexmap::IndexMap;
pub use schema::{
    Constraint, ConstraintClassification, ConstraintType, EnforcedLabelSite, EntityKind,
};
pub use value::{CypherValue, GeometryValue, VectorCoordKind};
pub use vector::coordinates::{dimension_range_text, VectorCoordToken, VECTOR_DIMENSION_RANGE};

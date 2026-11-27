//! The literal-kind vocabulary: which `Value` kinds a Cypher literal can
//! spell. The front end's static-type inference reads it from here, so the
//! literal-type spelling has one home.

use crate::Value;

/// The authored detail the `toString()` value-level type error carries after
/// its `InvalidArgumentValue:` token — one home for the executor's refusal
/// of the one kind with no string form (a byte array).
pub const TOSTRING_TYPE_ERROR_DETAIL: &str = "toString()";

/// The kinds a Cypher literal can spell — the one `Value` → literal-kind
/// table. Exhaustive over `Value`, so a new variant makes its author state
/// whether a literal can spell it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiteralValueKind {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    List,
    Map,
}

impl LiteralValueKind {
    /// The kind of a value a literal can spell; `None` for every
    /// runtime-only kind (an entity, a path, a temporal, a point, a UUID, a
    /// vector, a byte array).
    pub fn of_value(value: &Value) -> Option<Self> {
        Some(match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Boolean,
            Value::Int(_) => Self::Integer,
            Value::Float(_) => Self::Float,
            Value::String(_) => Self::String,
            Value::List(_) => Self::List,
            Value::Map(_) => Self::Map,
            Value::Node(_)
            | Value::Relationship(_)
            | Value::Path(_)
            | Value::Temporal(_)
            | Value::Point(_)
            | Value::Uuid(_)
            | Value::Vector { .. }
            | Value::Bytes(_) => return None,
        })
    }

    /// The kind's Cypher type-name spelling — the one vocabulary the static
    /// rules' rejection details render.
    pub fn name(self) -> &'static str {
        self.spellings().0
    }

    /// The kind's name as live's `Type mismatch: … but was <Type>`
    /// sentences spell it (CamelCase, not [`Self::name`]'s UPPER wire
    /// name) — the rendering the static argument checks use when they
    /// quote a kind in an authored mismatch sentence (other sites render
    /// their own live texts).
    pub fn camel_name(self) -> &'static str {
        self.spellings().1
    }

    /// The kind's two spellings — the UPPER wire name and live's CamelCase
    /// mismatch-text name — from one table, so a variant is named once.
    fn spellings(self) -> (&'static str, &'static str) {
        match self {
            Self::Null => ("NULL", "Null"),
            Self::Boolean => ("BOOLEAN", "Boolean"),
            Self::Integer => ("INTEGER", "Integer"),
            Self::Float => ("FLOAT", "Float"),
            Self::String => ("STRING", "String"),
            Self::List => ("LIST", "List"),
            Self::Map => ("MAP", "Map"),
        }
    }

    /// Whether the kind is a number (INTEGER or FLOAT).
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Float)
    }
}

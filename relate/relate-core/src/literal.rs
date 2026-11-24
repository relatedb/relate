//! The literal-kind vocabulary: which `Value` kinds a Cypher literal can
//! spell, their CIP-271 element class, and the one homogeneous-list rule.
//! The front end's static-type inference reads them from here, so the
//! literal-type spelling and the homogeneity rule have one home. The
//! executor deliberately does not: Neo4j draws the rule's boundary at the
//! static type (a property or parameter read renders), so a runtime value
//! carries no application of it.

use crate::Value;

/// The authored detail CIP-271's `toString()` rejection carries after its
/// `InvalidArgumentValue:` token — one home for the compile-phase front-end
/// rejection and the runtime value-level error.
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
        match self {
            Self::Null => "NULL",
            Self::Boolean => "BOOLEAN",
            Self::Integer => "INTEGER",
            Self::Float => "FLOAT",
            Self::String => "STRING",
            Self::List => "LIST",
            Self::Map => "MAP",
        }
    }

    /// Whether the kind is a number (INTEGER or FLOAT).
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Integer | Self::Float)
    }
}

/// The element class CIP-271's homogeneous-list gate compares: INTEGER and
/// FLOAT are one numeric element type, every other literal kind keeps its
/// own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiteralElementClass {
    Numeric,
    Boolean,
    String,
    List,
    Map,
}

impl LiteralElementClass {
    /// The class of a literal kind, or `None` for null: a null element makes
    /// the list `LIST<ANY>`, which is the CIP-271 rule rather than an
    /// unclassifiable value.
    pub fn of_kind(kind: LiteralValueKind) -> Option<Self> {
        Some(match kind {
            LiteralValueKind::Integer | LiteralValueKind::Float => Self::Numeric,
            LiteralValueKind::Boolean => Self::Boolean,
            LiteralValueKind::String => Self::String,
            LiteralValueKind::List => Self::List,
            LiteralValueKind::Map => Self::Map,
            LiteralValueKind::Null => return None,
        })
    }

    /// The class of a value, or `None` for null and every runtime-only kind.
    pub fn of_value(value: &Value) -> Option<Self> {
        LiteralValueKind::of_value(value).and_then(Self::of_kind)
    }
}

/// The one CIP-271 homogeneity rule, shared by the front-end's static
/// `toString()` check (over statically-known literal types) and the
/// executor's value-level one (over runtime values): `Some(class)` when the
/// list is non-empty and every element has the same known class; `None` for
/// an empty list, a null or otherwise unclassifiable element, or two classes
/// mixing — all of which are `LIST<ANY>`, which renders.
pub fn homogeneous_element_class(
    classes: impl IntoIterator<Item = Option<LiteralElementClass>>,
) -> Option<LiteralElementClass> {
    let mut classes = classes.into_iter();
    let first = classes.next()??;
    classes.all(|class| class == Some(first)).then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::Temporal;

    /// The one CIP-271 homogeneity rule: numbers are one class, null and
    /// non-literal kinds break it, and an empty list is `LIST<ANY>`.
    #[test]
    fn homogeneous_element_class_is_the_one_rule() {
        let homogeneous = |values: &[Value]| {
            homogeneous_element_class(values.iter().map(LiteralElementClass::of_value)).is_some()
        };
        assert!(homogeneous(&[Value::Int(1), Value::Int(2)]));
        assert!(homogeneous(&[Value::Int(1), Value::Float(2.0)]));
        assert!(homogeneous(&[Value::Bool(true), Value::Bool(false)]));
        assert!(homogeneous(&[
            Value::String("a".into()),
            Value::String("b".into())
        ]));
        assert!(homogeneous(&[Value::List(vec![Value::Int(1)].into())]));
        assert!(!homogeneous(&[]));
        assert!(!homogeneous(&[Value::Int(1), Value::String("a".into())]));
        assert!(!homogeneous(&[Value::Int(1), Value::Null]));
        // A kind a literal cannot spell leaves the list `LIST<ANY>`.
        assert!(!homogeneous(&[Value::Temporal(Temporal::CivilDate {
            year: 2020,
            month: 1,
            day: 1,
        })]));
    }
}

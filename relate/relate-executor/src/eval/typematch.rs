//! `IS TYPED` / property-value type checks against `TypeExpr`.

use relate_core::temporal::TemporalKind;
use relate_core::Value;
use relate_ir::{PredefinedType, TypeExpr};

pub(crate) fn value_matches_type(value: &Value, ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Predefined { kind, not_null } => {
            if value.is_null() {
                return !not_null && *kind != PredefinedType::Nothing;
            }
            match kind {
                PredefinedType::Nothing | PredefinedType::Null => false,
                PredefinedType::Any => true,
                PredefinedType::Boolean => matches!(value, Value::Bool(_)),
                PredefinedType::String => matches!(value, Value::String(_)),
                PredefinedType::Uuid => matches!(value, Value::Uuid(_)),
                PredefinedType::Integer => matches!(value, Value::Int(_)),
                PredefinedType::Float => matches!(value, Value::Float(_)),
                // Every temporal arm reads core's one kind partition
                // (`Temporal::kind`) — never a second variant list, so a
                // representation added to a kind answers here unchanged
                // (the civil far-year forms included, ADR-0683).
                PredefinedType::Date => matches_kind(value, TemporalKind::Date),
                PredefinedType::Duration => matches_kind(value, TemporalKind::Duration),
                PredefinedType::Point => matches!(value, Value::Point(_)),
                PredefinedType::LocalTime => matches_kind(value, TemporalKind::LocalTime),
                PredefinedType::ZonedTime => matches_kind(value, TemporalKind::Time),
                PredefinedType::LocalDateTime => matches_kind(value, TemporalKind::LocalDateTime),
                PredefinedType::ZonedDateTime => matches_kind(value, TemporalKind::DateTime),
                PredefinedType::Node => matches!(value, Value::Node(_)),
                PredefinedType::Relationship => matches!(value, Value::Relationship(_)),
                PredefinedType::Path => matches!(value, Value::Path(_)),
                PredefinedType::Map => matches!(value, Value::Map(_)),
            }
        }
        TypeExpr::List { element, not_null } => {
            if value.is_null() {
                !not_null
            } else {
                match value {
                    Value::List(xs) => xs.iter().all(|x| value_matches_type(x, element)),
                    // A byte array is a LIST<INTEGER NOT NULL> (ADR-0327).
                    Value::Bytes(b) => b
                        .iter()
                        .all(|&x| value_matches_type(&Value::Int(x.into()), element)),
                    _ => false,
                }
            }
        }
        TypeExpr::PropertyValue { not_null } => {
            if value.is_null() {
                !not_null
            } else {
                is_property_value(value)
            }
        }
        TypeExpr::Union(items) => items.iter().any(|t| value_matches_type(value, t)),
        TypeExpr::Vector {
            coord,
            dimension,
            not_null,
        } => {
            if value.is_null() {
                return !not_null;
            }
            matches!(
                value,
                Value::Vector { coord: actual, values }
                    if coord.as_ref().is_none_or(|c| c == actual.canonical_name())
                        && dimension.is_none_or(|d| d as usize == values.len())
            )
        }
    }
}

/// Whether `value` is storable as a property value — the exact predicate
/// `SET n.p = value` admits, derived from the one property-value walk
/// ([`crate::props::property_value_invalidity`]) instead of a second
/// taxonomy: a value is a property value when it is non-null and the walk
/// finds no invalidity. An empty list is therefore a property value (the
/// walk has no item to reject), matching `SET n.p = []` and
/// `[] IS :: ANY PROPERTY VALUE` on the reference engine.
pub(crate) fn is_property_value(value: &Value) -> bool {
    !value.is_null() && crate::props::property_value_invalidity(value).is_none()
}

/// Whether `value` is a temporal of `kind` — the one predicate every
/// temporal `IS ::` arm reads.
fn matches_kind(value: &Value, kind: TemporalKind) -> bool {
    value.as_temporal().is_some_and(|t| t.kind() == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: u128 = 0xf066_ecfc_5151_4121_8a36_b35e_00d7_2918;

    /// A property list is homogeneous: a mixed string/integer list is not a
    /// property value, and neither is a list with a nested list or a null.
    #[test]
    fn property_value_lists_are_homogeneous() {
        let list = |xs: Vec<Value>| Value::List(xs.into());
        assert!(is_property_value(&list(vec![Value::Int(42)])));
        // Regression: the property-value predicate is derived from the
        // storage walk, so an empty list stays a property value (`SET n.p =
        // []` stores it, and the reference engine answers `[] IS :: ANY
        // PROPERTY VALUE` true). A second scalar-family taxonomy had made
        // this false.
        assert!(is_property_value(&list(vec![])));
        assert!(value_matches_type(
            &list(vec![]),
            &TypeExpr::PropertyValue { not_null: true }
        ));
        assert!(is_property_value(&list(vec![
            Value::Int(1),
            Value::Float(2.5)
        ])));
        assert!(!is_property_value(&list(vec![
            Value::String("abc".into()),
            Value::Int(42)
        ])));
        assert!(!is_property_value(&list(vec![Value::Null])));
        assert!(!is_property_value(&list(vec![list(vec![Value::Int(42)])])));
    }

    // CIP-246: a UUID is a storable property type, so `IS :: ANY PROPERTY
    // VALUE` holds for it alone and as a list element, and the plain type
    // arms keep their answers.
    #[test]
    fn uuid_is_a_property_value() {
        let u = Value::Uuid(UUID);
        let list = Value::List(vec![u.clone()].into());
        let any_property_value = TypeExpr::PropertyValue { not_null: true };
        assert!(value_matches_type(&u, &any_property_value));
        assert!(value_matches_type(&list, &any_property_value));
        assert!(value_matches_type(
            &Value::Null,
            &TypeExpr::PropertyValue { not_null: false }
        ));
        assert!(!value_matches_type(&Value::Null, &any_property_value));
        // A UUID is still not a STRING / INTEGER, and a nested list is still
        // not a property value.
        assert!(!value_matches_type(
            &u,
            &TypeExpr::Predefined {
                kind: PredefinedType::String,
                not_null: false,
            }
        ));
        assert!(!value_matches_type(
            &Value::List(vec![list].into()),
            &any_property_value
        ));
    }

    // ADR-0327: a byte array is a sequence value, not a property value --
    // the UUID addition must not widen the set that far.
    #[test]
    fn bytes_stay_out_of_the_property_value_set() {
        let bytes = Value::Bytes(vec![1u8, 2].into());
        assert!(!is_property_value(&bytes));
        assert!(!value_matches_type(
            &bytes,
            &TypeExpr::PropertyValue { not_null: true }
        ));
    }
}

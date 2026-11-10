//! The one rule for dynamic property-key and label/type operands
//! (CIP-131 bracket targets, CIP-156/CIP-162 dynamic labels/types): the
//! value-type check and the token-name check (Neo4j GQLSTATUS 22N27 /
//! 22NB1 / 42I11).
//!
//! Two layers call this rule over the same values: the front's compile-time
//! fold (`relate-front/src/validate/dynamic.rs`) validates constant
//! operands before execution — so a bad constant fails even when the driving
//! `MATCH` matches zero rows — and the executor's runtime resolution
//! (`relate-executor/src/eval/labels.rs`) validates per-row operands.
//! Neither layer restates a message or a type rule.

use std::fmt;

use crate::error_kinds::ErrorKind;

use crate::typename::bare_type_name;
use crate::{Error, Value};

/// Which token namespace a dynamic name belongs to (the `tokenType` slot of
/// Neo4j's 42I11 "invalid name" status description). Owns the whole
/// namespace vocabulary — the 42I11 token phrase, the 22N27/22NB1 operand
/// position and accepted type — so neither layer spells any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicNameToken {
    PropertyKey,
    Label,
}

impl DynamicNameToken {
    /// The authored token-type phrase for the 42I11 detail text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PropertyKey => "property key",
            Self::Label => "label",
        }
    }

    /// The authored operand position for the 22N27/22NB1 detail text.
    pub const fn position(self) -> &'static str {
        match self {
            Self::PropertyKey => "node or relationship property key",
            Self::Label => "dynamic label",
        }
    }

    /// The authored accepted type for the 22N27/22NB1 detail text.
    pub const fn expected(self) -> &'static str {
        match self {
            Self::PropertyKey => "STRING",
            Self::Label => "STRING or LIST<STRING>",
        }
    }

    /// The *type* half of [`Self::check`], against a statically known
    /// operand's Cypher type class (`STRING`, `LIST`, … — a bare class, never
    /// a parameterized list type): a property key takes a `STRING`; a
    /// dynamic label/type a `STRING` or a `LIST`, whose elements the per-row
    /// rule then checks. Content (a string's text, a list's elements) is the
    /// other half, decided by [`Self::check`] per value.
    pub fn accepts_type(self, type_name: &str) -> bool {
        match self {
            Self::PropertyKey => type_name == "STRING",
            Self::Label => type_name == "STRING" || type_name == "LIST",
        }
    }

    /// The dynamic-operand rule for this namespace: a property key is one
    /// non-empty, NUL-free `STRING`; a label/type operand is a `STRING` or
    /// a `LIST<STRING>` of such names. Validation-only callers (the front's
    /// compile-time fold) use this token-keyed entry; callers that need the
    /// resolved name(s) call [`dynamic_property_key`] /
    /// [`dynamic_label_names`] directly.
    pub fn check(self, value: &Value) -> Result<(), DynamicAccessError> {
        match self {
            Self::PropertyKey => dynamic_property_key(value).map(|_| ()),
            Self::Label => dynamic_label_names(value).map(|_| ()),
        }
    }
}

/// One rejected dynamic operand, carrying the data both error channels need
/// (the front renders it as an `EngineError` (`relate-error`),
/// the executor as a [`Error`]). `Display` is the authored detail text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamicAccessError {
    /// 22N27 / 22NB1: the operand is not of the required type.
    WrongType {
        token: DynamicNameToken,
        /// The Cypher type name of the offending operand, e.g. `INTEGER`.
        type_name: String,
    },
    /// 42I11: a name is empty or contains a null byte.
    InvalidName {
        token: DynamicNameToken,
        input: String,
    },
}

impl DynamicAccessError {
    /// The 22N27/22NB1 rejection for `token`'s namespace: `type_name` is the
    /// offending operand's Cypher type name. One constructor for both token
    /// namespaces, so the position/expected vocabulary lives only on
    /// [`DynamicNameToken`].
    pub fn wrong_type(token: DynamicNameToken, type_name: impl Into<String>) -> Self {
        Self::WrongType {
            token,
            type_name: type_name.into(),
        }
    }

    /// The [`ErrorKind`] both channels classify the failure under: a wrong
    /// type is `InvalidArgumentType` (22N27/22NB1), an invalid token name is
    /// a `SyntaxError`-class violation (42I11).
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::WrongType { .. } => ErrorKind::InvalidArgumentType,
            Self::InvalidName { .. } => ErrorKind::SyntaxError,
        }
    }
}

impl fmt::Display for DynamicAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongType { token, type_name } => write!(
                f,
                "Invalid input '{type_name}' for {}. Expected to be {}.",
                token.position(),
                token.expected()
            ),
            Self::InvalidName { token, input } => write!(
                f,
                "invalid name. A {} name cannot be empty or contain any null-bytes: '{}'.",
                token.as_str(),
                escape_for_message(input)
            ),
        }
    }
}

impl From<DynamicAccessError> for Error {
    fn from(error: DynamicAccessError) -> Self {
        let detail = error.to_string();
        // Matched by variant, not through `kind()`: a wrong operand type is
        // an argument-type violation (22N27/22NB1), an invalid token name a
        // syntax violation (42I11). A new variant is therefore a compile
        // error here — never a silent `Semantic` degradation the bindings'
        // metadata scan cannot classify.
        match error {
            DynamicAccessError::WrongType { .. } => Error::invalid_argument_type(detail),
            DynamicAccessError::InvalidName { .. } => Error::syntax(detail),
        }
    }
}

/// The one dynamic property-key rule: the key must be a non-empty, NUL-free
/// `STRING`.
pub fn dynamic_property_key(value: &Value) -> Result<String, DynamicAccessError> {
    match value {
        Value::String(key) => validated_name(DynamicNameToken::PropertyKey, key),
        other => Err(DynamicAccessError::wrong_type(
            DynamicNameToken::PropertyKey,
            bare_type_name(other),
        )),
    }
}

/// The one dynamic label/type rule: the operand must be a `STRING` or a
/// `LIST<STRING>`, and every name must be non-empty and NUL-free.
pub fn dynamic_label_names(value: &Value) -> Result<Vec<String>, DynamicAccessError> {
    match value {
        Value::List(items) => items.iter().map(label_name).collect(),
        other => label_name(other).map(|name| vec![name]),
    }
}

/// The per-name rule a dynamic label/type operand applies at every position:
/// a scalar operand is one name, a list operand applies it per element, so a
/// wrong element and a wrong scalar report the identical 22NB1 detail.
fn label_name(item: &Value) -> Result<String, DynamicAccessError> {
    match item {
        Value::String(name) => validated_name(DynamicNameToken::Label, name),
        other => Err(DynamicAccessError::wrong_type(
            DynamicNameToken::Label,
            bare_type_name(other),
        )),
    }
}

/// The one token-name check: rejects an empty name or one carrying a NUL
/// byte (42I11), and returns the owned spelling of a valid name.
fn validated_name(token: DynamicNameToken, name: &str) -> Result<String, DynamicAccessError> {
    if name.is_empty() || name.contains('\0') {
        Err(DynamicAccessError::InvalidName {
            token,
            input: name.to_string(),
        })
    } else {
        Ok(name.to_string())
    }
}

/// The input as it can sit inside a single-quoted detail message: a NUL byte
/// renders as `\0` instead of breaking the line.
fn escape_for_message(input: &str) -> String {
    input.chars().flat_map(char::escape_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::Temporal;

    #[test]
    fn string_property_key_is_accepted_by_reference() {
        assert_eq!(
            dynamic_property_key(&Value::String("name".into())).unwrap(),
            "name"
        );
    }

    #[test]
    fn wrong_property_key_type_names_the_input_and_the_expected_type() {
        let error = dynamic_property_key(&Value::Int(1)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidArgumentType);
        assert_eq!(
            error.to_string(),
            "Invalid input 'INTEGER' for node or relationship property key. Expected to be STRING."
        );
    }

    #[test]
    fn empty_and_nul_property_keys_are_invalid_names() {
        for key in ["", "a\0b"] {
            let error = dynamic_property_key(&Value::String(key.into())).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::SyntaxError);
            assert!(error.to_string().starts_with(
                "invalid name. A property key name cannot be empty or contain any null-bytes: '"
            ));
        }
    }

    #[test]
    fn label_names_accept_a_string_and_a_string_list() {
        assert_eq!(
            dynamic_label_names(&Value::String("A".into())).unwrap(),
            vec!["A".to_string()]
        );
        let list = Value::List(vec![Value::String("A".into()), Value::String("B".into())].into());
        assert_eq!(
            dynamic_label_names(&list).unwrap(),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn label_names_reject_non_string_and_invalid_elements() {
        let wrong = dynamic_label_names(&Value::Int(1)).unwrap_err();
        assert_eq!(
            wrong.to_string(),
            "Invalid input 'INTEGER' for dynamic label. Expected to be STRING or LIST<STRING>."
        );
        let mixed = Value::List(vec![Value::String("A".into()), Value::Int(1)].into());
        assert_eq!(dynamic_label_names(&mixed).unwrap_err(), wrong);
        assert!(dynamic_label_names(&Value::String("".into())).is_err());
    }

    #[test]
    fn token_check_is_the_shared_validation_entry() {
        DynamicNameToken::PropertyKey
            .check(&Value::String("name".into()))
            .unwrap();
        DynamicNameToken::Label
            .check(&Value::List(vec![Value::String("A".into())].into()))
            .unwrap();
        assert_eq!(
            DynamicNameToken::PropertyKey
                .check(&Value::Int(1))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidArgumentType
        );
        assert_eq!(
            DynamicNameToken::Label
                .check(&Value::String("".into()))
                .unwrap_err()
                .kind(),
            ErrorKind::SyntaxError
        );
    }

    /// The type half: the accepted class set is the one [`DynamicNameToken::check`]
    /// enforces per value — a property key takes a `STRING`, a label also a
    /// `LIST`.
    #[test]
    fn accepts_type_is_the_type_half_of_check() {
        assert!(DynamicNameToken::PropertyKey.accepts_type("STRING"));
        assert!(!DynamicNameToken::PropertyKey.accepts_type("LIST"));
        assert!(!DynamicNameToken::PropertyKey.accepts_type("INTEGER"));
        assert!(DynamicNameToken::Label.accepts_type("STRING"));
        assert!(DynamicNameToken::Label.accepts_type("LIST"));
        assert!(!DynamicNameToken::Label.accepts_type("MAP"));
    }

    /// The core `Error` channel classifies through `kind()`, so a runtime
    /// `SET n[$k]` with `k = ""` fails as a syntax violation exactly like its
    /// compile-time constant twin (`SET n[''] = 2`).
    #[test]
    fn error_conversion_follows_the_shared_classification() {
        assert!(matches!(
            Error::from(dynamic_property_key(&Value::Int(1)).unwrap_err()),
            Error::InvalidArgumentType(_)
        ));
        assert!(matches!(
            Error::from(dynamic_property_key(&Value::String("".into())).unwrap_err()),
            Error::Syntax(_)
        ));
    }

    /// Lexicographic order would render FLOAT before INTEGER; the CIP-100
    /// order both consumers share renders INTEGER first in this module's
    /// 22N27 detail text.
    #[test]
    fn wrong_type_detail_orders_numerics_by_cip100_not_lexicographically() {
        let mixed = Value::List(vec![Value::Float(1.5), Value::Int(1)].into());
        assert_eq!(
            dynamic_property_key(&mixed).unwrap_err().to_string(),
            "Invalid input 'LIST<INTEGER | FLOAT>' for node or relationship property key. \
             Expected to be STRING."
        );
    }

    /// The 22N27/22NB1 detail text renders temporal values with the Cypher
    /// type spelling this module's type-name source produces.
    #[test]
    fn wrong_type_detail_renders_the_temporal_spelling() {
        let duration = Value::Temporal(Temporal::Duration {
            months: 0,
            days: 0,
            nanos: 0,
        });
        assert_eq!(
            dynamic_property_key(&duration).unwrap_err().to_string(),
            "Invalid input 'DURATION' for node or relationship property key. \
             Expected to be STRING."
        );
        let date = Value::Temporal(Temporal::CivilDate {
            year: 2020,
            month: 1,
            day: 1,
        });
        assert_eq!(
            dynamic_label_names(&date).unwrap_err().to_string(),
            "Invalid input 'DATE' for dynamic label. Expected to be STRING or LIST<STRING>."
        );
    }
}

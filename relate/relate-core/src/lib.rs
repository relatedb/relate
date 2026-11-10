//! Core values, errors, and identifiers for Relate.

/// The byte bound of one wire message every surface frames around — the
/// value Bolt's chunked framing caps a message at and the C ABI's params
/// builder caps one `put_string` value at (single-sourced so the two
/// surfaces can never disagree). Fixed constant, no knob (ADR-0514).
pub const MAX_WIRE_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

mod auto_literal;
mod cancel;
pub mod ce_messages;
mod convert;
mod dynamic;
mod error;
/// The shared error-kind vocabulary (ADR-0502 §2 moved it here from
/// `relate-error`, which re-exports it): the finest TCK-aligned
/// [`ErrorKind`], the eight [`TckErrorClass`]es, and the two detail
/// helpers both error channels render through.
mod error_kinds;
pub mod file_identity;
pub mod integer_args;
mod memory_limit;
mod namespace;
mod preview;
pub mod temporal;
pub mod tokens;
mod traversal;
mod typename;
mod value;

pub use auto_literal::{auto_literal, AutoLiteralGuard};
pub use cancel::CancelToken;
pub use convert::{column_type_name, cypher_to_value, value_to_cypher};
pub use dynamic::{
    dynamic_label_names, dynamic_property_key, DynamicAccessError, DynamicNameToken,
};
pub use error::{
    expected_type_detail, uniqueness_violation_detail, ArithmeticFailure, CapacityKind, Error,
    Result, FUNCTION_REQUIRES_ONE_ARGUMENT,
};
pub use error_kinds::{
    duplicate_variable_detail, undefined_variable_detail, unknown_function_detail, ErrorKind,
    TckErrorClass,
};
pub use memory_limit::{
    parse_expression_memory_limit, parse_memory_limit, parse_positive_memory_limit,
    require_positive_memory_limit,
};
pub use namespace::{
    graph_selector_arity, is_graph_selector_function_name, node_element_id, parse_node_element_id,
    parse_rel_element_id, rel_element_id, ElementIdRef, Namespace, GRAPH_SELECTOR_FUNCTIONS,
    GRAPH_SELECTOR_FUNCTION_NAMES,
};
pub use preview::{
    preview_feature_disabled_text, PreviewFeature, PreviewFeatures, PREVIEW_FEATURES,
};
pub use temporal::{QueryClockGuard, Temporal};
pub use tokens::{ExecutionReceipt, TokenEstimate, TokenOutcome};
pub use traversal::{
    require_traversal_depth, traversal_depth_exceeded_message, MAX_TRAVERSAL_DEPTH,
    MAX_TRAVERSAL_DEPTH_CEILING,
};
pub use typename::{bare_type_name, expected_value_type_detail, value_type_name};
pub use value::{
    bytes_logical_bytes, logical_bytes_within, next_depth, point_component, str_logical_bytes,
    tagged_str, uuid_hyphenated, uuid_parse, value_depth_exceeded, Node, Path, PointValue,
    PropertyMap, Relationship, TaggedStr, Value, VectorCoordKind, MAX_VALUE_DEPTH,
};

pub type NodeId = i64;
pub type RelId = i64;

/// The dialect fact on its own: true when `cypher_version` is an explicit
/// `CYPHER 5` preamble (CIP-150). The one spelling of the rule — the
/// executor's evaluation context and the front end's dialect gates both read
/// it, so the two cannot drift.
pub fn is_cypher5(cypher_version: Option<u32>) -> bool {
    cypher_version == Some(5)
}

/// Whether `id` belongs to a CIP-257 **virtual** entity — a graph-type
/// projection synthesized with a negative id and carried by its value alone
/// (it has no store entry). Persisted ids are non-negative
/// ([`validate_persisted_id`]), so the sign is the one virtual-entity test.
pub const fn is_virtual_id(id: i64) -> bool {
    id < 0
}

/// The single acceptance rule for a persisted local node or relationship ID.
///
/// The bound `0..i64::MAX` rejects a negative ID before it can seed a
/// next-write counter and reserves `i64::MAX` as the unsupported sentinel,
/// because installing a row advances the ID allocator. `what` labels the
/// caller's context in the returned storage error. See ADR-0096.
pub fn validate_persisted_id(id: i64, what: &str) -> Result<()> {
    if id < 0 || id == i64::MAX {
        return Err(Error::storage(format!(
            "{what} ID {id} is outside 0..i64::MAX"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_id_predicate_accepts_in_range_and_rejects_zero_nine_six() {
        for id in [0, 1, 42, i64::MAX - 1] {
            validate_persisted_id(id, "node").unwrap();
        }
        for id in [-1, i64::MIN, i64::MAX] {
            let error = validate_persisted_id(id, "node").unwrap_err();
            assert!(matches!(error, Error::Storage(_)), "{error}");
            assert_eq!(
                error.to_string(),
                format!("storage error: node ID {id} is outside 0..i64::MAX")
            );
        }
    }
}

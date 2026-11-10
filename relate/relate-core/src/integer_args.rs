//! The ADR-0175 integer-argument sites' authored texts.
//!
//! Written once so every layer that raises a site's rejection reads the
//! same text: the planner's checks at plan time and, for the sites with a
//! runtime coercion, the executor's argument coercion at run time. The
//! slice bounds are plan-time only — their runtime coercion has no type
//! rejection — and `SKIP` / `LIMIT` carry their own planner tokens.
//! [`crate::FUNCTION_REQUIRES_ONE_ARGUMENT`] sets the precedent for a
//! rejection spelling shared across layers.

/// One integer-argument site's authored texts.
pub struct IntegerArgSite {
    /// The `InvalidArgumentType` detail of a non-numeric argument.
    pub type_error: &'static str,
    /// The site label a `NumberOutOfRange` rejection names. `None` where the
    /// site has no runtime coercion to label: `range()`'s out-of-range
    /// rejection (zero step, overflowing length) is authored in its executor,
    /// not per argument.
    pub range: Option<&'static str>,
}

/// A site that only gates the argument's type at plan time.
const fn type_site(type_error: &'static str) -> IntegerArgSite {
    IntegerArgSite {
        type_error,
        range: None,
    }
}

/// A site whose runtime coercion raises `NumberOutOfRange`.
const fn coercion_site(type_error: &'static str, range: &'static str) -> IntegerArgSite {
    IntegerArgSite {
        type_error,
        range: Some(range),
    }
}

pub const RANGE_START: IntegerArgSite = type_site("range() start");
pub const RANGE_END: IntegerArgSite = type_site("range() end");
pub const RANGE_STEP: IntegerArgSite = type_site("range() step");
pub const SUBSTRING_START: IntegerArgSite =
    coercion_site("substring() start must be an integer", "substring() start");
pub const SUBSTRING_LENGTH: IntegerArgSite = coercion_site(
    "substring() length must be an integer",
    "substring() length",
);
pub const LEFT_LENGTH: IntegerArgSite =
    coercion_site("left() length must be an integer", "left() length");
pub const RIGHT_LENGTH: IntegerArgSite =
    coercion_site("right() length must be an integer", "right() length");
pub const LIST_INDEX: IntegerArgSite = type_site("list index must be an integer");
pub const SLICE_START: IntegerArgSite = type_site("slice start must be an integer");
pub const SLICE_END: IntegerArgSite = type_site("slice end must be an integer");

//! The builtin-function arity registry: the shared spellings of the arity
//! rejection details and the [`ArityContract`] the front-end check, the
//! planner's rule table and the executor's dispatch read as one fact.

/// The one spelling of the one-argument arity requirement shared by the
/// planner's `FIXED_ARITY_FUNCTIONS` row and the executor's list-builtin
/// backstop — the bare detail, without the `InvalidNumberOfArguments`
/// class token, which each layer composes exactly once
/// ([`crate::Error::invalid_number_of_arguments`], or the typed channel).
pub const FUNCTION_REQUIRES_ONE_ARGUMENT: &str = "function requires one argument";

/// The one spelling of `length()`'s non-path argument rejection, shared by
/// the planner's static rules and the executor's runtime backstop — the
/// bare detail, without the `InvalidArgumentType` class token, which each
/// layer composes (the [`FUNCTION_REQUIRES_ONE_ARGUMENT`] precedent).
pub const LENGTH_REQUIRES_A_PATH: &str = "length() needs a path";

/// The one spelling of `nullif()`'s two-argument arity requirement: the
/// front-end static-argument check and the executor's implementation both
/// state it from here, so the compile-time and runtime halves cannot drift
/// (the [`FUNCTION_REQUIRES_ONE_ARGUMENT`] precedent).
pub const NULLIF_REQUIRES_TWO_ARGUMENTS: &str = "nullif() requires two arguments";

/// The one spelling of `format()`'s one-or-two-argument arity requirement,
/// shared by the front-end check and the executor's implementation.
pub const FORMAT_REQUIRES_ONE_OR_TWO_ARGUMENTS: &str = "format() requires one or two arguments";

/// The one spelling of `vector()`'s three-argument arity requirement, shared
/// by the front-end check and the executor's implementation.
pub const VECTOR_REQUIRES_THREE_ARGUMENTS: &str = "vector() requires three arguments";

/// One builtin's compile-phase arity contract: the accepted argument-count
/// range together with the authored rejection *detail* (bare, without the
/// class token). Declared in the shared contract layer so the front-end
/// check, the planner's rule table and the executor's dispatch read one
/// fact instead of a count and a message each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArityContract {
    min: usize,
    max: usize,
    detail: &'static str,
}

impl ArityContract {
    /// Exactly `count` arguments.
    pub const fn exact(count: usize, detail: &'static str) -> Self {
        Self {
            min: count,
            max: count,
            detail,
        }
    }

    /// Between `min` and `max` arguments, inclusive.
    pub const fn range(min: usize, max: usize, detail: &'static str) -> Self {
        Self { min, max, detail }
    }

    /// The least accepted argument count — the executor's dispatch gate
    /// derives its row bounds from here, so the count has one home.
    pub const fn min(self) -> usize {
        self.min
    }

    /// The greatest accepted argument count (see [`Self::min`]).
    pub const fn max(self) -> usize {
        self.max
    }

    /// The authored rejection detail a call supplying `count` arguments
    /// violates, or `None` when it satisfies the contract. The accept test
    /// and the rejection travel together, so no consumer can pair them
    /// wrongly; each layer composes the class token once around the detail.
    pub fn violation(self, count: usize) -> Option<&'static str> {
        if (self.min..=self.max).contains(&count) {
            None
        } else {
            Some(self.detail)
        }
    }
}

/// `char_length()`/`character_length()`'s one-argument contract.
pub const CHAR_LENGTH_ARITY: ArityContract =
    ArityContract::exact(1, FUNCTION_REQUIRES_ONE_ARGUMENT);

/// `nullif()`'s two-argument contract.
pub const NULLIF_ARITY: ArityContract = ArityContract::exact(2, NULLIF_REQUIRES_TWO_ARGUMENTS);

/// `format()`'s one-or-two-argument contract.
pub const FORMAT_ARITY: ArityContract =
    ArityContract::range(1, 2, FORMAT_REQUIRES_ONE_OR_TWO_ARGUMENTS);

/// `vector()`'s three-argument contract.
pub const VECTOR_ARITY: ArityContract = ArityContract::exact(3, VECTOR_REQUIRES_THREE_ARGUMENTS);

/// The arity contract shared across the front-end check, the planner's rule
/// table and the executor's dispatch, by lowercased builtin name. `None` for
/// a name with no cross-layer contract (its arity, if any, is owned by one
/// layer alone).
pub fn builtin_arity(name: &str) -> Option<ArityContract> {
    Some(match name {
        "char_length" | "character_length" => CHAR_LENGTH_ARITY,
        "nullif" => NULLIF_ARITY,
        "format" => FORMAT_ARITY,
        "vector" => VECTOR_ARITY,
        _ => return None,
    })
}

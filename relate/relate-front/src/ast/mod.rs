//! The low, shared AST layer every semantic pass builds on. It sits below
//! `validate`, `call_scope`, `local_callables` and `lower`, which all
//! consume it downward — none of them owns it.
//!
//! - [`collect`]: the pure collectors (variables, patterns, string
//!   interpolation) plus the shared expression-scope check pair
//!   (`check_expr_vars_defined`/`check_name_defined`) and the UNION-chain
//!   enumeration (`union_arms`/`union_chain`) — neutral plumbing, not
//!   validator policy.
//! - [`naming`]: the projection-item naming rule — the output name a
//!   WITH/RETURN item projects, the implicit-name vocabulary its alias
//!   gates read, and the paren-transparency policy they classify
//!   through.
//! - [`search_spec`]: the one walk over a SEARCH source specification.
//! - [`stmt_ref`]: the statement-position type (real statement, UNION arm,
//!   NEXT stage, or FOREACH body) the whole-tree walkers visit.
//! - [`tree`]: the whole-tree statement/expression walker family and the
//!   ADR-0257 nesting-depth cap they enforce.

pub(crate) mod collect;
pub(crate) mod naming;
pub(crate) mod search_spec;
pub(crate) mod stmt_ref;
pub(crate) mod tree;

/// The shared nesting-depth cap, the parser's own constant (ADR-0257): the
/// structural walkers refuse a statement or expression nested deeper.
pub const MAX_NESTING_DEPTH: usize = relate_parser::parser::MAX_NESTING_DEPTH as usize;

#[cfg(test)]
mod tests {
    use super::MAX_NESTING_DEPTH;

    /// ADR-0462 pins the value cap to the parser's static nesting limit
    /// (ADR-0257): every parse-legal literal must also be a legal value.
    /// Pinned here, beside the front's own alias, because the front already
    /// depends on both crates (the engine's bottom crate does not).
    #[test]
    fn value_depth_cap_matches_the_parser_nesting_limit() {
        assert_eq!(relate_core::MAX_VALUE_DEPTH as usize, MAX_NESTING_DEPTH);
    }
}

//! The traversal-depth admission limit (LABS-610).
//!
//! Explicit variable-length hop bounds (`*1..N`, `{1,N}`) are executed by a
//! recursive walker whose native stack frames grow with `N`. Measured on a
//! 12k-node indexed chain through the real CLI: a debug native build
//! overflows the stack between 10,000 and 11,000 hops, a release native
//! build between 30,000 and 40,000, and the 16 MiB WebAssembly stack at
//! roughly 2,700 hops (debug) / 8,800 (release). The default below sits well
//! inside every one of those bounds; the ceiling is the largest value a
//! caller may opt into, with headroom below the smallest measured abort.
//!
//! The limit protects the process from a query that asks for an
//! unreasonable traversal, it is not a database limit: raising it is a
//! deliberate act with a documented platform risk.

use crate::{Error, Result};

/// Hops an explicit traversal bound may ask for without opting in
/// (`--max-traversal-depth` / `RELATE_MAX_TRAVERSAL_DEPTH`).
pub const MAX_TRAVERSAL_DEPTH: u32 = 500;

/// The largest `--max-traversal-depth` value an open accepts. Above this the
/// recursive walker risks a stack overflow on small-stack platforms
/// (WebAssembly's 16 MiB stack is the tightest supported surface).
pub const MAX_TRAVERSAL_DEPTH_CEILING: u32 = 2000;

/// Validate a caller-supplied traversal depth. `name` is the flag or
/// variable the caller read it from, so the error is self-explaining.
pub fn require_traversal_depth(name: &str, n: u32) -> Result<u32> {
    if (1..=MAX_TRAVERSAL_DEPTH_CEILING).contains(&n) {
        Ok(n)
    } else {
        Err(Error::semantic(format!(
            "invalid {name}: must be between 1 and {MAX_TRAVERSAL_DEPTH_CEILING}"
        )))
    }
}

/// The user-facing rejection for a query whose explicit hop bound asks for
/// more than the admission limit. The wording states what the limit
/// protects, that it is not a database limit, and how far it can be
/// raised (`ceiling`, [`MAX_TRAVERSAL_DEPTH_CEILING`]).
pub fn traversal_depth_exceeded_message(requested: u32, ceiling: u32) -> String {
    format!(
        "This query allows paths up to {} hops. This is a safety limit protecting \
         against excessive traversal (not a database limit) — lower the path bound, \
         or raise it with --max-traversal-depth (up to {}).",
        group_thousands(requested),
        ceiling
    )
}

/// `1000000` -> `1,000,000`.
fn group_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_limits_accept_the_range_and_reject_zero_and_over_ceiling() {
        assert_eq!(
            require_traversal_depth("--max-traversal-depth", 1).unwrap(),
            1
        );
        assert_eq!(
            require_traversal_depth("--max-traversal-depth", MAX_TRAVERSAL_DEPTH_CEILING).unwrap(),
            MAX_TRAVERSAL_DEPTH_CEILING
        );
        for bad in [0, MAX_TRAVERSAL_DEPTH_CEILING + 1] {
            let error = require_traversal_depth("--max-traversal-depth", bad).unwrap_err();
            assert_eq!(
                error.to_string(),
                "semantic error: invalid --max-traversal-depth: must be between 1 and 2000"
            );
        }
        assert_eq!(MAX_TRAVERSAL_DEPTH, 500);
    }

    #[test]
    fn message_names_the_bound_the_ceiling_and_the_knob() {
        let message = traversal_depth_exceeded_message(1_000_000, MAX_TRAVERSAL_DEPTH_CEILING);
        assert_eq!(
            message,
            "This query allows paths up to 1,000,000 hops. This is a safety limit \
             protecting against excessive traversal (not a database limit) — lower the \
             path bound, or raise it with --max-traversal-depth (up to 2000)."
        );
        assert!(group_thousands(500).eq("500"));
        assert!(group_thousands(1_000).eq("1,000"));
        assert!(group_thousands(1_000_000).eq("1,000,000"));
    }
}

//! Function name aliases and aggregation helpers.
//!
//! The canonical aggregate function set (`AGGREGATES`) is owned by
//! `relate-ir` — the lowest layer that needs aggregation detection —
//! and re-exported here so every consumer shares one definition. This
//! module adds the one function-alias table (`FUNCTION_ALIASES`) on top.
//! Consumers: the front validators (`validate`), the lowering rename in
//! `lower.rs` (which emits the display spelling), the planner, and the
//! executor's aggregation machinery / `SHOW FUNCTIONS` catalog.

pub use relate_ir::{AGGREGATES, SINGLE_INPUT_AGGREGATES};

use crate::ast::tree::expr_contains;
use relate_parser::{Expr, GroupBySpec, ReturnItem};

/// One accepted alternate lowercase spelling, the canonical lowercase name
/// it resolves to, and the spelling the lowering emits into IR: for an
/// aggregate the display spelling (so an unaliased `RETURN
/// percentile_cont(x)` keeps the Cypher-canonical column name
/// `percentileCont(x)`), for a scalar the canonical name itself.
#[derive(Clone, Copy, Debug)]
pub struct FunctionAlias {
    pub spelling: &'static str,
    pub canonical: &'static str,
    pub display: &'static str,
}

const fn aggregate(
    spelling: &'static str,
    canonical: &'static str,
    display: &'static str,
) -> FunctionAlias {
    FunctionAlias {
        spelling,
        canonical,
        display,
    }
}

const fn scalar(spelling: &'static str, canonical: &'static str) -> FunctionAlias {
    FunctionAlias {
        spelling,
        canonical,
        display: canonical,
    }
}

/// The one function-alias table: CIP-128's snake_case aggregate spellings
/// and CIP-245's snake_case GQL/SQL scalar spellings, scalar rows target names outside `AGGREGATES`.
/// (`toString` rendering is not a spelling policy: the executor's value
/// layer renders temporals at full precision by runtime type, ADR-0448.)
const FUNCTION_ALIASES: &[FunctionAlias] = &[
    aggregate("collect_list", "collect", "collect"),
    aggregate("stdev_samp", "stdev", "stDev"),
    aggregate("stddev_samp", "stdev", "stDev"),
    aggregate("stdev_pop", "stdevp", "stDevP"),
    aggregate("stddev_pop", "stdevp", "stDevP"),
    aggregate("percentile_cont", "percentilecont", "percentileCont"),
    aggregate("percentile_disc", "percentiledisc", "percentileDisc"),
    scalar("path_length", "length"),
    scalar("duration_between", "duration.between"),
    scalar("local_time", "localtime"),
    scalar("local_datetime", "localdatetime"),
    scalar("zoned_time", "time"),
    scalar("zoned_datetime", "datetime"),
];

/// The aggregate rows of the alias table: those whose canonical name is a
/// member of the canonical `AGGREGATES` set (the one source of truth).
pub fn aggregate_aliases() -> impl Iterator<Item = &'static FunctionAlias> {
    FUNCTION_ALIASES
        .iter()
        .filter(|a| AGGREGATES.contains(&a.canonical))
}

/// The canonical lowercase aggregate name for an accepted lowercase
/// spelling, or the input unchanged when it is not an aggregate alias.
pub fn canonical_agg_name(name: &str) -> &str {
    aggregate_aliases()
        .find(|a| a.spelling == name)
        .map_or(name, |a| a.canonical)
}

/// The IR name a lowercase alias spelling lowers to, `None` when `name` is
/// not an alias.
pub(crate) fn lowered_alias_name(name: &str) -> Option<&'static str> {
    FUNCTION_ALIASES
        .iter()
        .find(|a| a.spelling == name)
        .map(|a| a.display)
}

/// True when `name` (any casing, any accepted spelling) is an aggregate
/// function.
pub fn is_aggregate_name(name: &str) -> bool {
    AGGREGATES.contains(&canonical_agg_name(&name.to_lowercase()))
}

/// Whether an expression contains an aggregation function call, through the
/// historical reach: function arguments, binary/unary/comparison operands,
/// parentheses and property values, plus the postfix base of a subscript or
/// slice. Aggregates inside list literals, CASE arms, IN operands,
/// comprehensions or dynamic keys are not "contained" (the consumers that
/// must look there keep their own traversal; see
/// [`crate::validate::aggregation`]).
pub(crate) fn contains_aggregation(expr: &Expr) -> bool {
    contains_where(expr, Reach::Historical, is_aggregation_node)
}

/// Whether an expression contains an aggregation function call anywhere in
/// its own scope — the [`Reach::Full`] traversal, so an aggregate nested in
/// an `IN` operand, a list/map literal, a `CASE` arm or a dynamic key is
/// found. Subquery- and pattern-bearing variants stay opaque (they open
/// their own aggregation scope). The non-aggregating-position rejections
/// use this: an aggregate anywhere in a comprehension projection is
/// invalid, wherever it is spelled.
pub(crate) fn contains_aggregation_anywhere(expr: &Expr) -> bool {
    contains_where(expr, Reach::Full, is_aggregation_node)
}

/// Whether an expression contains an aggregation function call other than
/// `collect()`/`collect_list()`, through the same reach as
/// [`contains_aggregation`].
///
/// Safe mode (LABS-680, ADR-0477) uses this for its unbounded-read rule:
/// `collect()` is an aggregate but buffers every row in memory, so it does
/// not bound a scan and must not exempt a `MATCH … RETURN collect(x)`
/// without a `LIMIT`. `count()`, `sum()`, and the rest reduce as they go
/// and do exempt.
pub(crate) fn contains_non_collect_aggregation(expr: &Expr) -> bool {
    contains_where(expr, Reach::Historical, |node| {
        is_aggregation_node(node)
            && !matches!(node, Expr::FunctionCall(fc) if fc.name.with_str(is_collect_aggregate))
    })
}

/// Whether an expression contains an aggregate whose *value* retains the
/// whole group (unbounded in memory) instead of reducing it to a scalar.
///
/// Safe mode (LABS-680, ADR-0477) applies its unbounded-read exemption per
/// projected item: a single `collect()` beside a reducing aggregate still
/// returns the whole store, so it must not ride the reducing aggregate's
/// exemption. `collect()`/`collect_list()` is the one such aggregate in the
/// canonical set; the percentile aggregates sort a scratch buffer but
/// return a scalar.
///
/// Uses the **full** reach ([`Reach::Full`]): a `collect()` nested in a list
/// literal, CASE arm or comprehension still buffers every row, so the rule
/// must find it there — the historical reach would miss it and let a sibling
/// reducing aggregate grant the exemption (a fail-open).
pub(crate) fn contains_unbounded_aggregate(expr: &Expr) -> bool {
    contains_where(expr, Reach::Full, |node| {
        is_aggregation_node(node)
            && matches!(node, Expr::FunctionCall(fc) if fc.name.with_str(is_collect_aggregate))
    })
}

/// Whether an expression contains a pattern comprehension: a whole-graph
/// read that carries no `MATCH` clause, so safe mode's bounded-read guardrail
/// must count it as a graph read (the full reach finds one nested in a larger
/// expression). A pattern *predicate* is a boolean per row and is not a read
/// of its own, so it does not count.
pub(crate) fn contains_pattern_comprehension(expr: &Expr) -> bool {
    contains_where(expr, Reach::Full, |node| {
        matches!(node, Expr::PatternComprehension(_))
    })
}

/// Whether a projection establishes the aggregating context by its own
/// shape: an aggregate item, or an explicit `GROUP BY` (which aggregates
/// even with no aggregate item). The planner's `agg::projection_aggregates`
/// additionally folds `DISTINCT` in; the front routes DISTINCT to its own
/// ORDER BY mode first, so this copy never sees it (ADR-0453).
pub(crate) fn projection_aggregates(items: &[ReturnItem], group_by: Option<&GroupBySpec>) -> bool {
    items.iter().any(|item| contains_aggregation(&item.expr)) || group_by.is_some()
}

/// The canonical collect test, alias-aware (`collect_list` included): one
/// predicate beside [`is_aggregate_name`] so the spelling never drifts.
fn is_collect_aggregate(name: &str) -> bool {
    canonical_agg_name(&name.to_lowercase()) == "collect"
}

/// Which child-descent a find-in-expression walk takes.
#[derive(Clone, Copy)]
enum Reach {
    /// The historical aggregation reach: function arguments, binary/unary/
    /// comparison operands, parentheses and property values, plus the
    /// postfix base of a subscript or slice. Stops at list literals, CASE
    /// arms, comprehensions and dynamic keys.
    Historical,
    /// Every expression child ([`walk_expr`]) — a nested `collect()` or
    /// pattern comprehension is found wherever it sits.
    Full,
}

/// The reach-selecting adapter over the shared find-in-expression walk
/// ([`expr_contains`]): the two reaches differ only in their edge filter.
fn contains_where(expr: &Expr, reach: Reach, admit: impl Fn(&Expr) -> bool) -> bool {
    match reach {
        Reach::Historical => expr_contains(expr, historical_enter_child, admit),
        Reach::Full => expr_contains(expr, |_, _| true, admit),
    }
}

/// Whether one expression node is an aggregation call (`count(*)` or an
/// aggregate function call). The aggregation predicates differ only in
/// reach, never in this node test.
fn is_aggregation_node(node: &Expr) -> bool {
    matches!(node, Expr::CountStar)
        || matches!(node, Expr::FunctionCall(fc) if fc.name.with_str(is_aggregate_name))
}

/// The historical child-descent filter: the reach the aggregation predicates
/// have always had (see [`Reach::Historical`]).
fn historical_enter_child(parent: &Expr, child: &Expr) -> bool {
    match parent {
        Expr::Property(p) => std::ptr::eq(child, &p.expr),
        Expr::Subscript(s) => std::ptr::eq(child, &s.expr),
        Expr::FunctionCall(_)
        | Expr::Binary(_)
        | Expr::Unary(_)
        | Expr::Paren(_)
        | Expr::Comparison(_) => true,
        // CIP-261: embeds are full expressions and must be walked.
        Expr::StringInterp(_) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_canonical_names() {
        assert_eq!(canonical_agg_name("percentile_cont"), "percentilecont");
        assert_eq!(canonical_agg_name("stddev_samp"), "stdev");
        assert_eq!(canonical_agg_name("collect_list"), "collect");
        assert_eq!(canonical_agg_name("count"), "count");
        assert_eq!(canonical_agg_name("abs"), "abs");
    }

    #[test]
    fn display_spellings_keep_cypher_casing() {
        assert_eq!(
            lowered_alias_name("percentile_cont"),
            Some("percentileCont")
        );
        assert_eq!(lowered_alias_name("stddev_pop"), Some("stDevP"));
        assert_eq!(lowered_alias_name("count"), None);
    }

    #[test]
    fn aggregate_name_detection_covers_every_spelling() {
        for name in [
            "count",
            "SUM",
            "stDev",
            "stdev",
            "stdevp",
            "stDevP",
            "percentileCont",
            "percentilecont",
            "percentile_disc",
            "collect_list",
        ] {
            assert!(is_aggregate_name(name), "{name} must be aggregate");
        }
        assert!(!is_aggregate_name("abs"));
    }

    #[test]
    fn aggregate_alias_targets_are_members_of_the_canonical_set() {
        // The alias table may only point at members of the canonical set
        // (re-exported from `relate-ir`), and each display spelling must
        // be exactly its canonical target re-cased.
        for a in aggregate_aliases() {
            let (alias, canonical, display) = (a.spelling, a.canonical, a.display);
            assert!(
                AGGREGATES.contains(&canonical),
                "alias {alias} targets {canonical}, which is not in the canonical set"
            );
            assert_eq!(
                display.to_lowercase(),
                canonical,
                "alias {alias} display spelling {display} drifted from {canonical}"
            );
            assert!(
                !AGGREGATES.contains(&alias),
                "alias {alias} collides with a canonical name"
            );
            assert!(is_aggregate_name(alias), "alias {alias} must match");
        }
    }

    #[test]
    fn the_aggregate_alias_rows_are_pinned() {
        // A mistyped canonical would silently reclassify a row as scalar
        // (the class is derived from `AGGREGATES` membership), so the
        // expected aggregate spellings are pinned here.
        let mut got: Vec<&str> = aggregate_aliases().map(|a| a.spelling).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "collect_list",
                "percentile_cont",
                "percentile_disc",
                "stddev_pop",
                "stddev_samp",
                "stdev_pop",
                "stdev_samp",
            ]
        );
    }

    #[test]
    fn alias_spellings_are_unique() {
        for a in FUNCTION_ALIASES {
            assert_eq!(
                FUNCTION_ALIASES
                    .iter()
                    .filter(|b| b.spelling == a.spelling)
                    .count(),
                1,
                "duplicate alias spelling {}",
                a.spelling
            );
        }
    }

    #[test]
    fn scalar_aliases_lower_to_their_canonical_name() {
        let mut scalars = 0;
        for a in FUNCTION_ALIASES
            .iter()
            .filter(|a| !AGGREGATES.contains(&a.canonical))
        {
            scalars += 1;
            assert_eq!(a.display, a.canonical, "scalar alias {}", a.spelling);
            assert_eq!(lowered_alias_name(a.spelling), Some(a.canonical));
            assert!(!is_aggregate_name(a.spelling));
        }
        assert!(scalars > 0);
    }

    #[test]
    fn ir_and_front_matchers_recognize_exactly_the_canonical_set() {
        use std::collections::BTreeSet;
        // Set-equality guard between the ir-owned canonical set and the two
        // matchers built on it. Probe pool: every canonical name, every
        // alias spelling, mixed-case variants, and near-miss non-aggregates.
        let probes = [
            "count",
            "sum",
            "avg",
            "min",
            "max",
            "collect",
            "stdev",
            "stdevp",
            "percentilecont",
            "percentiledisc",
            "COUNT",
            "stDev",
            "stDevP",
            "percentileCont",
            "percentileDisc",
            "collect_list",
            "stdev_samp",
            "stddev_samp",
            "stdev_pop",
            "stddev_pop",
            "percentile_cont",
            "percentile_disc",
            "abs",
            "rand",
            "stdevx",
            "percentile",
            "percentiledis",
            "collects",
        ];
        let canonical: BTreeSet<&str> = AGGREGATES.iter().copied().collect();
        // Pin the exact membership, so perturbing the ir-owned definition
        // itself (not just a matcher built on it) fails this test.
        let expected: BTreeSet<&str> = [
            "avg",
            "collect",
            "count",
            "max",
            "min",
            "percentilecont",
            "percentiledisc",
            "stdev",
            "stdevp",
            "sum",
        ]
        .into_iter()
        .collect();
        assert_eq!(canonical, expected, "AGGREGATES membership changed");
        // The ir matcher recognizes exactly the canonical set — the CIP-128
        // alias spellings are a front-layer concern and must NOT match here.
        let ir_hits: BTreeSet<String> = probes
            .iter()
            .filter(|n| relate_ir::is_agg_fn(n))
            .map(|n| n.to_lowercase())
            .collect();
        assert_eq!(
            ir_hits,
            canonical.iter().map(|n| n.to_string()).collect(),
            "relate_ir::is_agg_fn drifted from the canonical set"
        );
        // The front matcher recognizes the canonical set plus the alias
        // spellings, resolving to exactly the canonical set.
        let front_hits: BTreeSet<String> = probes
            .iter()
            .filter(|n| is_aggregate_name(n))
            .map(|n| canonical_agg_name(&n.to_lowercase()).to_string())
            .collect();
        assert_eq!(
            front_hits,
            canonical.iter().map(|n| n.to_string()).collect(),
            "front matcher resolved to names outside the canonical set"
        );
    }
}

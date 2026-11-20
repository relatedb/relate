//! RETURN/WITH grouping-key ambiguity: in an expression that mixes
//! aggregation with non-aggregation, every non-aggregation variable must be
//! a grouping-key variable, and the non-aggregation parts must be simple
//! (variables, property accesses, literals, lists of grouped vars, scalar
//! function calls over those, or an expression that *is* one of the
//! grouping keys) — anything more complex is ambiguous.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_parser::Expr;

use super::collect_non_agg_variables;
use crate::agg::{contains_aggregation, contains_aggregation_anywhere};
use crate::ast::collect::{collect_expr_variables, interp_embeds};
use crate::ast::tree::{unparen, var_property};

/// The grouping keys of a projection clause, bundled with the derived
/// facts the ambiguity checks need about them: the variables the key
/// expressions mention (extended by [`GroupingKeys::with_aliases`] with
/// projected-alias names), and whether a `*` item is among the keys.
/// Bundling them keeps the ADR-0198 ruling-1 policy in one place and
/// collapses the per-leaf pass-through plumbing to a single
/// `&GroupingKeys` parameter.
pub(super) struct GroupingKeys<'a> {
    /// The grouping-key expressions themselves.
    keys: &'a [&'a Expr],
    vars: IdentHashSet<SmallName>,
    star: bool,
}

impl<'a> GroupingKeys<'a> {
    pub(super) fn new(key_exprs: &'a [&'a Expr]) -> Self {
        Self {
            keys: key_exprs,
            vars: grouping_key_vars(key_exprs),
            // ADR-0198 ruling 1: a `*` item carries every in-scope variable
            // as a grouping key.
            star: key_exprs.iter().any(|k| matches!(k, Expr::Star)),
        }
    }

    /// "A projected alias counts as a grouping reference" (e.g.
    /// `me.age AS age` makes `age` a valid grouping reference): the alias
    /// NAMES extend the model's key variables — an alias behaves exactly
    /// like a key variable (coverage and property-target rule).
    pub(super) fn with_aliases(&self, aliases: impl IntoIterator<Item = SmallName>) -> Self {
        let mut vars = self.vars.clone();
        vars.extend(aliases);
        Self {
            keys: self.keys,
            vars,
            star: self.star,
        }
    }

    /// ADR-0198 ruling 1: a `*` item covers every in-scope variable;
    /// otherwise the exact-variable rule decides.
    pub(super) fn covers_variable(&self, v: &SmallName) -> bool {
        self.star || self.key_variable(v)
    }

    /// The variable appears in a grouping key expression (or is an
    /// extended alias) — `*` coverage does NOT apply here: `*` projects
    /// the variables, not their property expressions, so property
    /// targets keep the exact-variable rule.
    fn key_variable(&self, v: &SmallName) -> bool {
        self.vars.contains(v)
    }

    /// The expression is paren-transparently identical to one of the
    /// clause's grouping keys — a projected key and a key the projection
    /// never restates (an explicit `GROUP BY <expr>`) alike; redundant
    /// parens on either side name the same key ([`unparen`], LABS-1615 —
    /// the spelling `Restatable::restates_whole` already uses). Being a
    /// projected item alone is not enough — only a *key* is a key.
    fn matches_key(&self, expr: &Expr) -> bool {
        self.keys.iter().any(|key| unparen(key) == unparen(expr))
    }
}

/// Collect every variable mentioned by the grouping key expressions into
/// one set. Runs once per [`GroupingKeys::new`].
fn grouping_key_vars(grouping_key_exprs: &[&Expr]) -> IdentHashSet<SmallName> {
    let mut vars = IdentHashSet::default();
    for key in grouping_key_exprs {
        collect_expr_variables(key, &mut vars);
    }
    vars
}

/// Check if an expression that mixes aggregation and non-aggregation is ambiguous.
/// Returns true if the expression is ambiguous (invalid).
///
/// The rule: in a mixed expression, the non-aggregation variables must all be
/// grouping key variables (i.e., variables that appear in non-aggregation projection items).
/// However, the non-agg sub-expression itself must not be a "complex" expression
/// (i.e., more than a simple variable, property access, literal, or list of grouped vars).
///
/// Special cases that are always valid:
/// - Constants/literals mixed with aggregation (e.g., count(*) + 10)
/// - Parameters mixed with aggregation
/// - Simple variables that are grouping keys (e.g., n + count(*) when n is grouped)
/// - Property accesses on grouping key variables (e.g., n.age + count(*) when n.age is grouped)
/// - Scalar function calls over grouping key variables (e.g.,
///   toFloat(c) / count(m) when c is grouped, REL8-1869)
///
/// The caller constructs the [`GroupingKeys`] model once per clause
/// (base keys for the projection-item check, alias-extended for the
/// WITH ORDER BY check) and passes it here. The precondition set is
/// this predicate's own: the expression must actually mix aggregation
/// with non-aggregation (a non-aggregating expression is never
/// ambiguous), and a bare aggregate is never ambiguous either.
pub(super) fn is_ambiguous_aggregation(expr: &Expr, keys: &GroupingKeys) -> bool {
    if !contains_aggregation(expr) {
        return false;
    }

    // Collect non-aggregation variables from the expression
    let mut non_agg_vars = IdentHashSet::default();
    collect_non_agg_variables(expr, &mut non_agg_vars);

    // If no non-agg variables, it's not ambiguous (pure agg + constants)
    if non_agg_vars.is_empty() {
        return false;
    }

    // If any non-agg variable is not covered by the grouping keys, it's ambiguous
    if non_agg_vars.iter().any(|v| !keys.covers_variable(v)) {
        return true;
    }

    // Even if all vars are grouping vars, check if the non-agg sub-expression
    // is more complex than what the grouping keys provide.
    // The non-agg parts must structurally match a grouping key expression
    // or be a simple expression built from grouping key variables.
    check_non_agg_parts_match_keys(expr, keys)
}

/// Check if non-aggregation sub-expressions in a mixed aggregation expression
/// structurally match grouping key expressions.
/// Returns true if there's a mismatch (ambiguous).
fn check_non_agg_parts_match_keys(expr: &Expr, keys: &GroupingKeys) -> bool {
    match expr {
        Expr::Binary(b) => check_mixed_sides(&b.left, &b.right, keys),
        Expr::Comparison(c) => check_mixed_sides(&c.left, &c.right, keys),
        Expr::Paren(inner) => check_non_agg_parts_match_keys(inner, keys),
        Expr::StringInterp(parts) => {
            interp_embeds(parts).any(|e| check_non_agg_parts_match_keys(e, keys))
        }
        _ => false,
    }
}

/// Two-sided dispatch shared by the Binary and Comparison arms. Both arms
/// state the same policy: the planner's IR lowers comparisons to the same
/// `Expr::Binary` node and checks them identically, so the front end does
/// too (stellar F-1-11 — the Comparison arm's missing both-sides recursion
/// was a copy-paste divergence inherited from the original import, not a
/// ruling).
///
/// - Exactly one side aggregates: the other side must be a valid
///   non-aggregation part.
/// - Both sides aggregate: recurse into each; a nested mixed sub-expression
///   can still hide an invalid non-aggregation part.
/// - Neither side aggregates: nothing to check here.
fn check_mixed_sides(left: &Expr, right: &Expr, keys: &GroupingKeys) -> bool {
    let left_agg = contains_aggregation(left);
    let right_agg = contains_aggregation(right);
    if left_agg != right_agg {
        let non_agg_side = if left_agg { right } else { left };
        return !is_valid_non_agg_part(non_agg_side, keys);
    }
    if left_agg {
        return check_non_agg_parts_match_keys(left, keys)
            || check_non_agg_parts_match_keys(right, keys);
    }
    false
}

/// Check if a non-aggregation sub-expression is valid (matches a grouping key
/// or is a simple/constant expression).
fn is_valid_non_agg_part(expr: &Expr, keys: &GroupingKeys) -> bool {
    // The caller reaches here on a side the historical aggregation reach
    // ([`contains_aggregation`]) classified as non-aggregating — a reach
    // blind to list literals, CASE arms, comprehensions and dynamic keys.
    // The one full-reach predicate closes those holes up front: a part
    // hiding an aggregate anywhere is that aggregation's own input, never
    // a grouping-key reference, so no arm below re-decides aggregate-ness.
    !contains_aggregation_anywhere(expr) && is_valid_non_agg_shape(expr, keys)
}

/// The shape walk under [`is_valid_non_agg_part`]'s one-shot aggregate
/// gate: the entry's full-reach scan has cleared the whole subtree, so
/// the `List` and `FunctionCall` recursions come straight here and the
/// scan never re-fires on a child.
fn is_valid_non_agg_shape(expr: &Expr, keys: &GroupingKeys) -> bool {
    match expr {
        // Constants and literals are always valid
        Expr::Literal(_) | Expr::Parameter(_) => true,
        // List of grouped items is fine
        Expr::List(items) => items.iter().all(|i| is_valid_non_agg_shape(i, keys)),
        // A non-aggregate part that is *verbatim* a grouping key expression is
        // that key, however complex: `RETURN me.age + you.age, count(*) AS cnt
        // ORDER BY me.age + you.age + count(*)` (ReturnOrderBy6 [5]) decomposes
        // the projected expression. Any other complex shape stays ambiguous.
        _ if keys.matches_key(expr) => true,
        // Simple variables covered by the grouping keys are valid.
        Expr::Variable(v) => keys.covers_variable(v),
        // Property access on a grouping key variable (`*` coverage does not
        // extend to property expressions — see `key_variable`); a property
        // of anything else is not valid. The base resolves through the one
        // paren-transparent property recognizer ([`var_property`],
        // LABS-1615): `(n).p1` reads the key variable `n` like every other
        // spelling.
        Expr::Property(_) => var_property(expr).is_some_and(|(v, _)| keys.key_variable(v)),
        // Redundant parens are transparent, the paren arm the sibling walk
        // (`check_non_agg_parts_match_keys`) has always had: `(n.p1) +
        // count(*)` beside a projected `n.p1` validates exactly like the
        // bare spelling.
        Expr::Paren(inner) => is_valid_non_agg_shape(inner, keys),
        // A scalar function call whose arguments are all valid
        // non-aggregation parts: a grouping key is referenceable through a
        // function exactly as the Property arm covers a property of one —
        // `WITH count(n) AS c MATCH (m) RETURN toFloat(c) / count(m) AS
        // coverage, c` (REL8-1869) reads the projected key `c` through
        // `toFloat`. Non-deterministic calls ride the same arm — live Neo4j
        // 2026.10.1 admits `coalesce(rand(), c) / count(m)`, grouping by
        // the per-row remainder — while the group key here stays the
        // projected items, so such a call takes one value per group.
        Expr::FunctionCall(fc) => fc.args.iter().all(|arg| is_valid_non_agg_shape(arg, keys)),
        // Every other shape — a map, a CASE, a comprehension — is not a
        // grouping-key reference: valid only as a verbatim key (the guard
        // arm above), rejected here.
        _ => false,
    }
}

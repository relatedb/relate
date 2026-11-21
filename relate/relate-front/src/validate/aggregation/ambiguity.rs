//! RETURN/WITH grouping-key ambiguity: in an expression that mixes
//! aggregation with non-aggregation, every non-aggregation variable must be
//! a grouping-key variable — anything else is ambiguous. The *shape*
//! discipline over those variables (each leaf an exact key or a property of
//! a bare-variable key, so `RETURN n.x, n.y + count(n.x)` is ambiguous while
//! `RETURN n.x, n.x[0] + count(*)` is not) is the planner's
//! `check_item_grouping`, which sees the clause's grouping keys and the
//! leaves a nested scope contributes; this module owns the variable-coverage
//! gate that runs before lowering.

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_parser::Expr;

use super::collect_non_agg_variables;
use crate::agg::contains_aggregation;
use crate::ast::collect::collect_expr_variables;

/// The grouping keys of a projection clause, bundled with the derived
/// facts the ambiguity checks need about them: the variables the key
/// expressions mention (extended by [`GroupingKeys::with_aliases`] with
/// projected-alias names), and whether a `*` item is among the keys.
/// Bundling them keeps the ADR-0198 ruling-1 policy in one place and
/// collapses the per-leaf pass-through plumbing to a single
/// `&GroupingKeys` parameter.
pub(super) struct GroupingKeys {
    vars: IdentHashSet<SmallName>,
    star: bool,
}

impl GroupingKeys {
    pub(super) fn new(key_exprs: &[&Expr]) -> Self {
        Self {
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
            vars,
            star: self.star,
        }
    }

    /// ADR-0198 ruling 1: a `*` item covers every in-scope variable;
    /// otherwise the exact-variable rule decides.
    pub(super) fn covers_variable(&self, v: &SmallName) -> bool {
        self.star || self.vars.contains(v)
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
/// The rule this gate owns: in a mixed expression, every non-aggregation
/// variable must be a grouping-key variable (a variable the clause's key
/// expressions mention, or a projected alias — [`GroupingKeys`]), and a `*`
/// key covers every in-scope variable (ADR-0198 ruling 1). Constants,
/// parameters and pure-aggregate expressions are never ambiguous.
///
/// The gate is deliberately variable-level only: whether the non-aggregation
/// *shape* around those variables is admissible — `n.x + count(*)` beside a
/// projected `n.x` is, `n.y + count(n.x)` is not — is decided once, by the
/// planner's `check_item_grouping` leaf discipline, which sees the clause's
/// grouping keys and the leaves nested scopes contribute. Live Neo4j
/// 2026.10.1 accepts the shapes this gate admits and the leaf discipline
/// clears (`MATCH (n) WITH n.x + 1 AS nx RETURN nx, nx - 1 + count(nx)`;
/// probed 2026-11-21) and rejects exactly what one of the two refuses.
///
/// The caller constructs the [`GroupingKeys`] model once per clause
/// (base keys for the projection-item check, alias-extended for the
/// WITH ORDER BY check) and passes it here.
pub(super) fn is_ambiguous_aggregation(expr: &Expr, keys: &GroupingKeys) -> bool {
    if !contains_aggregation(expr) {
        return false;
    }

    // Collect non-aggregation variables from the expression
    let mut non_agg_vars = IdentHashSet::default();
    collect_non_agg_variables(expr, &mut non_agg_vars);

    // A pure aggregate over constants/parameters has no variable to cover;
    // any non-agg variable outside the grouping keys is ambiguous.
    non_agg_vars.iter().any(|v| !keys.covers_variable(v))
}

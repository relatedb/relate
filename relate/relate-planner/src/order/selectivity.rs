//! Pricing of predicates no index serves.
//!
//! An endpoint whose predicates no index serves used to price at its
//! whole label count, as if the filter removed nothing. This module
//! prices those predicates: an equality of an unindexed key from the key's
//! statistics (the heavy-value count or rows over distinct values, with the
//! answer's provenance), a function-wrapped equality (`tolower(p.gender)`)
//! from the raw distinct count, and everything else from one documented
//! default table (an equality keeps a tenth of the rows, a one-sided range
//! a third). The result says which: [`crate::Estimate::Statistic`] when
//! every term came from statistics, [`crate::Estimate::Defaulted`] when any
//! term fell to the table — never a measurement.

use relate_ir::{orient, pattern_prop_key, var_prop, BinaryOp, Expr, NodePattern};
use relate_names::{NamedToken, SmallName, Token};

use super::estimate::Basis;
use crate::stats::{PatternStats, Provenance};

/// Equality (`=`) or a function-wrapped equality of one property.
const EQUALITY: f64 = 0.1;
/// A one-sided range (`<`, `<=`, `>`, `>=`).
const RANGE_ONE_SIDED: f64 = 0.3;
/// Both bounds of one property, collapsed as a pair rather than
/// multiplied (two independent thirds would claim 0.09 for a window the
/// one property bounds at both ends).
const RANGE_PAIR: f64 = 0.1;
/// `<>` keeps nearly everything.
const NOT_EQUAL: f64 = 0.9;
/// `STARTS WITH` / `ENDS WITH` over a literal of length `len` keeps
/// `STRING_AFFIX / max(len, 1)`; a parameter assumes [`STRING_LEN`].
const STRING_AFFIX: f64 = 0.3;
const STRING_LEN: f64 = 6.0;
/// An `IN` list of unknown length.
const LIST_LEN: f64 = 25.0;
/// The floor of every selectivity: an estimate never reaches zero rows.
const MIN_SELECTIVITY: f64 = 1e-9;
/// Conjuncts combined by exponential backoff: the most selective
/// `BACKOFF_TERMS` count, each weighted by half the previous exponent.
const BACKOFF_TERMS: usize = 4;
/// Terms priced per endpoint. The ladder runs once per (pattern, row) in
/// the executor's cheapest-start loop, which stays allocation-free, so
/// the terms live in a fixed stack buffer. The stated policy: the backoff
/// reads only the [`BACKOFF_TERMS`] most selective terms, and a node with
/// more than `MAX_TERMS` priceable predicates keeps the first `MAX_TERMS`
/// in written order — the remainder is ignored, which can only make the
/// estimate less selective (the safe direction).
const MAX_TERMS: usize = 16;

/// Which end of the property's range a bound limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Lower,
    Upper,
}

impl Side {
    fn opposite(self) -> Side {
        match self {
            Side::Lower => Side::Upper,
            Side::Upper => Side::Lower,
        }
    }
}

/// What one conjunct says about the start node's property: a fixed
/// selectivity with its basis, or a one-sided range bound that may pair
/// with its opposite on the same property.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Term<'a> {
    Scalar(f64, Basis),
    Bound(&'a SmallName, Side),
}

/// A selectivity and the least trustworthy basis of its terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Priced {
    pub selectivity: f64,
    pub basis: Basis,
}

/// A comparison of the node's own property (or a function of it) with a
/// constant, oriented so the property is the left operand — the one
/// two-sided policy (`relate_ir::orient`) every recognizer in this crate
/// shares. A join predicate between two properties never matches. The key
/// comes with `wrapped`: whether a function sits between the comparison and
/// the property.
fn comparison<'a>(expr: &'a Expr, var: &SmallName) -> Option<(Property<'a>, BinaryOp, &'a Expr)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    let (property, constant, op) = orient(|e| targeted_property(e, var), left, right, *op)?;
    is_constant(constant).then_some((property, op, constant))
}

/// The property a comparison targets: its key beside the plan-resolved
/// token, and whether a function sits between the comparison and the
/// property.
#[derive(Clone, Copy)]
struct Property<'a> {
    key: &'a SmallName,
    token: Token,
    wrapped: bool,
}

impl<'a> Property<'a> {
    /// The unwrapped property `key`: the one constructor of the plain form
    /// (a recognized bare `var.key` and a folded property-map pair share it).
    fn plain(key: &'a SmallName, token: Token) -> Self {
        Property {
            key,
            token,
            wrapped: false,
        }
    }
}

impl Property<'_> {
    fn named(&self) -> NamedToken {
        (self.key.clone(), self.token)
    }
}

/// Everything the pricing reads about the start node: its labels, the
/// label's row count (the population a selectivity scales) and the stats.
pub(crate) struct Context<'s> {
    pub node: &'s NodePattern,
    pub label_rows: u64,
    pub stats: &'s dyn PatternStats,
}

impl Context<'_> {
    /// Selectivity of `property = value` from the key's statistics: the
    /// smallest per-label estimate (a multi-label node is the conjunction),
    /// as a fraction of the label's rows. `None` when no label's
    /// statistics are built.
    fn equality(&self, property: &NamedToken, value: &Expr) -> Option<(f64, Basis)> {
        let best = self
            .node
            .labels
            .iter()
            .filter_map(|label| self.stats.equality_estimate(label, property, value))
            .min_by_key(|estimate| estimate.rows)?;
        Some((self.fraction(best.rows), Basis::Statistic(best.provenance)))
    }

    /// Selectivity of a function-wrapped equality of `property`: one over
    /// the key's raw distinct count (folding can only merge values, so the
    /// raw count under-estimates the selected rows — the answer is a
    /// sketch, not a bound).
    fn wrapped_equality(&self, property: &NamedToken) -> Option<(f64, Basis)> {
        let distinct = self
            .node
            .labels
            .iter()
            .filter_map(|label| self.stats.key_stats(label, property))
            .map(|key| key.distinct)
            .filter(|distinct| *distinct > 0)
            .min()?;
        Some((
            1.0 / distinct as f64,
            Basis::Statistic(Provenance::Sketched),
        ))
    }

    fn fraction(&self, rows: u64) -> f64 {
        (rows as f64 / (self.label_rows.max(1) as f64)).clamp(MIN_SELECTIVITY, 1.0)
    }
}

/// The selectivity of every predicate on the node the ladder's measured
/// levels did not price: the folded property map's pairs and the
/// where-clause conjuncts that compare one of the node's properties (or a
/// function of it) with a constant. `None` when no such predicate exists,
/// so the caller keeps the plain label count.
pub(crate) fn price<'a>(
    ctx: &Context<'_>,
    where_conjuncts: impl Iterator<Item = &'a Expr>,
) -> Option<Priced> {
    let node = ctx.node;
    let mut terms = [Term::Scalar(1.0, Basis::Default); MAX_TERMS];
    let mut len = 0;
    let mut push = |term| {
        if len < MAX_TERMS {
            terms[len] = term;
            len += 1;
        }
    };
    if let Some(Expr::Map(pairs)) = node.properties.as_ref() {
        // like the where path, only a constant value is a filter: a map
        // value that reads another variable is a join key, not a narrowing
        for (key, value) in pairs.iter().filter(|(_, value)| is_constant(value)) {
            let (key, token) = pattern_prop_key(&node.prop_keys, key);
            let property = Property::plain(&key, token);
            push(equality_term(ctx, &property, value));
        }
    }
    if let Some(var) = node.variable.as_ref() {
        for expr in where_conjuncts {
            if let Some(term) = conjunct_term(ctx, expr, var) {
                push(term);
            }
        }
    }
    (len > 0).then(|| combine(&terms[..len]))
}

/// An equality of the property: from statistics when they speak (the
/// wrapped form from the raw distinct count), else the table.
fn equality_term<'a>(ctx: &Context<'_>, property: &Property<'_>, value: &Expr) -> Term<'a> {
    let key = property.named();
    let priced = if property.wrapped {
        ctx.wrapped_equality(&key)
    } else {
        ctx.equality(&key, value)
    };
    match priced {
        Some((selectivity, basis)) => Term::Scalar(selectivity, basis),
        None => Term::Scalar(EQUALITY, Basis::Default),
    }
}

/// `rows` scaled by `selectivity`, rounded up: a non-empty population
/// keeps at least one row and an empty one stays empty. The one place the
/// integer/float conversion lives; the selectivity is finite and in
/// `[MIN_SELECTIVITY, 1]` by construction ([`price`]).
pub(crate) fn scale_rows(rows: u128, selectivity: f64) -> u128 {
    debug_assert!(
        selectivity.is_finite() && (MIN_SELECTIVITY..=1.0).contains(&selectivity),
        "selectivity {selectivity} outside [MIN_SELECTIVITY, 1]"
    );
    ((rows as f64) * selectivity).ceil() as u128
}

/// Collapse opposite bounds of one property into one [`RANGE_PAIR`]
/// selectivity (each bound looks for an unconsumed opposite-side bound of
/// the same property later in the list), then back off over the most
/// selective terms: sorted ascending, `s0 * s1^(1/2) * s2^(1/4) *
/// s3^(1/8)`. That sits between full independence (a product, which
/// collapses a multi-property filter to near zero) and full correlation
/// (the minimum).
fn combine(terms: &[Term<'_>]) -> Priced {
    let mut selectivities = [1.0_f64; MAX_TERMS];
    let mut n = 0;
    let mut basis = Basis::Statistic(Provenance::Exact);
    let mut consumed = [false; MAX_TERMS];
    for (i, term) in terms.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        let (selectivity, term_basis) = match *term {
            Term::Scalar(s, term_basis) => (s, term_basis),
            Term::Bound(key, side) => {
                let partner = (i + 1..terms.len())
                    .find(|&j| !consumed[j] && terms[j] == Term::Bound(key, side.opposite()));
                match partner {
                    Some(j) => {
                        consumed[j] = true;
                        (RANGE_PAIR, Basis::Default)
                    }
                    None => (RANGE_ONE_SIDED, Basis::Default),
                }
            }
        };
        selectivities[n] = selectivity;
        basis = basis.min(term_basis);
        n += 1;
    }
    let selectivities = &mut selectivities[..n];
    selectivities.sort_by(f64::total_cmp);
    let selectivity = selectivities
        .iter()
        .take(BACKOFF_TERMS)
        .zip([1.0, 0.5, 0.25, 0.125])
        .map(|(s, exponent)| s.powf(exponent))
        .product::<f64>()
        .clamp(MIN_SELECTIVITY, 1.0);
    Priced { selectivity, basis }
}

/// One conjunct as a term on `var`'s property, or `None` when it is not a
/// comparison of that property with a constant.
fn conjunct_term<'a>(ctx: &Context<'_>, expr: &'a Expr, var: &SmallName) -> Option<Term<'a>> {
    let (property, op, constant) = comparison(expr, var)?;
    let scalar = |s| Term::Scalar(s, Basis::Default);
    Some(match op {
        BinaryOp::Eq => equality_term(ctx, &property, constant),
        BinaryOp::Neq => scalar(NOT_EQUAL),
        // a function of the property bounds a different value than the
        // property, so it never pairs: one one-sided term
        BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte if property.wrapped => {
            scalar(RANGE_ONE_SIDED)
        }
        BinaryOp::Lt | BinaryOp::Lte => Term::Bound(property.key, Side::Upper),
        BinaryOp::Gt | BinaryOp::Gte => Term::Bound(property.key, Side::Lower),
        BinaryOp::In => {
            let k = match constant {
                Expr::List(items) => items.len() as f64,
                _ => LIST_LEN,
            };
            scalar(1.0 - (1.0 - EQUALITY).powf(k))
        }
        BinaryOp::StartsWith | BinaryOp::EndsWith => {
            let len = match constant {
                Expr::String(text) => text.chars().count() as f64,
                _ => STRING_LEN,
            };
            scalar(STRING_AFFIX / len.max(1.0))
        }
        _ => return None,
    })
}

/// The property when `expr` is `var.key` or a function of it with
/// constant remaining arguments (`tolower(var.key)`).
fn targeted_property<'a>(expr: &'a Expr, var: &SmallName) -> Option<Property<'a>> {
    if let (Some((name, key)), Expr::Property { key_id, .. }) = (var_prop(expr), expr) {
        return (name == var).then_some(Property::plain(key, *key_id));
    }
    let Expr::Function { args, .. } = expr else {
        return None;
    };
    let (first, rest) = args.split_first()?;
    rest.iter().all(is_constant).then(|| {
        targeted_property(first, var).map(|property| Property {
            wrapped: true,
            ..property
        })
    })?
}

/// Functions whose value changes between rows or calls (the engine's
/// registry marks `e()` and `pi()` deterministic and these not): a
/// comparison against one is not a fixed-value filter.
const VOLATILE_FUNCTIONS: [&str; 3] = ["rand", "randomUUID", "timestamp"];

/// A value fixed before the statement runs: a literal, a parameter, or a
/// function / list / negation of such.
fn is_constant(expr: &Expr) -> bool {
    match expr {
        leaf if leaf.is_seek_const() || matches!(leaf, Expr::RuntimeValue(_)) => true,
        Expr::List(items) => items.iter().all(is_constant),
        // a volatile call varies per row, so it never fixes a value; every
        // other function of constants is a fixed value for the statement
        Expr::Function { name, args, .. } => {
            !name.with_str(|n| VOLATILE_FUNCTIONS.iter().any(|v| n.eq_ignore_ascii_case(v)))
                && args.iter().all(is_constant)
        }
        // every unary operator distributes over a constant operand
        Expr::Unary { expr, .. } => is_constant(expr),
        _ => false,
    }
}

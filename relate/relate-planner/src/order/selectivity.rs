//! The default selectivity table for predicates no statistic can price.
//!
//! An endpoint whose predicates no index serves used to price at its
//! whole label count, as if the filter removed nothing. This module
//! prices those predicates from one documented table, so an unindexed or
//! function-wrapped filter (`c.city = 'London'`,
//! `tolower(p.gender) = tolower($g)`) narrows its endpoint's estimate, and
//! says so: the result is [`crate::Estimate::Defaulted`], never a
//! measurement. Constants follow the values mature planners settle on
//! (an equality keeps a tenth of the rows, a one-sided range a third);
//! the stats-backed levels (distinct counts, heavy hitters) replace them
//! per key as those statistics land.

use relate_ir::{key_of, orient, BinaryOp, Expr, NodePattern};
use relate_names::SmallName;

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
/// selectivity, or a one-sided range bound that may pair with its
/// opposite on the same property.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Term<'a> {
    Scalar(f64),
    Bound(&'a SmallName, Side),
}

/// A comparison of the node's own property (or a function of it) with a
/// constant, oriented so the property is the left operand — the one
/// two-sided policy (`relate_ir::orient`) every recognizer in this crate
/// shares. A join predicate between two properties never matches.
fn comparison<'a>(expr: &'a Expr, var: &SmallName) -> Option<(&'a SmallName, BinaryOp, &'a Expr)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    let (key, constant, op) = orient(|e| targeted_key(e, var), left, right, *op)?;
    is_constant(constant).then_some((key, op, constant))
}

/// The default selectivity of every predicate on `node` the ladder's
/// measured levels did not price: the folded property map's pairs and the
/// where-clause conjuncts that compare one of the node's properties (or a
/// function of it) with a constant. `None` when no such predicate exists,
/// so the caller keeps the plain label count.
pub(crate) fn default_selectivity<'a>(
    node: &NodePattern,
    where_conjuncts: impl Iterator<Item = &'a Expr>,
) -> Option<f64> {
    let mut terms = [Term::Scalar(1.0); MAX_TERMS];
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
        for _ in pairs.iter().filter(|(_, value)| is_constant(value)) {
            push(Term::Scalar(EQUALITY));
        }
    }
    if let Some(var) = node.variable.as_ref() {
        for expr in where_conjuncts {
            if let Some(term) = conjunct_term(expr, var) {
                push(term);
            }
        }
    }
    (len > 0).then(|| combine(&terms[..len]))
}

/// `rows` scaled by `selectivity`, rounded up: a non-empty population
/// keeps at least one row and an empty one stays empty. The one place the
/// integer/float conversion lives; the selectivity is finite and in
/// `[MIN_SELECTIVITY, 1]` by construction ([`default_selectivity`]).
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
fn combine(terms: &[Term<'_>]) -> f64 {
    let mut selectivities = [1.0_f64; MAX_TERMS];
    let mut n = 0;
    let mut consumed = [false; MAX_TERMS];
    for (i, term) in terms.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        selectivities[n] = match *term {
            Term::Scalar(s) => s,
            Term::Bound(key, side) => {
                let partner = (i + 1..terms.len())
                    .find(|&j| !consumed[j] && terms[j] == Term::Bound(key, side.opposite()));
                match partner {
                    Some(j) => {
                        consumed[j] = true;
                        RANGE_PAIR
                    }
                    None => RANGE_ONE_SIDED,
                }
            }
        };
        n += 1;
    }
    let selectivities = &mut selectivities[..n];
    selectivities.sort_by(f64::total_cmp);
    selectivities
        .iter()
        .take(BACKOFF_TERMS)
        .zip([1.0, 0.5, 0.25, 0.125])
        .map(|(s, exponent)| s.powf(exponent))
        .product::<f64>()
        .clamp(MIN_SELECTIVITY, 1.0)
}

/// One conjunct as a term on `var`'s property, or `None` when it is not a
/// comparison of that property with a constant.
fn conjunct_term<'a>(expr: &'a Expr, var: &SmallName) -> Option<Term<'a>> {
    let (key, op, constant) = comparison(expr, var)?;
    Some(match op {
        BinaryOp::Eq => Term::Scalar(EQUALITY),
        BinaryOp::Neq => Term::Scalar(NOT_EQUAL),
        BinaryOp::Lt | BinaryOp::Lte => Term::Bound(key, Side::Upper),
        BinaryOp::Gt | BinaryOp::Gte => Term::Bound(key, Side::Lower),
        BinaryOp::In => {
            let k = match constant {
                Expr::List(items) => items.len() as f64,
                _ => LIST_LEN,
            };
            Term::Scalar(1.0 - (1.0 - EQUALITY).powf(k))
        }
        BinaryOp::StartsWith | BinaryOp::EndsWith => {
            let len = match constant {
                Expr::String(text) => text.chars().count() as f64,
                _ => STRING_LEN,
            };
            Term::Scalar(STRING_AFFIX / len.max(1.0))
        }
        _ => return None,
    })
}

/// The property key when `expr` is `var.key` or a function of it with
/// constant remaining arguments (`tolower(var.key)`).
fn targeted_key<'a>(expr: &'a Expr, var: &SmallName) -> Option<&'a SmallName> {
    if let Some(key) = key_of(expr, var) {
        return Some(key);
    }
    let Expr::Function { args, .. } = expr else {
        return None;
    };
    let (first, rest) = args.split_first()?;
    rest.iter()
        .all(is_constant)
        .then(|| targeted_key(first, var))
        .flatten()
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

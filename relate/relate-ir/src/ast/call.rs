//! The `CALL { … } IN TRANSACTIONS` AST family: the scope clause
//! ([`CallScope`], CIP-141 / CIP-32) and the transaction modifiers it
//! carries ([`InTransactions`] and its option enums, CIP-34 / CIP-198 /
//! CIP-260). Split from [`super`] so the clause vocabulary stays one
//! concern per file; [`super`] re-exports every item, so the public path
//! is unchanged.

use super::Expr;
use relate_names::SmallName;

#[derive(Debug, Clone, PartialEq)]
pub enum CallScope {
    None,
    Wildcard,
    Vars(Vec<SmallName>),
}

/// A `CALL`'s one passing-mode value, lowered once by the front end
/// from the parser's args-plus-flag pair: the explicit positional
/// argument expressions of the parenthesized form, or the implicit
/// parameter/row-column form a paren-less call takes. The mode is the
/// clause's own fact — never a bool beside a bare argument slice — so
/// no consumer re-encodes it; the executors borrow this value and
/// match its variants.
#[derive(Debug, Clone, PartialEq)]
pub enum CallArgs {
    /// `CALL proc(expr, …)`: the positional expressions, in order.
    Explicit(Vec<Expr>),
    /// `CALL proc`: no argument list. The arguments resolve by name
    /// (parameters, row columns) in the executor's binder.
    Implicit,
}

impl CallArgs {
    /// The positional expressions — the explicit form's list, or the
    /// empty slice the implicit form passes by the grammar (a
    /// paren-less call has no argument list). Arity checks and
    /// positional readers consult this; the mode itself is the
    /// variant, never a bool.
    pub fn args(&self) -> &[Expr] {
        match self {
            CallArgs::Explicit(args) => args,
            CallArgs::Implicit => &[],
        }
    }

    /// Whether this is the explicit parenthesized form.
    pub fn is_explicit(&self) -> bool {
        matches!(self, CallArgs::Explicit(_))
    }
}

impl CallScope {
    /// Whether the subquery is a scope clause (`CALL (…) { … }` /
    /// `CALL (*) { … }`) rather than the naked `CALL { … }`. Only a scope
    /// clause carries an import set; the naked form imports
    /// the whole row without one. The one statement of that fact.
    pub fn has_scope_clause(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InTransactions {
    pub concurrency: Concurrency,
    pub batch_size: Option<Expr>,
    pub on_error: OnError,
    pub report_status: Option<SmallName>,
    /// `DISJOINT BY` operand (a concurrent-batching hint).
    pub disjoint: DisjointBy,
}

impl InTransactions {
    /// Every value expression the modifiers carry, in source order: the
    /// batch size, the concurrency count, the retry duration, and each
    /// `DISJOINT BY` key. The one slot enumeration — the IR walker and the
    /// planner's modifier checks read it instead of re-spelling the four
    /// fields, so a new slot cannot be walked by one and forgotten by the
    /// other.
    pub fn exprs(&self) -> impl Iterator<Item = &Expr> {
        let mut out = Vec::new();
        out.extend(self.batch_size.as_ref());
        if let Concurrency::Value(expr) = &self.concurrency {
            out.push(expr);
        }
        if let OnError::Retry {
            duration: Some(duration),
            ..
        } = &self.on_error
        {
            out.push(duration);
        }
        if let DisjointBy::Expressions(keys) = &self.disjoint {
            out.extend(keys);
        }
        out.into_iter()
    }
}

/// `DISJOINT BY` operand: absent, the key expressions that
/// partition the input rows, or the `AUTO`/`NONE` keyword forms.
///
/// Absence is a variant rather than an `Option` wrapper so the parser
/// produces exactly one value and every downstream match is total — the
/// clause's `NONE` keyword form stays spellable next to "no clause".
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DisjointBy {
    #[default]
    Absent,
    Expressions(Vec<Expr>),
    Auto,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Concurrency {
    Sequential,
    Default,
    Value(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum OnError {
    Fail,
    Continue,
    Break,
    Retry {
        duration: Option<Expr>,
        fallback: RetryFallback,
    },
}

impl OnError {
    /// The batch's failure handling as the executor applies it: `FAIL`,
    /// `CONTINUE`, `BREAK`, or a `RETRY`'s explicit `THEN` fallback. The
    /// one projection of the `OnError` vocabulary onto [`RetryFallback`].
    pub fn outcome(&self) -> RetryFallback {
        match self {
            Self::Fail => RetryFallback::Fail,
            Self::Continue => RetryFallback::Continue,
            Self::Break => RetryFallback::Break,
            Self::Retry { fallback, .. } => *fallback,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryFallback {
    Fail,
    Continue,
    Break,
}

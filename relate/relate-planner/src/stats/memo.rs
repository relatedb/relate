//! ADR-0508's per-statement memo over the planner's value-keyed stats
//! consults: one [`PatternStats`] wrapper, built once by
//! [`crate::plan_with_stats`] for a whole plan call and dropped at plan
//! return, that answers the three value-keyed consults
//! ([`PatternStats::equality_rows`], [`PatternStats::range_rows`],
//! [`PatternStats::contains_rows`]) from a table of the consults already
//! made and forwards every other method unmemoized — the counter reads
//! (`label_rows`, `rel_type_rows`, `degree_tail`) are O(1) keyed by
//! interned names, where a memo lookup would cost about as much as the
//! answer, so the split is stated once, here, and nowhere else.
//!
//! The greedy `order_patterns` loop re-prices every pending pattern on
//! each iteration, so one statement asks the same `(label, property,
//! operand)` question repeatedly; the memo answers repeats from the table
//! instead of the store. No invalidation, by construction (ADR-0508 §3):
//! the memo lives inside one plan call, and the admission path holds the
//! store's publication read guard for that whole call, so no commit,
//! DDL, or `--follow` reload can publish between two consults it
//! de-duplicates. A future cross-statement layer would have to key on
//! `(graph identity, commit_ts)` instead — never a DDL-only counter,
//! because data commits move every number the ladder reads.

use std::cell::RefCell;
use std::sync::Arc;

use relate_ir::{BinaryOp, Expr};
use relate_names::{NamedToken, SmallName};

use super::ContainsCount;
use super::PatternStats;

/// The borrowed classification of a consult operand into its memo identity
/// (ADR-0508 §1): a parameter, keyed by its parameter name (a sound key
/// because the memo never outlives one plan call, whose parameter map is
/// fixed), and a literal, keyed by its literal value. Mirrors
/// [`Expr::seek_value`]'s parameter and literal arms; any other operand
/// shape — including the runtime-value arm seek-value resolves — has no
/// identity to key on inside one plan call. Both the stored key
/// ([`SeekOperand::from_ref`], which clones this) and the membership
/// test ([`SeekOperand`'s `PartialEq<SeekOperandRef>`], which compares
/// against it) derive from this one classification, so storing and
/// matching cannot disagree.
#[derive(Clone, Copy)]
enum SeekOperandRef<'a> {
    Parameter(&'a str),
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
}

impl<'a> SeekOperandRef<'a> {
    /// The one classification (ADR-0508 §1).
    fn of(expr: &'a Expr) -> Option<Self> {
        match expr {
            Expr::Parameter(name) => Some(Self::Parameter(name.as_str())),
            Expr::Null => Some(Self::Null),
            Expr::Bool(value) => Some(Self::Bool(*value)),
            Expr::Int(value) => Some(Self::Int(*value)),
            Expr::Float(value) => Some(Self::Float(*value)),
            Expr::String(text) => Some(Self::Str(text.as_str())),
            _ => None,
        }
    }
}

/// The seek operand's stored memo identity, cloned once from
/// [`SeekOperandRef::of`] at first sight (ADR-0508 §1). `PartialEq`
/// against the borrowed classification is the membership test.
enum SeekOperand {
    Parameter(String),
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
}

impl SeekOperand {
    /// The stored key: the borrowed classification, cloned.
    fn from_ref(operand: SeekOperandRef<'_>) -> Self {
        match operand {
            SeekOperandRef::Parameter(name) => SeekOperand::Parameter(name.to_owned()),
            SeekOperandRef::Null => SeekOperand::Null,
            SeekOperandRef::Bool(value) => SeekOperand::Bool(value),
            SeekOperandRef::Int(value) => SeekOperand::Int(value),
            SeekOperandRef::Float(value) => SeekOperand::Float(value),
            SeekOperandRef::Str(text) => SeekOperand::Str(Arc::from(text)),
        }
    }
}

impl PartialEq<SeekOperandRef<'_>> for SeekOperand {
    fn eq(&self, other: &SeekOperandRef<'_>) -> bool {
        match (self, other) {
            (SeekOperand::Parameter(name), SeekOperandRef::Parameter(other_name)) => {
                name.as_str() == *other_name
            }
            (SeekOperand::Null, SeekOperandRef::Null) => true,
            (SeekOperand::Bool(a), SeekOperandRef::Bool(b)) => a == b,
            (SeekOperand::Int(a), SeekOperandRef::Int(b)) => a == b,
            (SeekOperand::Float(a), SeekOperandRef::Float(b)) => a == b,
            (SeekOperand::Str(a), SeekOperandRef::Str(b)) => &**a == *b,
            _ => false,
        }
    }
}

/// The `(label, property, operand)` question `equality_rows` and
/// `contains_rows` ask. One field list serves both sides: the stored
/// [`PairQuestion`] (cloned once from the consult arguments at first
/// sight, ADR-0508 §1) and the borrowed [`PairQuestionRef`] (the
/// consult's own arguments) compare through `PartialEq`, so key
/// construction and membership matching cannot disagree.
struct PairQuestion {
    label: SmallName,
    property: SmallName,
    operand: SeekOperand,
}

/// The borrowed form of [`PairQuestion`]: the consult's arguments
/// themselves, no clone.
struct PairQuestionRef<'a> {
    label: &'a SmallName,
    property: &'a SmallName,
    operand: SeekOperandRef<'a>,
}

impl PartialEq<PairQuestionRef<'_>> for PairQuestion {
    fn eq(&self, other: &PairQuestionRef<'_>) -> bool {
        &self.label == other.label
            && &self.property == other.property
            && self.operand == other.operand
    }
}

/// The `range_rows` question: the pair question plus the comparison op —
/// the same bound under `<` and `<=` is a different window (ADR-0508
/// §1). Composes [`PairQuestion`] rather than restating its identity, so
/// the `(label, property, operand)` fields and their comparison are
/// stated once.
struct RangeQuestion {
    pair: PairQuestion,
    op: BinaryOp,
}

/// The borrowed form of [`RangeQuestion`].
struct RangeQuestionRef<'a> {
    pair: PairQuestionRef<'a>,
    op: BinaryOp,
}

impl PartialEq<RangeQuestionRef<'_>> for RangeQuestion {
    fn eq(&self, other: &RangeQuestionRef<'_>) -> bool {
        self.op == other.op && self.pair == other.pair
    }
}

/// The memo's per-method consult tables — one linear-scan `Vec` per
/// value-keyed method, the executor `ContainsMemo`'s reasoning (a query
/// supplies the keys and per-statement counts are small) without hashing
/// an [`Expr`] or a float-bearing literal. Each entry is a stored
/// question beside the consult's full typed answer — never a
/// `SampleScan` collapsed to its rows.
#[derive(Default)]
struct Consults {
    equality: Vec<(PairQuestion, u64)>,
    range: Vec<(RangeQuestion, u64)>,
    contains: Vec<(PairQuestion, ContainsCount)>,
}

/// ADR-0508's per-statement memo: see the [module](self) docs.
pub(crate) struct StatsMemo<'a> {
    inner: &'a dyn PatternStats,
    consults: RefCell<Consults>,
}

impl<'a> StatsMemo<'a> {
    /// Wrap `inner` for one plan call — the one construction site,
    /// [`crate::plan_with_stats`].
    pub(crate) fn new(inner: &'a dyn PatternStats) -> Self {
        Self {
            inner,
            consults: RefCell::default(),
        }
    }

    /// The memo's one consult policy, reference-keyed (the reviewer
    /// medium's repair). The operand's memo identity is classified here,
    /// once — the one bypass: an operand with no identity
    /// ([`SeekOperandRef::of`]) can never match a stored question and is
    /// never stored, so it consults the inner stats directly and its
    /// repeats re-consult. With an identity, the scan compares the
    /// consult's **borrowed** question against each stored question
    /// through `PartialEq` — no clone, no allocation, whether it hits or
    /// misses — and only a consult that produced an answer is stored,
    /// its question cloned once from the arguments at first sight. A
    /// `None` answer (no index on the pair, unmeasurable count) is never
    /// stored either, so the zero-hit profile pays nothing per consult:
    /// repeats re-consult the inner stats, whose `None` is the cheap
    /// counter read.
    fn consult<'q, K, Q, A>(
        &self,
        table: &mut Vec<(K, A)>,
        operand: &'q Expr,
        query: impl FnOnce(SeekOperandRef<'q>) -> Q,
        question: impl FnOnce(SeekOperandRef<'q>) -> K,
        ask: impl FnOnce() -> Option<A>,
    ) -> Option<A>
    where
        A: Copy,
        K: PartialEq<Q>,
    {
        let Some(classified) = SeekOperandRef::of(operand) else {
            return ask();
        };
        let query = query(classified);
        if let Some((_, answer)) = table.iter().find(|(k, _)| *k == query) {
            return Some(*answer);
        }
        let answer = ask()?;
        table.push((question(classified), answer));
        Some(answer)
    }

    /// The `PairQuestion` consults' shared wiring (`equality_rows` and
    /// `contains_rows` ask the same `(label, property, operand)`
    /// question; only the table and the inner answer differ).
    fn pair_consult<A>(
        &self,
        table: &mut Vec<(PairQuestion, A)>,
        label: &SmallName,
        property: &SmallName,
        operand: &Expr,
        ask: impl FnOnce() -> Option<A>,
    ) -> Option<A>
    where
        A: Copy,
    {
        self.consult(
            table,
            operand,
            |classified| PairQuestionRef {
                label,
                property,
                operand: classified,
            },
            |classified| PairQuestion {
                label: label.clone(),
                property: property.clone(),
                operand: SeekOperand::from_ref(classified),
            },
            ask,
        )
    }
}

impl PatternStats for StatsMemo<'_> {
    fn label_rows(&self, label: &NamedToken) -> Option<u64> {
        self.inner.label_rows(label)
    }

    fn equality_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        value: &Expr,
    ) -> Option<u64> {
        self.pair_consult(
            &mut self.consults.borrow_mut().equality,
            &label.0,
            &property.0,
            value,
            || self.inner.equality_rows(label, property, value),
        )
    }

    fn range_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        self.consult(
            &mut self.consults.borrow_mut().range,
            bound,
            |classified| RangeQuestionRef {
                pair: PairQuestionRef {
                    label: &label.0,
                    property: &property.0,
                    operand: classified,
                },
                op,
            },
            |classified| RangeQuestion {
                pair: PairQuestion {
                    label: label.0.clone(),
                    property: property.0.clone(),
                    operand: SeekOperand::from_ref(classified),
                },
                op,
            },
            || self.inner.range_rows(label, property, op, bound),
        )
    }

    fn contains_rows(
        &self,
        label: &NamedToken,
        property: &NamedToken,
        needle: &Expr,
    ) -> Option<ContainsCount> {
        self.pair_consult(
            &mut self.consults.borrow_mut().contains,
            &label.0,
            &property.0,
            needle,
            || self.inner.contains_rows(label, property, needle),
        )
    }

    fn rel_type_rows(&self, rel_type: &NamedToken) -> Option<u64> {
        self.inner.rel_type_rows(rel_type)
    }

    fn degree_tail(&self, label: &NamedToken, rel_type: &NamedToken) -> Option<u64> {
        self.inner.degree_tail(label, rel_type)
    }

    fn any_degree_tail(&self, rel_type: Option<&SmallName>) -> Option<u64> {
        self.inner.any_degree_tail(rel_type)
    }
}

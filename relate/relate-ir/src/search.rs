//! The SEARCH clause's specification family (CIP-225/CIP-278/CIP-240):
//! index and combined source specs, their expression-slot enumeration,
//! and the decomposed single-stage filter predicates.

use crate::ast::Expr;

use crate::BinaryOp;
use relate_names::SmallName;

/// `<source specification clause>`:
/// `<index specification clause> | <combined specification clause>`.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchSpec {
    /// Boxed to keep the variant sizes close (the flat index
    /// specification carries five `Expr` slots).
    Index(Box<SearchIndexSpec>),
    Combine(SearchCombineSpec),
}

/// `<index specification clause>`: `{ VECTOR | FULLTEXT } INDEX
/// <index name> <index for and control clause>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchIndexSpec {
    /// CIP-225: which index kind the `<index specification clause>` names.
    pub kind: SearchKind,
    pub index_name: String,
    pub for_expr: Expr,
    /// `WITH ANALYZER <expr>` (fulltext only; parsed, accepted, and
    /// not differentiated -- Relate has one tokenizer). Boxed to keep
    /// the enclosing `Clause` enum variant small.
    pub analyzer: Option<Box<Expr>>,
    /// The inner `WHERE <search condition>` decomposed once, at
    /// the front's lowering boundary, into its filter predicates (the
    /// `NOT <property>` and flipped-operand rewrites applied); empty when
    /// the clause carries no inner WHERE. The raw where expression does
    /// not travel past lowering.
    pub filters: Vec<SearchFilterPred>,
    /// `<offset clause>` (`SKIP`/`OFFSET`), shared with vector.
    pub offset_expr: Option<Box<Expr>>,
    pub limit_expr: Expr,
}

/// CIP-240: one decomposed inner-WHERE filter predicate. The front
/// decomposes the grammar once at lowering (the `NOT <property>` ->
/// `= FALSE` and flipped-operand rewrites applied) and the executor
/// consumes this list without re-walking it. The six shapes are exactly
/// the accepted `VectorFilterExpression` forms: a property comparison, a
/// property existence check, a property-in-set membership test, the
/// entity-set match filter (`<binding variable> IN <expr>`), and its
/// id-spelled twin (`id(<binding variable>) IN <expr>`). The LHS
/// variable is not carried: lowering already enforced the
/// binding-variable-only rule, and every predicate is evaluated against
/// the candidate entity.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchFilterPred {
    /// `<property> <comp op> <rhs>`.
    Comparison {
        property: String,
        op: SearchOp,
        rhs: Expr,
    },
    /// `<property> IS NOT NULL`.
    Exists { property: String },
    /// `<property> IS NULL`.
    NotExists { property: String },
    /// `<property> IN <rhs>`: the RHS evaluates to a list of scalars.
    InSet { property: String, rhs: Expr },
    /// `<binding variable> IN <rhs>`: the RHS evaluates to an entity set
    /// and the candidate must be a member of it.
    EntityInSet { rhs: Expr },
    /// `id(<binding variable>) IN <rhs>`: the RHS evaluates to a list of
    /// ids (or of entities, whose ids it reads) and the candidate's id
    /// must be a member — the id-spelled twin of the entity-set match
    /// filter, right-hand side a correlated `COLLECT` subquery included.
    /// `call` is the function name exactly as the lowering recognized
    /// it — the consumed left-hand call site's own spelling, carried so
    /// no later consumer re-derives it. The LHS variable is not carried:
    /// lowering already enforced the binding-variable-only rule, and
    /// every predicate is evaluated against the candidate entity.
    IdInSet { call: SmallName, rhs: Expr },
}

impl SearchFilterPred {
    /// The indexed-property name this predicate reads, if it reads one.
    /// [`SearchFilterPred::EntityInSet`] and
    /// [`SearchFilterPred::IdInSet`] match on candidate identity, so
    /// they have no property (the index-property-membership check skips
    /// them).
    pub fn property_name(&self) -> Option<&str> {
        self.parts().property
    }

    /// The value expression this predicate evaluates, if it has one.
    /// Existence predicates carry no RHS.
    pub fn rhs(&self) -> Option<&Expr> {
        self.parts().rhs
    }

    /// The function call site this predicate consumed, if it did: the
    /// recognized left-hand call's own name, carried from the lowering
    /// that matched it — the third per-variant accessor beside
    /// [`Self::property_name`] and [`Self::rhs`], so spec-wide
    /// enumerations fold over the walk without re-matching variants or
    /// re-deriving spellings. Crate-internal: its one consumer is this
    /// crate's own [`SearchSpec::consumed_function_calls`].
    fn consumed_call(&self) -> Option<&SmallName> {
        self.parts().call
    }

    /// Whether this predicate is one of the two entity-set match filters
    /// — [`SearchFilterPred::EntityInSet`] (the bare-variable form) or
    /// [`SearchFilterPred::IdInSet`] (its id-spelled twin): a filter on
    /// the candidates' identity, not on an indexed property. The
    /// classification the windows' entity-filter counting reads, stated
    /// in the one [`Self::parts`] projection the per-variant accessors
    /// share.
    pub fn is_entity_filter(&self) -> bool {
        self.parts().entity_filter
    }

    /// The one exhaustive projection over the predicate vocabulary —
    /// the single `match self` every per-variant accessor reads, so a
    /// new variant is one edit here plus a decision, never a third
    /// literal variant list.
    fn parts(&self) -> FilterPredParts<'_> {
        match self {
            SearchFilterPred::Comparison { property, rhs, .. } => FilterPredParts {
                property: Some(property),
                rhs: Some(rhs),
                call: None,
                entity_filter: false,
            },
            SearchFilterPred::Exists { property } | SearchFilterPred::NotExists { property } => {
                FilterPredParts {
                    property: Some(property),
                    rhs: None,
                    call: None,
                    entity_filter: false,
                }
            }
            SearchFilterPred::InSet { property, rhs } => FilterPredParts {
                property: Some(property),
                rhs: Some(rhs),
                call: None,
                entity_filter: false,
            },
            SearchFilterPred::EntityInSet { rhs } => FilterPredParts {
                property: None,
                rhs: Some(rhs),
                call: None,
                entity_filter: true,
            },
            SearchFilterPred::IdInSet { call, rhs } => FilterPredParts {
                property: None,
                rhs: Some(rhs),
                call: Some(call),
                entity_filter: true,
            },
        }
    }
}

/// The four per-variant facts of one [`SearchFilterPred`], as the one
/// [`SearchFilterPred::parts`] projection yields them: the indexed
/// property it reads, the value expression it evaluates, the function
/// call site it consumed, and whether it is one of the two entity-set
/// match filters.
struct FilterPredParts<'a> {
    property: Option<&'a str>,
    rhs: Option<&'a Expr>,
    call: Option<&'a SmallName>,
    /// Whether the predicate is one of the two entity-set match filters
    /// — the classification [`SearchFilterPred::is_entity_filter`] reads.
    entity_filter: bool,
}

/// The five `<simple comp op>`s the search-filter grammar admits (the
/// rest are banned) — the typed subset the filter predicates carry, so the op
/// vocabulary is stated once and every downstream match is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOp {
    Eq,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl SearchOp {
    /// The mirror for the flipped-operand rewrite — total over the
    /// subset, panic-free (`flip_cmp` serves the wider `BinaryOp`
    /// vocabulary, where the operand-asymmetry decline is real).
    pub fn flip(self) -> Self {
        match self {
            SearchOp::Eq => SearchOp::Eq,
            SearchOp::Lt => SearchOp::Gt,
            SearchOp::Gt => SearchOp::Lt,
            SearchOp::Lte => SearchOp::Gte,
            SearchOp::Gte => SearchOp::Lte,
        }
    }

    /// The one construction site: where a `BinaryOp` narrows into the
    /// simple subset. `None` is the decline of everything else.
    pub fn from_binary(op: BinaryOp) -> Option<Self> {
        Some(match op {
            BinaryOp::Eq => SearchOp::Eq,
            BinaryOp::Lt => SearchOp::Lt,
            BinaryOp::Lte => SearchOp::Lte,
            BinaryOp::Gt => SearchOp::Gt,
            BinaryOp::Gte => SearchOp::Gte,
            _ => return None,
        })
    }
}

impl From<SearchOp> for BinaryOp {
    fn from(op: SearchOp) -> Self {
        match op {
            SearchOp::Eq => BinaryOp::Eq,
            SearchOp::Lt => BinaryOp::Lt,
            SearchOp::Lte => BinaryOp::Lte,
            SearchOp::Gt => BinaryOp::Gt,
            SearchOp::Gte => BinaryOp::Gte,
        }
    }
}

/// `<index specification clause>`: `{ VECTOR | FULLTEXT } INDEX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Vector,
    Fulltext,
}

/// `<combined specification clause>`: `COMBINE ( <weighted source
/// clause>, ... ) USING WRRF [ WITH CONSTANT <expr> ] [ LIMIT <expr> ]`.
/// Two or more weighted sources (the parser rejects fewer).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCombineSpec {
    pub sources: Vec<SearchWeightedSource>,
    /// `None` means the default WRRF constant 60.
    pub constant_expr: Option<Box<Expr>>,
    /// `None` keeps all fused results.
    pub limit_expr: Option<Box<Expr>>,
}

/// `<weighted source clause>`:
/// `<source specification clause> [ WEIGHT <value expression> ]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchWeightedSource {
    pub spec: SearchSpec,
    /// `None` means the default weight 1.
    pub weight_expr: Option<Box<Expr>>,
}

impl SearchSpec {
    /// Every value expression of this source specification tree, in
    /// source order: the index control expressions (FOR, ANALYZER, the
    /// CIP-240 decomposed predicates' RHS, OFFSET, LIMIT) and the combine
    /// tree's WEIGHT/CONSTANT/LIMIT expressions.
    pub fn exprs(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        self.for_each_slot(&mut |slot| match slot {
            SearchSlot::Expr(expr) => out.push(expr),
            SearchSlot::Pred(pred) => {
                if let Some(rhs) = pred.rhs() {
                    out.push(rhs);
                }
            }
        });
        out
    }

    /// The function call sites the filter decomposition consumed: the
    /// recognized left-hand call names carried by the predicates
    /// (read through [`SearchFilterPred::consumed_call`], the third
    /// per-variant accessor), so the spelling exists only where the
    /// lowering matched it and no consumer re-derives it. The
    /// statement-notification observer maps each name to its
    /// deprecation occurrence — the same occurrence scope a surviving
    /// `Expr::Function` call site gets.
    pub fn consumed_function_calls(&self) -> Vec<&SmallName> {
        let mut out = Vec::new();
        self.for_each_slot(&mut |slot| {
            if let SearchSlot::Pred(pred) = slot {
                out.extend(pred.consumed_call());
            }
        });
        out
    }

    /// The one structural descent over this source tree, visiting every
    /// slot in source order — each value expression and each decomposed
    /// filter predicate, the combine tree's nested sources and its own
    /// control expressions included. Every spec-wide enumeration
    /// ([`Self::exprs`], [`Self::consumed_function_calls`]) derives from
    /// this walk, so the tree's structure knowledge stays in this single
    /// match and a new spec-wide need extends it instead of copying the
    /// recursion.
    fn for_each_slot<'a>(&'a self, f: &mut impl FnMut(SearchSlot<'a>)) {
        match self {
            SearchSpec::Index(index) => {
                f(SearchSlot::Expr(&index.for_expr));
                if let Some(expr) = &index.analyzer {
                    f(SearchSlot::Expr(expr));
                }
                for pred in &index.filters {
                    f(SearchSlot::Pred(pred));
                }
                if let Some(expr) = &index.offset_expr {
                    f(SearchSlot::Expr(expr));
                }
                f(SearchSlot::Expr(&index.limit_expr));
            }
            SearchSpec::Combine(combine) => {
                for source in &combine.sources {
                    source.spec.for_each_slot(f);
                    if let Some(expr) = &source.weight_expr {
                        f(SearchSlot::Expr(expr));
                    }
                }
                if let Some(expr) = &combine.constant_expr {
                    f(SearchSlot::Expr(expr));
                }
                if let Some(expr) = &combine.limit_expr {
                    f(SearchSlot::Expr(expr));
                }
            }
        }
    }
}

/// One slot of a source specification tree: a value expression, or one
/// decomposed filter predicate (whose right-hand side is an expression
/// slot of its own, read through [`SearchFilterPred::rhs`]).
enum SearchSlot<'a> {
    Expr(&'a Expr),
    Pred(&'a SearchFilterPred),
}

impl From<relate_parser::SearchKind> for SearchKind {
    fn from(kind: relate_parser::SearchKind) -> Self {
        match kind {
            relate_parser::SearchKind::Vector => SearchKind::Vector,
            relate_parser::SearchKind::Fulltext => SearchKind::Fulltext,
        }
    }
}

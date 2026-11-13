//! The SEARCH clause's specification family (CIP-225/CIP-278/CIP-240):
//! index and combined source specs, their expression-slot enumeration,
//! and the decomposed single-stage filter predicates.

use crate::ast::Expr;

use crate::BinaryOp;

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
/// consumes this list without re-walking it. The five shapes are exactly
/// the accepted `VectorFilterExpression` forms: a property comparison, a
/// property existence check, a property-in-set membership test, and the
/// entity-set match filter (`<binding variable> IN <expr>`). The LHS
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
}

impl SearchFilterPred {
    /// The indexed-property name this predicate reads, if it reads one.
    /// [`SearchFilterPred::EntityInSet`] matches on candidate identity, so
    /// it has no property (the index-property-membership check skips it).
    pub fn property_name(&self) -> Option<&str> {
        match self {
            SearchFilterPred::Comparison { property, .. }
            | SearchFilterPred::Exists { property }
            | SearchFilterPred::NotExists { property }
            | SearchFilterPred::InSet { property, .. } => Some(property),
            SearchFilterPred::EntityInSet { .. } => None,
        }
    }

    /// The value expression this predicate evaluates, if it has one.
    /// Existence predicates carry no RHS.
    pub fn rhs(&self) -> Option<&Expr> {
        match self {
            SearchFilterPred::Comparison { rhs, .. }
            | SearchFilterPred::InSet { rhs, .. }
            | SearchFilterPred::EntityInSet { rhs } => Some(rhs),
            SearchFilterPred::Exists { .. } | SearchFilterPred::NotExists { .. } => None,
        }
    }
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
        push_spec_exprs(self, &mut out);
        out
    }
}

fn push_spec_exprs<'a>(spec: &'a SearchSpec, out: &mut Vec<&'a Expr>) {
    match spec {
        SearchSpec::Index(index) => {
            out.push(&index.for_expr);
            if let Some(expr) = &index.analyzer {
                out.push(expr);
            }
            for pred in &index.filters {
                if let Some(rhs) = pred.rhs() {
                    out.push(rhs);
                }
            }
            if let Some(expr) = &index.offset_expr {
                out.push(expr);
            }
            out.push(&index.limit_expr);
        }
        SearchSpec::Combine(combine) => {
            for source in &combine.sources {
                push_spec_exprs(&source.spec, out);
                if let Some(expr) = &source.weight_expr {
                    out.push(expr);
                }
            }
            if let Some(expr) = &combine.constant_expr {
                out.push(expr);
            }
            if let Some(expr) = &combine.limit_expr {
                out.push(expr);
            }
        }
    }
}

//! The expression model: [`Expr`] and the two small types its variants
//! carry ([`MapProjectionItem`], [`VectorMetric`]), with the methods that
//! answer questions about an expression itself, plus the binary-operator
//! vocabulary's shared facts ([`BinaryOp`]'s token spelling, boolean
//! connectives, and absorbing-constant fold). Split from [`super`] so the
//! statement/clause vocabulary and the expression vocabulary stay one
//! concern per file; [`super`] re-exports every item, so the public path is
//! unchanged (`ast::call` sets the same precedent).

use super::{BinaryOp, ChronoOp, LabelExpr, PathPattern, QuantifierKind, Query, TypeExpr, UnaryOp};
use relate_names::SmallName;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Executor-internal eager value; never produced by either parser.
    RuntimeValue(relate_core::Value),
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Parameter(String),
    Variable(SmallName),
    Star,
    Property {
        expr: Box<Expr>,
        key: SmallName,
    },
    DynamicProperty {
        expr: Box<Expr>,
        key: Box<Expr>,
    },
    LabelPredicate {
        expr: Box<Expr>,
        label_expr: LabelExpr,
        strict: bool,
    },
    Map(Vec<(String, Expr)>),
    List(Vec<Expr>),
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Function {
        name: SmallName,
        distinct: bool,
        args: Vec<Expr>,
    },
    CountStar,
    Case {
        /// `(predicate, result)` pairs, evaluated as booleans in order. The
        /// parser lowers a simple-CASE test operand into each arm's equality
        /// comparison, so the searched form is the only one that reaches the
        /// executor.
        when_then: Vec<(Expr, Expr)>,
        else_expr: Option<Box<Expr>>,
    },
    Subscript {
        expr: Box<Expr>,
        index: Box<Expr>,
    },
    Slice {
        expr: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    ListComp {
        ident: SmallName,
        list: Box<Expr>,
        predicate: Option<Box<Expr>>,
        projection: Box<Expr>,
    },
    /// CIP-272 map comprehension (lowered from the parser's
    /// `MapComprehension`).
    MapComp {
        /// Key binding of a map source (`None` for a list source).
        key_ident: Option<SmallName>,
        value_ident: SmallName,
        source: Box<Expr>,
        predicate: Option<Box<Expr>>,
        key_expr: Box<Expr>,
        value_expr: Box<Expr>,
    },
    Quantifier {
        kind: QuantifierKind,
        ident: SmallName,
        list: Box<Expr>,
        predicate: Box<Expr>,
    },
    Reduce {
        accumulator: SmallName,
        initial: Box<Expr>,
        ident: SmallName,
        list: Box<Expr>,
        expression: Box<Expr>,
    },
    AllReduce {
        accumulator: SmallName,
        initial: Box<Expr>,
        ident: SmallName,
        list: Box<Expr>,
        expression: Box<Expr>,
        predicate: Box<Expr>,
    },
    IsType {
        expr: Box<Expr>,
        type_expr: TypeExpr,
        negated: bool,
    },
    IsNormalized {
        expr: Box<Expr>,
        form: Option<String>,
        negated: bool,
    },
    ChronoCompare {
        left: Box<Expr>,
        op: ChronoOp,
        right: Box<Expr>,
        negated: bool,
    },
    /// Map projection `expr { key: value, .key, var, .* }`. Typed form of
    /// what used to be tunnelled through a `__map_projection` pseudo-call.
    MapProjection {
        expr: Box<Expr>,
        items: Vec<MapProjectionItem>,
    },
    /// CIP-214 distance-metric keyword (e.g. the `EUCLIDEAN` in
    /// `vector_distance(a, b, EUCLIDEAN)`), resolved at lowering from the
    /// bare identifier in the final argument of `vector_distance` /
    /// `vector_norm`. Only meaningful as an argument to those functions.
    DistanceMetric(VectorMetric),
    PatternPred {
        path: PathPattern,
    },
    PatternComp {
        path: PathPattern,
        predicate: Option<Box<Expr>>,
        projection: Box<Expr>,
    },
    ShortestPath {
        all: bool,
        path: PathPattern,
    },
    ExistsSubquery {
        query: Query,
    },
    CountSubquery {
        query: Query,
    },
    CollectSubquery {
        query: Query,
    },
}

/// One entry of a map projection (`Expr::MapProjection`).
#[derive(Debug, Clone, PartialEq)]
pub enum MapProjectionItem {
    /// Explicit `key: value` entry.
    KeyValue { key: SmallName, value: Expr },
    /// `.key` property shorthand, read from the projection target.
    Property { key: SmallName },
    /// Bare variable entry: `var` shorthand for `var: var`.
    Variable { name: SmallName },
    /// `.*`: every property of the target.
    AllProperties,
}

/// CIP-214 distance metric for `vector_distance` / `vector_norm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorMetric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Cosine,
    Dot,
    Hamming,
}

impl VectorMetric {
    /// Every metric with its keyword — the one vocabulary table
    /// [`from_name`](Self::from_name) parses and [`name`](Self::name)
    /// renders, so a new variant cannot parse without also rendering.
    /// Public beyond the crate so downstream storable-subset tests
    /// iterate the table itself: a new variant is covered by
    /// construction, never by a re-derived literal list.
    pub const ALL: [(&'static str, Self); 6] = [
        ("EUCLIDEAN", Self::Euclidean),
        ("EUCLIDEAN_SQUARED", Self::EuclideanSquared),
        ("MANHATTAN", Self::Manhattan),
        ("COSINE", Self::Cosine),
        ("DOT", Self::Dot),
        ("HAMMING", Self::Hamming),
    ];

    /// Parse the bare metric keyword (case-insensitive). `None` when the
    /// identifier is not a known metric, in which case lowering leaves it as
    /// an ordinary variable reference for the binder to reject.
    pub fn from_name(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        Self::ALL
            .into_iter()
            .find(|(keyword, _)| *keyword == upper)
            .map(|(_, metric)| metric)
    }

    pub fn name(self) -> &'static str {
        Self::ALL
            .into_iter()
            .find(|(_, metric)| *metric == self)
            .map(|(keyword, _)| keyword)
            .expect("every VectorMetric is listed in ALL")
    }
}

impl Expr {
    /// Fold a seek predicate into a value. Literals fold directly.
    /// Parameters resolve through `params`. `RuntimeValue` is already a
    /// value. `None` means the expression is not a seek constant.
    pub fn seek_value(
        &self,
        params: &std::collections::BTreeMap<String, relate_core::Value>,
    ) -> Option<relate_core::Value> {
        use relate_core::Value;
        match self {
            Expr::Null => Some(Value::Null),
            Expr::Bool(value) => Some(Value::Bool(*value)),
            Expr::Int(value) => Some(Value::Int(*value)),
            Expr::Float(value) => Some(Value::Float(*value)),
            Expr::String(value) => Some(Value::String(std::sync::Arc::from(value.as_str()))),
            Expr::Parameter(name) => params.get(name).cloned(),
            Expr::RuntimeValue(value) => Some(value.clone()),
            _ => None,
        }
    }

    /// A literal or parameter that a planner may store on a pattern
    /// property map: the value forms [`seek_value`](Self::seek_value) folds
    /// without a runtime row. `Parameter` is a seek constant even though its
    /// value is only known at run time; `RuntimeValue` folds through
    /// `seek_value` but is too late for the planner.
    ///
    /// The set is stated directly instead of probed through `seek_value`,
    /// so a future parameterless arm there does not silently widen what the
    /// planner folds.
    pub fn is_seek_const(&self) -> bool {
        matches!(
            self,
            Expr::Null
                | Expr::Bool(_)
                | Expr::Int(_)
                | Expr::Float(_)
                | Expr::String(_)
                | Expr::Parameter(_)
        )
    }

    /// The subquery body this expression opens, if it is an
    /// expression-subquery variant (`EXISTS`/`COUNT`/`COLLECT`), else
    /// `None` — the one published answer to "which expressions open a
    /// subquery body", beside [`seek_value`](Self::seek_value), so a
    /// future variant extends this accessor instead of being hunted down
    /// in every consumer's match.
    pub fn subquery_body(&self) -> Option<&Query> {
        match self {
            Expr::ExistsSubquery { query }
            | Expr::CountSubquery { query }
            | Expr::CollectSubquery { query } => Some(query),
            _ => None,
        }
    }
}

/// The binary-operator vocabulary's shared facts, beside the operators they
/// name: the token spelling every rendering and operator-naming message
/// reads, the boolean-connective set, and the absorbing-constant fold.
impl BinaryOp {
    /// The operator's Cypher token — the one spelling every message naming
    /// an operator reads (ADR-0511's boolean-operand rejection among them)
    /// and the one expression rendering spells, so no site re-spells the
    /// vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Or => "OR",
            BinaryOp::Xor => "XOR",
            BinaryOp::And => "AND",
            BinaryOp::Eq => "=",
            BinaryOp::Neq => "<>",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Lte => "<=",
            BinaryOp::Gte => ">=",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Pow => "^",
            BinaryOp::Concat => "+",
            BinaryOp::In => "IN",
            BinaryOp::StartsWith => "STARTS WITH",
            BinaryOp::EndsWith => "ENDS WITH",
            BinaryOp::Contains => "CONTAINS",
            BinaryOp::Regex => "=~",
        }
    }

    /// Whether the operator is a boolean connective (`AND`/`OR`/`XOR`):
    /// exactly the operators whose operands are predicate positions.
    pub fn is_boolean(self) -> bool {
        matches!(self, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor)
    }

    /// The constant this binary node folds to when an operand is the
    /// operator's absorbing boolean constant (`x OR true` → `true`,
    /// `x AND false` → `false`), or `None` when it does not fold: the
    /// reference engine's rewriter discards the whole node — operands
    /// included — before variable resolution (`MiscAcceptance` "IN
    /// predicate with undefined variable is accepted when it can be
    /// rewritten away"). Exact under Cypher's three-valued logic
    /// (`null OR true` is `true`, `null AND false` is `false`).
    pub fn absorbing_constant(self, left: &Expr, right: &Expr) -> Option<bool> {
        match (self, left, right) {
            (BinaryOp::Or, Expr::Bool(true), _) | (BinaryOp::Or, _, Expr::Bool(true)) => Some(true),
            (BinaryOp::And, Expr::Bool(false), _) | (BinaryOp::And, _, Expr::Bool(false)) => {
                Some(false)
            }
            _ => None,
        }
    }
}

//! Expression nodes.

use super::*;

/// One segment of a CIP-261 interpolated string literal.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    /// Literal text (escaped braces already resolved to plain braces).
    Text(String),
    /// An `{expression}` embed.
    Interp(Box<Expr>),
}

/// All expression types.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(LiteralValue),
    List(Vec<Expr>),
    /// `PATH [n1, r1, n2, ...]` path constructor (preview `path-construction`):
    /// the element list, parsed but not yet checked for alternation.
    PathConstruct(Vec<Expr>),
    Map(Vec<MapPair>),
    /// CIP-261 interpolated string literal: text segments interleaved with
    /// `{expression}` embeds, lowered to the internal `string.interp`
    /// function (see `relate/relate-front/src/lower.rs`).
    StringInterp(Vec<InterpPart>),
    Variable(SmallName),
    Parameter(String),
    Property(Box<PropertyAccess>),
    FunctionCall(FunctionCallExpr),
    Binary(Box<BinaryExpr>),
    Unary(Box<UnaryExpr>),
    Comparison(Box<ComparisonExpr>),
    In(Box<InExpr>),
    IsNull(Box<IsNullExpr>),
    IsType(Box<IsTypeExpr>),
    IsNormalized(Box<IsNormalizedExpr>),
    ChronologicalCompare(Box<ChronologicalCompareExpr>),
    StringMatch(Box<StringMatchExpr>),
    CountStar,
    Star,
    Case(Box<CaseExpr>),
    ExistsSubquery(Box<ExistsSubqueryExpr>),
    CountSubquery(Box<CountSubqueryExpr>),
    CollectSubquery(Box<CollectSubqueryExpr>),
    ListComprehension(Box<ListComprehensionExpr>),
    MapComprehension(Box<MapComprehensionExpr>),
    PatternComprehension(Box<PatternComprehensionExpr>),
    Reduce(Box<ReduceExpr>),
    AllReduce(Box<AllReduceExpr>),
    ShortestPath(Box<ShortestPathExpr>),
    Subscript(Box<SubscriptExpr>),
    Quantifier(Box<QuantifierExpr>),
    HasLabel(Box<HasLabelExpr>),
    Paren(Box<Expr>),
    PatternPredicate(Box<PatternPredicateExpr>),
    MapProjection(Box<MapProjectionExpr>),
    /// The canonical `?` marker for an `OPTIONS` value outside the persisted
    /// five-literal vocabulary (a list, map, temporal, …), produced only when
    /// a whole-map `OPTIONS $param` is resolved into literal entries. The
    /// parser never produces it and it references nothing, so no walker can
    /// mistake it for a variable.
    OptionValuePlaceholder,
}

/// A literal value in the AST.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

/// A key-value pair in a map literal.
#[derive(Debug, Clone, PartialEq)]
pub struct MapPair {
    pub key: SmallName,
    pub value: Expr,
}

/// `<variable> <map projection>` (`n {.foo, bar, baz: qux}`): openCypher's
/// map projection, syntactically always rooted at a plain variable
/// (`expr` is always an `Expr::Variable`). Needed for CIP-190's
/// near-ambiguity coverage: `exists {name: exists.name}` etc. must parse
/// as a map projection on a variable named `exists`, not as
/// `EXISTS { <subquery> }`, when the filler between the braces looks like
/// projection items rather than subquery content (near-ambiguities.feature).
#[derive(Debug, Clone, PartialEq)]
pub struct MapProjectionExpr {
    pub expr: Expr,
    pub items: Vec<MapProjectionItem>,
}

/// One item of a `<map projection>`.
#[derive(Debug, Clone, PartialEq)]
pub enum MapProjectionItem {
    /// `.*` -- every property of the base entity.
    AllProperties,
    /// `.prop` -- shorthand for `prop: <base>.prop`.
    Property(SmallName),
    /// A bare `variable` selector -- shorthand for `variable: variable`,
    /// pulling a same-named variable from the surrounding scope (*not* a
    /// property of the base expression).
    Variable(SmallName),
    /// `key: <expr>` -- a literal entry.
    KeyValue(SmallName, Expr),
}

/// Property access like n.prop.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyAccess {
    pub expr: Expr,
    pub property: SmallName,
    /// CIP-131: when a SET/REMOVE target uses the dynamic-property bracket
    /// form (`n[<expr>]`) instead of the static `n.prop` form, the property
    /// key is computed at runtime from this expression rather than being the
    /// statically-known `property` string (which is left empty). `None` for
    /// every static `.prop` access, keeping `property` back-compatible for
    /// all existing (non-SET/REMOVE) uses of `PropertyAccess`.
    pub key_expr: Option<Expr>,
}

/// Function call like count(x).
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCallExpr {
    pub name: SmallName,
    pub args: Vec<Expr>,
    pub distinct: bool,
}

/// Binary expression like a + b. `op` is the operator's AST spelling, a
/// `&'static str` drawn from the parser's operator-constant table
/// (`parser/mod.rs`, ADR-0505 §2) — never a runtime-built `String`.
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExpr {
    pub left: Expr,
    pub op: &'static str,
    pub right: Expr,
}

/// Unary expression like NOT x or -x. `op` is the operator's AST spelling,
/// a `&'static str` drawn from the parser's operator-constant table
/// (`parser/mod.rs`, ADR-0505 §2) — never a runtime-built `String`.
#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExpr {
    pub op: &'static str,
    pub expr: Expr,
}

/// Comparison expression like a > b. `op` is the operator's AST spelling, a
/// `&'static str` from the `comparison_op` token-to-spelling table
/// (`parser/mod.rs`, ADR-0505 §2) — never a runtime-built `String`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonExpr {
    pub left: Expr,
    pub op: &'static str,
    pub right: Expr,
}

/// x IN list.
#[derive(Debug, Clone, PartialEq)]
pub struct InExpr {
    pub expr: Expr,
    pub list: Expr,
}

/// x IS NULL or x IS NOT NULL.
#[derive(Debug, Clone, PartialEq)]
pub struct IsNullExpr {
    pub expr: Expr,
    pub negated: bool,
}

/// x IS [NOT] :: <value type> (CIP-86/CIP-87 type predicate expression);
/// also produced by the `IS [NOT] TYPED <value type>` and bare `x :: <value
/// type>` surface forms, which are equivalent.
#[derive(Debug, Clone, PartialEq)]
pub struct IsTypeExpr {
    pub expr: Expr,
    pub type_expr: TypeExpr,
    pub negated: bool,
}

/// A parsed `<value type>` per the CIP-86 grammar: the vocabulary used by
/// the CIP-87 type predicate expression (`IS [NOT] :: <type>`) and, once
/// CIP-90 ships, by `valueType()`.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// A predefined leaf type: a scalar, temporal, reference, point, map,
    /// nothing/null, or the open `ANY` type. `not_null` reflects an
    /// explicit `NOT NULL` suffix directly on this leaf's own grammar
    /// production (e.g. `INTEGER NOT NULL`).
    Predefined {
        kind: PredefinedKind,
        not_null: bool,
    },
    /// `LIST<T>` / `T LIST` / `ARRAY<T>` / `T ARRAY`, with an optional
    /// `NOT NULL` on the list value itself (not on its elements -- element
    /// nullability lives in `element`).
    List {
        element: Box<TypeExpr>,
        not_null: bool,
    },
    /// `[ANY] PROPERTY VALUE [NOT NULL]`: the dynamic union of every
    /// property-storable scalar type and lists thereof.
    PropertyValue { not_null: bool },
    /// A closed dynamic union -- `ANY [VALUE] <A | B | ...>` or the bare
    /// `A | B | ...` form (equivalent). The grammar gives closed unions no
    /// `[NOT NULL]` production of their own, so the parser never attaches
    /// one here; a trailing `NOT NULL` after a union is a syntax error.
    Union(Vec<TypeExpr>),
    /// `VECTOR<TYPE [NOT NULL]>(DIM)` (Cypher style) or `VECTOR(DIM, TYPE
    /// [NOT NULL])` (GQL style) (CIP-216). Either the coordinate type or
    /// the dimension may be grammatically omitted (`VECTOR(3)`,
    /// `VECTOR<INTEGER32>`) -- both parse fine but are rejected at
    /// constraint-creation time, since a property type constraint requires
    /// both. The coordinate type's own `NOT NULL` is mandatory/implicit
    /// (always rendered NOT NULL in canonical form) and so isn't tracked
    /// separately here; `not_null` is the *outer* NOT NULL on the whole
    /// vector type (valid grammatically, but rejected for independent
    /// property type constraints -- CIP-216 doc, "for dependent property
    /// type constraints only").
    Vector {
        coord: Option<VectorCoordKind>,
        dimension: Option<i64>,
        not_null: bool,
    },
}

impl Expr {
    /// A plain (non-`DISTINCT`) function call node, the shape every
    /// parser-lowered special form (`trim`, `normalize`, `vector`, ...) takes.
    pub fn call(name: &str, args: Vec<Expr>) -> Expr {
        Expr::FunctionCall(FunctionCallExpr {
            name: SmallName::from_name(name),
            args,
            distinct: false,
        })
    }

    /// The expression with any wrapping parentheses stripped (the lowering
    /// unwraps them, so every classifier reads through them).
    pub fn unparen(&self) -> &Expr {
        match self {
            Expr::Paren(inner) => inner.unparen(),
            other => other,
        }
    }

    /// The projected variable of a map projection over a variable
    /// (`n {.p}`), paren-transparent at the outer wrapper and the base
    /// alike -- the shape that carries its own variable's name.
    pub fn map_projection_var(&self) -> Option<&SmallName> {
        if let Expr::MapProjection(mp) = self.unparen() {
            if let Expr::Variable(v) = mp.expr.unparen() {
                return Some(v);
            }
        }
        None
    }
}

impl TypeExpr {
    /// The `NOT NULL` flag attached directly to this node's own grammar
    /// production. Used only to check that a closed union's components all
    /// agree on nullability (CIP-86 "Not Null Uses"); closed unions have no
    /// such production of their own, so they report `false`.
    pub fn direct_not_null(&self) -> bool {
        match self {
            TypeExpr::Predefined { not_null, .. } => *not_null,
            TypeExpr::List { not_null, .. } => *not_null,
            TypeExpr::PropertyValue { not_null } => *not_null,
            TypeExpr::Vector { not_null, .. } => *not_null,
            TypeExpr::Union(_) => false,
        }
    }

    /// The same type with `NOT NULL` applied. A closed union has no
    /// `NOT NULL` of its own (nullability is per member), so each member
    /// takes it.
    pub fn with_not_null(self) -> TypeExpr {
        match self {
            TypeExpr::Predefined { kind, .. } => TypeExpr::Predefined {
                kind,
                not_null: true,
            },
            TypeExpr::List { element, .. } => TypeExpr::List {
                element,
                not_null: true,
            },
            TypeExpr::PropertyValue { .. } => TypeExpr::PropertyValue { not_null: true },
            TypeExpr::Union(members) => {
                TypeExpr::Union(members.into_iter().map(TypeExpr::with_not_null).collect())
            }
            TypeExpr::Vector {
                coord, dimension, ..
            } => TypeExpr::Vector {
                coord,
                dimension,
                not_null: true,
            },
        }
    }
}

/// A vector coordinate type (CIP-216): the six numeric types valid inside
/// `VECTOR<...>`/`VECTOR(...)`. `Int64`/`Float64` double as ordinary plain
/// property types (aliases for `INTEGER`/`FLOAT`, see `PredefinedKind`)
/// outside a vector type; the other four (`Float32`, `Int8`/`Int16`/
/// `Int32`) are valid only as a vector coordinate type.
///
/// Defined in `relate-model` (so the runtime `CypherValue::Vector`
/// variant can carry it without a circular crate dependency) and
/// re-exported here under its original name/location.
pub use relate_model::VectorCoordKind;

/// The predefined leaf type names from the `<value type>` grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredefinedKind {
    Nothing,
    Null,
    Any,
    Boolean,
    String,
    Integer,
    Float,
    Date,
    Duration,
    Point,
    LocalTime,
    ZonedTime,
    LocalDateTime,
    ZonedDateTime,
    Node,
    Relationship,
    Path,
    Map,
    /// `UUID [NOT NULL]`.
    Uuid,
}

/// x IS [NOT] [<normal form>] NORMALIZED (CIP-133). `form` is one of
/// "NFC"/"NFD"/"NFKC"/"NFKD", or None when the form is omitted (defaults to
/// NFC at evaluation time).
#[derive(Debug, Clone, PartialEq)]
pub struct IsNormalizedExpr {
    pub expr: Expr,
    pub form: Option<String>,
    pub negated: bool,
}

/// The four chronological comparison operators (Cypher 25):
/// `a IS [NOT] BEFORE | AFTER | SAME INSTANT | SAME REPRESENTATION b`.
/// The four words stay non-reserved identifiers everywhere else
/// (TemporalChronologicalCompare's non-reserved-identifier scenario), so
/// the parser recognizes them contextually after an `IS` introducer, the
/// same policy as `TYPED`/`NORMALIZED`/`LABELED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronologicalOp {
    Before,
    After,
    SameInstant,
    SameRepresentation,
}

/// `a IS [NOT] <chronological operator> b` (Cypher 25). `SameInstant`
/// compares UTC instants, `SameRepresentation` the local clock plus zone
/// identity (the `=` relation), and `Before`/`After` the instant ordering;
/// the executor owns the null/mixed-kind policy.
#[derive(Debug, Clone, PartialEq)]
pub struct ChronologicalCompareExpr {
    pub left: Expr,
    pub op: ChronologicalOp,
    pub right: Expr,
    pub negated: bool,
}

/// STARTS WITH, ENDS WITH, CONTAINS. `op` is the operator's AST spelling, a
/// `&'static str` drawn from the parser's operator-constant table
/// (`parser/mod.rs`, ADR-0505 §2) — never a runtime-built `String`.
#[derive(Debug, Clone, PartialEq)]
pub struct StringMatchExpr {
    pub left: Expr,
    pub op: &'static str,
    pub right: Expr,
}

/// CASE expression. The simple form's test operand is lowered at parse time
/// into each `WHEN`'s equality comparison (`parser/case.rs`), so the stored
/// model is the searched form only: every condition is a predicate.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseExpr {
    pub whens: Vec<WhenClause>,
    pub else_expr: Option<Expr>,
}

/// WHEN ... THEN ... part of a CASE: the condition is always a predicate
/// (the simple form's `operand = value` lowering included).
#[derive(Debug, Clone, PartialEq)]
pub struct WhenClause {
    pub condition: Expr,
    pub result: Expr,
}

/// EXISTS { ... } subquery expression.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistsSubqueryExpr {
    pub statement: Statement,
}

/// COUNT { ... } subquery expression: evaluates to the number of rows the
/// subquery produces against the current row, run independently per row
/// (like `EXISTS { ... }`, just returning a count instead of a boolean).
#[derive(Debug, Clone, PartialEq)]
pub struct CountSubqueryExpr {
    pub statement: Statement,
}

/// `COLLECT { <RegularQuery> }` subquery expression (CIP-71): evaluates the
/// inner `<RegularQuery>` once per outer row and collects the single
/// projected column's value from every inner row into a list. Unlike
/// `EXISTS`/`COUNT`, `COLLECT` has no bare-pattern shorthand -- the inner
/// query is always a full `<RegularQuery>` (any read clause, including
/// UNION) and must end in a `RETURN` with exactly one non-`*` return item.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectSubqueryExpr {
    pub statement: Statement,
}

/// CIP-272 map comprehension: `{k : v IN map [WHERE p] | keyExpr : valueExpr}`
/// (map source) or `{item IN list [WHERE p] | keyExpr : valueExpr}` (list
/// source).
#[derive(Debug, Clone, PartialEq)]
pub struct MapComprehensionExpr {
    /// The key binding variable of a map source (`None` for a list source).
    pub key_var: Option<SmallName>,
    /// The value binding variable (map source) or the item variable (list
    /// source).
    pub value_var: SmallName,
    pub source_expr: Expr,
    pub where_expr: Option<Expr>,
    pub key_expr: Expr,
    pub value_expr: Expr,
}

/// [x IN list WHERE pred | expr]
#[derive(Debug, Clone, PartialEq)]
pub struct ListComprehensionExpr {
    pub variable: SmallName,
    pub list_expr: Expr,
    pub where_expr: Option<Expr>,
    pub map_expr: Option<Expr>,
}

/// [p = (n)-->() | expr] or [(n)-->() | expr]
#[derive(Debug, Clone, PartialEq)]
pub struct PatternComprehensionExpr {
    pub path_variable: Option<SmallName>,
    pub pattern: PatternPath,
    pub where_expr: Option<Expr>,
    pub map_expr: Expr,
}

/// reduce(accumulator = init, x IN list | expression)
#[derive(Debug, Clone, PartialEq)]
pub struct ReduceExpr {
    pub accumulator: SmallName,
    pub init_expr: Expr,
    pub variable: SmallName,
    pub list_expr: Expr,
    pub expression: Expr,
}

/// allReduce(accumulator = init, x IN list | expression, predicate)
#[derive(Debug, Clone, PartialEq)]
pub struct AllReduceExpr {
    pub accumulator: SmallName,
    pub init_expr: Expr,
    pub variable: SmallName,
    pub list_expr: Expr,
    pub expression: Expr,
    pub predicate: Expr,
}

/// shortestPath() / allShortestPaths()
#[derive(Debug, Clone, PartialEq)]
pub struct ShortestPathExpr {
    pub pattern: PatternPath,
    pub all: bool,
}

/// expr[index] or expr[from..to]
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptExpr {
    pub expr: Expr,
    pub index: Option<Expr>,
    pub from: Option<Expr>,
    pub to: Option<Expr>,
    pub slice: bool,
}

/// all/any/none/single(x IN list WHERE pred)
#[derive(Debug, Clone, PartialEq)]
pub struct QuantifierExpr {
    pub quantifier: String,
    pub variable: SmallName,
    pub list: Expr,
    pub predicate: Option<Expr>,
}

/// A label/type predicate: `n:Label`, `n IS Label` (CIP-14), or
/// `n IS [NOT] LABELED <label-expr>` / the `n IS NOT <label-expr>` shorthand
/// (CIP-249).
#[derive(Debug, Clone, PartialEq)]
pub struct HasLabelExpr {
    pub expr: Expr,
    pub label_expr: LabelExpr,
    /// True for `IS NOT LABELED <LE>` and its `IS NOT <LE>` shorthand, which
    /// negate the *whole* expression (unlike an inline `!` inside `label_expr`,
    /// which negates only the following primary).
    pub negated: bool,
    /// True for the `IS [NOT] LABELED <LE>` forms (and the `IS NOT
    /// <LE>` shorthand), which raise a runtime type error for a non-node/
    /// relationship LHS. False for the pre-existing `:`/bare `IS <LE>`
    /// forms (CIP-14), which fall back to `false` for a non-entity LHS,
    /// preserving their original behavior.
    pub strict: bool,
}

/// Pattern used as boolean predicate in WHERE, e.g. (n)-[:REL]->(m)
#[derive(Debug, Clone, PartialEq)]
pub struct PatternPredicateExpr {
    pub pattern: PatternPath,
}

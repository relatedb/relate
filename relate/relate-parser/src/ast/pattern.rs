//! Graph pattern nodes: paths, elements, quantifiers and label expressions.

use super::*;

/// A path pattern like (a)-[r]->(b), optionally named: p = (a)-[r]->(b).
#[derive(Debug, Clone, PartialEq)]
pub struct PatternPath {
    pub variable: Option<SmallName>,
    /// CIP-73: the subpath variable of a named parenthesized group that is
    /// itself the whole path body (`MATCH p = ANY SHORTEST (q = (:A)-->+(:B))`,
    /// `MATCH ANY SHORTEST (q = (:A)-->+(:B))`). [`Self::variable`] carries
    /// only the enclosing `p = …` name; both names bind the same path value
    /// and [`Self::path_names`] lists them. A lone group name (no enclosing
    /// path name) stays here, not in `variable`, so a group reading its own
    /// name inside its `WHERE` stays distinguishable from an enclosing path
    /// variable read there (42I21).
    pub subpath_variable: Option<SmallName>,
    pub elements: Vec<PatternElement>,
    /// The trailing `WHERE` of a parenthesized path pattern, e.g.
    /// `((n)-[r]->(m) WHERE n.p > m.q AND r:S)` (CIP-14 "parenthesized path
    /// pattern" location). Evaluated once every element in the path is
    /// bound, ANDed with the pattern match itself.
    pub where_expr: Option<Expr>,
    /// CIP-51 explicit path mode prefix (`WALK`/`TRAIL`/`ACYCLIC`/`SIMPLE`),
    /// written directly or nested inside a `selector`'s own `[path mode]`
    /// slot. `None` means no explicit keyword (resolves to `Walk`).
    pub mode: Option<PathMode>,
    /// CIP-73/CIP-191 path selector prefix (`ALL`/`ANY`/`SHORTEST`/...).
    /// `None` means no explicit selector (equivalent to an implicit `ALL`).
    pub selector: Option<PathSelector>,
    /// True if this path pattern is sugar for the pre-GPM
    /// `shortestPath(...)`/`allShortestPaths(...)` function (CIP-40/CIP-73),
    /// rather than an explicit `SHORTEST`/`ALL SHORTEST`
    /// selector keyword -- even though both normalize to the same
    /// `selector` value. The two are NOT interchangeable for CIP-40's
    /// mixing rules: the legacy function forms are exempt from the
    /// explicit-match-mode and multiple-selective-path-pattern
    /// restrictions that apply to the keyword selector syntax (see
    /// `Parser::parse_match`).
    pub is_shortest_path_function: bool,
}

impl PatternPath {
    /// The names this path binds its path value under, in declaration order:
    /// the outer path variable, then a named parenthesized subpath. The two
    /// name the same path value; the IR twin is `PathPattern::path_names`.
    pub fn path_names(&self) -> impl Iterator<Item = &SmallName> {
        [self.variable.as_ref(), self.subpath_variable.as_ref()]
            .into_iter()
            .flatten()
    }
}

/// A pattern element is either a node pattern, a relationship pattern, or a
/// quantified path pattern (CIP-28).
#[derive(Debug, Clone, PartialEq)]
pub enum PatternElement {
    Node(NodePattern),
    Relationship(RelPattern),
    Quantified(QuantifiedPathPattern),
}

/// The `{min, max}` bounds of a CIP-28 quantifier (`*`, `+`, `{m,n}`, `{m,}`,
/// `{,n}`, `{,}`, `{n}`). `max = None` means unbounded. Bounds are already
/// normalized and validated by the parser: `{,}` -> `{0,}` and the
/// bounds checks (upper bound > 0, lower bound <= upper bound) are
/// enforced at parse time, so every `PathQuantifier` that reaches the AST
/// is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathQuantifier {
    pub min: u32,
    pub max: Option<u32>,
}

/// A quantified path pattern: a parenthesized subpath (`elements`),
/// optionally assigned a subpath variable (e.g. `(p2 = (x)-->(y))*`) and/or a trailing `WHERE` evaluated once per
/// iteration, followed by a quantifier. Appears as a `PatternElement`
/// alongside plain `Node`/`Relationship` elements so it composes freely with
/// them by concatenation.
///
/// A bare quantified relationship (`-[r]->*`, Form 5) does *not* produce this
/// variant: it normalizes directly into the ordinary variable-length
/// `RelPattern` (`min_hops`/`max_hops`), since it declares no element
/// variables of its own to expose as a group and its matching semantics are
/// identical to the legacy `-[*min..max]->` matcher (only the quantifier's
/// own zero-inclusive defaults differ, e.g. bare `*` means 0 or more here
/// versus 1 or more for legacy bare `*`). See `Parser::parse_rel_pattern`.
#[derive(Debug, Clone, PartialEq)]
pub struct QuantifiedPathPattern {
    pub subpath_variable: Option<SmallName>,
    pub elements: Vec<PatternElement>,
    pub where_expr: Option<Expr>,
    pub quantifier: PathQuantifier,
}

/// A node pattern like (n:Label {prop: value}).
#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub variable: Option<SmallName>,
    pub label_expr: Option<LabelExpr>,
    pub properties: Vec<(SmallName, Expr)>,
    pub has_properties: bool,
    /// The `$paramName` whole-property-map shorthand (an alternative to
    /// `{...}`), e.g. `CREATE (n:Label $props)`. Mutually exclusive with
    /// `properties`/`has_properties` -- the parser only ever populates one
    /// property-map form per pattern element.
    pub properties_param: Option<Expr>,
    /// The element-pattern `WHERE` of `(n WHERE <expr>)` (CIP-14 "node
    /// element pattern" location), evaluated once `variable` is bound.
    pub where_expr: Option<Expr>,
}

/// A relationship pattern like -[r:TYPE {prop: value}]->.
#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    pub variable: Option<SmallName>,
    pub type_expr: Option<LabelExpr>,
    pub properties: Vec<(SmallName, Expr)>,
    /// Whether a property specification was *written*, in either spelling.
    /// The rel twin of [`NodePattern`]'s field: set for `{}` too, so
    /// `-[] {}]-` and `(a {})` agree wherever "has predicates" is asked
    /// (42N56) — live-verified against Neo4j.
    pub has_properties: bool,
    /// The `$paramName` whole-property-map shorthand -- see
    /// `NodePattern::properties_param`.
    pub properties_param: Option<Expr>,
    pub direction: Direction,
    pub min_hops: Option<u32>,
    pub max_hops: Option<u32>,
    /// The element-pattern `WHERE` of `[r WHERE <expr>]`.
    pub where_expr: Option<Expr>,
    /// True when `min_hops`/`max_hops` came from a CIP-28 postfix quantifier
    /// (`-[r]->*`, `-->+`, `-[]->{1,6}`, Form 5) rather than the legacy
    /// bracket-internal `[r*min..max]` syntax. Distinguishes the two so the
    /// engine can apply `where_expr` per traversed edge for the CIP-28 form
    /// without changing the legacy matcher's (TCK-tested) behavior.
    pub quantified: bool,
}

impl RelPattern {
    /// Whether the relationship is variable-length: either hop bound was
    /// written (`*m..n`, or a CIP-28 quantifier lowered onto the bounds).
    /// The one predicate every consumer consults instead of re-spelling
    /// the two `is_some` checks.
    pub fn is_variable_length(&self) -> bool {
        self.min_hops.is_some() || self.max_hops.is_some()
    }
}

/// A GQL label/relationship-type expression (CIP-14): `&` (conjunction),
/// `|` (disjunction), `!` (negation), `%` (wildcard -- matches any non-empty
/// label set, or any relationship since every relationship has exactly one
/// type), parenthesized grouping (folded directly into the tree, so no
/// explicit `Group` variant is needed), and CIP-249's dynamic label name
/// `$(expr)`. The legacy colon-conjunction shorthand `:A:B:C` lowers to
/// `And([Label(A), Label(B), Label(C)])`, and the legacy relationship-type
/// disjunction shorthand `:A|B` lowers to `Or([Label(A), Label(B)])`,
/// preserving pre-CIP-14 behavior exactly.
#[derive(Debug, Clone, PartialEq)]
pub enum LabelExpr {
    /// A single label/type name.
    Label(SmallName),
    /// `%`
    Wildcard,
    /// `$(expr)` / `$all(expr)` / `$any(expr)` -- a label/type name (or, for
    /// list-valued `expr`, a set of label/type names) resolved at runtime
    /// (CIP-249 / CIP-156 / CIP-221).
    Dynamic(DynamicLabelExpr),
    /// `!<label-expr>` -- negates only the following primary.
    Not(Box<LabelExpr>),
    /// `<label-expr> & <label-expr> & ...`
    And(Vec<LabelExpr>),
    /// `<label-expr> | <label-expr> | ...`
    Or(Vec<LabelExpr>),
}

/// A dynamic label/type expression's quantifier: `$(expr)` (`Default`),
/// `$all(expr)` (`All`), or `$any(expr)` (`Any`). `Default` and `All` share
/// conjunction semantics for list-valued `expr` (and are otherwise
/// interchangeable); `Any` is disjunction. Kept distinct from `All` so
/// callers (e.g. `CREATE`/`MERGE` label resolution) can reject the explicit
/// `$any(...)` form while still accepting the implicit-all `$(...)` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicLabelQuantifier {
    Default,
    All,
    Any,
}

/// The payload of `LabelExpr::Dynamic`: the value expression plus its
/// quantifier.
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicLabelExpr {
    pub expr: Box<Expr>,
    pub quantifier: DynamicLabelQuantifier,
}

impl LabelExpr {
    /// Returns the flat list of label names if this expression is a plain
    /// label list: a single label, or an `And` of only plain labels (as
    /// produced by legacy colon-conjunction `:A:B:C`, or a comma-separated
    /// SET/REMOVE label item). Returns `None` for anything using `|`, `!`,
    /// `%`, or a dynamic label -- those aren't valid label lists (CREATE /
    /// MERGE / SET / REMOVE label specifications, per CIP-14's "DML
    /// statements" exclusion and CIP-126).
    pub fn as_label_list(&self) -> Option<Vec<SmallName>> {
        match self {
            LabelExpr::Label(name) => Some(vec![name.clone()]),
            LabelExpr::And(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        LabelExpr::Label(name) => out.push(name.clone()),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// True if this expression contains `!` (negation) or `%` (wildcard)
    /// anywhere, recursively. Used to reject a variable-length quantifier
    /// combined with negation/wildcard (CIP-14).
    pub fn contains_not_or_wildcard(&self) -> bool {
        match self {
            LabelExpr::Label(_) | LabelExpr::Dynamic(_) => false,
            LabelExpr::Wildcard | LabelExpr::Not(_) => true,
            LabelExpr::And(items) | LabelExpr::Or(items) => {
                items.iter().any(LabelExpr::contains_not_or_wildcard)
            }
        }
    }

    /// Returns the flat list of label/type names if this expression is a
    /// plain label or `|`-disjunction list: a single label, or an `Or` of
    /// only plain labels (as produced by `A|B|C`). Returns `None` for
    /// anything using `&`, `!`, `%`, or a dynamic label. Used by the
    /// CREATE INDEX / CREATE CONSTRAINT `FOR` target (CIP-11/CIP-38/
    /// CIP-235's `FOR (n:A|B)` multi-label DDL surface) -- a narrower
    /// grammar than general MATCH label expressions.
    pub fn as_ddl_list(&self) -> Option<Vec<SmallName>> {
        match self {
            LabelExpr::Label(name) => Some(vec![name.clone()]),
            LabelExpr::Or(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        LabelExpr::Label(name) => out.push(name.clone()),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// True if this expression contains a `$(...)`/`$all(...)`/`$any(...)`
    /// dynamic label/type atom anywhere, recursively (CIP-156). Used by
    /// CREATE/MERGE's static "exactly one relationship type" check to defer
    /// to runtime resolution when the count can't be known until the
    /// dynamic expression is evaluated.
    pub fn contains_dynamic(&self) -> bool {
        match self {
            LabelExpr::Label(_) | LabelExpr::Wildcard => false,
            LabelExpr::Dynamic(_) => true,
            LabelExpr::Not(inner) => inner.contains_dynamic(),
            LabelExpr::And(items) | LabelExpr::Or(items) => {
                items.iter().any(LabelExpr::contains_dynamic)
            }
        }
    }
}

/// Direction of a relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Right, // (a)-[]->(b)
    Left,  // (a)<-[]-(b)
    Both,  // (a)-[]-(b)
}

// -- Expression nodes --

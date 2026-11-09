//! The pattern model: [`PathPattern`] and the element vocabulary it is
//! built from ([`NodePattern`], [`RelPattern`], [`QuantifiedPathPattern`],
//! [`PatternElement`]), the match/path modes and selectors, and the label
//! and type expression families ([`LabelExpr`], [`TypeExpr`]). Split from
//! [`super`] so the statement/clause vocabulary and the pattern vocabulary
//! stay one concern per file; [`super`] re-exports every item, so the
//! public path is unchanged (`ast::call` and `ast::expr` set the same
//! precedent).

use relate_names::{NamedToken, SmallName, Token};

use super::Expr;

/// The token of the pattern-map key `name` from a pattern's `prop_keys`;
/// a key the plan did not spell reads by name (`Token::Unresolved`).
pub fn pattern_prop_key(keys: &[NamedToken], name: &str) -> NamedToken {
    keys.iter()
        .find(|(known, _)| *known == name)
        .cloned()
        .unwrap_or_else(|| (SmallName::from_name(name), Token::Unresolved))
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathPattern {
    /// Named path: `MATCH p = (a)-[]->(b)`.
    pub variable: Option<SmallName>,
    /// A second name bound to the same path value when the path's whole body
    /// is a named parenthesized group (`MATCH p = ANY SHORTEST
    /// (q = (a)-[]->(b))`). The executor binds it beside [`Self::variable`].
    pub subpath_variable: Option<SmallName>,
    /// Lossless GPM element sequence. Empty only for legacy callers that
    /// construct the compact `nodes`/`rels` representation directly.
    pub elements: Vec<PatternElement>,
    pub nodes: Vec<NodePattern>,
    pub rels: Vec<RelPattern>,
    pub where_clause: Option<Box<Expr>>,
    pub mode: Option<PathMode>,
    pub selector: Option<PathSelector>,
    pub is_shortest_path_function: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternElement {
    Node(NodePattern),
    Relationship(RelPattern),
    Quantified(QuantifiedPathPattern),
}

impl PathPattern {
    /// The names this pattern binds its path value under, in declaration
    /// order: the outer path variable, then a named parenthesized subpath.
    /// The one enumeration every binder, scope walk and variable collector
    /// consults — the two names name the same path value.
    pub fn path_names(&self) -> impl Iterator<Item = &SmallName> {
        [self.variable.as_ref(), self.subpath_variable.as_ref()]
            .into_iter()
            .flatten()
    }

    /// Every relationship variable the pattern binds, in element order,
    /// recursing into quantified groups. Both views are consulted: the
    /// compact `rels` view retains a var-length relationship's name, while
    /// the GPM `elements` view quantifies it.
    pub fn relationship_variables(&self) -> impl Iterator<Item = &SmallName> {
        let mut names: Vec<&SmallName> = Vec::new();
        collect_element_rel_vars(&self.elements, &mut names);
        for rel in &self.rels {
            if let Some(name) = rel.variable.as_ref() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names.into_iter()
    }

    /// Whether the pattern binds a path value under any of its names (the
    /// outer path variable or a named parenthesized subpath).
    pub fn has_path_name(&self) -> bool {
        self.path_names().next().is_some()
    }

    /// Whether this path must be matched by the GPM engine rather than the
    /// compact node/rel walker: either the compact `nodes`/`rels`
    /// representation was cleared at lowering (non-alternating or quantified
    /// element sequence), or GPM-only features are present (path mode, path
    /// selector, path-level or element-level predicates).
    pub fn needs_gpm(&self) -> bool {
        self.nodes.is_empty()
            || self.mode.is_some()
            || self.selector.is_some()
            || self.where_clause.is_some()
            || self.elements.iter().any(|element| match element {
                PatternElement::Node(node) => node.where_clause.is_some(),
                PatternElement::Relationship(rel) => rel.where_clause.is_some() || rel.quantified,
                PatternElement::Quantified(_) => true,
            })
    }
}

/// Collect the relationship variables of a pattern element list, recursing
/// into quantified groups ([`PathPattern::relationship_variables`]).
fn collect_element_rel_vars<'a>(elements: &'a [PatternElement], out: &mut Vec<&'a SmallName>) {
    for element in elements {
        match element {
            PatternElement::Relationship(rel) => {
                if let Some(name) = rel.variable.as_ref() {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
            PatternElement::Quantified(qpp) => collect_element_rel_vars(&qpp.elements, out),
            PatternElement::Node(_) => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuantifiedPathPattern {
    pub subpath_variable: Option<SmallName>,
    pub elements: Vec<PatternElement>,
    pub where_clause: Option<Expr>,
    pub min: u32,
    pub max: Option<u32>,
}

impl QuantifiedPathPattern {
    /// The inner rel when this element is exactly a quantified var-length
    /// relationship — one plain one-hop rel between two implicit anonymous
    /// boundary nodes, no subpath variable, no group predicate: the form
    /// every `-[r:T*m..n]-` spelling lowers to (LABS-661), with the hop
    /// bounds on the quantifier (`min`/`max`). The rel's `quantified`
    /// marker (the postfix `->{m,n}` spelling's origin, carrying its
    /// per-edge inline-WHERE semantics) does not change the shape. Any
    /// other quantified shape (a parenthesized group with named inner
    /// elements) answers `None`. The one decoder for this shape — the
    /// shortest-route guard, the plan-text renderer, and the group-name
    /// origin rules all consult it.
    pub fn implicit_var_length_rel(&self) -> Option<&RelPattern> {
        if self.subpath_variable.is_some() || self.where_clause.is_some() {
            return None;
        }
        let [PatternElement::Node(head), PatternElement::Relationship(rel), PatternElement::Node(tail)] =
            self.elements.as_slice()
        else {
            return None;
        };
        let implicit = |node: &NodePattern| {
            node.variable.is_none()
                && node.labels.is_empty()
                && node.label_expr.is_none()
                && node.properties.is_none()
                && node.where_clause.is_none()
        };
        if !implicit(head) || !implicit(tail) || rel.var_length {
            return None;
        }
        Some(rel)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    #[default]
    DifferentRelationships,
    RepeatableElements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    Walk,
    Trail,
    Acyclic,
    Simple,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathCount {
    Literal(u32),
    Parameter(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PathSelector {
    All,
    Any(Option<PathCount>),
    AnyShortest,
    AllShortest,
    Shortest {
        count: Option<PathCount>,
        group: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub variable: Option<SmallName>,
    /// Static conjunction, retained for catalog/index candidate selection,
    /// each name beside its token (`Unresolved` until planning resolves the
    /// pattern against a store, ADR-0541).
    pub labels: Vec<NamedToken>,
    /// Complete CIP-14/CIP-156 label expression.
    pub label_expr: Option<LabelExpr>,
    pub properties: Option<Expr>,
    /// The literal keys of `properties`, each beside its token
    /// (`Unresolved` until planning resolves them, ADR-0541). A key the
    /// map does not spell literally has no entry here and reads by name.
    pub prop_keys: Vec<NamedToken>,
    pub where_clause: Option<Expr>,
    /// CIP-79 `USING SCAN <var>:<Label>`: candidate selection must take the
    /// label scan, never an index seek. Unlike the other hints this is not
    /// merely a plan preference — an indexed equality seek and a scan
    /// genuinely answer differently for a zoned-datetime DST overlap (the
    /// index keys instant plus zone, `=` keys the wall clock), which the
    /// measured acceptance suite pins.
    pub force_scan: bool,
}

impl NodePattern {
    /// The implicit anonymous boundary node of a synthesized quantified
    /// group — anonymous and unconstrained, exactly what `(()-[r:T]-()){m,n}`
    /// lowers for its `()`. The one spelling of the shape, at the type's
    /// home: the front's lowering and the executor's NFA compile and GPM
    /// anonymous start both build it.
    pub fn anonymous() -> Self {
        NodePattern {
            variable: None,
            labels: Vec::new(),
            label_expr: None,
            properties: None,
            prop_keys: Vec::new(),
            where_clause: None,
            force_scan: false,
        }
    }

    /// The label names, without their tokens.
    pub fn label_names(&self) -> impl Iterator<Item = &SmallName> {
        self.labels.iter().map(|(name, _)| name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    pub variable: Option<SmallName>,
    /// Static disjunction, retained for typed-CSR candidate selection, each
    /// name beside its token (`Unresolved` until planning resolves the
    /// pattern against a store, ADR-0541).
    pub types: Vec<NamedToken>,
    /// Complete CIP-14/CIP-156 relationship type expression.
    pub type_expr: Option<LabelExpr>,
    pub direction: Direction,
    pub properties: Option<Expr>,
    /// The literal keys of `properties`, each beside its token — see
    /// [`NodePattern::prop_keys`].
    pub prop_keys: Vec<NamedToken>,
    pub var_length: bool,
    pub min_hops: u32,
    pub max_hops: Option<u32>,
    pub where_clause: Option<Expr>,
    pub quantified: bool,
}

impl RelPattern {
    /// The type names, without their tokens.
    pub fn type_names(&self) -> impl Iterator<Item = &SmallName> {
        self.types.iter().map(|(name, _)| name)
    }

    /// Whether `type_expr` spells exactly the static `types` list — a
    /// single label, or an `Or` of the same labels in the same order
    /// (LABS-801). Such an expression is fully applied by any walk that
    /// filters on `types`, so per-edge admission need not re-evaluate it.
    /// Anything else — a negated or conjunctive expression, or a list the
    /// expression does not spell (the CREATE/MERGE rewrite sets `types`
    /// from a resolved type list beside an `And` expression, exactly that
    /// case) — answers `false` and keeps the per-edge evaluation.
    pub fn type_expr_is_static_types(&self) -> bool {
        match self.type_expr.as_ref() {
            Some(LabelExpr::Label(label)) => self.types.len() == 1 && label == &self.types[0].0,
            Some(LabelExpr::Or(items)) => {
                items.len() == self.types.len()
                    && items
                        .iter()
                        .zip(&self.types)
                        .all(|(item, ty)| matches!(item, LabelExpr::Label(label) if *label == ty.0))
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

impl Direction {
    /// The opposite traversal direction (`Both` flips to itself).
    pub fn flip(self) -> Direction {
        match self {
            Direction::Outgoing => Direction::Incoming,
            Direction::Incoming => Direction::Outgoing,
            Direction::Both => Direction::Both,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LabelExpr {
    Label(SmallName),
    Wildcard,
    Dynamic {
        expr: Box<Expr>,
        quantifier: DynamicLabelQuantifier,
    },
    Not(Box<LabelExpr>),
    And(Vec<LabelExpr>),
    Or(Vec<LabelExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicLabelQuantifier {
    Default,
    All,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantifierKind {
    All,
    Any,
    None,
    Single,
}

impl LabelExpr {
    /// Static conjunction: a single label or an `And`-chain of static labels.
    pub fn is_static_conjunction(&self) -> bool {
        match self {
            LabelExpr::Label(_) => true,
            // An empty conjunction is degenerate, not static — and it
            // is constructible: the parser never builds one, but
            // exec/create.rs's post-resolution synthesis can when a
            // dynamic label list resolves empty. Callers (node_ok's
            // label arm, label_index_seekable) must not trust a
            // construction invariant, so `And([])` declines here and
            // routes to the dynamic/scan arms instead (PR #359
            // F-2-6/F-3-2).
            LabelExpr::And(items) => {
                !items.is_empty() && items.iter().all(LabelExpr::is_static_conjunction)
            }
            _ => false,
        }
    }

    /// Static disjunction: a single label or an `Or`-chain of static labels.
    pub fn is_static_disjunction(&self) -> bool {
        match self {
            LabelExpr::Label(_) => true,
            // The degenerate-case twin of the `And` arm above: an empty
            // `Or` is constructible the same way and declines here so
            // no caller trusts a construction invariant (PR #359
            // F-3-1/F-3-3).
            LabelExpr::Or(items) => {
                !items.is_empty() && items.iter().all(LabelExpr::is_static_disjunction)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    Predefined {
        kind: PredefinedType,
        not_null: bool,
    },
    List {
        element: Box<TypeExpr>,
        not_null: bool,
    },
    PropertyValue {
        not_null: bool,
    },
    Union(Vec<TypeExpr>),
    Vector {
        coord: Option<String>,
        dimension: Option<i64>,
        not_null: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredefinedType {
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
    /// CIP-246.
    Uuid,
}

impl PredefinedType {
    /// The type's Cypher keyword — the one type→text map, so a diagnostic
    /// that echoes a type expression (`x IS :: STRING`) spells the keyword
    /// the query wrote instead of a `Debug` rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            PredefinedType::Nothing => "NOTHING",
            PredefinedType::Null => "NULL",
            PredefinedType::Any => "ANY",
            PredefinedType::Boolean => "BOOLEAN",
            PredefinedType::String => "STRING",
            PredefinedType::Integer => "INTEGER",
            PredefinedType::Float => "FLOAT",
            PredefinedType::Date => "DATE",
            PredefinedType::Duration => "DURATION",
            PredefinedType::Point => "POINT",
            PredefinedType::LocalTime => "LOCAL TIME",
            PredefinedType::ZonedTime => "ZONED TIME",
            PredefinedType::LocalDateTime => "LOCAL DATETIME",
            PredefinedType::ZonedDateTime => "ZONED DATETIME",
            PredefinedType::Node => "NODE",
            PredefinedType::Relationship => "RELATIONSHIP",
            PredefinedType::Path => "PATH",
            PredefinedType::Map => "MAP",
            PredefinedType::Uuid => "UUID",
        }
    }
}

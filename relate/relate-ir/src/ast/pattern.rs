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

    /// The pattern's boundary node patterns: the first and last elements,
    /// when both are nodes. The one endpoint read every boundary consumer
    /// (the frontier compile, the shortest-path static common-end rule)
    /// consults; `None` for any other element sequence.
    pub fn boundary_nodes(&self) -> Option<(&NodePattern, &NodePattern)> {
        match (self.elements.first(), self.elements.last()) {
            (Some(PatternElement::Node(start)), Some(PatternElement::Node(end))) => {
                Some((start, end))
            }
            _ => None,
        }
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
    ///
    /// An empty list is ambiguous on its own: it means both "the node
    /// names no label" and "the label expression is not a plain
    /// conjunction" (a disjunction, negation or wildcard keeps its shape
    /// only in `label_expr`). A consumer that must tell the two apart reads
    /// `label_expr`; one that only wants candidate labels reads this.
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

    /// The disjunction arms of the pattern's label expression (LABS-1083)
    /// — the one accessor every consumer of the folded shape reads (the
    /// executor's union serve, the plan narration, the warning's scan
    /// shape), so the derivation is spelled once at the type's home.
    /// `None` for every other shape: no label expression, a plain
    /// conjunction, or a shape the classifier declines.
    pub fn static_disjunction_arms(&self) -> Option<DisjunctionArms<'_>> {
        self.label_expr.as_ref()?.static_disjunction_arms()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// The per-arm borrowed views of a disjunction-bearing label
    /// expression (LABS-1083): the two shapes the planner's disjunction
    /// fold and the parsed `(p:A|B)` produce. `Or([A, B])` answers one
    /// view — empty base, disjuncts `[A, B]` — each disjunct its own arm;
    /// `And([C, Or([A, B])])` (the pattern's own conjunction carried
    /// beside the folded `Or`) answers base `[C]`, disjuncts `[A, B]` —
    /// every arm the shared conjunction plus one disjunct. Every arm is a
    /// static conjunction, and the union of the arms' conjunctions is
    /// equivalent to the expression, so a consumer that serves each arm's
    /// conjunction and unions the answers (deduplicated) serves the
    /// expression. `None` for everything else: no disjunction (a plain
    /// label or conjunction), a nested `Or`, an `And` of several `Or`s (a
    /// DNF no producer builds), an empty `Or`, or any wildcard, dynamic,
    /// or negated leaf. The arm-extraction twin of
    /// [`is_static_conjunction`](Self::is_static_conjunction):
    /// conjunction consumers read a node's retained `labels`, disjunction
    /// consumers read this.
    ///
    /// The views borrow: arm *i* is
    /// [`DisjunctionArms::arms`](DisjunctionArms::arm) — the base's names
    /// plus one disjunct, iterated over the expression's own names —
    /// never a per-disjunct `Vec` clone of the shared conjunction (the
    /// O(n·k) copies an owned per-arm answer would pay for an O(n+k)
    /// expression).
    pub fn static_disjunction_arms(&self) -> Option<DisjunctionArms<'_>> {
        // The shared conjunction every arm carries: empty under a bare
        // `Or`, the `And`'s plain labels beside a folded one. Each arm
        // of this match yields the `Or`'s own item slice directly — the
        // shape is discriminated once here, never re-matched below.
        let (base, items): (Vec<&SmallName>, &[LabelExpr]) = match self {
            LabelExpr::Or(items) => (Vec::new(), items),
            LabelExpr::And(items) => {
                // A plain conjunction carries no `Or` — decline before
                // building the base list, the per-funnel-call allocation
                // the LABS-1083 review flags (the fold's shape always
                // spells a non-conjunction `And`).
                if self.is_static_conjunction() {
                    return None;
                }
                // Two lists built in one pass: the plain labels, and the
                // at-most-one `Or` (the fold appends exactly one; a second
                // is a DNF this classification declines).
                let mut base: Vec<&SmallName> = Vec::new();
                let mut or = None;
                for item in items {
                    match item {
                        LabelExpr::Label(label) => base.push(label),
                        LabelExpr::Or(items) if or.is_none() => or = Some(items.as_slice()),
                        _ => return None,
                    }
                }
                (base, or?)
            }
            _ => return None,
        };
        // The degenerate empty `Or` is constructible (the conjunction
        // classifier's twin decline) and answers no arm.
        if items.is_empty() {
            return None;
        }
        // Every disjunct a plain label — a nested `Or`, wildcard, dynamic,
        // or negated leaf declines the whole classification. The disjuncts
        // collect as borrowed names (never a per-disjunct `Vec` clone of
        // the base); each arm is the base plus one of these, composed per
        // consumer use by [`DisjunctionArms::arms`].
        let disjuncts: Option<Vec<&SmallName>> = items
            .iter()
            .map(|item| match item {
                LabelExpr::Label(label) => Some(label),
                _ => None,
            })
            .collect();
        Some(DisjunctionArms {
            base,
            disjuncts: disjuncts?,
        })
    }
}

/// The borrowed per-arm views of a disjunction-bearing label expression
/// (LABS-1083): the shared conjunction base every arm carries (empty
/// under a bare `Or`) and the disjunct labels — arm *i* is the base plus
/// disjunct *i*. Constructed only by
/// [`LabelExpr::static_disjunction_arms`](LabelExpr::static_disjunction_arms)
/// and [`DisjunctionArms::single`](Self::single) (the single-label view
/// the keyed narration unifies with the disjunction's arms); the arms
/// borrow the expression's own names, so serving them costs no
/// per-disjunct clone of the shared conjunction.
#[derive(Debug, Clone, PartialEq)]
pub struct DisjunctionArms<'a> {
    /// The shared conjunction labels every arm carries.
    base: Vec<&'a SmallName>,
    /// The disjunct labels — one per arm: arm *i* is `base` plus
    /// `disjuncts[i]`.
    disjuncts: Vec<&'a SmallName>,
}

impl<'a> DisjunctionArms<'a> {
    /// The single-label arm: one arm, no shared base — the view shape a
    /// lone static label and a disjunction's arms read through alike, so
    /// a consumer that serves per arm serves both without a second
    /// spelling (LABS-1083's keyed narration unifies them).
    pub fn single(label: &'a SmallName) -> Self {
        DisjunctionArms {
            base: Vec::new(),
            disjuncts: vec![label],
        }
    }

    /// The shared conjunction labels every arm carries — empty under a
    /// bare `Or`, the `And`'s plain labels beside a folded one.
    pub fn base(&self) -> &[&'a SmallName] {
        &self.base
    }

    /// The disjunct labels, statement order — arm *i* is
    /// [`base`](Self::base) plus `disjuncts()[i]`.
    pub fn disjuncts(&self) -> &[&'a SmallName] {
        &self.disjuncts
    }

    /// The arm count — one per disjunct; a capacity hint, never an
    /// iteration protocol (the arms are iterated, not indexed).
    pub fn len(&self) -> usize {
        self.disjuncts.len()
    }

    /// Whether the view answers no arm — no live construction does (the
    /// classifier declines an empty `Or`); held beside [`len`](Self::len)
    /// for the slice-shaped API.
    pub fn is_empty(&self) -> bool {
        self.disjuncts.is_empty()
    }

    /// Every arm's labels, one iterator per arm — each arm is the shared
    /// base plus one disjunct, iterated over the expression's own
    /// borrowed names, never a per-disjunct `Vec`. The natural
    /// iteration protocol: no unchecked arm index for a consumer
    /// pattern whose only shape is "every arm".
    pub fn arms(&self) -> impl Iterator<Item = impl Iterator<Item = &'a SmallName> + '_> + '_ {
        self.disjuncts
            .iter()
            .map(|disjunct| self.base.iter().copied().chain(std::iter::once(*disjunct)))
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

#[cfg(test)]
mod tests {
    //! LABS-1083's arm-classifier pins: the two fold shapes answer their
    //! per-arm views (base plus one disjunct each, borrowed), and every
    //! other shape declines — the same decline vocabulary
    //! `is_static_conjunction` keeps for its shapes.

    use super::{DisjunctionArms, LabelExpr, SmallName};

    fn label(name: &str) -> LabelExpr {
        LabelExpr::Label(SmallName::from_name(name))
    }

    /// One view's arms, each arm's names rendered and collected — the
    /// readable form the assertions compare.
    fn rendered(arms: &DisjunctionArms<'_>) -> Vec<Vec<String>> {
        arms.arms()
            .map(|arm| arm.map(|label| label.to_string()).collect())
            .collect()
    }

    /// The two fold shapes answer their per-arm views: a bare `Or` is one
    /// arm per disjunct over no shared base, and an `And` carrying the
    /// pattern's own conjunction beside the folded `Or` is every arm that
    /// base plus one disjunct — the union of the arms equivalent to the
    /// expression.
    #[test]
    fn the_fold_shapes_answer_their_per_arm_views() {
        let bare = LabelExpr::Or(vec![label("Model"), label("Source")]);
        let arms = bare.static_disjunction_arms().expect("the bare Or folds");
        assert_eq!(
            rendered(&arms),
            vec![vec!["Model".to_string()], vec!["Source".to_string()]]
        );
        assert!(arms.base().is_empty());
        assert_eq!(arms.len(), 2);

        let nested = LabelExpr::And(vec![
            label("C"),
            LabelExpr::Or(vec![label("A"), label("B")]),
        ]);
        let arms = nested.static_disjunction_arms().expect("the And folds");
        assert_eq!(
            rendered(&arms),
            vec![
                vec!["C".to_string(), "A".to_string()],
                vec!["C".to_string(), "B".to_string()]
            ]
        );
        assert_eq!(
            arms.base(),
            vec![&SmallName::from_name("C")],
            "the shared conjunction every arm carries"
        );
    }

    /// Everything else declines: no disjunction (a plain label or
    /// conjunction), a nested `Or`, an `And` of several `Or`s, an empty
    /// `Or`, and any wildcard, dynamic, or negated leaf.
    #[test]
    fn every_other_shape_declines() {
        assert!(label("A").static_disjunction_arms().is_none());
        assert!(LabelExpr::And(vec![label("A"), label("B")])
            .static_disjunction_arms()
            .is_none());
        // A nested Or: the disjunct is not a plain label.
        let nested = LabelExpr::Or(vec![
            label("A"),
            LabelExpr::Or(vec![label("B"), label("C")]),
        ]);
        assert!(nested.static_disjunction_arms().is_none());
        // An And of two Ors: a DNF no producer builds.
        let dnf = LabelExpr::And(vec![
            LabelExpr::Or(vec![label("A"), label("B")]),
            LabelExpr::Or(vec![label("C"), label("D")]),
        ]);
        assert!(dnf.static_disjunction_arms().is_none());
        // An empty Or is constructible and answers no arm.
        assert!(LabelExpr::Or(Vec::new())
            .static_disjunction_arms()
            .is_none());
        // Wildcard and negated leaves decline the whole classification.
        let wild = LabelExpr::Or(vec![label("A"), LabelExpr::Wildcard]);
        assert!(wild.static_disjunction_arms().is_none());
        let negated = LabelExpr::Or(vec![label("A"), LabelExpr::Not(Box::new(label("B")))]);
        assert!(negated.static_disjunction_arms().is_none());
        assert!(LabelExpr::Wildcard.static_disjunction_arms().is_none());
    }

    /// The single-label arm: one arm, no shared base — the same view shape
    /// a lone label and a disjunction's arms read through.
    #[test]
    fn the_single_label_view_answers_one_arm() {
        let name = SmallName::from_name("Model");
        let arms = DisjunctionArms::single(&name);
        assert_eq!(rendered(&arms), vec![vec!["Model".to_string()]]);
        assert!(arms.base().is_empty());
        assert_eq!(arms.disjuncts(), vec![&name]);
        assert!(!arms.is_empty());
    }
}

//! Graph-type DDL clause nodes (`CREATE/ALTER/SHOW ... GRAPH TYPE`) and the graph-type body.

use super::*;

/// See `Clause::AlterCurrentGraphType`.
#[derive(Debug, Clone, PartialEq)]
pub struct AlterCurrentGraphTypeClause {
    pub verb: AlterGraphTypeVerb,
    pub target: AlterCurrentGraphTypeTarget,
}

/// What an `ALTER CURRENT GRAPH TYPE` verb applies: an inline `{ <body> }`,
/// or — ADR-0211's `ALTER CURRENT GRAPH TYPE SET GRAPH TYPE <name>` — a
/// reference to a `CREATE GRAPH TYPE`-declared named type, promoted to
/// current. The invalid combinations (a verb body *and* a named reference)
/// are unrepresentable; no parser placeholder and no consumer-side doc
/// invariant.
#[derive(Debug, Clone, PartialEq)]
pub enum AlterCurrentGraphTypeTarget {
    Body(GraphTypeBody),
    Named(SmallName),
}

/// See `Clause::ShowCurrentGraphType`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShowCurrentGraphTypeClause {
    pub yields: YieldList,
    /// Paging stages following YIELD, preserved in source order.
    pub stages: Vec<PageStage>,
    /// CIP-184: an optional trailing `WHERE <expr>`, permitted with or
    /// without an explicit `YIELD` (mirrors `ShowDatabasesClause`).
    pub where_expr: Option<Expr>,
    /// `AS GRAPH [SINGLE | SEPARATE]` -- renders the current graph
    /// type as `nodes`/`relationships` columns instead of the plain
    /// `specification` string column. `None` when `AS GRAPH` wasn't
    /// written at all.
    pub as_graph: Option<AnyNodeMode>,
}

/// CIP-257's "Future improvements" any-node keyword, controlling how many
/// synthetic `(:AnyLabel {})` nodes an unconstrained relationship endpoint
/// gets: `Separate` (the default, whether or not the keyword is written) --
/// one any-node per relationship that needs one; `Single` -- every
/// unconstrained endpoint across the whole graph type shares one any-node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnyNodeMode {
    #[default]
    Separate,
    Single,
}

/// The four `ALTER CURRENT GRAPH TYPE` verbs (CIP-176/CIP-182 overview
/// scope): `SET` replaces the whole current graph type (and, as observed
/// live, every constraint in the database); `ADD`
/// extends it; `ALTER` replaces an existing element type (matched by
/// identifying label) with a new declaration; `DROP` removes the element
/// types named in its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlterGraphTypeVerb {
    Set,
    Add,
    Alter,
    Drop,
}

/// See `Clause::CreateGraphType`.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateGraphTypeClause {
    pub name: SmallName,
    pub body: GraphTypeBody,
    /// Whether the `AS` spelling was used (the scaffolding form,
    /// `CREATE GRAPH TYPE <name> AS { <body> }`, vs. CIP-178's plain
    /// `CREATE GRAPH TYPE <name> { <body> }`). The worked
    /// example (`()-[:REL =>]->()`, no properties, both endpoints the any
    /// type) is an ineffectual single-label edge
    /// type -- the `AS` form exists to exercise the CIP-180 canonicalizer,
    /// so the engine's `CreateGraphType` handler exempts it from the
    /// ineffectual-type rejection and
    /// the one-index-backing-per-subject clash (both shapes the canonicalizer's own
    /// scenarios use), and validates every other body rule normally.
    pub via_as: bool,
}

/// See `Clause::ShowGraphType`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowGraphTypeClause {
    pub name: SmallName,
    pub yields: YieldList,
}

/// A fully parsed `<graph type specification body>` (CIP-178): the `{
/// ... }` contents shared by `CreateGraphTypeClause` and
/// `AlterCurrentGraphTypeClause`. Parsing is grammar-only (always succeeds
/// once the tokens match); the CIP-178 "shall" rules (alias/label
/// uniqueness, reference resolution, ...) are enforced separately by
/// `relate-executor`'s graph-type validator (`validate_graph_type_body`),
/// which only `CREATE GRAPH
/// TYPE` invokes -- `ALTER CURRENT GRAPH TYPE` bodies legitimately reference
/// element types declared by an *earlier* statement (e.g. `ADD`), which a
/// single-body validation pass cannot see.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GraphTypeBody {
    pub node_types: Vec<GraphTypeNodeTypeSpec>,
    pub edge_types: Vec<GraphTypeEdgeTypeSpec>,
    pub constraints: Vec<GraphTypeConstraintSpec>,
    /// `<constraint drop specification>` = `CONSTRAINT <name>` (bare,
    /// no `FOR ...`) -- only allowed in an `ALTER CURRENT GRAPH TYPE DROP {
    /// ... }` body. The parser reads it in any body, and every other verb
    /// rejects a non-empty list as a parse error
    /// (`Parser::reject_bare_constraint_drops`), so this is always empty for
    /// `CREATE GRAPH TYPE`/`SET`/`ADD`/`ALTER` bodies.
    pub drop_constraint_names: Vec<SmallName>,
}

/// `'(' [alias] (':'|IS) <identifying label> ('=>'|IMPLIES) [(':'|IS)
/// <implied label> ('&' <implied label>)*] ['{' <property types> '}'] ')'`
/// (`<node type specification>`).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTypeNodeTypeSpec {
    pub alias: Option<SmallName>,
    pub identifying_label: SmallName,
    pub implied_labels: Vec<SmallName>,
    pub properties: Vec<GraphTypePropertyType>,
}

/// `<source endpoint> '-' '[' [alias] (':'|IS) <identifying label>
/// ('=>'|IMPLIES) ['{' <property types> '}'] ']' '->' <destination
/// endpoint>` (`<edge type specification>`).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTypeEdgeTypeSpec {
    pub alias: Option<SmallName>,
    pub identifying_label: SmallName,
    pub properties: Vec<GraphTypePropertyType>,
    pub source: GraphTypeElementRef,
    pub target: GraphTypeElementRef,
}

/// One `<property type>`: a name, its declared value type, an optional
/// `DEFAULT ON COMMIT <value expression>` (CIP-262), and an optional
/// inline `IS [NODE|REL|RELATIONSHIP] (KEY|UNIQUE)` constraint (CIP-178
/// Rules 10-12 rewrite these into standalone `GraphTypeConstraintSpec`s
/// during registration).
///
/// The default value is parsed as a full expression here (grammar-only,
/// like the rest of the body): the CIP-262 shape restriction -- a literal
/// or an allowed type constructor, never `null` -- is enforced by the
/// graph-type semantic layer, and the construct itself sits behind the
/// ADR-0371 `graph-type-default-values` preview gate at statement
/// admission (the trio's decision: ADR-0388). `None` is the no-default
/// case.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTypePropertyType {
    pub name: SmallName,
    pub value_type: TypeExpr,
    pub default_value: Option<Expr>,
    pub inline_constraint: Option<GraphTypeInlineConstraint>,
}

/// An inline property-type constraint (`{prop :: TYPE IS [qualifier]
/// KEY|UNIQUE}`). `qualifier` records which (if any) `NODE`/`REL`/
/// `RELATIONSHIP` keyword was written, so the graph-type validator can
/// enforce that a node type must not contain `REL`/
/// `RELATIONSHIP`; an edge type must not contain `NODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphTypeInlineConstraint {
    pub verb: GraphTypeInlineVerb,
    pub qualifier: Option<GraphTypeEntityQualifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphTypeInlineVerb {
    Key,
    Unique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphTypeEntityQualifier {
    Node,
    Rel,
}

/// A `<node type reference>` / `<edge type reference>` (CIP-178): how a
/// node/edge type spec's endpoint, or a constraint specification's `FOR`
/// target, names the element type it applies to.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphTypeElementRef {
    /// `()` -- the any node/edge type.
    Any,
    /// `(alias)` / `()-[alias]->()` -- resolves to a previously declared
    /// node/edge type alias (it must already be declared).
    AliasRef(SmallName),
    /// `[alias] (':'|IS) <label> ['=>'|IMPLIES]` -- an in-situ reference.
    /// `implies` (`=>`/`IMPLIES` present) requires `label` to name an
    /// already-declared identifying label; without it, the
    /// label need not be declared. `alias` is only legal in constraint
    /// `FOR`-clause context (forbidden on an edge type's own
    /// endpoint references).
    InSitu {
        alias: Option<SmallName>,
        label: SmallName,
        implies: bool,
    },
}

/// `[CONSTRAINT [<name>]] FOR <node/edge type reference> REQUIRE
/// <property name spec> <constraint verb> [OPTIONS <map>]` (CIP-178
/// `<constraint specification>`) -- also the target of a node/edge type
/// spec's trailing inline `REQUIRE ...` list and a property's own inline
/// `IS KEY`/`IS UNIQUE` (both rewritten to this same shape at parse time,
/// per Rules 10-12).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphTypeConstraintSpec {
    pub name: Option<SmallName>,
    pub target: GraphTypeElementRef,
    /// True when `target` is an edge type reference (`FOR ()-[...]->()`);
    /// false for a node type reference (`FOR (...)`).
    pub is_edge: bool,
    pub properties: Vec<SmallName>,
    pub kind: GraphTypeConstraintVerb,
    /// The parsed `<value type>` for a `PropertyType`-kind constraint,
    /// `None` for every other kind.
    pub property_type: Option<TypeExpr>,
    /// The `OPTIONS` tail: absent, a literal map, or a whole-map parameter
    /// (resolved before the body is captured/persisted).
    pub options: OptionsForm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphTypeConstraintVerb {
    Key,
    Unique,
    Exists,
    PropertyType,
}

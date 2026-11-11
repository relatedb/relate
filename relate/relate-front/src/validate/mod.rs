//! Semantic validation passes for Cypher statements, run by
//! [`crate::lower::lower_statement`] before lowering.
//!
//! The family is split by concern:
//!
//! - [`show`]: `SHOW …`/`TERMINATE …` column and composability rules
//!   (CIP-42/46/84/123/157/187/242/253).
//! - [`scope`]: cross-clause variable binding and usage-type tracking
//!   (`VariableAlreadyBound` / `VariableTypeConflict`, keyed on the *reuse*
//!   usage, matching the openCypher TCK).
//! - [`search`]: the CIP-224 SEARCH-clause rules that don't fit a generic
//!   scope tracker (binding-variable reference, SCORE conflicts, pattern
//!   shape, per-index CIP-225/240 kind restrictions).
//! - [`writes`]: CREATE/MERGE pattern legality (single relationship type,
//!   direction, no variable length, no QPPs, parameter property maps,
//!   CIP-156 dynamic label quantifier rules).
//! - [`self_reference`]: the 42I58 same-clause created-entity reference rule
//!   (Cypher 25), both the property-value half and the dynamic-label half,
//!   in its exact-scope and conservative tiers (ADR-0513).
//! - [`dynamic`]: compile-time rejection of constant dynamic property-key
//!   and label/type operands (LABS-709), riding the whole-tree walk's
//!   operand visit.
//! - [`expressions`]: expression-position rules (pattern predicates in
//!   WHERE, patterns in projections/SET values, property access on paths,
//!   `labels()`/`type()` entity argument rules, DELETE targets).
//! - [`aggregation`]: aggregation semantics (nesting, non-constant
//!   arguments, grouping-key ambiguity, ORDER BY aggregation rules,
//!   aggregation in list comprehensions).
//! - [`functions`]: the canonical known-function registry
//!   (`KNOWN_FUNCTIONS`, `is_known_function`) the planner binds against, and
//!   the ADR-0497 preview-name admission gate
//!   (`validate_function_admission`).
//! - [`static_types`]: the literal-type/constant vocabulary and the operator
//!   rules decidable from *literal* operand types (boolean-operator
//!   operands, `IN` lists, `properties()`, quantifier/arithmetic
//!   mismatches).
//! - [`page_bounds`]: the `LIMIT`/`SKIP`/`OFFSET` compile-time rules, in the
//!   constant-ness and value-domain tiers that `RETURN`/`WITH` own and the
//!   CIP-168 standalone page statement partly shares (its module doc states
//!   the split).
//! - [`static_calls`]: the builtin calls whose literal argument types or
//!   value domains the statement text decides (`vector*`, `char_length`,
//!   `nullif`/`format` arity, `replace` limit, temporal constructors,
//!   quantifier/`allReduce` operands, path-selector counts, `IN TRANSACTIONS`
//!   concurrency).
//! - [`dialect`]: `CYPHER <version>` preamble gates (CIP-150): the
//!   Cypher-25-only surface rejected under `CYPHER 5`.
//! - [`structure`]: clause-composition rules (USE placement, NEXT chains,
//!   UNION column shapes, duplicate column names, relationship
//!   uniqueness).
//! - [`match_shape`]: `MATCH` graph-pattern shape rules (a path variable in
//!   a quantified path pattern, a group `WHERE` reading an out-of-scope path
//!   variable, strict-interior variable reuse under `REPEATABLE ELEMENTS`);
//!   the parser owns the shape rejections that need pre-AST information
//!   (42N42, the path/sub-path name clash).
//! - [`delete`]: the DELETE-target admission policy (`check_delete_expr`,
//!   the one stated rule the executor's runtime fold cites); the
//!   scope-coupled clause dispatch stays in [`expressions`].
//!
//! This module's root holds the ADR-0257 nesting-depth guard
//! ([`validate_nesting_depth`]), the nested-statement validation battery
//! ([`validate_statement_tree`]), the CIP-150 preview-gate predicates, and
//! the canonical error constructor every pass shares
//! ([`EngineError::duplicate_variable`]; the outer-scope collision constructor
//! [`crate::call_scope::duplicate_outer_variable`] lives at the crate root
//! beside its consumers). The generic AST
//! helpers those passes build on — the collectors, the per-clause
//! scope-introduction fold ([`crate::ast::collect::clause_introduced_names`]),
//! the UNION-chain enumeration and the walker family — live in
//! [`crate::ast`], which this module consumes downward.

use relate_core::PreviewFeatures;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, SearchSpec, Statement};

mod aggregation;
mod delete;
mod dialect;
mod dynamic;
mod expressions;
mod functions;
mod graph_type_gate;
mod index_hint;
mod match_shape;
mod page_bounds;
mod safe_mode;
mod scope;
mod search;
mod self_reference;
mod show;
mod static_calls;
mod static_types;
mod structure;
mod writes;

pub(crate) use self::aggregation::{alias_shadows_its_own_base, is_aliased};
pub use self::aggregation::{
    validate_aggregation_semantics, validate_clause_slots, validate_expression_slots,
    validate_order_by_aggregation,
};
pub use self::dialect::validate_cypher5_restrictions;
pub use self::expressions::validate_expression_constraints;
pub(crate) use self::functions::validate_function_admission;
pub use self::functions::{
    every_name_gate, function_passes_name_gate, gated_function_names, is_known_function,
    KNOWN_FUNCTIONS,
};
pub use self::index_hint::validate_index_hints;
pub(crate) use self::match_shape::validate_expr_pattern_shape;
pub use self::match_shape::validate_match_shape;
pub use self::page_bounds::validate_limit_skip;
pub(crate) use self::safe_mode::validate_statement_shape;
pub use self::scope::validate_variable_types;
pub(crate) use self::search::search_pattern_is_complex;
pub use self::search::validate_search_clauses;
pub(crate) use self::self_reference::validate_create_self_reference_conservative;
pub use self::show::{
    validate_show_column_names, validate_show_composability, validate_show_output_uniqueness,
    validate_show_requires_yield_before_return, validate_show_system_graph_reads,
};
pub use self::static_calls::validate_static_call_types;
pub use self::static_types::validate_static_operator_types;
pub use self::structure::{
    validate_column_name_conflicts, validate_count_subquery_union_shapes,
    validate_expr_traversal_bounds, validate_foreach_body, validate_next_no_admin_commands,
    validate_relationship_uniqueness, validate_shortest_path_mixing, validate_traversal_bounds,
    validate_union_columns, validate_use_placement,
};
pub(crate) use self::writes::validate_param_property_shorthand;

/// The store's resolved preview set, carried into validation so the gates
/// that do not parse syntax can read it: today the `complex-search-patterns`
/// opt-in, which lifts CIP-225 Rule 0's SEARCH pattern-shape restriction,
/// and the ADR-0497 function-name admission gate. The default is every gate
/// closed -- the dialect floor. One struct so a nested or speculative
/// lowering inherits its caller's dialect by plain field copy; a gate that
/// only *detects* a construct (`chronological-comparisons`,
/// `float16-coordinate-types`, …) never lands here: its floor form is
/// unchanged, so it fails closed in the front's detector loop instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidateOptions {
    /// The store's `RELATE_PREVIEW` set (ADR-0371). Every validation
    /// gate derives its own on/off state from the one set, so a gate cannot
    /// drift from the store's opt-in.
    pub preview: PreviewFeatures,
}

/// The admission policy a statement is parsed and lowered under (LABS-680,
/// ADR-0477): the traversal-depth limit (LABS-610) plus the opt-in safe
/// mode's shape rules. One `Copy` carrier, so adding a policy knob does not
/// grow the lowering entry-point signature again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementPolicy {
    /// Refuse the agent-typical dangerous shapes safe mode bans.
    pub safe_mode: bool,
    /// The largest explicit traversal hop bound the statement may ask for.
    pub max_traversal_depth: u32,
}

impl StatementPolicy {
    /// The safe-off policy: no safe mode, with the given traversal limit.
    pub fn unsafened(max_traversal_depth: u32) -> Self {
        Self {
            safe_mode: false,
            max_traversal_depth,
        }
    }
}

impl Default for StatementPolicy {
    fn default() -> Self {
        Self::unsafened(relate_core::MAX_TRAVERSAL_DEPTH)
    }
}

/// The CIP graph-type trio's ADR-0371 preview-gate predicates (CIP-283
/// optional properties, CIP-262 default values, CIP-266 implied labels
/// identifying node element types): the AST-level half of the gates,
/// consulted by `parse_and_lower_statement_with_preview`. The
/// stored-state half (an `ADD` whose new identifying label a stored node
/// element type already implies) fails closed in the executor's
/// graph-type layer with the same authored error.
pub(crate) use graph_type_gate::statement_graph_type_gates;

/// Whether any clause of `stmt` — at any nesting depth, FOREACH bodies
/// included — satisfies `pred`. The one statement-wide existence walk the
/// preview-gate predicates ride, so each is a predicate rather than another
/// copy of the visited-flag scaffold.
pub(crate) fn statement_any_clause(
    stmt: &Statement,
    pred: impl Fn(&Clause) -> bool,
) -> Result<bool, EngineError> {
    statement_any_clause_node(stmt, |_, clause| pred(clause))
}

/// [`statement_any_clause`] with the statement node the clause belongs to,
/// for a predicate that must tell a FOREACH body ([`StmtRef::Body`]) apart
/// from an ordinary statement node.
pub(crate) fn statement_any_clause_node(
    stmt: &Statement,
    pred: impl Fn(StmtRef<'_>, &Clause) -> bool,
) -> Result<bool, EngineError> {
    statement_any_hit(
        stmt,
        |node| node.clauses().iter().any(|clause| pred(node, clause)),
        |_| false,
    )
}

/// Whether any expression of `stmt` — at any nesting depth — satisfies
/// `pred`. [`statement_any_clause`]'s expression half.
pub(crate) fn statement_any_expr(
    stmt: &Statement,
    pred: impl Fn(&Expr) -> bool,
) -> Result<bool, EngineError> {
    statement_any_hit(stmt, |_| false, pred)
}

/// The one early-exit existence scaffold the two halves above ride: walks
/// the statement tree and reports whether either probe fired — the
/// visited-flag mechanics (first hit wins; nothing walks past it) spelled
/// once instead of copied per half.
fn statement_any_hit(
    stmt: &Statement,
    on_statement: impl Fn(StmtRef<'_>) -> bool,
    on_expr: impl Fn(&Expr) -> bool,
) -> Result<bool, EngineError> {
    let found = std::cell::Cell::new(false);
    walk_statement_tree(
        stmt,
        &mut |node| {
            if !found.get() {
                found.set(on_statement(node));
            }
            Ok(())
        },
        &mut |_| Ok(()),
        &mut |expr| {
            if !found.get() {
                found.set(on_expr(expr));
            }
            Ok(())
        },
    )?;
    Ok(found.get())
}

/// Whether the statement's AST names a CIP-278 `COMBINE` source anywhere
/// (a MATCH search clause, at any subquery depth) -- the predicate the
/// ADR-0371 `hybrid-search` preview gate checks before lowering. A walk
/// error propagates: the gate never answers "no" over a tree it could not
/// walk.
pub(crate) fn statement_uses_hybrid_search(stmt: &Statement) -> Result<bool, EngineError> {
    statement_any_clause(stmt, |clause| {
        matches!(
            clause,
            Clause::Match(match_clause)
                if match_clause
                    .search
                    .as_ref()
                    .is_some_and(|search| matches!(search.spec, SearchSpec::Combine(_)))
        )
    })
}

/// Whether the statement's AST uses one of the four Cypher 25 chronological
/// comparison operators anywhere (at any subquery depth) -- the predicate
/// the ADR-0371 `chronological-comparisons` preview gate checks before
/// anything of the statement validates or lowers. The operators always
/// parse into the `Expr::ChronologicalCompare` node (the parser recognizes
/// them contextually after `IS`, so no wider grammar changes), and the
/// gate decides only whether a statement carrying one may run. A walk error
/// propagates: the gate never answers "no" over a tree it could not walk.
pub(crate) fn statement_uses_chronological_comparisons(
    stmt: &Statement,
) -> Result<bool, EngineError> {
    statement_any_expr(stmt, |expr| matches!(expr, Expr::ChronologicalCompare(_)))
}

/// Whether the statement's AST uses one of the FLOAT16/BFLOAT16 vector
/// coordinate types (the ADR-0371 `float16-coordinate-types` gate of
/// ADR-0464, mirroring Neo4j's own toggle). Two
/// syntactic homes, both read here:
///
/// - a `VECTOR<…>` type expression — a type predicate (`x IS :: VECTOR<FLOAT16>(3)`),
///   a property-type constraint, or a graph-type element property type;
/// - the `vector(<list>, <dim>, <type>)` constructor, whose third argument
///   `parse_vector_call` normalizes into a coordinate-type name string.
///
/// The `VectorCoordKind` vocabulary is not re-spelled here: both arms test
/// against the model's one coordinate-type table.
pub(crate) fn statement_uses_float16_coordinate_types(
    stmt: &Statement,
) -> Result<bool, EngineError> {
    // The two syntactic homes are separate existence walks over the one
    // scaffold; either hit arms the gate.
    if statement_any_clause(stmt, clause_uses_float16_property_type)? {
        return Ok(true);
    }
    statement_any_expr(stmt, |expr| match expr {
        Expr::IsType(is_type) => type_uses_float16(&is_type.type_expr),
        Expr::FunctionCall(call) => is_float16_vector_constructor(call),
        _ => false,
    })
}

/// Whether a type expression names FLOAT16/BFLOAT16 as a vector coordinate
/// type, at any nesting depth (`VECTOR<LIST<FLOAT16>>` included).
fn type_uses_float16(type_expr: &relate_parser::TypeExpr) -> bool {
    use relate_parser::TypeExpr;
    match type_expr {
        TypeExpr::Vector { coord, .. } => matches!(
            coord,
            Some(
                relate_parser::VectorCoordKind::Float16 | relate_parser::VectorCoordKind::BFloat16
            )
        ),
        TypeExpr::List { element, .. } => type_uses_float16(element),
        TypeExpr::Union(members) => members.iter().any(type_uses_float16),
        TypeExpr::Predefined { .. } | TypeExpr::PropertyValue { .. } => false,
    }
}

/// Whether the call is `vector(<list>, <dim>, FLOAT16|BFLOAT16)` — the
/// parser stores the coordinate-type keyword as its own argument, a
/// coordinate-name string resolved through the model's table.
fn is_float16_vector_constructor(call: &relate_parser::FunctionCallExpr) -> bool {
    if !call.name.with_str(|n| n.eq_ignore_ascii_case("vector")) || call.args.len() != 3 {
        return false;
    }
    matches!(
        &call.args[2],
        Expr::Literal(relate_parser::LiteralValue::String(name))
            if matches!(
                relate_parser::VectorCoordKind::from_alias(name),
                Some(
                    relate_parser::VectorCoordKind::Float16
                        | relate_parser::VectorCoordKind::BFloat16
                )
            )
    )
}

/// Whether a clause's own `<value type>` declarations name FLOAT16/BFLOAT16:
/// a CIP-85 property-type constraint, or a graph type's element property
/// types.
fn clause_uses_float16_property_type(clause: &Clause) -> bool {
    match clause {
        Clause::CreateConstraint(constraint) => constraint
            .property_type
            .as_ref()
            .is_some_and(type_uses_float16),
        Clause::CreateGraphType(graph_type) => graph_type_body_uses_float16(&graph_type.body),
        Clause::AlterCurrentGraphType(alter) => match &alter.target {
            relate_parser::AlterCurrentGraphTypeTarget::Body(body) => {
                graph_type_body_uses_float16(body)
            }
            relate_parser::AlterCurrentGraphTypeTarget::Named(_) => false,
        },
        _ => false,
    }
}

/// Whether any element property type of a graph type body names
/// FLOAT16/BFLOAT16.
fn graph_type_body_uses_float16(body: &relate_parser::GraphTypeBody) -> bool {
    body.node_types
        .iter()
        .flat_map(|node| node.properties.iter())
        .chain(
            body.edge_types
                .iter()
                .flat_map(|edge| edge.properties.iter()),
        )
        .any(|property| type_uses_float16(&property.value_type))
}

// ---- ADR-0257 nesting-depth guard and whole-tree validation ----

use crate::ast::stmt_ref::StmtRef;
use crate::ast::tree::{walk_statement_tree, walk_statement_tree_with_operands};
/// ADR-0257: maximum statement/structural nesting depth accepted at the
/// lowering boundary. Every nested statement counts one level: `CALL { }`
/// bodies, CIP-135 `WHEN`/`ELSE` bodies, FOREACH bodies,
/// `EXISTS`/`COUNT`/`COLLECT` subquery bodies, and each link of a UNION or
/// NEXT chain. Expression nesting counts one level per expression node
/// along a walk (including per-hop postfix forms: `.key` property links,
/// subscripts/slices, map projections), and the expression count is
/// *cumulative* across expression-embedded subquery statement boundaries
/// (see `crate::ast::tree::walk_statement_tree_at`), so composed nesting — statement
/// depth combined with per-statement expression chains — cannot multiply
/// past what one native stack can lower. The value is single-sourced from
/// the parser (`relate_parser::MAX_NESTING_DEPTH`), which enforces the
/// same cap for the expression nesting it counts but builds postfix chains
/// iteratively, so this guard is the only accounting those hops ever get;
/// it also covers the structural recursion the parser lets through (and
/// acts as a second, independent enforcement point), so an over-nested
/// query is a typed error instead of a stack-overflow abort in the
/// recursive lowering/validation walkers (LABS-70).
pub use crate::ast::MAX_NESTING_DEPTH;

/// Reject a statement tree whose structural nesting exceeds
/// [`MAX_NESTING_DEPTH`]. The check is structural in the walker core: the
/// statement cap fires in `crate::ast::tree::walk_statement_tree_at` (before the
/// statement visitor, so the walk early-exits at the first over-deep
/// statement, bounding its own recursion at the limit + 1) and the
/// expression cap in `crate::ast::tree::walk_expr_at`, with the cumulative entry-depth
/// semantics documented on `crate::ast::tree::walk_statement_tree_at` — so a plain
/// whole-tree walk with no-op visitors *is* the depth guard.
/// [`validate_statement_tree`] runs this pass first, before its
/// per-statement battery; this standalone entry point remains for callers
/// that want the depth guard only.
pub fn validate_nesting_depth(stmt: &Statement) -> Result<(), EngineError> {
    walk_statement_tree(stmt, &mut |_| Ok(()), &mut |_| Ok(()), &mut |_| Ok(()))
}

/// LABS-709: the clause-ordered dynamic-operand rule over bindings and
/// parameters ([`dynamic::validate_scoped_operands`]). The operand's own
/// text is already checked by the whole-tree walk inside
/// [`validate_statement_tree`]; this pass adds the operands whose value is
/// statically known only from the surrounding clause scope or from a bound
/// parameter — both constants at admission, which is why Neo4j rejects a bad
/// one even on a zero-row `MATCH`. Scope-sensitive, so it runs per top-level
/// statement body; nested `CALL { }` / `FOREACH` bodies are reached by its
/// own walk.
pub(crate) fn validate_scoped_dynamic_operands(
    stmt: &Statement,
    params: &std::collections::BTreeMap<String, relate_core::Value>,
) -> Result<(), EngineError> {
    dynamic::validate_scoped_operands(stmt, params)
}

/// Run the ADR-0257 depth guard and then every scope-free semantic
/// validator on `stmt` *and* on every nested statement it contains (UNION
/// arms, NEXT stages, `CALL { }` / `WHEN` / `ELSE` bodies, FOREACH bodies,
/// and `EXISTS`/`COUNT`/`COLLECT` subquery bodies), so an over-nested query
/// is a typed error before lowering recurses and a rule violation cannot
/// hide inside nesting (LABS-70/LABS-81: `CALL { RETURN 1 LIMIT -1 }` used
/// to lower while its top-level equivalent was rejected).
///
/// Two cheap passes over the borrowed tree. Pass 1 is
/// [`validate_nesting_depth`] over the whole tree (cumulative entry-depth
/// semantics, early exit at the limit + 1). Pass 2 is the battery walk. The
/// split exists because the battery's statement visitor fires *before* its
/// own walk descends into that statement's expressions, and some battery
/// validators still walk expressions by hand — the grouping-key-aware
/// ORDER BY aggregation check (`aggregation::order_by::check_ob_agg_sub_exprs` /
/// `validate_ob_non_agg_part`), the list-comprehension aggregation check
/// (`aggregation::comprehension::check_list_comp_aggregation`), and the focused
/// static-types helpers (`page_bounds::expr_references_variables`,
/// `predicate_applies_arithmetic_to_var`, `expr_is_bare_var`,
/// `list_literal_all_non_numeric`) — with a descent keyed on grouping keys
/// and comprehension boundaries rather than plain child structure, so they
/// cannot ride the self-capping walker. Running the depth pass first means
/// each of them only ever runs on an expression tree already known to be
/// within [`MAX_NESTING_DEPTH`], and the guard's early exit means those
/// hand-recursion frames stack on zero live walk frames — the same
/// native-frame bound the historical depth-guard-first ordering gave.
///
/// The validator set applied per statement is deliberately limited to
/// validators whose checks are decided by the statement's own text:
/// clause-local rules (LIMIT/SKIP shape, static literal operator types,
/// duplicate column names, ORDER BY/list-comprehension aggregation,
/// relationship uniqueness, `$param` property-map shorthand placement),
/// per-body composition rules (USE placement, NEXT-chain admin commands,
/// UNION column shapes, SEARCH-clause rules, SHOW output uniqueness),
/// plus the COUNT-subquery UNION shape rules
/// ([`validate_count_subquery_union_shapes`]), which ride the walk's
/// *expression* visitor — the one place the battery can see which
/// subquery kind a body belongs to (`StmtRef` cannot tell a COUNT body
/// from an EXISTS one, and only COUNT carries the rules; CIP-63 leaves
/// EXISTS unrestricted). The
/// SEARCH inner-WHERE shape check runs at the lowering boundary instead
/// (`crate::search_filter`, the one grammar walk). Scope-tracking
/// validators (`validate_variable_types`, `validate_expression_constraints`,
/// `validate_aggregation_semantics`, `validate_show_column_names`, the SHOW
/// composability pair) need the enclosing body sequence — and, for a nested
/// body, the correlated outer bindings they cannot see — so they stay
/// top-level-only (FOREACH bodies, which share the enclosing scope, are
/// reached by `validate_variable_types`'s own recursion instead). The
/// CIP-150 dialect gates walk the whole tree themselves with version
/// inheritance.
pub fn validate_statement_tree(stmt: &Statement) -> Result<(), EngineError> {
    validate_statement_tree_with_options_and_policy(
        stmt,
        ValidateOptions::default(),
        StatementPolicy::default(),
    )
}

/// [`validate_statement_tree`] with the store's resolved traversal-depth
/// admission limit (LABS-610): every explicit hop bound in the statement
/// tree, at any nesting depth, is checked against `max_traversal_depth`.
pub fn validate_statement_tree_with_limits(
    stmt: &Statement,
    max_traversal_depth: u32,
) -> Result<(), EngineError> {
    validate_statement_tree_with_options_and_policy(
        stmt,
        ValidateOptions::default(),
        StatementPolicy::unsafened(max_traversal_depth),
    )
}

/// The rules every subquery body owns, named once: 42I39's shortest-path
/// mixing in every body, plus the UNION-shape rules COUNT alone carries.
///
/// The CALL clause arm and the expression-subquery arm both route through
/// this, so the body rule set has one home. (A full fix would carry a
/// subquery-body role on the statement walk and apply the rules in
/// `validate_statement_node`; this narrower shape leaves the walk untouched
/// while removing the four hand-listed rule invocations.)
fn validate_subquery_body(body: &Statement, counts: bool) -> Result<(), EngineError> {
    if counts {
        validate_count_subquery_union_shapes(body)?;
    }
    validate_shortest_path_mixing(body)
}

/// Parse `query` and run one clause-battery checker over its top-level
/// clauses — the shared scaffold of the clause-rule tests (index hints,
/// match shape, …), so each test module's `validate` helper is one line.
#[cfg(test)]
pub(crate) fn parse_and_validate(
    query: &str,
    check: impl Fn(&[relate_parser::Clause]) -> Result<(), EngineError>,
) -> Result<(), String> {
    let stmt = relate_parser::parse(query).map_err(|error| error.to_string())?;
    check(&stmt.clauses).map_err(|error| error.to_string())
}

/// Parse `query` and run the production validation battery — the shared
/// scaffold of the end-to-end validation tests.
#[cfg(test)]
pub(crate) fn parse_and_validate_tree(query: &str) -> Result<(), String> {
    let stmt = relate_parser::parse(query).map_err(|error| error.to_string())?;
    validate_statement_tree(&stmt).map_err(|error| error.to_string())
}

/// The one implementation behind the entry points: the ADR-0371 validation
/// gates, the LABS-610 traversal-depth admission limit, and the LABS-680
/// safe-mode shape rules all ride the same battery. Crate-internal (every
/// caller is inside `relate-front`); the historical public entries
/// (`validate_statement_tree`) remain the public surface.
pub(crate) fn validate_statement_tree_with_options_and_policy(
    stmt: &Statement,
    options: ValidateOptions,
    policy: StatementPolicy,
) -> Result<(), EngineError> {
    validate_nesting_depth(stmt)?;
    // The CALL composition rules apply to a statement body and to every
    // CALL-subquery body (below), never to an expression subquery: a
    // `CALL … YIELD` may legitimately conclude an `EXISTS`/`COUNT` body.
    validate_call_composition(&stmt.clauses)?;
    // LABS-709: constant dynamic property-key/label operands are rejected
    // here, before execution, so a bad constant cannot hide behind a
    // zero-row MATCH. The walk owns the operand positions and fires the
    // visit during the clause descent — after the statement battery above,
    // so the historical first-error precedence is unchanged.
    let mut validate_operand =
        |expr: &Expr, token: relate_core::DynamicNameToken| dynamic::validate_operand(expr, token);
    // A version preamble scopes the whole query text (nested statements cannot
    // carry their own), so every node's battery sees the root statement's
    // `CYPHER 5` fact.
    let cypher_version = stmt.cypher_version;
    walk_statement_tree_with_operands(
        stmt,
        &mut |node| validate_statement_node(node, options, policy, cypher_version),
        // A subquery body is its own statement, so `validate_statement_node`
        // cannot tell it from a top-level one; the 42I39 mixing rule applies
        // only inside those bodies (CIP-40 path-selector-mixing [11]/[13]/
        // [18] keep separate top-level MATCH clauses legal).
        &mut |clause| match clause {
            Clause::CallSubquery(csq) => {
                validate_subquery_body(&csq.statement, false)?;
                validate_call_composition(&csq.statement.clauses)?;
                // A scoped `CALL (…) { … }` body is never a canonical
                // standalone SHOW/TERMINATE statement, so the composability
                // rules apply (live 2026.10: `CALL () { SHOW SETTINGS }` is
                // the 22N93 missing YIELD). A naked `CALL { … }` body keeps
                // the legacy leniency (`CALL { SHOW CONSTRAINTS }` runs).
                if matches!(
                    csq.scope,
                    relate_parser::CallScope::Vars(_) | relate_parser::CallScope::Wildcard
                ) {
                    show::validate_nested_show_composability(&csq.statement)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        },
        &mut |expr| {
            validate_expr_traversal_bounds(expr, policy.max_traversal_depth)?;
            // The expression-embedded pattern shapes: a shortest-path
            // expression's own shape and its bound endpoints.
            validate_expr_pattern_shape(expr)?;
            // The expression-node half of the non-aggregating-position
            // policy (comprehension/quantifier/reduce slots): the walk
            // visits every expression of every clause at every depth, so a
            // CREATE/MERGE/SET/CALL position is covered without a second
            // clause enumeration.
            validate_expression_slots(expr)?;
            // The same body rule set the CALL arm applies: COUNT is the one
            // kind carrying the UNION-shape rule (CIP-63 leaves EXISTS and
            // COLLECT unrestricted), and every body gets the mixing check.
            match expr {
                Expr::CountSubquery(cs) => validate_subquery_body(&cs.statement, true),
                Expr::ExistsSubquery(cs) => validate_subquery_body(&cs.statement, false),
                Expr::CollectSubquery(cs) => validate_subquery_body(&cs.statement, false),
                _ => Ok(()),
            }
        },
        &mut validate_operand,
    )
}

/// The per-statement battery applied by [`validate_statement_tree`]; see its
/// doc comment for the inclusion rule. Order preserves the historical
/// top-level relative order of these passes. `stmt` is a borrowed
/// [`StmtRef`]: a real statement of the AST, or a FOREACH body's clause
/// list (which has no UNION/NEXT chain of its own, so the chain-aware
/// validators see `None` there exactly as they did for the synthetic
/// statement the walk used to fabricate). Chain links arrive as
/// [`StmtRef::UnionArm`]/[`StmtRef::NextStage`] so the chain-aware
/// validators run their whole-chain scan at the chain's head only — the
/// walk visits every link, and a non-head link's suffix re-scan was
/// redundant (and quadratic on long chains).
fn validate_statement_node(
    stmt: StmtRef<'_>,
    options: ValidateOptions,
    policy: StatementPolicy,
    cypher_version: Option<u32>,
) -> Result<(), EngineError> {
    let clauses = stmt.clauses();
    validate_show_output_uniqueness(clauses)?;
    validate_param_property_shorthand(clauses)?;
    // 42I58 (Cypher 25): a CREATE clause may not reference an entity it
    // creates from a property definition in the same clause. The rule owns
    // its own `CYPHER 5` gate (01N00 keeps the warning semantics).
    // This battery reaches nested bodies without their correlated outer
    // bindings, so it applies the conservative tier (only unambiguous
    // declarations count as created); the scope-tracking pass
    // (`validate_variable_types`) re-runs the rule with the exact enclosing
    // names at the top level and in `FOREACH` bodies. The bodies only this
    // battery reaches (`CALL { }`, UNION arms, `NEXT` stages) keep the
    // under-approximation ADR-0513 §Consequences records.
    validate_create_self_reference_conservative(clauses, cypher_version)?;
    validate_relationship_uniqueness(clauses)?;
    validate_order_by_aggregation(clauses)?;
    validate_column_name_conflicts(clauses)?;
    validate_use_placement(stmt)?;
    validate_next_no_admin_commands(stmt)?;
    validate_search_clauses(clauses, options)?;
    validate_static_operator_types(clauses, cypher_version)?;
    validate_static_call_types(clauses)?;
    validate_limit_skip(clauses)?;
    validate_union_columns(stmt)?;
    validate_clause_slots(clauses)?;
    // LABS-610: appended so every historical validator keeps its
    // first-error precedence. The expression-embedded half (pattern
    // predicates, pattern comprehensions, `shortestPath()`) rides the
    // whole-tree walk's expression visitor instead.
    validate_traversal_bounds(clauses, policy.max_traversal_depth)?;
    // V1 harness wave: the MATCH-shape rules Neo4j rejects at compile time
    // (path-selector / REPEATABLE ELEMENTS / quantified path patterns).
    validate_match_shape(clauses)?;
    validate_index_hints(clauses)?;
    // 42N71: a statement body that is nothing but `USE` clauses is
    // incomplete (`RETURN EXISTS { USE neo4j }`, a bare `USE neo4j`).
    reject_use_only_statement(clauses)?;
    // LABS-680 (ADR-0477): the safe-mode shape rules ride the same
    // per-statement battery, so every nested body — UNION arms, NEXT
    // stages, CALL/WHEN/ELSE/FOREACH bodies, and expression-embedded
    // subqueries — is checked. A no-op when safe mode is off.
    validate_statement_shape(clauses, policy.safe_mode)?;
    // Last, so a body violating two rules keeps the historical first error:
    // before this rule existed, the battery's earlier validators fired and
    // the RETURN-in-FOREACH rejection happened only at execution. A FOREACH
    // body arrives as the walk's borrowed `StmtRef::Body` (the only
    // clause-list body); a RETURN anywhere else is a projection.
    if let StmtRef::Body(foreach_clauses) = stmt {
        validate_foreach_body(foreach_clauses)?;
    }
    Ok(())
}

/// The `CALL` composition rules the parser and the per-clause validators
/// cannot decide alone (live 2026.10):
///
/// - parentheses are mandatory in a `CALL` that is not the standalone
///   single-clause query (`MATCH (n) CALL db.labels` /
///   `CALL db.labels YIELD … RETURN …` are 42001);
/// - a standalone call cannot carry its `YIELD … WHERE` filter;
/// - a query cannot conclude with `CALL … YIELD` unless the call is the
///   whole query (42N71) — the missing-RETURN half of the procedure rules.
///
/// The separate 42N25 rule (a non-void in-query call must name its columns)
/// needs the procedure registry and lives in the facade's `validate_calls`.
fn validate_call_composition(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        let Clause::CallProcedure(cp) = clause else {
            continue;
        };
        if !cp.explicit_args && (clauses.len() > 1 || cp.where_expr.is_some()) {
            return Err(EngineError::semantic(
                ErrorKind::UnexpectedSyntax,
                "InvalidArgumentPassingMode: a procedure call is missing parentheses".to_string(),
            ));
        }
        if clauses.len() == 1 && cp.where_expr.is_some() {
            return Err(EngineError::semantic(
                ErrorKind::InvalidClauseComposition,
                "InvalidClauseComposition: a standalone call cannot carry a WHERE clause"
                    .to_string(),
            ));
        }
    }
    if clauses.len() > 1 {
        if let Some(Clause::CallProcedure(cp)) = clauses.last() {
            if cp.yields.present() {
                return Err(EngineError::semantic(
                    ErrorKind::IncompleteQuery,
                    "incomplete query: a query cannot conclude with CALL together with YIELD"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// 42N71 (incomplete query): a statement body consisting of nothing but
/// `USE` clauses cannot stand as a query (`RETURN EXISTS { USE neo4j }`, a
/// bare top-level `USE neo4j`). Neo4j's rule is that the query must conclude
/// with a RETURN, a FINISH, an update clause, a unit subquery call, or a
/// procedure call; a leading `USE` is legal, an only-`USE` body is not. The
/// EXISTS/COUNT/COLLECT- and CALL-subquery bodies all arrive here as their
/// own statement node, so one check covers every surface.
fn reject_use_only_statement(clauses: &[Clause]) -> Result<(), EngineError> {
    if !clauses.is_empty()
        && clauses
            .iter()
            .all(|clause| matches!(clause, Clause::Use(_)))
    {
        return Err(EngineError::semantic(
            ErrorKind::IncompleteQuery,
            "incomplete query: a query must conclude with a RETURN clause, a FINISH clause, an update clause, a unit subquery call, or a procedure call".to_string(),
        ));
    }
    Ok(())
}

// Float literal overflow (a numeric literal whose text parses to +-infinity,
// e.g. `1.34E999`) is rejected at parse time in
// `relate-parser::parser::parse_primary` — see the `TokenType::Float`
// arm there. It must happen at parse time rather than in a validator: once a
// numeric literal has been folded into `Expr::Literal(LiteralValue::Float(
// f64::INFINITY))`, it is indistinguishable from the legitimate CIP-55
// `Infinity`/`Inf`/`NaN` keyword literals, which also evaluate to an
// infinite/NaN `Float` and must NOT be rejected.

/// Parse `query` and run the expression-constraints pass — the one
/// spelling both the `expressions` and `delete` test batteries call (the
/// pass's Delete arm dispatches into the `delete` concern module's
/// policy). The identifier names its pass so a third concern battery
/// extracted later has an unambiguous entry; `scope`'s own local helper
/// keeps its name (different pass, different return type).
#[cfg(test)]
pub(crate) fn parse_and_validate_expression_constraints(query: &str) -> Result<(), EngineError> {
    let stmt = relate_parser::parse(query).expect("parse");
    expressions::validate_expression_constraints(&stmt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_test_support::run_with_big_stack;

    #[test]
    fn expression_chain_depth_is_counted_by_the_depth_guard() {
        // The parser's postfix loop counts no nesting depth for `.key`
        // property hops (or other per-hop forms), so the whole-tree walker
        // counts them instead: an over-cap chain is a typed depth error
        // from `validate_nesting_depth`, and a just-under-cap chain passes.
        // Big-stack threads as in the lower.rs ADR-0257 tests: even the
        // guard's own early-exit recursion (~500 levels) overflows the
        // default 2 MiB test-thread stack in debug builds.
        for (hops, want_err) in [(600usize, true), (499usize, false)] {
            run_with_big_stack(move || {
                let mut query = String::from("RETURN n");
                for _ in 0..hops {
                    query.push_str(".a");
                }
                let stmt = relate_parser::parse(&query).unwrap();
                let result = validate_nesting_depth(&stmt);
                if want_err {
                    let err = result.unwrap_err().to_string();
                    let expected = format!("maximum nesting depth of {MAX_NESTING_DEPTH} exceeded");
                    assert!(err.contains(&expected), "wrong error: {err}");
                } else {
                    result.unwrap();
                }
            });
        }
    }
}

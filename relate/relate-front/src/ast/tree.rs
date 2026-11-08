//! The whole-tree walker family shared by the semantic validation passes:
//! the ADR-0257 nesting-depth guard (`super::validate_nesting_depth`), the
//! nested-statement validation battery (`super::validate_statement_tree`),
//! and the CIP-150 dialect gates ([`super::dialect`]) all ride
//! [`walk_statement_tree`]; the find-and-check validators that treat
//! subqueries as opaque leaves ride [`walk_expr`], a thin wrapper over the
//! same expression-walker core ([`walk_expr_at`]).

use relate_core::DynamicNameToken;
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, Expr, LocalDefinitionBody, PatternElement, Statement, StructuralEdge};

use super::stmt_ref::StmtRef;

use super::MAX_NESTING_DEPTH;

mod clause;

use clause::walk_clause_exprs_tree;

pub(crate) type StmtVisit<'v> = dyn FnMut(StmtRef<'_>) -> Result<(), EngineError> + 'v;
pub(crate) type ClauseVisit<'v> = dyn FnMut(&Clause) -> Result<(), EngineError> + 'v;
pub(crate) type ExprVisit<'v> = dyn FnMut(&Expr) -> Result<(), EngineError> + 'v;

/// A visit for one dynamic name operand and the token namespace its position
/// implies: the inner expression of a dynamic label/type atom (`Label`), or a
/// write site's dynamic property key / `labels()` assignment value
/// (`PropertyKey` / `Label`). Fired by the walk's structural descents, so a
/// validator never re-derives which AST fields carry a label expression or a
/// dynamic key.
pub(crate) type DynamicOperandVisit<'v> =
    dyn FnMut(&Expr, DynamicNameToken) -> Result<(), EngineError> + 'v;

/// Whether the walk recurses from an expression node (first argument) into
/// one of its expression children (second argument). The whole-tree walk
/// and plain [`walk_expr`] pass an always-true filter; a find-and-check
/// validator whose semantics deliberately stop at a node boundary the
/// walker would otherwise cross (a scope-introducing comprehension, a
/// dynamic property *key*, a function's non-expression argument, …) passes
/// a narrower one via [`walk_expr_with`] so the walk visits exactly the
/// nodes its hand-rolled predecessor reached. The filter gates only
/// expression→expression edges inside [`walk_expr_at`]; structural descents
/// (into patterns, label expressions, nested statements) are unaffected.
pub(crate) type ExprChildFilter<'v> = dyn Fn(&Expr, &Expr) -> bool + 'v;

/// The two ADR-0257 nesting counters the walker family threads through its
/// recursion, bundled into one `Copy` argument so no walker signature
/// carries a loose `depth`/`expr_depth` pair:
///
/// - `statement` — statement nesting: UNION arms, NEXT stages, `CALL { }` /
///   `WHEN`/`ELSE`/FOREACH bodies, and expression-embedded subquery bodies
///   each count one level. Capped in [`walk_statement_tree_at`].
/// - `expr` — the expression-nesting depth the expressions currently being
///   walked sit at: 0 for a clause-level expression of a top-level
///   statement, carried *across* expression-embedded subquery boundaries
///   (see [`walk_statement_tree_at`]). Capped in [`walk_expr_at`].
#[derive(Clone, Copy)]
pub(crate) struct Depths {
    pub(crate) statement: usize,
    pub(crate) expr: usize,
}

impl Depths {
    /// The top-level entry counts: no enclosing statement or expression.
    const ZERO: Self = Depths {
        statement: 0,
        expr: 0,
    };

    /// One statement level deeper (a UNION arm, NEXT stage, or nested
    /// body), inheriting the entry expression depth: lowering lowers
    /// arms/stages/clause-level bodies from the same activation, with no
    /// enclosing expression frames of their own.
    fn deeper_statement(self) -> Self {
        Depths {
            statement: self.statement + 1,
            ..self
        }
    }

    /// One expression level deeper (an expression child, or the contained
    /// expressions of a pattern/label expression embedded at this node).
    fn deeper_expr(self) -> Self {
        Depths {
            expr: self.expr + 1,
            ..self
        }
    }

    /// An expression-embedded subquery body: one statement level deeper
    /// *and* one past the enclosing expression's depth, because
    /// `lower_expr` lowers `EXISTS`/`COUNT`/`COLLECT { ... }` bodies from
    /// inside its own recursion, so the body's expressions nest on top of
    /// the enclosing expression's frames on the same native stack.
    fn deeper_subquery(self) -> Self {
        Depths {
            statement: self.statement + 1,
            expr: self.expr + 1,
        }
    }
}

/// The four visitors a whole-tree walk carries, bundled so the walker family
/// passes one argument instead of a repeated parameter list, plus the walk's
/// [`SubqueryDescent`] mode and expression-child edge filter.
pub(crate) struct TreeWalk<'a, 'v> {
    pub(crate) stmt: &'a mut StmtVisit<'v>,
    pub(crate) clause: &'a mut ClauseVisit<'v>,
    pub(crate) expr: &'a mut ExprVisit<'v>,
    /// The dynamic name-operand visit. One walk owns the knowledge of where
    /// a label expression or a dynamic property key sits, so a validation
    /// pass over those operands never re-derives the AST descent.
    pub(crate) operand: &'a mut DynamicOperandVisit<'v>,
    pub(crate) descend: SubqueryDescent,
    pub(crate) enter_expr_child: &'a ExprChildFilter<'v>,
}

/// Whether the walk enters subquery statements and pattern-embedded
/// expressions, or treats them as opaque leaves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubqueryDescent {
    /// [`walk_expr`] semantics: `EXISTS`/`COUNT`/`COLLECT { ... }` bodies,
    /// clause-level nested bodies (`CALL { }` / `WHEN` / `ELSE` / FOREACH),
    /// the patterns of `shortestPath()`/pattern predicates/pattern
    /// comprehensions, and dynamic-label expressions are leaves.
    Opaque,
    /// Whole-tree semantics: nested statements are entered one statement
    /// level deeper (expression-embedded subqueries with the expression
    /// count carried across the boundary), and expression-embedded patterns
    /// and dynamic labels are walked.
    Enter,
}

/// Unlike [`walk_expr`] (which treats subquery expressions as opaque
/// leaves), this walk reaches *inside* every nested statement: UNION arms,
/// CIP-213 NEXT stages, `CALL { }` and `WHEN`/`ELSE` bodies, FOREACH clause
/// lists (presented to the statement visitor as a borrowed [`StmtRef::Body`],
/// since a FOREACH body is a clause list rather than a `Statement` in the
/// AST), and `EXISTS`/`COUNT`/`COLLECT` expression subqueries. The ADR-0257
/// statement-nesting cap fires structurally inside [`walk_statement_tree_at`]
/// — before the statement visitor, so the walk early-exits at the limit + 1
/// and no [`StmtVisit`] closure can opt out of it — and the walk carries the
/// expression-nesting depth the statement is *entered* at (see
/// [`walk_statement_tree_at`]). The walk is pre-order and stops at the
/// first `Err`.
pub(crate) fn walk_statement_tree(
    stmt: &Statement,
    visit_stmt: &mut StmtVisit,
    visit_clause: &mut ClauseVisit,
    visit_expr: &mut ExprVisit,
) -> Result<(), EngineError> {
    walk_statement_tree_with_operands(stmt, visit_stmt, visit_clause, visit_expr, &mut |_, _| {
        Ok(())
    })
}

/// Walk one clause list's expressions with whole-tree semantics (nested
/// statement bodies are entered), but without following the enclosing
/// statement's UNION/NEXT chain. A validator whose rule is decided per UNION
/// arm — the SHOW system-routing rules — walks each arm's own list, so one
/// arm's routing is never applied to a sibling arm's expressions. The
/// statement-nesting cap is enforced separately by
/// [`validate_nesting_depth`](crate::validate::validate_nesting_depth), which
/// runs before every such validator, so the per-arm entry depth of zero is
/// safe.
pub(crate) fn walk_clause_list_tree(
    clauses: &[Clause],
    visit_expr: &mut ExprVisit,
) -> Result<(), EngineError> {
    let mut skip_stmt = |_: StmtRef<'_>| Ok(());
    let mut skip_clause = |_: &Clause| Ok(());
    let mut skip_operand = |_: &Expr, _: DynamicNameToken| Ok(());
    let enter_all = |_: &Expr, _: &Expr| true;
    let mut walk = TreeWalk {
        stmt: &mut skip_stmt,
        clause: &mut skip_clause,
        expr: visit_expr,
        operand: &mut skip_operand,
        descend: SubqueryDescent::Enter,
        enter_expr_child: &enter_all,
    };
    walk_clauses_tree(clauses, Depths::ZERO, &mut walk)
}

/// Fire `visit` on each *root* expression of `clause` — every expression the
/// clause hands to the expression walker directly (a projection item, a
/// property value, a `WHERE`, …) — without any expression-internal descent.
/// Nested statements and expression-embedded patterns stay opaque
/// ([`SubqueryDescent::Opaque`]): a caller that owns its own scope fold (the
/// 42I58 self-reference scan applies each comprehension's binder set and
/// recurses into subquery bodies itself) enumerates roots here instead of
/// riding [`walk_statement_tree`], whose expression visit fires on every
/// nested expression node and so cannot tell a binder occurrence from a
/// reference.
pub(crate) fn walk_clause_expr_roots(
    clause: &Clause,
    visit_expr: &mut ExprVisit<'_>,
) -> Result<(), EngineError> {
    let mut skip_stmt = |_: StmtRef<'_>| Ok(());
    let mut skip_clause = |_: &Clause| Ok(());
    let mut skip_operand = |_: &Expr, _: DynamicNameToken| Ok(());
    let enter_none = |_: &Expr, _: &Expr| false;
    let mut walk = TreeWalk {
        stmt: &mut skip_stmt,
        clause: &mut skip_clause,
        expr: visit_expr,
        operand: &mut skip_operand,
        descend: SubqueryDescent::Opaque,
        enter_expr_child: &enter_none,
    };
    walk_clause_exprs_tree(clause, Depths::ZERO, &mut walk)
}

/// [`walk_statement_tree`] with the dynamic name-operand visit. The visit
/// fires at every dynamic label/type atom's inner expression
/// (`walk_label_expr_tree`) and at each write site's dynamic property key /
/// `labels()` assignment value (`walk_set_item_exprs_tree` and the `REMOVE`
/// arm), so a consumer validates exactly the operands the AST carries
/// without re-deriving which fields those are.
pub(crate) fn walk_statement_tree_with_operands(
    stmt: &Statement,
    visit_stmt: &mut StmtVisit,
    visit_clause: &mut ClauseVisit,
    visit_expr: &mut ExprVisit,
    visit_operand: &mut DynamicOperandVisit,
) -> Result<(), EngineError> {
    let enter_all = |_: &Expr, _: &Expr| true;
    let mut walk = TreeWalk {
        stmt: visit_stmt,
        clause: visit_clause,
        expr: visit_expr,
        operand: visit_operand,
        descend: SubqueryDescent::Enter,
        enter_expr_child: &enter_all,
    };
    walk_statement_tree_at(StmtRef::Real(stmt), Depths::ZERO, &mut walk)
}

/// The ADR-0257 depth-cap error, shared by the statement check
/// ([`walk_statement_tree_at`]), the expression check ([`walk_expr_at`])
/// and the self-reference collector's own bounded recursion
/// (`validate::self_reference`), so all three produce the identical
/// message.
pub(crate) fn nesting_depth_error() -> EngineError {
    EngineError::semantic(
        ErrorKind::SyntaxError,
        format!("maximum nesting depth of {MAX_NESTING_DEPTH} exceeded"),
    )
}

/// The statement-nesting half of the ADR-0257 cap fires at the head of
/// this function (see the check inside), so every whole-tree walk enforces
/// it regardless of what the `StmtVisit` closure does.
///
/// `depths.expr` is the expression-nesting depth the statement's own clause
/// expressions sit at — the `lower_expr` frames live on the native stack
/// when lowering reaches this statement. It is 0 for the top-level
/// statement and is inherited unchanged by clause-level nested statements
/// (`CALL { }` / `WHEN` / `ELSE` / FOREACH bodies, UNION arms, NEXT
/// stages): lowering lowers a statement's clauses sequentially, so a nested
/// clause body is entered with exactly the frames the enclosing statement
/// was entered with, whatever sibling expressions peaked at. Only
/// expression-embedded subquery bodies (`EXISTS`/`COUNT`/`COLLECT { ... }`)
/// are entered deeper — one past the enclosing expression's depth — because
/// `lower_expr` lowers those bodies from *inside* its own recursion
/// (`crate::lower::lower_expr`'s subquery arms call `lower_statement_impl`
/// directly), so their expressions nest on top of the enclosing
/// expression's frames on the same native stack. Resetting the count at
/// that boundary let statement depth × per-statement expression depth
/// multiply past the native-stack budget (3 nested `collect { }` levels ×
/// 450 subscript hops composed to ~1350 uncapped `lower_expr` frames and
/// SIGABRT'd on an 8 MiB release stack); carrying it makes the walk count
/// exactly the frames lowering will consume, so the combined composed depth
/// stays bounded by [`MAX_NESTING_DEPTH`].
fn walk_statement_tree_at(
    stmt: StmtRef<'_>,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    // The ADR-0257 statement-nesting cap, structural in the walker core:
    // it fires before the statement visitor (so the recursion early-exits
    // at the limit + 1) and there is no way to walk a statement tree
    // without it.
    if depths.statement > MAX_NESTING_DEPTH {
        return Err(nesting_depth_error());
    }
    (walk.stmt)(stmt)?;
    walk_clauses_tree(stmt.clauses(), depths, walk)?;
    // CIP-231: a `DEFINE` block heads this statement; its definition bodies
    // are nested statements (and expressions) of the same tree, so every
    // whole-tree validator covers them -- a rule violation cannot hide
    // inside a local callable body. The ADR-0257 depth cap above recurses
    // through here as well, so definition nesting is capped on the walk
    // side too.
    for definition in stmt.definitions() {
        match &definition.body {
            LocalDefinitionBody::Expression(expr) => {
                walk_expr_at(expr, depths, walk)?;
            }
            LocalDefinitionBody::Query(body) => {
                walk_statement_tree_at(StmtRef::Real(body), depths.deeper_statement(), walk)?;
            }
        }
    }
    // UNION arms and NEXT stages are chained statements; each link nests one
    // level deeper, since the walkers that consume them (this one included)
    // recurse per link. They inherit this statement's entry expression
    // depth: lowering lowers arms/stages from the same activation, with no
    // enclosing expression frames of their own.
    if let Some(union) = stmt.union() {
        walk_statement_tree_at(
            StmtRef::UnionArm(&union.statement),
            depths.deeper_statement(),
            walk,
        )?;
    }
    if let Some(next) = stmt.next() {
        walk_statement_tree_at(StmtRef::NextStage(next), depths.deeper_statement(), walk)?;
    }
    Ok(())
}

/// The one policy for entering a nested statement from either walker mode:
/// only [`SubqueryDescent::Enter`] descends (the whole-tree walker);
/// [`SubqueryDescent::Opaque`] leaves the body opaque for the caller's own
/// validators. `depths` is the depth the caller computed for the body —
/// `deeper_statement()` for a clause-level body, `deeper_subquery()` for an
/// expression-embedded one.
fn enter_substatement(
    walk: &mut TreeWalk<'_, '_>,
    stmt: StmtRef<'_>,
    depths: Depths,
) -> Result<(), EngineError> {
    if walk.descend != SubqueryDescent::Enter {
        return Ok(());
    }
    walk_statement_tree_at(stmt, depths, walk)
}

fn walk_clauses_tree(
    clauses: &[Clause],
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    for clause in clauses {
        (walk.clause)(clause)?;
        walk_clause_exprs_tree(clause, depths, walk)?;
    }
    Ok(())
}

fn walk_projection_exprs_tree(
    items: &[relate_parser::ReturnItem],
    order_by: &[relate_parser::OrderByItem],
    skip: Option<&Expr>,
    limit: Option<&Expr>,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    for item in items {
        if !item.all {
            walk_expr_at(&item.expr, depths, walk)?;
        }
    }
    for ob in order_by {
        walk_expr_at(&ob.expr, depths, walk)?;
    }
    if let Some(s) = skip {
        walk_expr_at(s, depths, walk)?;
    }
    if let Some(l) = limit {
        walk_expr_at(l, depths, walk)?;
    }
    Ok(())
}

fn walk_page_stages_tree(
    stages: &[relate_parser::PageStage],
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    for stage in stages {
        match stage {
            relate_parser::PageStage::OrderBy(items) => {
                for ob in items {
                    walk_expr_at(&ob.expr, depths, walk)?;
                }
            }
            relate_parser::PageStage::Skip(e) | relate_parser::PageStage::Limit(e) => {
                walk_expr_at(e, depths, walk)?;
            }
        }
    }
    Ok(())
}

/// The one write-site property-access descent: walk the owned value, fire
/// the dynamic-operand visit for a computed key (CIP-131 `n[<expr>]`), then
/// walk the key expression itself. Shared by the `SET` items and the
/// `REMOVE` targets so neither restates the key visit.
fn walk_property_access_tree(
    property: &relate_parser::PropertyAccess,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    walk_expr_at(&property.expr, depths, walk)?;
    if let Some(key) = &property.key_expr {
        (walk.operand)(key, DynamicNameToken::PropertyKey)?;
        walk_expr_at(key, depths, walk)?;
    }
    Ok(())
}

fn walk_set_item_exprs_tree(
    item: &relate_parser::SetItem,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    if let Some(p) = &item.property {
        walk_property_access_tree(p, depths, walk)?;
    }
    // `SET labels(n) = <expr>` is one construct: the function target is an
    // ordinary expression, and its value resolves through the dynamic-label
    // rule. Both are visited here, once, so the shape is decided in one
    // place.
    if let Some((target, _)) = &item.labels_function {
        walk_expr_at(target, depths, walk)?;
        if let Some(value) = &item.value {
            (walk.operand)(value, DynamicNameToken::Label)?;
        }
    }
    for label in &item.labels {
        walk_label_expr_tree(label, depths, walk)?;
    }
    if let Some(v) = &item.value {
        walk_expr_at(v, depths, walk)?;
    }
    Ok(())
}

/// `depths.expr` is the expression-nesting depth the path's contained
/// expressions sit at: 0 for a clause-level pattern, one past the enclosing
/// expression's depth for a pattern embedded in an expression (a pattern
/// comprehension, pattern predicate, or shortestPath()), since lowering
/// inlines those expressions into the enclosing `lower_expr` recursion.
fn walk_path_exprs_tree(
    path: &relate_parser::PatternPath,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    for elem in &path.elements {
        walk_element_exprs_tree(elem, depths, walk)?;
    }
    if let Some(w) = &path.where_expr {
        walk_expr_at(w, depths, walk)?;
    }
    Ok(())
}

fn walk_element_exprs_tree(
    elem: &PatternElement,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    match elem {
        PatternElement::Node(np) => {
            if let Some(le) = &np.label_expr {
                walk_label_expr_tree(le, depths, walk)?;
            }
            for (_, v) in &np.properties {
                walk_expr_at(v, depths, walk)?;
            }
            if let Some(p) = &np.properties_param {
                walk_expr_at(p, depths, walk)?;
            }
            if let Some(w) = &np.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
        PatternElement::Relationship(rp) => {
            if let Some(te) = &rp.type_expr {
                walk_label_expr_tree(te, depths, walk)?;
            }
            for (_, v) in &rp.properties {
                walk_expr_at(v, depths, walk)?;
            }
            if let Some(p) = &rp.properties_param {
                walk_expr_at(p, depths, walk)?;
            }
            if let Some(w) = &rp.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
        PatternElement::Quantified(qpp) => {
            for inner in &qpp.elements {
                walk_element_exprs_tree(inner, depths, walk)?;
            }
            if let Some(w) = &qpp.where_expr {
                walk_expr_at(w, depths, walk)?;
            }
        }
    }
    Ok(())
}

/// The plain label-expression walker: `visit` runs on every node of the
/// label-expression tree (pre-order; an `Err` stops the walk) — the same
/// variant set [`walk_label_expr_tree`] walks, without the tree-walk
/// plumbing. The find-and-check validators over label expressions
/// ([`crate::validate::writes`]'s CIP-156 checks, [`crate::validate::dynamic`]'s
/// operand check) ride it with a leaf closure instead of each re-spelling
/// the `Not`/`And`/`Or` fold.
pub(crate) fn walk_label_expr<F>(
    label: &relate_parser::LabelExpr,
    visit: &mut F,
) -> Result<(), EngineError>
where
    F: FnMut(&relate_parser::LabelExpr) -> Result<(), EngineError>,
{
    visit(label)?;
    match label {
        relate_parser::LabelExpr::Dynamic(_)
        | relate_parser::LabelExpr::Label(_)
        | relate_parser::LabelExpr::Wildcard => Ok(()),
        relate_parser::LabelExpr::Not(inner) => walk_label_expr(inner, visit),
        relate_parser::LabelExpr::And(items) | relate_parser::LabelExpr::Or(items) => {
            for item in items {
                walk_label_expr(item, visit)?;
            }
            Ok(())
        }
    }
}

/// `depths.expr` follows the same convention as [`walk_path_exprs_tree`]:
/// the depth a contained dynamic-label expression (`$(expr)`) sits at.
fn walk_label_expr_tree(
    label: &relate_parser::LabelExpr,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    match label {
        relate_parser::LabelExpr::Dynamic(d) => {
            (walk.operand)(&d.expr, DynamicNameToken::Label)?;
            walk_expr_at(&d.expr, depths, walk)?;
        }
        relate_parser::LabelExpr::Not(inner) => {
            walk_label_expr_tree(inner, depths, walk)?;
        }
        relate_parser::LabelExpr::And(items) | relate_parser::LabelExpr::Or(items) => {
            for item in items {
                walk_label_expr_tree(item, depths, walk)?;
            }
        }
        relate_parser::LabelExpr::Label(_) | relate_parser::LabelExpr::Wildcard => {}
    }
    Ok(())
}

/// The one expression-walker core: a pre-order walk where `walk.expr` runs
/// on every node before its children and an `Err` stops the walk.
/// `walk.descend` selects between the two historical walkers that each
/// re-encoded this same child-descent boilerplate:
///
/// - [`SubqueryDescent::Enter`] (under [`walk_statement_tree`]): the
///   whole-tree walker. Subquery expressions (`EXISTS`/`COUNT`/`COLLECT
///   { ... }`) recurse into their nested statement so statement-level
///   visitors see those bodies too, and the walk descends into the
///   patterns of pattern comprehensions, pattern predicates and
///   `shortestPath()` (via [`walk_path_exprs_tree`]), so subquery
///   statements and rule violations cannot hide inside a pattern embedded
///   in an expression.
///
/// - [`SubqueryDescent::Opaque`] (via [`walk_expr`]): the dialect-family
///   walker the find-and-check validators build on. It descends into every
///   *expression* child — including CASE, comprehension, reduce, quantifier
///   and map-projection bodies — but never into a subquery expression's
///   nested `Statement` or a pattern-bearing leaf: those validators treat
///   `EXISTS/COUNT/COLLECT { ... }` as opaque leaves (a validator that must
///   look inside a subquery, like the dialect gates in [`super::dialect`],
///   runs on every nested statement through the whole-tree walk instead).
///
/// Either way, every expression node along the walk counts one level of
/// expression nesting against [`MAX_NESTING_DEPTH`]: the parser's postfix
/// loop builds `.key` property chains, subscripts and map projections
/// *iteratively*, so its own depth counter never sees them — without this
/// accounting `lower_expr` recursed one native frame per hop and an
/// over-cap chain overflowed the stack (a 16000-hop property chain
/// SIGABRT'd on an 8 MiB release stack). The count starts at 0 for each
/// clause-level expression of a top-level statement, continues into
/// expression-embedded patterns and dynamic-label expressions (which
/// lowering inlines), and — in `Enter` mode — is *carried* into
/// expression-embedded subquery bodies: `lower_expr` lowers an
/// `EXISTS`/`COUNT`/`COLLECT { ... }` body from inside its own recursion,
/// so the body's expressions consume native frames on top of the enclosing
/// expression's — resetting the count there let statement depth and
/// per-statement expression depth multiply past the stack budget (see
/// [`walk_statement_tree_at`]). `Opaque` walks start the count at 0 too,
/// which keeps every consumer's own recursion bounded by the same cap.
fn walk_expr_at(
    expr: &Expr,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    if depths.expr > MAX_NESTING_DEPTH {
        return Err(nesting_depth_error());
    }
    (walk.expr)(expr)?;
    // The structural descents the expression children cannot express come
    // from the parser's one edge table (`Expr::structural_edges`), so a new
    // pattern- or subquery-bearing variant cannot be dropped here. Patterns
    // and subquery statements are entered before the expression children, a
    // label expression after them (the historical order).
    let edges = expr.structural_edges();
    let enter = walk.descend == SubqueryDescent::Enter;
    for edge in &edges {
        match edge {
            StructuralEdge::Pattern(pattern) if enter => {
                walk_path_exprs_tree(pattern, depths.deeper_expr(), walk)?;
            }
            // Expression-embedded subquery: lowering lowers the body from
            // inside `lower_expr`'s recursion, so the body's expressions sit
            // one past this node's depth on the same native stack — carry
            // the count across the statement boundary instead of resetting.
            StructuralEdge::Subquery(statement) => {
                enter_substatement(walk, StmtRef::Real(statement), depths.deeper_subquery())?;
            }
            StructuralEdge::Pattern(_) | StructuralEdge::Label(_) => {}
        }
    }
    for child in expr.children() {
        walk_expr_child(expr, child, depths.deeper_expr(), walk)?;
    }
    for edge in &edges {
        if let StructuralEdge::Label(label) = edge {
            if enter {
                walk_label_expr_tree(label, depths.deeper_expr(), walk)?;
            }
        }
    }
    Ok(())
}

/// Recurse from `parent` into its expression child `child` when the walk's
/// [`ExprChildFilter`] admits the edge; otherwise the child's whole subtree
/// is skipped (but the walk continues with the parent's remaining
/// children). Only expression→expression edges are filtered; the structural
/// descents (patterns, label expressions, nested statements) are not.
fn walk_expr_child(
    parent: &Expr,
    child: &Expr,
    depths: Depths,
    walk: &mut TreeWalk<'_, '_>,
) -> Result<(), EngineError> {
    if (walk.enter_expr_child)(parent, child) {
        walk_expr_at(child, depths, walk)?;
    }
    Ok(())
}

/// The boolean-operator vocabulary: the binary logical operators and the
/// unary `NOT`. One table beside the walkers, so the predicate-position
/// rule, the WHERE walk, the projection prune and the static-operand check
/// all agree on which operator spellings read a boolean.
pub(crate) fn is_boolean_operator(op: &str) -> bool {
    matches!(op, "NOT" | "AND" | "OR" | "XOR")
}

/// Generic expression walker: one pre-order traversal of an expression tree,
/// replacing the family of near-identical hand-rolled recursions that each
/// re-encoded the same child-descent boilerplate. `visit` runs on every node
/// before its children; an `Err` stops the walk and propagates (early exit).
///
/// It descends into every *expression* child — including CASE, comprehension,
/// reduce, quantifier and map-projection bodies — but never into a subquery
/// expression's nested `Statement`: the find-and-check validators built on it
/// treat `EXISTS/COUNT/COLLECT { ... }` as opaque leaves. A validator that
/// must look inside a subquery (like the dialect gates in [`super::dialect`])
/// runs on every nested statement through [`walk_statement_tree`] instead, or
/// keeps a bespoke traversal; a validator that must deliberately *not*
/// descend into a scope-introducing construct rides [`walk_expr_with`] with
/// an edge filter matching its historical reach, or keeps its own traversal.
/// Expression nesting is capped at [`MAX_NESTING_DEPTH`] from the root `expr`
/// (see [`walk_expr_at`]), so the walk's own recursion stays bounded.
pub(crate) fn walk_expr<F>(expr: &Expr, visit: &mut F) -> Result<(), EngineError>
where
    F: FnMut(&Expr) -> Result<(), EngineError>,
{
    walk_expr_with(expr, visit, |_, _| true)
}

/// Whether any node of `expr` satisfies `admit`, searching the edges
/// `enter_child` allows. The one find-in-expression scaffolding (the visited
/// flag, the depth-cap invariant) every `contains_*` predicate rides.
///
/// The walk's only failure is the [`MAX_NESTING_DEPTH`] cap, and every call
/// site runs inside `validate_statement_tree`, whose first pass
/// ([`validate_nesting_depth`](crate::validate::validate_nesting_depth)) has
/// already rejected an over-deep statement — so the error is an invariant
/// violation here, not a policy question. It panics with that invariant
/// rather than returning an answer the walk never computed (a silent `true`
/// would make `contains_aggregation` claim an aggregate it did not see).
pub(crate) fn expr_contains(
    expr: &Expr,
    enter_child: impl Fn(&Expr, &Expr) -> bool,
    admit: impl Fn(&Expr) -> bool,
) -> bool {
    let mut found = false;
    let mut visit = |node: &Expr| {
        if admit(node) {
            found = true;
        }
        Ok(())
    };
    walk_expr_with(expr, &mut visit, enter_child)
        .expect("the depth cap is rejected before validation");
    found
}

/// [`walk_expr`] with an [`ExprChildFilter`]: the walk recurses from an
/// expression node into one of its expression children only when
/// `enter_child(parent, child)` returns true, so a find-and-check validator
/// whose historical hand-rolled recursion stopped at specific node
/// boundaries (a dynamic property key, a scope-introducing comprehension, a
/// function's non-expression argument, …) can ride the shared self-capping
/// walker while visiting exactly the node set its predecessor did. The root
/// `expr` is always visited; everything else is unchanged from
/// [`walk_expr`] (pre-order, early exit on `Err`, opaque subqueries,
/// [`MAX_NESTING_DEPTH`] cap).
pub(crate) fn walk_expr_with<F, G>(
    expr: &Expr,
    visit: &mut F,
    enter_child: G,
) -> Result<(), EngineError>
where
    F: FnMut(&Expr) -> Result<(), EngineError>,
    G: Fn(&Expr, &Expr) -> bool,
{
    let mut skip_stmt = |_: StmtRef<'_>| Ok(());
    let mut skip_clause = |_: &Clause| Ok(());
    let mut skip_operand = |_: &Expr, _: DynamicNameToken| Ok(());
    let mut walk = TreeWalk {
        stmt: &mut skip_stmt,
        clause: &mut skip_clause,
        expr: visit,
        operand: &mut skip_operand,
        descend: SubqueryDescent::Opaque,
        enter_expr_child: &enter_child,
    };
    walk_expr_at(expr, Depths::ZERO, &mut walk)
}

#[cfg(test)]
pub(crate) mod tests;

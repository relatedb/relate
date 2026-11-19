//! The query and update clause nodes.

use super::*;
use relate_model::identifier::quote_identifier_shaped;

/// CIP-135 conditional clause: `WHEN <pred> THEN { <body> } [WHEN <pred>
/// THEN { <body> }]* [ELSE { <body> }]`. Each branch's `body`/`else_body`
/// is itself a `<single-query>` (own `Statement`, possibly carrying its own
/// `.union` chain).
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalClause {
    pub branches: Vec<WhenBranch>,
    pub else_body: Option<Statement>,
}

impl ConditionalClause {
    /// The one body that stands in for the whole clause's output shape:
    /// every branch and the ELSE body project the same shape (validated
    /// separately), so the first WHEN branch represents them, falling back
    /// to ELSE. `None` only for a clause with neither.
    pub fn representative_body(&self) -> Option<&Statement> {
        self.branches
            .first()
            .map(|b| &b.body)
            .or(self.else_body.as_ref())
    }
}

/// One `WHEN <predicate> THEN <single-query>` branch of a `ConditionalClause`.
#[derive(Debug, Clone, PartialEq)]
pub struct WhenBranch {
    pub predicate: Expr,
    pub body: Statement,
}

/// MATCH clause.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    pub optional: bool,
    /// CIP-51/CIP-60 match mode: whether relationship uniqueness (the
    /// default `DIFFERENT RELATIONSHIPS`) is enforced across the whole
    /// comma-separated graph pattern, or relaxed (`REPEATABLE ELEMENTS`).
    pub match_mode: MatchMode,
    pub pattern: Vec<PatternPath>,
    /// CIP-79 variables named by a `USING SCAN <var>:<Label>` hint.
    /// Relate's planner is not cost-based, but a scan hint is not a
    /// mere plan preference here: an indexed equality seek and a label
    /// scan genuinely answer differently for a zoned-datetime DST overlap
    /// (the index keys on instant plus zone, `=` on the wall clock), so
    /// the hint must reach the executor — the one hint family that does;
    /// `USING INDEX` stops at the front-end fulfillability check and
    /// `USING JOIN ON` at the plan-time notification, per the two
    /// fields below.
    pub using_scan: Vec<ScanHint>,
    /// CIP-79 `USING INDEX [SEEK] <var>:<Label>(<prop>[, …])` hints. The
    /// planner is not cost-based so they steer nothing, but the front end
    /// still validates that each hint is *fulfillable* — Neo4j rejects an
    /// undefined hint variable and an unfulfillable hint at compile time
    /// (SemanticErrorPart2 "An index hint … is rejected"), which needs the
    /// hint preserved here.
    pub using_index: Vec<IndexHint>,
    /// CIP-79 `USING JOIN ON <var>[, <var>…]` hints. The planner is not
    /// cost-based so a join hint steers nothing — every one is
    /// unfulfillable, which the `01N30` plan-time notification reports
    /// (ADR-0363); retaining the hint here is what lets the IR carry it
    /// to that detector.
    pub using_join: Vec<JoinHint>,
    pub where_expr: Option<Expr>,
    /// `<search clause>`: `SEARCH <binding var ref> IN (VECTOR
    /// INDEX <index name> FOR <expr> LIMIT <expr>) [SCORE AS <var>]`, a
    /// subclause of `<match statement>`. The grammar allows the `<graph
    /// pattern where clause>` to appear either before or after the search
    /// clause with no semantic difference (CIP-224 "Canonical syntax"), so
    /// the parser folds both orderings into this same `where_expr` field --
    /// the source order isn't tracked in the AST.
    pub search: Option<SearchClause>,
}

/// One `USING SCAN <variable>:<Label>` hint. The label is retained at the
/// same fidelity as [`IndexHint`]'s properties, so the hint family keeps
/// one shape; the planner's scan fork keys on the variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanHint {
    pub variable: SmallName,
    pub label: SmallName,
}

/// The index kind a `USING INDEX`-family hint names: the plain
/// `USING INDEX [SEEK]` form or the range-typed `USING RANGE INDEX` form.
/// The kinds share one fulfillability policy — the front end validates the
/// variable, label and WHERE predicate for both, exactly as it does for the
/// plain hint — and differ only in the spelling rendered back to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHintKind {
    Index,
    RangeIndex,
}

impl IndexHintKind {
    /// The kind as written between `USING` and the variable — `INDEX` or
    /// `RANGE INDEX` — the one spelling the front end's
    /// unfulfillable-hint error (42N76) and the plan-time `01N31`
    /// description render.
    pub fn as_written(&self) -> &'static str {
        match self {
            IndexHintKind::Index => "INDEX",
            IndexHintKind::RangeIndex => "RANGE INDEX",
        }
    }
}

/// One `USING [RANGE] INDEX <variable>:<Label>(<property>[, …])` hint —
/// the `[SEEK]` modifier belongs to the plain spelling only
/// (`USING INDEX SEEK`; `USING RANGE INDEX` takes no modifier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHint {
    pub kind: IndexHintKind,
    pub variable: SmallName,
    pub label: SmallName,
    pub properties: Vec<SmallName>,
}

impl IndexHint {
    /// The hinted property list as one text — `name`, `name, surname` —
    /// the one spelling both the front end's unfulfillable-hint error
    /// (42N76) and the plan-time `01N31` description render. Each name
    /// goes through the canonical display quoting
    /// (`quote_identifier_shaped`: bare when plain, backtick-escaped
    /// otherwise), so an exotic name cannot break the message carrying
    /// it.
    pub fn properties_text(&self) -> String {
        self.properties
            .iter()
            .map(|property| property.with_str(quote_identifier_shaped))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// One `USING JOIN ON <variable>[, <variable>…]` hint: the join key
/// identifiers, in written order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinHint {
    pub variables: Vec<SmallName>,
}

/// CIP-51/CIP-60 match mode: governs the default relationship-uniqueness
/// pool that spans one MATCH clause's entire comma-separated graph pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatchMode {
    /// `MATCH DIFFERENT RELATIONSHIPS ...` (also the implicit default): no
    /// two relationship slots anywhere in the graph pattern may bind to the
    /// same physical relationship.
    #[default]
    DifferentRelationships,
    /// `MATCH REPEATABLE ELEMENTS ...`: relaxes the relationship-uniqueness
    /// pool entirely -- nodes and relationships may repeat freely, subject
    /// only to each path's own path mode (which must resolve to `WALK`
    /// under this match mode).
    RepeatableElements,
}

/// CIP-51 path mode: an explicit restrictor on one path pattern's candidate
/// bindings. `PatternPath::mode` is `None` when no explicit keyword was
/// written -- the resolved default is `Walk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// Nodes and relationships may repeat freely (subject to the enclosing
    /// match mode's own relationship-uniqueness pool, which under the
    /// default `DIFFERENT RELATIONSHIPS` match mode still applies).
    Walk,
    /// No two relationship bindings in this one path may bind the same
    /// relationship (already the effective default under `DIFFERENT
    /// RELATIONSHIPS`, since that pool subsumes a single path's own).
    Trail,
    /// No two node bindings in this one path may bind the same node, except
    /// where two connectors are juxtaposed at a shared boundary node.
    Acyclic,
    /// Like `Acyclic`, but the very first and very last node of the path may
    /// coincide (an equijoin on the endpoints).
    Simple,
}

/// The `<number of paths>`/`<number of groups>` non-terminal (extended
/// by CIP-191 to accept a `$parameter`). A literal is validated
/// (`> 0`) at parse time; a parameter is validated at runtime once its value
/// is known (CIP-191 "Non-negative integer" rule).
#[derive(Debug, Clone, PartialEq)]
pub enum PathCount {
    Literal(u32),
    Parameter(String),
}

/// CIP-73 path selector prefix (`<path search prefix>`). `PatternPath` has
/// no selector when this is `None` -- the path pattern is then
/// non-selective, equivalent to an implicit `ALL`.
#[derive(Debug, Clone, PartialEq)]
pub enum PathSelector {
    /// `ALL [path mode] [PATH|PATHS]` -- also the implicit default.
    All,
    /// `ANY [<number of paths>] [path mode] [PATH|PATHS]`; `None` count
    /// defaults to 1.
    Any(Option<PathCount>),
    /// `ANY SHORTEST [path mode] [PATH|PATHS]` -- normalizes to `SHORTEST 1
    /// [path mode] PATH`.
    AnyShortest,
    /// `ALL SHORTEST [path mode] [PATH|PATHS]` -- normalizes to `SHORTEST 1
    /// [path mode] GROUP`.
    AllShortest,
    /// `SHORTEST <number of paths> [path mode] [PATH|PATHS]` (`group` false)
    /// or `SHORTEST [<number of groups>] [path mode] [PATH|PATHS]
    /// (GROUP|GROUPS)` (`group` true); `count` is `None` only in the GROUP
    /// form (defaults to 1).
    Shortest {
        count: Option<PathCount>,
        group: bool,
    },
}

/// CREATE clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateClause {
    pub pattern: Vec<PatternPath>,
    /// True when this clause was written as `INSERT` (CIP-114) rather than
    /// `CREATE` -- executed identically in every other respect, but the
    /// `+labels` side-effect stat is *not* subject to CREATE's row-scoped
    /// same-label-name dedup (empirically, INSERT counts every label
    /// application on every node it creates, unlike CREATE/MERGE).
    pub is_insert: bool,
}

/// MERGE clause.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeClause {
    pub pattern: PatternPath,
    pub on_create: Vec<SetItem>,
    pub on_match: Vec<SetItem>,
}

/// DELETE clause.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteClause {
    pub detach: bool,
    pub expressions: Vec<Expr>,
}

/// SET clause.
#[derive(Debug, Clone, PartialEq)]
pub struct SetClause {
    pub items: Vec<SetItem>,
}

/// A single item in a SET clause.
#[derive(Debug, Clone, PartialEq)]
pub struct SetItem {
    pub property: Option<PropertyAccess>,
    pub variable: Option<SmallName>,
    /// Each element is a `LabelExpr::Label` (static, `SET n:A`) or
    /// `LabelExpr::Dynamic` (CIP-162, `SET n:$(<expr>)`/`SET n IS $(<expr>)`)
    /// atom -- the parser never produces any other `LabelExpr` variant here
    /// (no `&`/`|`/`!`/`%`/grouping). See `RemoveItem::labels`.
    pub labels: Vec<LabelExpr>,
    pub value: Option<Expr>,
    pub merge: bool,
    /// True if this label item used the CIP-126 `IS` keyword (`SET n IS A`)
    /// rather than `:` (`SET n:A`). Only meaningful when `labels` is
    /// non-empty. Used for the CIP-40/CIP-126 statement-level rule that
    /// forbids mixing `IS` with a colon-conjunction (`:A:B`) anywhere in the
    /// same statement.
    pub is_keyword: bool,
    /// CIP-162 "Alternatives" section: `SET labels(<expr>) = / += / -=
    /// <expr>`. Per the proposal's own text this was never shipped in Neo4j
    /// ("we have decided to implement the rest first, and leave this as a
    /// potential future extension") -- Relate implements it anyway as
    /// the CIP's stated target semantics. `Some((target_expr, op))` when
    /// this item is that form, with `target_expr` the node expression
    /// inside `labels(...)` and `value` (above) holding the RHS list
    /// expression.
    pub labels_function: Option<(Expr, LabelsFunctionOp)>,
}

impl SetItem {
    /// `SET <property> = <value>`.
    pub fn property_assignment(property: PropertyAccess, value: Expr) -> Self {
        Self {
            property: Some(property),
            variable: None,
            labels: Vec::new(),
            value: Some(value),
            merge: false,
            is_keyword: false,
            labels_function: None,
        }
    }

    /// `SET <variable> = <value>`, or `SET <variable> += <value>` when
    /// `merge` is set.
    pub fn variable_assignment(variable: SmallName, value: Expr, merge: bool) -> Self {
        Self {
            property: None,
            variable: Some(variable),
            labels: Vec::new(),
            value: Some(value),
            merge,
            is_keyword: false,
            labels_function: None,
        }
    }

    /// `SET <variable>:<labels>` (or `IS <labels>` when `is_keyword`).
    pub fn label_set(variable: SmallName, labels: Vec<LabelExpr>, is_keyword: bool) -> Self {
        Self {
            property: None,
            variable: Some(variable),
            labels,
            value: None,
            merge: false,
            is_keyword,
            labels_function: None,
        }
    }

    /// `SET labels(<target>) <op> <value>`.
    pub fn labels_function(target: Expr, op: LabelsFunctionOp, value: Expr) -> Self {
        Self {
            property: None,
            variable: None,
            labels: Vec::new(),
            value: Some(value),
            merge: false,
            is_keyword: false,
            labels_function: Some((target, op)),
        }
    }
}

/// The operator of a `SET labels(<expr>) <op> <expr>` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelsFunctionOp {
    /// `=` -- replace the full label set.
    Overwrite,
    /// `+=` -- add every listed label, keep existing ones.
    Extend,
    /// `-=` -- remove every listed label that is present.
    Remove,
}

/// REMOVE clause.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveClause {
    pub items: Vec<RemoveItem>,
}

/// A single item in a REMOVE clause.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoveItem {
    pub property: Option<PropertyAccess>,
    pub variable: Option<SmallName>,
    /// See `SetItem::labels` (CIP-162 dynamic-label atoms allowed here too).
    pub labels: Vec<LabelExpr>,
    /// See `SetItem::is_keyword`.
    pub is_keyword: bool,
}

/// RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub group_by: Option<GroupBySpec>,
    pub distinct: bool,
    pub items: Vec<ReturnItem>,
    pub order_by: Vec<OrderByItem>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
}

/// A single item in a RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<SmallName>,
    pub all: bool,
    pub source_text: Option<String>,
}

/// An item in ORDER BY.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderByItem {
    pub expr: Expr,
    pub descending: bool,
}

/// Which surface syntax produced a `WithClause`. Plain `WITH`, CIP-206's
/// `LET`, and CIP-203's `FILTER` all desugar into the same `WithClause` so
/// they share WITH's projection/aggregation/ORDER BY/SKIP/LIMIT/WHERE
/// execution machinery and its generic variable-scope tracking -- but LET
/// and FILTER each carry restrictions ordinary WITH does not (LET: no
/// shadowing of an in-scope variable, no sibling/forward/self reference
/// within one LET's item list, no aggregation function in a value
/// expression; FILTER: its search condition's declared type must be
/// Boolean). The origin is recorded so the validation passes enforcing
/// those extra rules only fire for the syntax that actually carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WithOrigin {
    #[default]
    Explicit,
    Let,
    Filter,
}

/// `<group by clause>` on RETURN/WITH.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupBySpec {
    /// `GROUP BY <expr> [, <expr>...]`
    Keys(Vec<Expr>),
    /// `GROUP BY ()` -- the empty grouping set: one global group.
    Empty,
    /// `GROUP BY ALL` -- every non-aggregating return item is a key.
    All,
}

/// WITH clause.
#[derive(Debug, Clone, PartialEq)]
pub struct WithClause {
    pub group_by: Option<GroupBySpec>,
    pub distinct: bool,
    pub items: Vec<ReturnItem>,
    pub order_by: Vec<OrderByItem>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
    pub where_expr: Option<Expr>,
    pub origin: WithOrigin,
}

/// One stage of a CIP-168 standalone `<order by and page statement>`.
/// Neo4j 2026.05 accepts `ORDER BY`/`OFFSET`/`SKIP`/`LIMIT` in any order
/// at the start of the clause and applies them sequentially in the order
/// written -- not the fixed ORDER-BY-then-OFFSET-then-LIMIT semantic
/// order the CIP-168 text describes (e.g. `LIMIT 3 OFFSET 1` keeps 3
/// rows then drops the first 1 of those, rather than offsetting first).
/// A `Vec<PageStage>` records exactly the
/// written sequence so execution can replay it faithfully.
#[derive(Debug, Clone, PartialEq)]
pub enum PageStage {
    OrderBy(Vec<OrderByItem>),
    Skip(Expr),
    Limit(Expr),
}

/// CIP-168: a standalone `<order by and page statement>`. See
/// `Clause::StandaloneOrderBy`.
#[derive(Debug, Clone, PartialEq)]
pub struct StandaloneOrderByClause {
    pub stages: Vec<PageStage>,
}

/// UNWIND clause. A `FOR <variable> IN <expression>`
/// clause normalizes here at parse time -- the same expr/alias pair with
/// [`UnwindClause::for_syntax`] recording the source spelling -- so every
/// downstream consumer (validation, lowering, planning, execution) treats
/// the two spellings identically, exactly as the CIP requires (no new
/// operators). The flag is the parse-spelling record the FOREACH-body
/// renderer reads (`FOR`, not `UNWIND`, in the 42I01 text) — the one
/// consumer that must distinguish the spellings.
#[derive(Debug, Clone, PartialEq)]
pub struct UnwindClause {
    pub expr: Expr,
    pub alias: SmallName,
    /// True when the clause was written `FOR <variable> IN <expression>`
    /// (CIP-250) rather than `UNWIND <expression> AS <variable>`.
    pub for_syntax: bool,
}

/// FOREACH clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeachClause {
    pub variable: SmallName,
    pub list_expr: Expr,
    pub clauses: Vec<Clause>,
}

/// LOAD CSV clause.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadCSVClause {
    pub with_headers: bool,
    pub source: Expr,
    pub alias: SmallName,
    pub field_terminator: Option<String>,
}

/// CALL { ... } subquery clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CallSubqueryClause {
    pub statement: Statement,
    /// CIP-141 variable scope clause: `None` for a naked `CALL { ... }`
    /// (the deprecated legacy form, where an `IMPORTING WITH` as the
    /// subquery's own first clause is the only way to correlate outer
    /// variables in); `Some(CallScope::...)` for `CALL (...) { ... }`.
    pub scope: CallScope,
    /// `OPTIONAL CALL { ... }` -- when the inline subquery yields
    /// zero rows for a given outer row, that outer row is null-padded
    /// (every column the subquery would export set to `NULL`) instead of
    /// being dropped.
    pub optional: bool,
    /// CIP-32/CIP-34/CIP-198/CIP-30 trailing `IN [<concExpr>] CONCURRENT?
    /// TRANSACTIONS [OF <n> ROW(S)] [ON ERROR ...] [REPORT STATUS AS
    /// <var>]` production. `None` for a plain `CALL { ... }` subquery.
    pub in_transactions: Option<InTransactionsClause>,
}

/// The full `IN [<concExpr>] CONCURRENT? TRANSACTIONS [OF <n> ROW|ROWS] [ON
/// ERROR ...] [REPORT STATUS AS <var>]` production trailing a CALL
/// subquery. Parsed in its entirety (CIP-32 base batching, CIP-34
/// concurrency, CIP-198 retry, CIP-30 status/error) so later CIPs only need
/// to add validation/execution on top of this AST -- CIP-32 itself only
/// implements sequential batching plus the nesting/preceding-write
/// restrictions; concurrency and retry are parsed but currently execute as
/// plain sequential batching (see `Engine`'s `Clause::CallSubquery` arm).
#[derive(Debug, Clone, PartialEq)]
pub struct InTransactionsClause {
    /// `IN TRANSACTIONS` (no `CONCURRENT`) vs `IN [<expr>] CONCURRENT
    /// TRANSACTIONS` (CIP-34).
    pub concurrency: Concurrency,
    /// `OF <n> ROW|ROWS`. `None` means the batch size is
    /// implementation-chosen (batching[6]).
    pub batch_size: Option<Expr>,
    /// `ON ERROR CONTINUE|BREAK|FAIL|RETRY ...`. Defaults to `Fail` when
    /// `ON ERROR` is omitted.
    pub on_error: OnErrorMode,
    /// `REPORT STATUS AS <var>` (CIP-30).
    pub report_status: Option<SmallName>,
    /// CIP-260 `DISJOINT BY <exprs> | AUTO | NONE`. A concurrency hint that
    /// partitions the input rows by the listed key expressions (or lets the
    /// implementation choose / disables disjointness); it never changes the
    /// statement's result. Only legal on the `CONCURRENT` form.
    pub disjoint: Option<DisjointBy>,
}

/// `DISJOINT BY` operand.
#[derive(Debug, Clone, PartialEq)]
pub enum DisjointBy {
    /// `DISJOINT BY (<expr>, ...)`: partition rows by these deterministic
    /// key expressions.
    Expressions(Vec<Expr>),
    /// `DISJOINT BY AUTO`: the implementation picks the partitioning.
    Auto,
    /// `DISJOINT BY NONE`: no disjointness requirement.
    None,
}

impl InTransactionsClause {
    /// Every value expression of the `IN TRANSACTIONS` tail, in visit
    /// order: the CIP-34 concurrency value, the CIP-32 batch size, the
    /// CIP-198 retry duration and the `DISJOINT BY` key
    /// expressions. The exhaustive matches on the three operand enums are
    /// the slot list: a new operand variant is a compile error here until
    /// its expression is enumerated, so a consumer cannot drift from the
    /// parser shape.
    pub fn exprs(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        match &self.concurrency {
            Concurrency::Value(expr) => out.push(expr),
            Concurrency::Sequential | Concurrency::DefaultUnspecified => {}
        }
        out.extend(self.batch_size.as_ref());
        match &self.on_error {
            OnErrorMode::Retry { duration, .. } => out.extend(duration.as_ref()),
            OnErrorMode::Fail | OnErrorMode::Continue | OnErrorMode::Break => {}
        }
        match &self.disjoint {
            Some(DisjointBy::Expressions(keys)) => out.extend(keys),
            Some(DisjointBy::Auto | DisjointBy::None) | None => {}
        }
        out
    }
}

/// CIP-34 `IN [<concExpr>] CONCURRENT? TRANSACTIONS` concurrency modifier.
#[derive(Debug, Clone, PartialEq)]
pub enum Concurrency {
    /// No `CONCURRENT` keyword -- sequential (base CIP-32 form).
    Sequential,
    /// `CONCURRENT` with no explicit value -- implementation-chosen
    /// concurrency.
    DefaultUnspecified,
    /// `<expr> CONCURRENT` -- an explicit concurrency value.
    Value(Expr),
}

/// `ON ERROR ...` mode.
#[derive(Debug, Clone, PartialEq)]
pub enum OnErrorMode {
    /// The default when `ON ERROR` is omitted entirely.
    Fail,
    Continue,
    Break,
    /// CIP-198 `RETRY [FOR]? [<dur> SEC|SECOND|SECONDS]? [THEN
    /// CONTINUE|BREAK|FAIL]?`.
    Retry {
        /// `FOR <dur> SEC[OND[S]]`; `None` means the retry duration is
        /// implementation-chosen.
        duration: Option<Expr>,
        /// The mode to fall back to once retries are exhausted. Defaults to
        /// `Fail` when no `THEN` clause is given (CIP-198's documented
        /// deviation from a naive "defaults to CONTINUE" reading).
        fallback: RetryFallback,
    },
}

/// The `THEN CONTINUE|BREAK|FAIL` fallback mode of `ON ERROR RETRY`.
#[derive(Debug, Clone, PartialEq)]
pub enum RetryFallback {
    Fail,
    Continue,
    Break,
}

/// The `(...)` variable scope clause on a `CALL` subquery (CIP-141).
#[derive(Debug, Clone, PartialEq)]
pub enum CallScope {
    /// Naked `CALL { ... }` -- no scope clause at all.
    None,
    /// `CALL (*) { ... }` -- import every variable currently in scope.
    Wildcard,
    /// `CALL (a, b) { ... }` / `CALL () { ... }` -- import exactly these
    /// variables (the list may be empty).
    Vars(Vec<SmallName>),
}

/// `USE <graph-expr>` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct UseClause {
    pub graph_expr: Expr,
}

/// CALL db.procedure() YIELD ... clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CallProcedureClause {
    pub procedure: SmallName,
    pub args: Vec<Expr>,
    /// Whether the procedure name was followed by an explicit argument list,
    /// including an empty `()`. Relate needs this to distinguish `CALL p`
    /// (implicit arguments) from `CALL p()` (no arguments).
    pub explicit_args: bool,
    pub yields: YieldList,
    /// CIP-159: `OPTIONAL CALL proc(...)` -- when the procedure call yields
    /// zero rows (after any `WHERE` in `YIELD`) for a given outer row, that
    /// outer row is null-padded instead of being dropped. Void-procedure
    /// and zero-outer-row semantics are handled by the engine.
    pub optional: bool,
    /// CIP-159: `WHERE` inside the `YIELD` clause -- part of the
    /// `<procedure call>` production, filtering the procedure's own result
    /// rows (may reference the outer row's variables) before they're
    /// merged into the pipeline.
    pub where_expr: Option<Expr>,
}

// -- Pattern nodes --

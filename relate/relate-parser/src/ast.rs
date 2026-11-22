use relate_names::SmallName;

mod clause;
pub use clause::*;
mod expr;
pub use expr::*;
mod graph_type;
pub use graph_type::*;
mod pattern;
pub use pattern::*;
mod schema;
pub use schema::*;
mod search;
pub use search::*;
mod show;
pub use show::*;

/// The statement preamble mode (CIP-150): which of `EXPLAIN`/`PROFILE`
/// heads the statement, or neither. The preamble grammar is top-level
/// only and each keyword is admitted at most once, so exactly one mode
/// heads a statement — never a pair of flags that could disagree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreambleMode {
    /// No preamble keyword: the statement executes.
    #[default]
    Execute,
    /// An `EXPLAIN` preamble: the statement compiles and its plan renders,
    /// never executes.
    Explain,
    /// A `PROFILE` preamble: the statement executes with per-operator
    /// metrics attached.
    Profile,
}

/// AST node types for the Cypher parser.
///
/// The AST uses enums for Clause, Expr, and PatternElement to avoid trait objects.
/// Literal values use native Rust types (not CypherValue) since the AST is a compile-time concept.
///
/// Top-level AST node for a Cypher statement.
#[derive(Debug, Clone, Default)]
pub struct Statement {
    pub clauses: Vec<Clause>,
    pub union: Option<Box<UnionPart>>,
    /// The CIP-150 preamble mode — top-level only; every nested body
    /// carries [`PreambleMode::Execute`].
    pub preamble: PreambleMode,
    /// CIP-213 `<statement block> ::= <statement> [<next statement>...]`,
    /// `<next statement> ::= NEXT <statement>`. `next` chains this
    /// statement (which may itself carry its own `.union` chain, or be a
    /// sole `Clause::Conditional`) to the following NEXT stage's own
    /// statement (which may in turn have a `.next` of its own). `None`
    /// means this is either not part of a NEXT chain, or is its final
    /// stage. Deliberately a *sibling* field to `union`/`Conditional`
    /// rather than nested inside them: per the CIP grammar, NEXT operates
    /// on a whole `<statement>` (union-chain-or-conditional), one level
    /// above where UNION/WHEN operate, so it's threaded through the
    /// outermost `Statement` of each stage only -- a stage's own `.union`
    /// arms never carry their own independent `.next` chain.
    pub next: Option<Box<Statement>>,
    /// CIP-146/CIP-242: true when this `SingleQuery` was written as
    /// `[ USE <graph-expr> ] '{' <RegularQuery> '}'` -- top-level braces --
    /// rather than the plain unbraced form. The brace form's *content* is
    /// otherwise indistinguishable from an unbraced statement (same clause
    /// list, same `.union` chain) once parsed, but CIP-242's SHOW/TERMINATE
    /// composability rule ("Composing a SHOW command without YIELD fails")
    /// treats an explicitly brace-wrapped bare `SHOW` (e.g. `{ SHOW INDEXES
    /// }`) as *composed*, requiring `YIELD`, even though the identical
    /// clause list unbraced (`SHOW INDEXES` alone) is the lenient
    /// "canonical standalone" form that doesn't need one -- so this needs
    /// its own marker distinct from clause shape. Set only by
    /// `parse_braced_single_query`; every other `Statement` constructor
    /// leaves it `false` (via `#[derive(Default)]` field-by-field
    /// construction elsewhere).
    pub brace_wrapped: bool,
    /// CIP-231: the `DEFINE <procedure definition> | <function
    /// definition>` block parsed at the head of this statement block (only
    /// the top-level statement and braced nested bodies carry definitions;
    /// every statement-block parse site funnels through
    /// `parse_statement_block`, where they are parsed). Rewritten away --
    /// definitions and their call sites eliminated -- by the front-end
    /// resolver (`relate-front`'s `local_callables`) before lowering;
    /// the planner/executor never see local callables. `Default`-empty so
    /// the derived and hand-written constructors of un-gated statements
    /// are unchanged.
    pub definitions: Vec<LocalDefinition>,
}

/// Manual `PartialEq`: deliberately ignores `brace_wrapped`. The unwrap
/// rule -- and the parser tests pinning it (`test_brace_enclosed_top_level_query_is_transparent` and friends) --
/// treat a brace-wrapped `SingleQuery` as producing an AST indistinguishable
/// from its unbraced spelling; `brace_wrapped` is out-of-band provenance a
/// couple of CIP-242 composability checks read directly off the field
/// (`validate_show_composability`), not part of that observable-content
/// equivalence.
impl Statement {
    /// The end of this statement's `NEXT` chain: its true terminal stage
    /// (CIP-213), not its own first stage.
    pub fn tail_stage(&self) -> &Statement {
        let mut stage = self;
        while let Some(next) = &stage.next {
            stage = next;
        }
        stage
    }

    /// The stage whose clauses decide what the statement projects (CIP-213):
    /// a UNION head owns the whole composite, so it stands for itself; every
    /// other statement projects through the tail of its own `NEXT` chain
    /// (`A UNION B NEXT C` parses as `(A UNION B) NEXT C`).
    pub fn projection_stage(&self) -> &Statement {
        if self.union.is_some() {
            self
        } else {
            self.tail_stage()
        }
    }

    /// The result shape of the statement's projection stage — the one
    /// derivation, over [`Self::projection_stage`] and [`branch_shape`],
    /// that the result-header naming paths read (the facade's
    /// authored-column restore, ADR-0663), never a hand-rolled walk
    /// beside it. A UNION head's own arm is the shape the composite
    /// stands for; a sole conditional's is its representative body's.
    pub fn projection_shape(&self) -> BranchShape<'_> {
        branch_shape(&self.projection_stage().clauses)
    }

    /// Whether the statement's final stage projects columns: it ends in a
    /// `RETURN`, or in a conditional / nested `CALL` subquery that does. A
    /// unit subquery call with no `RETURN` makes the enclosing arm a
    /// no-column arm.
    pub fn projects_columns(&self) -> bool {
        match self.projection_stage().clauses.last() {
            Some(Clause::Return(_)) => true,
            Some(Clause::Conditional(cc)) => {
                // The grammar guarantees at least one WHEN branch
                // (`parse_when_query` pushes one before its loop can
                // break), so an empty branch list is a broken invariant to
                // surface -- never a fabricated "does not project" verdict.
                cc.branches
                    .first()
                    .expect("a WHEN clause has at least one branch")
                    .body
                    .projects_columns()
            }
            Some(Clause::CallSubquery(inner)) => inner.statement.projects_columns(),
            _ => false,
        }
    }
}

impl PartialEq for Statement {
    fn eq(&self, other: &Self) -> bool {
        self.clauses == other.clauses
            && self.union == other.union
            && self.preamble == other.preamble
            && self.next == other.next
            && self.definitions == other.definitions
    }
}

/// CIP-231 `DEFINE` block entry: one local callable definition, parsed at
/// the head of a `<statement block>` (top level or a braced nested body)
/// and rewritten away by the front-end resolver
/// (`relate-front`'s `local_callables`) before lowering -- the
/// planner/executor never see local callables.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalDefinition {
    /// `PROCEDURE` or `FUNCTION`.
    pub kind: LocalCallableKind,
    /// The fully-qualified callable name as written (`foo.bar`), namespace
    /// segments joined on `.`. Matched against call-site names verbatim.
    pub name: SmallName,
    /// The `<callable input signature>`: parameters in declaration order.
    pub params: Vec<LocalParam>,
    /// The declared `<procedure output type>` (`(field :: T, ...)`) or
    /// `<function output type>` (`:: T`), when written.
    pub output: Option<LocalOutput>,
    /// `= <value expression>` (simple function body) or a braced
    /// `<procedure specification>` (procedure body or subquery function
    /// body).
    pub body: LocalDefinitionBody,
}

/// `DEFINE PROCEDURE` vs `DEFINE FUNCTION` (CIP-231). The two namespaces
/// are independent (CIP-230: a procedure and a function may share a name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalCallableKind {
    Procedure,
    Function,
}

/// One `<input field signature>` of a local callable (CIP-231): a field
/// name, an optional `[<typed>] <value type>`, and an optional
/// `= <value expression>` default (which makes the parameter optional).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalParam {
    pub name: SmallName,
    /// The written value type, `None` when omitted (implicitly ANY).
    /// Parsed and syntactically validated, deliberately not enforced at
    /// runtime (ADR-0382).
    pub type_expr: Option<TypeExpr>,
    /// `= <value expression>`; `None` for a mandatory parameter. Defaults
    /// may not reference variables (checked at parse time).
    pub default: Option<Expr>,
}

/// A declared output type of a local callable (CIP-231): the procedure
/// form `:: (a :: INT, ...)` or the function form `:: STRING`.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalOutput {
    /// `:: (<mandatory field signature>, ...)` on a procedure.
    Fields(Vec<LocalOutputField>),
    /// `:: <value type>` on a function.
    Type(TypeExpr),
}

/// One field of a procedure's `:: (a :: INT, ...)` output signature.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalOutputField {
    pub name: SmallName,
    pub type_expr: Option<TypeExpr>,
}

/// The body of a local callable definition (CIP-231).
#[derive(Debug, Clone, PartialEq)]
pub enum LocalDefinitionBody {
    /// `FUNCTION f(...) = <value expression>` (simple function body).
    Expression(Expr),
    /// A braced `<procedure specification>`: a procedure body, or a
    /// subquery function body.
    Query(Statement),
}

/// Represents a UNION or UNION ALL between statements.
#[derive(Debug, Clone, PartialEq)]
pub struct UnionPart {
    pub all: bool,
    pub statement: Statement,
}

/// All clause types in the Cypher language.
#[derive(Debug, Clone, PartialEq)]
pub enum Clause {
    Match(MatchClause),
    Create(CreateClause),
    Merge(MergeClause),
    Delete(DeleteClause),
    Set(SetClause),
    Remove(RemoveClause),
    Return(ReturnClause),
    With(WithClause),
    Unwind(UnwindClause),
    CreateIndex(CreateIndexClause),
    DropIndex(DropIndexClause),
    CreateConstraint(CreateConstraintClause),
    DropConstraint(DropConstraintClause),
    Foreach(ForeachClause),
    LoadCSV(LoadCSVClause),
    CallSubquery(CallSubqueryClause),
    CallProcedure(CallProcedureClause),
    ShowFunctions(ShowFunctionsClause),
    ShowConstraints(ShowConstraintsClause),
    ShowIndexes(ShowIndexesClause),
    /// `SHOW DATABASE[S] [<name>] [YIELD ...] [WHERE ...]` / `SHOW DEFAULT
    /// DATABASE` / `SHOW HOME DATABASE` (CIP-68's singular/plural KIND
    /// acceptance; CIP-42's four non-default columns). A minimal virtual
    /// catalog -- Relate is a single-database engine, so this reports a
    /// small set of deterministic built-in databases rather than modeling
    /// real multi-database administration.
    ShowDatabases(ShowDatabasesClause),
    /// `SHOW [ALL|POPULATED] ROLE[S] [WITH USER[S]] [YIELD ...] [WHERE ...]`
    /// (CIP-68). A minimal virtual catalog of built-in roles.
    ShowRoles(ShowRolesClause),
    /// `SHOW SERVER[S] [YIELD ...] [WHERE ...]` (CIP-68). Reports a single
    /// row, matching a single-process embedded engine.
    ShowServers(ShowServersClause),
    /// `SHOW USER[S] [YIELD ...] [WHERE ...]` / `SHOW CURRENT USER`
    /// (CIP-68). A minimal virtual catalog of a single built-in user.
    ShowUsers(ShowUsersClause),
    /// `SHOW PROCEDURE[S] [YIELD ...] [WHERE ...]` (CIP-68 grammar
    /// foundation). Lists the built-in procedure names the engine's `CALL`
    /// handler recognizes.
    ShowProcedures(ShowProceduresClause),
    /// `SHOW SETTING[S] [<names>] [YIELD ...] [WHERE ...]` (CIP-68 grammar
    /// foundation). Relate has no configurable settings surface, so
    /// this always reports an empty catalog.
    ShowSettings(ShowSettingsClause),
    /// `SHOW TRANSACTION[S] [<ids>] [YIELD ...] [WHERE ...]` (CIP-68
    /// grammar foundation). Relate executes each statement to
    /// completion without exposing concurrent transaction identifiers, so
    /// this always reports an empty catalog.
    ShowTransactions(ShowTransactionsClause),
    /// `TERMINATE TRANSACTION[S] <ids> [YIELD ...]` (CIP-68 grammar
    /// foundation). Since `ShowTransactions` never reports a live
    /// transaction, every requested id is reported as not found.
    TerminateTransactions(TerminateTransactionsClause),
    /// `USE <graph-expr>` (CIP-141 feature-interaction scenarios) -- a
    /// graph-selector clause. Relate is a single-database engine, so
    /// this is parsed and scope-checked (its expression's variables must
    /// be in scope, same as any other clause) but has no runtime effect
    /// beyond that: there is no composite/multi-graph database to switch
    /// to.
    Use(UseClause),
    /// `FINISH` (CIP-140) -- a terminal clause producing a truly empty
    /// result (0 rows, 0 columns) instead of RETURN's projected rows,
    /// while still running any earlier updating clauses. Only the minimal
    /// semantics needed by CIP-218's `CALL (a) { FINISH }` scenario are
    /// implemented: cardinality is preserved (one output row per input
    /// row) but that row carries no columns, so it contributes nothing
    /// when merged back into an enclosing CALL's outer row.
    Finish,
    /// CIP-168: a standalone `<order by and page statement>` -- e.g.
    /// `ORDER BY n.name SKIP 1 LIMIT 3` appearing as its own clause, not
    /// attached to WITH/RETURN/YIELD. Acts as an implicit `WITH *`
    /// horizon clause: changes row order/count but never the column set,
    /// and may sit between any two other clauses (including two write
    /// clauses). See `StandaloneOrderByClause` for why this is a sequence
    /// of stages rather than three fixed fields.
    StandaloneOrderBy(StandaloneOrderByClause),
    /// CIP-135: a standalone conditional clause chain --
    /// `<when query> ::= <when>, {<when>}, [<else>]`. This is a *sibling*
    /// alternative to `<union query>`/`<single query>` at the `<regular
    /// query>` grammar level (see `parse_regular_query`), not an ordinary
    /// mid-statement clause -- so this variant only ever appears as the
    /// sole element of a `Statement.clauses` vec, never alongside other
    /// clauses in the same statement.
    Conditional(ConditionalClause),
    /// `ALTER CURRENT GRAPH TYPE SET/ADD/ALTER/DROP { <graph type
    /// specification body> }` (CIP-176/182/183's mechanism; CIP-187's
    /// constraint derivation builds on it). `body` is the full CIP-178
    /// `<graph type specification body>` grammar (node/edge type specs,
    /// property types, and constraint specs) -- see `GraphTypeBody`.
    AlterCurrentGraphType(AlterCurrentGraphTypeClause),
    /// `CREATE GRAPH TYPE <name> [AS] { <graph type specification body> }`
    /// (CIP-178 grammar; the `AS` keyword is CIP-180's spelling of the same
    /// body-introducing form). Registers a *named* graph type, independent
    /// of the database's single unnamed "current" graph type that `ALTER
    /// CURRENT GRAPH TYPE` manages.
    CreateGraphType(CreateGraphTypeClause),
    /// `SHOW GRAPH TYPE <name> [YIELD ...]` (CIP-180's SHOW surface): a
    /// single-row catalog whose `name`/`body` columns report the named
    /// graph type's own name and its CIP-180 canonical specification
    /// string.
    ShowGraphType(ShowGraphTypeClause),
    /// `SHOW CURRENT GRAPH TYPE [YIELD ...]` (CIP-176 overview / CIP-184):
    /// the database's persistent current graph type, reported as a single
    /// `specification` column (the CIP-180 canonical body string wrapped in
    /// `{ }`, or the literal `{}` when empty).
    ShowCurrentGraphType(ShowCurrentGraphTypeClause),
    /// `ALTER DATABASE <name> [IF EXISTS] SET DEFAULT LANGUAGE CYPHER
    /// <n>` (CIP-170). Under ADR-0116 §2's fixed namespaces this is the
    /// only supported `ALTER DATABASE` form; every other form is
    /// recognized and rejected at parse time with CE's
    /// `UnsupportedAdministrationCommand`.
    AlterDatabase(AlterDatabaseClause),
}

impl Clause {
    /// The Cypher clause keyword(s) for this clause — e.g. `"MATCH"`,
    /// `"SHOW INDEXES"`, `"LOAD CSV"`. Exhaustive over the enum, so a new
    /// clause cannot ship without a renderable name; the planner's
    /// `clause_keyword` delegates here rather than keeping a second table.
    pub fn keyword(&self) -> &'static str {
        match self {
            Clause::Match(_) => "MATCH",
            Clause::Create(_) => "CREATE",
            Clause::Merge(_) => "MERGE",
            Clause::Delete(_) => "DELETE",
            Clause::Set(_) => "SET",
            Clause::Remove(_) => "REMOVE",
            Clause::Return(_) => "RETURN",
            Clause::With(_) => "WITH",
            Clause::Unwind(_) => "UNWIND",
            Clause::CreateIndex(_) => "CREATE INDEX",
            Clause::DropIndex(_) => "DROP INDEX",
            Clause::CreateConstraint(_) => "CREATE CONSTRAINT",
            Clause::DropConstraint(_) => "DROP CONSTRAINT",
            Clause::Foreach(_) => "FOREACH",
            Clause::LoadCSV(_) => "LOAD CSV",
            Clause::CallSubquery(_) | Clause::CallProcedure(_) => "CALL",
            Clause::ShowFunctions(_) => "SHOW FUNCTIONS",
            Clause::ShowConstraints(_) => "SHOW CONSTRAINTS",
            Clause::ShowIndexes(_) => "SHOW INDEXES",
            Clause::ShowDatabases(_) => "SHOW DATABASES",
            Clause::ShowRoles(_) => "SHOW ROLES",
            Clause::ShowServers(_) => "SHOW SERVERS",
            Clause::ShowUsers(_) => "SHOW USERS",
            Clause::ShowProcedures(_) => "SHOW PROCEDURES",
            Clause::ShowSettings(_) => "SHOW SETTINGS",
            Clause::ShowTransactions(_) => "SHOW TRANSACTIONS",
            Clause::TerminateTransactions(_) => "TERMINATE TRANSACTIONS",
            Clause::Use(_) => "USE",
            Clause::Finish => "FINISH",
            Clause::StandaloneOrderBy(_) => "ORDER BY",
            Clause::Conditional(_) => "WHEN",
            Clause::AlterCurrentGraphType(_) => "ALTER CURRENT GRAPH TYPE",
            Clause::CreateGraphType(_) => "CREATE GRAPH TYPE",
            Clause::ShowGraphType(_) => "SHOW GRAPH TYPE",
            Clause::ShowCurrentGraphType(_) => "SHOW CURRENT GRAPH TYPE",
            Clause::AlterDatabase(_) => "ALTER DATABASE",
        }
    }
}

mod expr_children;
pub use expr_children::{ExprEdge, ExprEdgeMut};

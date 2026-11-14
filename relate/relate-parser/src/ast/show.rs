//! `SHOW`/`TERMINATE`/administration clause nodes and the shared `YIELD` list.

use super::*;

/// See `Clause::AlterDatabase`.
#[derive(Debug, Clone, PartialEq)]
pub struct AlterDatabaseClause {
    pub name: SchemaName,
    pub if_exists: bool,
    /// The `SET DEFAULT LANGUAGE CYPHER <n>` digits (e.g. `5`, `25`, or an
    /// unsupported value like `6`), unvalidated at parse time (CIP-170):
    /// an out-of-range version here is a *runtime*
    /// data exception (`22N04`), unlike the CIP-150 statement-preamble
    /// `CYPHER <version>` prefix, which rejects at compile time.
    pub default_language: u32,
}

/// One `YIELD <name> [AS <alias>]` item of a procedure call or catalog
/// command. `alias` is `None` when no `AS` was written, so "no alias" is
/// represented by the optional itself rather than an empty-string sentinel
/// in a parallel array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YieldItem {
    pub name: SmallName,
    pub alias: Option<SmallName>,
}

impl YieldItem {
    /// The name this item binds in the enclosing scope: the alias when one
    /// was written, else the yielded column name. A `SmallName` copy (the
    /// inline arm is a `u64` move, the spilled arm a refcount bump,
    /// ADR-0509 §1) — consumers bridging into a `String`-keyed scope map
    /// pay the text conversion at that one feed site.
    pub fn effective_name(&self) -> SmallName {
        self.alias.clone().unwrap_or_else(|| self.name.clone())
    }
}

/// A clause's `YIELD` tail, modeled once: the explicit items in source
/// order plus whether `YIELD *` was written. Empty `items` with `all ==
/// false` means no `YIELD` at all (the grammar never reads an empty
/// explicit list).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct YieldList {
    pub items: Vec<YieldItem>,
    pub all: bool,
}

/// A clause's yield mode, derived once from its [`YieldList`]: `YIELD *`
/// projects every column, a bare clause its default table, and an explicit
/// list its named (post-alias) columns. Consumers select their behavior
/// from this instead of each re-testing `(all, is_empty)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YieldMode {
    /// `YIELD *`: every column of the command.
    Star,
    /// No `YIELD` at all: the command's default table.
    Default,
    /// An explicit `YIELD` list: the named, post-alias columns.
    Named,
}

impl YieldList {
    /// The clause's yield mode — the one derivation.
    pub fn mode(&self) -> YieldMode {
        if self.all {
            YieldMode::Star
        } else if self.items.is_empty() {
            YieldMode::Default
        } else {
            YieldMode::Named
        }
    }

    /// Whether any `YIELD` tail was written (an explicit list or `*`).
    pub fn present(&self) -> bool {
        !matches!(self.mode(), YieldMode::Default)
    }

    /// Each item's raw yielded name, in source order.
    pub fn names(&self) -> impl Iterator<Item = &SmallName> {
        self.items.iter().map(|item| &item.name)
    }

    /// Each item's raw name, optional alias, and effective (post-alias)
    /// name, in source order. Consumers that need both sides of the alias
    /// rule read them from this one iterator instead of re-deriving the
    /// precedence.
    pub fn pairs(&self) -> impl Iterator<Item = (&SmallName, Option<&SmallName>, SmallName)> {
        self.items
            .iter()
            .map(|item| (&item.name, item.alias.as_ref(), item.effective_name()))
    }

    /// Each item's effective (post-alias) name, in source order.
    pub fn effective_names(&self) -> impl Iterator<Item = SmallName> + '_ {
        self.items.iter().map(YieldItem::effective_name)
    }
}

/// `SHOW [ALL|RANGE|POINT|TEXT|FULLTEXT|VECTOR|LOOKUP] INDEXES [YIELD ...]
/// [WHERE ...]` (CIP-11). Mirrors `ShowConstraintsClause`; `kind_filter`
/// is `None` for bare `SHOW INDEXES`/`SHOW ALL INDEXES` (every index kind).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowIndexesClause {
    pub kind_filter: Option<IndexKind>,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    /// `ORDER BY`/`SKIP`/`LIMIT` written directly on the `YIELD` (before
    /// any trailing `WHERE`), same paging sub-grammar and rationale as
    /// `ShowConstraintsClause::stages`.
    pub stages: Vec<PageStage>,
}

/// `SHOW FUNCTIONS [YIELD ...] [WHERE ...]` — the built-in function catalog
/// (CIP-245/CIP-247/CIP-263 discoverability scenarios). A minimal, dedicated
/// clause rather than a generic SHOW-command framework: only the function
/// catalog is modeled, not indexes/constraints/databases (out of this
/// batch's scope). `where_expr` is embedded here (rather than requiring a
/// standalone `WHERE` clause, which this AST has no variant for — WHERE is
/// always embedded in the clause that produces its rows, matching
/// `MatchClause`) so `SHOW FUNCTIONS YIELD name WHERE name = 'x' RETURN
/// name` parses as a single clause producing already-filtered rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowFunctionsClause {
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    /// The YIELD's own paging sub-grammar, written between the YIELD list
    /// and the trailing `WHERE` (live Neo4j
    /// accepts that order for every SHOW command).
    pub stages: Vec<PageStage>,
    /// `[ALL | BUILT IN | USER DEFINED]` scope filter (CIP-84/CIP-157
    /// `SHOW BUILT IN FUNCTIONS` / `SHOW USER DEFINED FUNCTIONS`
    /// scenarios). `All` (the default) and `BuiltIn` are equivalent for
    /// Relate, which ships only built-in functions; `UserDefined`
    /// always yields an empty catalog (no way to register a UDF).
    pub scope: FunctionScope,
    /// `EXECUTABLE [BY <user>]`, when written.
    pub executable: Option<ExecutableBy>,
}

/// The `EXECUTABLE BY <user>` selector of `SHOW FUNCTIONS`/`SHOW
/// PROCEDURES`: the commands a user may execute. The bare `EXECUTABLE` form
/// applies no privilege filter, so it is the same state as writing no
/// selector at all (`None` on the clause) and has no variant of its own.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutableBy {
    /// `EXECUTABLE BY <user>` -- the named user.
    User(String),
    /// `EXECUTABLE BY CURRENT USER` -- the session's own user.
    Current,
}

/// Scope filter for `SHOW [ALL | BUILT IN | USER DEFINED] FUNCTIONS`
/// (CIP-84/CIP-157). See `ShowFunctionsClause::scope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FunctionScope {
    #[default]
    All,
    BuiltIn,
    UserDefined,
}

/// `SHOW [<type filter>] CONSTRAINTS [YIELD ...] [WHERE ...]` (CIP-85's
/// observation surface, needed by CIP-100 to expose a property type
/// constraint's normalized `propertyType`; the `<type filter>` grammar is
/// CIP-62/CIP-64/CIP-74's `[ALL] [NODE|REL[ATIONSHIP]] [PROPERTY]?
/// (UNIQUE[NESS]|KEY|EXIST[ENCE]|TYPE)` surface). Mirrors
/// `ShowFunctionsClause` otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowConstraintsClause {
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub type_filter: ConstraintTypeFilter,
    /// `ORDER BY`/`SKIP`/`LIMIT` written directly on the `YIELD` (before
    /// any trailing `WHERE`) as part of the `YIELD`'s own paging
    /// sub-grammar, e.g. `YIELD name ORDER BY
    /// name WHERE ... RETURN ...` -- distinct from a `SHOW ... YIELD ...`
    /// followed by a separate top-level `ORDER BY`/`RETURN ... ORDER BY`
    /// clause, which parses as its own `StandaloneOrderBy`/`Return` clause
    /// instead and never populates this field.
    pub stages: Vec<PageStage>,
}

/// The entity half of a `SHOW <type filter> CONSTRAINTS` filter -- `Any`
/// for bare `SHOW CONSTRAINTS`/`SHOW ALL CONSTRAINTS` or a kind-only
/// filter with no `NODE`/`REL[ATIONSHIP]` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintEntityFilter {
    Any,
    Node,
    Relationship,
}

/// The constraint-kind half of a `SHOW <type filter> CONSTRAINTS` filter.
/// `None` (on `ConstraintTypeFilter::kind`) means every kind (bare `SHOW
/// CONSTRAINTS`/`SHOW ALL CONSTRAINTS`/`SHOW NODE CONSTRAINTS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKindFilter {
    /// `UNIQUE`/`UNIQUENESS`, optionally `PROPERTY`-prefixed (CIP-74) --
    /// matches `constraint_type == "unique"`.
    Unique,
    /// `KEY` -- matches `constraint_type == "key"`. Never combines with
    /// `PROPERTY`.
    Key,
    /// `TYPE`, optionally `PROPERTY`-prefixed -- matches `constraint_type
    /// == "propertyType"` (CIP-85/CIP-100).
    PropertyType,
    /// `EXIST`/`EXISTENCE` -- property existence plus the four
    /// label/endpoint-label kinds (CIP-187's filtering table: a bare
    /// `EXIST[ENCE]`/`NODE EXIST[ENCE]` matches `NODE_LABEL_EXISTENCE`
    /// too).
    Existence,
    /// `PROPERTY EXIST[ENCE]` (CIP-187) -- property existence only
    /// (`constraint_type == "exists"`), never the label/endpoint-label
    /// kinds the bare [`Self::Existence`] also admits. Its own variant —
    /// the grammar distinguishes the two spellings at parse time, so the
    /// AST carries the distinction typed, never as a side-channel bool
    /// beside the kind.
    PropertyExistence,
}

/// `SHOW <type filter> CONSTRAINTS` -- `entity` narrows by `entityType`,
/// `kind` narrows by constraint kind. `ConstraintTypeFilter::all()` (empty
/// entity, no kind) is the default for bare `SHOW CONSTRAINTS`/`SHOW ALL
/// CONSTRAINTS`, matching every constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstraintTypeFilter {
    pub entity: ConstraintEntityFilter,
    pub kind: Option<ConstraintKindFilter>,
}

impl ConstraintTypeFilter {
    pub fn all() -> Self {
        ConstraintTypeFilter {
            entity: ConstraintEntityFilter::Any,
            kind: None,
        }
    }
}

/// `SHOW DATABASE[S] [<filter>] [YIELD ...] [WHERE ...]` (CIP-42/CIP-68).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowDatabasesClause {
    pub filter: DatabaseFilter,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// Which database(s) a `SHOW DATABASE[S]` clause reports (CIP-68's
/// `DEFAULT`/`HOME` singular-only forms).
#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseFilter {
    /// Bare `SHOW DATABASE[S]` -- every database.
    All,
    /// `SHOW DEFAULT DATABASE` -- singular only, CIP-68 rejects the plural
    /// `SHOW DEFAULT DATABASES`.
    Default,
    /// `SHOW HOME DATABASE` -- singular only, same restriction as `Default`.
    Home,
    /// `SHOW DATABASE[S] <name>` -- one named database.
    Named(SchemaName),
}

/// `SHOW [ALL|POPULATED] ROLE[S] [WITH USER[S]] [YIELD ...] [WHERE ...]`
/// (CIP-68).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowRolesClause {
    pub modifier: RoleModifier,
    /// `WITH USER[S]` -- when true, the catalog carries one row per
    /// (role, member) pair (adding the `member` column) instead of one row
    /// per role.
    pub with_users: bool,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// The `[ALL|POPULATED]` modifier of `SHOW ROLE[S]` -- `Any` (bare `SHOW
/// ROLE[S]`) and `All` (`SHOW ALL ROLE[S]`) report the identical row set
/// (every role); `Populated` narrows to roles with at least one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleModifier {
    Any,
    All,
    Populated,
}

/// `SHOW SERVER[S] [YIELD ...] [WHERE ...]` (CIP-68).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowServersClause {
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// `SHOW USER[S] [YIELD ...] [WHERE ...]` / `SHOW CURRENT USER` (CIP-68).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowUsersClause {
    /// `SHOW CURRENT USER` -- singular only, CIP-68 rejects the plural
    /// `SHOW CURRENT USERS`.
    pub current_only: bool,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// `SHOW PROCEDURE[S] [YIELD ...] [WHERE ...]` (CIP-68 grammar
/// foundation).
#[derive(Debug, Clone, PartialEq)]
pub struct ShowProceduresClause {
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
    /// `EXECUTABLE [BY <user>]`, when written.
    pub executable: Option<ExecutableBy>,
}

/// `SHOW SETTING[S] [<names>] [YIELD ...] [WHERE ...]` (CIP-68 grammar
/// foundation). `names` is the optional expression naming the setting(s) to
/// filter to: a STRING or a LIST<STRING> (or a `null`/parameter resolving to
/// either). The executor selects from its own fixed settings catalog -- a
/// null selects nothing, an empty list means "no filter" (CIP-253), and any
/// other shape is a `TypeError`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowSettingsClause {
    pub names: Option<Expr>,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// `SHOW TRANSACTION[S] [<id-list>] [YIELD ...] [WHERE ...]` (CIP-68
/// grammar foundation). `ids` is the optional comma-separated list of
/// transaction-id expressions to filter to (empty = no filter) --
/// Relate has no live-transaction catalog, so it's parsed but never
/// changes the (always empty) result.
#[derive(Debug, Clone, PartialEq)]
pub struct ShowTransactionsClause {
    pub ids: Vec<Expr>,
    pub yields: YieldList,
    pub where_expr: Option<Expr>,
    pub stages: Vec<PageStage>,
}

/// `TERMINATE TRANSACTION[S] <id-list> [YIELD ...]` (CIP-68 grammar
/// foundation). Since `ShowTransactions` never reports a live transaction,
/// every requested id is reported as not found.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminateTransactionsClause {
    pub ids: Vec<Expr>,
    pub yields: YieldList,
}

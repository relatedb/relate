//! The `SEARCH` clause nodes.

use super::*;

/// `<search clause>`: the `SEARCH ... IN (<source specification
/// clause>) [SCORE AS ...]` subclause of `MatchClause`.
///
/// `binding_var`/`score_var` (and each nested `index_name`) are plain
/// identifiers (never a general expression, a parameter, or a property
/// access -- Rules 1, 4, 5) rather than `Expr`, matching the grammar's own
/// `<binding variable reference>` / `<index name specification>` /
/// `<binding variable>` productions.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchClause {
    /// `<binding variable reference>`: must name a variable declared by the
    /// same `<match statement>`'s own pattern.
    pub binding_var: SmallName,
    /// `<source specification clause>`: one index search, or a
    /// `COMBINE` of two or more weighted sources fused with WRRF.
    pub spec: SearchSpec,
    /// `[SCORE AS <binding variable>]` -- `None` when the score column is
    /// omitted (results are still ordered by descending score either way).
    pub score_var: Option<SmallName>,
}

/// `<source specification clause>`:
/// `<index specification clause> | <combined specification clause>`.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchSpec {
    Index(SearchIndexSpec),
    Combine(SearchCombineSpec),
}

/// `<index specification clause>`: `{ VECTOR | FULLTEXT } INDEX
/// <index name> <index for and control clause>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchIndexSpec {
    /// CIP-225: which index kind the specification names.
    pub kind: SearchKind,
    /// `<index name specification>`: the literal name of an existing index.
    pub index_name: SmallName,
    /// `<for clause>`'s `<value expression>` -- the search query.
    pub for_expr: Expr,
    /// `WITH ANALYZER <expr>` (fulltext only).
    pub analyzer: Option<Expr>,
    /// CIP-240 "Vector Search with Single-stage Filtering": the optional
    /// inner `WHERE <search condition>` between `FOR <expr>` and `LIMIT
    /// <expr>`. Its `<search condition>` must decompose into one or more
    /// `<binding variable reference> <static property reference> <simple
    /// comp op> <arithmetic value expression>` predicates joined by `AND`
    /// (or a form rewritable into that, e.g. `NOT x.prop` or flipped
    /// operands) -- enforced by `relate_front::search_filter`, not here;
    /// the parser accepts any general `<expression>` here, exactly like
    /// `MatchClause.where_expr`.
    pub inner_where: Option<Expr>,
    /// `<offset clause>` (`SKIP`/`OFFSET`).
    pub offset_expr: Option<Expr>,
    /// `<limit clause>`'s `<value expression>` (mandatory for an index
    /// source, in and out of a COMBINE).
    pub limit_expr: Expr,
}

/// `<index specification clause>`: `{ VECTOR | FULLTEXT } INDEX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Vector,
    Fulltext,
}

/// `<combined specification clause>`: `COMBINE ( <weighted source
/// clause>, ... ) USING WRRF [ WITH CONSTANT <expr> ] [ LIMIT <expr> ]`.
///
/// `sources` holds two or more weighted source clauses (fewer is a
/// parse error, raised where the list is parsed). A source may itself be a
/// nested `<combined specification clause>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCombineSpec {
    pub sources: Vec<SearchWeightedSource>,
    /// `<constant specification clause>`'s `<value expression>` -- the WRRF
    /// dampening constant; `None` means the default 60.
    pub constant_expr: Option<Expr>,
    /// The optional `<limit clause>` of the combined specification -- the n
    /// highest-scoring results kept; `None` keeps all.
    pub limit_expr: Option<Expr>,
}

/// `<weighted source clause>`:
/// `<source specification clause> [ WEIGHT <value expression> ]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchWeightedSource {
    pub spec: SearchSpec,
    /// `None` means the default weight 1.
    pub weight_expr: Option<Expr>,
}

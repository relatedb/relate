//! The `MATCH`-level `WHERE` conjuncts a routed shortest path reads,
//! split by what they read: the path itself, or only the endpoints.
//!
//! The pattern's own inline `WHERE` and the end node pattern's inline
//! `WHERE` (admitted by the guard when it names no path variable) must hold
//! **during** the search, and so must a clause-level conjunct that reads the
//! *path* (the named path variable or the relationship variable) of the
//! function spellings (`shortestPath()` / `allShortestPaths()`): live Neo4j
//! answers the shortest path that satisfies them ([`PathFilter`]).
//!
//! A clause-level conjunct that reads the path of a **keyword selector**
//! (`SHORTEST k`, `ANY SHORTEST`, `ALL SHORTEST`, `SHORTEST k GROUPS`) is
//! not a search filter: live Neo4j selects first and filters the selected
//! paths afterwards, so a selected path that violates it leaves no row
//! instead of promoting a longer one. Such a conjunct rides the residual
//! the caller applies to every emitted row, like an endpoint-only one.
//!
//! An endpoint-only conjunct is deliberately **not** a search filter: the
//! route answers one shortest path per `(start, end)` partition, so a
//! predicate on the endpoints selects whole partitions and post-filtering
//! it is exact — keeping those queries on the BFS search instead of the
//! enumerated predicate walk. It rides the [`PairFilter`], which decides
//! the one case post-filtering cannot: a same-node pair that would raise
//! 51N23 before the caller's `WHERE` could drop it. Live Neo4j plans an
//! endpoint-only conjunct ahead of the search, so
//! `shortestPath((a:L)-[*1..3]-(n)) WHERE n <> a` never meets the pair
//! `(a, a)`; the route emits one partition per reached node and the origin
//! is one of them, so it reads the same conjuncts at the one place that
//! could raise. The conjuncts stay in the residual too — the caller
//! applies them to every emitted row — so the gate only ever removes a
//! rejection, never a row.

use std::collections::HashSet;

use relate_core::{NodeId, Result};
use relate_ir::{Expr, NodePattern, PathPattern};

use crate::ctx::ExecCtx;
use crate::eval::expr_vars;
use crate::exec::gpm::gpm_bind_node;
use crate::exec::pushdown::{conjunct_ready, conjuncts_hold, residual_ok, Residual};
use crate::row::Row;

use super::guard::Shape;

/// The predicates a routed shortest path must satisfy during the search:
/// the MATCH-level `WHERE` conjuncts that read the path itself (function
/// spellings only; a keyword selector post-filters them), the
/// pattern's own inline `WHERE`, and the end node pattern's inline `WHERE`
/// (GPM checks that one at every arrival; the route answers it against
/// each bound trail, which binds the same names — the guard admits it only
/// when it reads no path variable, the one name no arrival row carries).
/// `scope` is the MATCH's bound-variable scope, the readiness rule's input
/// (see [`crate::exec::pushdown::conjunct_ready`]).
pub(super) struct PathFilter<'a> {
    /// The search's predicates in one list: the path-reading clause
    /// conjuncts (function spellings only), then the pattern's own inline
    /// `WHERE` and the end node's — they play one role (the search answers
    /// each per arrival), so no consumer reads them apart.
    predicates: Vec<&'a Expr>,
    scope: &'a HashSet<String>,
}

impl<'a> PathFilter<'a> {
    /// The predicates the search must decide.
    fn predicates(&self) -> impl Iterator<Item = &'a Expr> + '_ {
        self.predicates.iter().copied()
    }

    /// Whether every predicate is ready on `probe`, a row carrying every
    /// name the finished trail row will (see `Route::predicates_decidable`).
    pub(super) fn decidable_on(&self, probe: &Row) -> bool {
        self.predicates()
            .all(|predicate| conjunct_ready(predicate, probe, self.scope, None))
    }

    /// Whether one bound trail row satisfies every predicate. Each is
    /// decidable against a trail row (`decidable_on` established that before
    /// the search started).
    pub(super) fn holds(&self, ctx: ExecCtx<'_>, row: &Row) -> Result<bool> {
        // The one predicate policy (ADR-0458 §7): a list/path converts
        // (live: `ANY SHORTEST ... -[r WHERE r.ls]-> ...` accepts a
        // non-empty list), a non-boolean is the typed rejection live
        // raises (22N01). Readiness was fixed route-wide by
        // `decidable_on`, so the canonical conjunct loop evaluates the
        // list as it stands.
        conjuncts_hold(ctx.graph, row, &self.predicates, ctx.params)
    }
}

/// The same-node-pair gate of one routed `MATCH`: present when the
/// spelling's 51N23 policy rejects a same-node pair (see
/// [`RouteFilters::split`]), it owns the gate's one predicate list — the
/// endpoint-only conjuncts plus the end pattern's inline `WHERE`, which
/// every arrival satisfies so an origin failing it is not a reached end —
/// under the bound-name scope (the readiness rule's input).
pub(super) struct PairFilter<'a> {
    predicates: Vec<Expr>,
    scope: &'a HashSet<String>,
}

impl PairFilter<'_> {
    /// Whether the gate's predicates drop the pair `(start row, end)`: the
    /// end bound beside the start row, every predicate that reads only
    /// names that row binds is evaluated, and one that does not hold drops
    /// the pair. A predicate reading a name no row binds yet is the
    /// caller's later predicate pass, never a reason to skip the
    /// rejection. A pair whose end cannot bind emits no row (as
    /// `Route::bind_trail`), so there is nothing left to reject.
    pub(super) fn drops(
        &self,
        ctx: ExecCtx<'_>,
        end: &NodePattern,
        e: NodeId,
        srow: &Row,
    ) -> Result<bool> {
        let mut row = srow.clone();
        if !gpm_bind_node(ctx.graph, end, e, &mut row) {
            return Ok(true);
        }
        // The canonical residual pass: ready predicates on the row.
        let residual = Residual {
            conjuncts: &self.predicates,
            scope: self.scope,
        };
        Ok(!residual_ok(ctx.graph, &row, ctx.params, &residual)?)
    }
}

/// One routed `MATCH`'s residual conjuncts, partitioned once by whether
/// they read the path.
pub(super) struct RouteFilters<'a> {
    /// The endpoint-only partition of the residual, whatever the spelling —
    /// the slice the route hands the candidate funnel so a `WHERE`-borne
    /// `=` / `IN` on an endpoint seeks it like an inline property map does
    /// (ADR-0556). They stay in the residual, so the seek only narrows;
    /// every emitted row is still checked against them. `path` and `pair`
    /// are the spelling-gated filters.
    pub endpoint: Vec<Expr>,
    /// `Some` when a predicate filters the *search* itself.
    pub path: Option<PathFilter<'a>>,
    /// `Some` when the spelling's 51N23 policy rejects a same-node pair:
    /// the gate over the endpoint-only conjuncts plus the end pattern's
    /// inline `WHERE` — either can drop the pair before the rejection (an
    /// origin failing the end's own predicate is not a reached end).
    pub pair: Option<PairFilter<'a>>,
    /// The MATCH's bound-variable scope, the readiness rule's input: which
    /// names a conjunct reads count (see [`conjunct_ready`]).
    pub scope: &'a HashSet<String>,
}

impl<'p> RouteFilters<'p> {
    /// Split the residual by what each conjunct reads. `shape` carries the
    /// end node and the spelling: a keyword selector selects first, so its
    /// clause-level conjuncts all stay the residual's post-filter and only
    /// the inline predicates filter the search; it needs no pair filter
    /// either (it never rejects a same-node pair, 51N23). The function and
    /// expression spellings classify the conjuncts by whether they read
    /// the path.
    pub(super) fn split(
        path: &'p PathPattern,
        shape: &Shape<'p>,
        residual: &'p Residual<'_>,
    ) -> Self {
        let scope = residual.scope;
        let (path_reading, endpoint_only) = Self::classify(path, residual);
        // The endpoint-only conjuncts seek whatever the spelling: they
        // select whole partitions, so narrowing the candidates is exact
        // even where the clause conjuncts are a post-filter. The spelling
        // gates only the search predicates and the pair filter.
        let filters_search = shape.spelling.filters_search_by_clause_where();
        // The pair gate's predicates, one owned list: the endpoint-only
        // conjuncts plus the end pattern's inline `WHERE` — every arrival
        // satisfies the end's own predicate, so an origin that fails it is
        // not a reached end and the (s, s) pair never forms (the TCK's
        // `shortestPath((:N {level: 0})-[*]-(e:N WHERE e.level > 0))`).
        let pair_predicates: Vec<Expr> = endpoint_only
            .iter()
            .map(|expr| (*expr).clone())
            .chain(shape.end.where_clause.iter().cloned())
            .collect();
        let endpoint: Vec<Expr> = endpoint_only.into_iter().cloned().collect();
        // One list, one role (the search answers each per arrival): the
        // path-reading clause conjuncts of a spelling whose clause `WHERE`
        // filters the search (a keyword selector's ride the caller's
        // residual instead), then the pattern's own inline `WHERE` and
        // the end node's.
        let predicates: Vec<&Expr> = if filters_search {
            path_reading
        } else {
            Vec::new()
        }
        .into_iter()
        .chain(path.where_clause.as_deref())
        .chain(shape.end.where_clause.as_ref())
        .collect();
        Self {
            // The gate exists exactly for the spellings whose 51N23 policy
            // rejects a same-node pair — the one policy table decides both
            // the rejection and the gate that can remove it.
            pair: (shape.spelling.common_end_nodes() == crate::shortest::CommonEndNodes::Forbidden)
                .then_some(PairFilter {
                    predicates: pair_predicates,
                    scope,
                }),
            endpoint,
            path: (!predicates.is_empty()).then_some(PathFilter { predicates, scope }),
            scope,
        }
    }

    /// The residual's conjuncts partitioned into those that read the path
    /// (the named path variable or a relationship variable) and the
    /// endpoint-only rest.
    fn classify(path: &PathPattern, residual: &'p Residual<'_>) -> (Vec<&'p Expr>, Vec<&'p Expr>) {
        // Text render: the carrier set compares against the String variable
        // sets of the filter walk.
        // The IR's one enumeration of the pattern's relationship variables,
        // covering quantified and var-length spellings too.
        let mut carriers: HashSet<String> = path
            .relationship_variables()
            .map(|name| name.to_string())
            .collect();
        for name in path.path_names() {
            carriers.insert(name.to_string());
        }
        residual.conjuncts.iter().partition(|conjunct| {
            let mut vars = HashSet::new();
            expr_vars(conjunct, &mut vars);
            vars.iter().any(|name| carriers.contains(name.as_str()))
        })
    }
}

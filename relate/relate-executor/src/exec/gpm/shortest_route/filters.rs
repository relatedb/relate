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

use relate_core::{Error, NodeId, Result};
use relate_ir::{Expr, NodePattern, PathPattern};

use crate::ctx::ExecCtx;
use crate::eval::expr_vars;
use crate::exec::gpm::gpm_bind_node;
use crate::exec::pushdown::{conjunct_ready, residual_ok, Residual};
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
            .all(|predicate| conjunct_ready(predicate, probe, self.scope))
    }

    /// Whether one bound trail row satisfies every predicate. Each is
    /// decidable against a trail row (`decidable_on` established that before
    /// the search started), so one that is not ready here is a broken
    /// invariant, reported as a typed internal error rather than silently
    /// counted as satisfied.
    pub(super) fn holds(&self, ctx: ExecCtx<'_>, row: &Row) -> Result<bool> {
        for predicate in self.predicates() {
            if !conjunct_ready(predicate, row, self.scope) {
                return Err(Error::internal(
                    "shortest-path route: predicate not decidable against a bound trail",
                ));
            }
            // The one predicate policy (ADR-0458 §7): a list/path converts
            // (live: `ANY SHORTEST ... -[r WHERE r.ls]-> ...` accepts a
            // non-empty list), a non-boolean is the typed rejection live
            // raises (22N01).
            if !crate::eval::predicate_holds(ctx.graph, row, predicate, ctx.params)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The endpoint-only conjuncts of one routed `MATCH`, and its bound-name
/// scope (the readiness rule's input).
pub(super) struct PairFilter<'a> {
    conjuncts: Vec<Expr>,
    scope: &'a HashSet<String>,
}

impl PairFilter<'_> {
    /// Whether the conjuncts drop the pair `(start row, end)`: the end
    /// bound beside the start row, every conjunct that reads only names
    /// that row binds is evaluated, and one that does not hold drops the
    /// pair. A conjunct reading a name no row binds yet is the caller's
    /// later predicate pass, never a reason to skip the rejection. A pair
    /// whose end cannot bind emits no row (as `Route::bind_trail`), so
    /// there is nothing left to reject.
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
        // The canonical residual pass: ready conjuncts evaluated on the row.
        let residual = Residual {
            conjuncts: &self.conjuncts,
            scope: self.scope,
        };
        Ok(!residual_ok(ctx.graph, &row, ctx.params, &residual)?)
    }
}

/// One routed `MATCH`'s residual conjuncts, partitioned once by whether
/// they read the path.
pub(super) struct RouteFilters<'a> {
    /// `Some` when a predicate filters the *search* itself.
    pub path: Option<PathFilter<'a>>,
    /// `Some` when an endpoint-only conjunct exists to drop a same-node pair.
    pub pair: Option<PairFilter<'a>>,
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
        let (path_reading, endpoint_only) = if shape.spelling.filters_search_by_clause_where() {
            Self::classify(path, residual)
        } else {
            (Vec::new(), Vec::new())
        };
        // The two inline predicates — the pattern's own `WHERE` and the
        // end node's — are one role in the filter, so one list.
        let mut predicates = path_reading;
        predicates.extend(path.where_clause.as_deref());
        predicates.extend(shape.end.where_clause.as_ref());
        Self {
            path: (!predicates.is_empty()).then_some(PathFilter { predicates, scope }),
            pair: (!endpoint_only.is_empty()).then_some(PairFilter {
                conjuncts: endpoint_only.into_iter().cloned().collect(),
                scope,
            }),
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

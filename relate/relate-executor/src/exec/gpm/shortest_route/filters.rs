//! The `MATCH`-level `WHERE` conjuncts a routed shortest path reads,
//! split by what they read: the path itself, or only the endpoints.
//!
//! A conjunct that reads the *path* (the named path variable or the
//! relationship variable), the pattern's own inline `WHERE`, and the end
//! node pattern's inline `WHERE` (admitted by the guard when it names no
//! path variable) must hold **during** the search: a route that only
//! filtered the selected shortest path afterwards answered zero rows
//! whenever that path violated such a predicate, instead of the shortest
//! path that satisfies it ([`PathFilter`]).
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

/// The predicates a routed shortest path must satisfy during the search:
/// the MATCH-level `WHERE` conjuncts that read the path itself, the
/// pattern's own inline `WHERE`, and the end node pattern's inline `WHERE`
/// (GPM checks that one at every arrival; the route answers it against
/// each bound trail, which binds the same names — the guard admits it only
/// when it reads no path variable, the one name no arrival row carries).
/// `scope` is the MATCH's bound-variable scope, the readiness rule's input
/// (see [`crate::exec::pushdown::conjunct_ready`]).
pub(super) struct PathFilter<'a> {
    conjuncts: Vec<&'a Expr>,
    /// The pattern's own inline `WHERE` and the end node's, held as one
    /// list — they play one role (the search answers both per arrival),
    /// so no consumer ever reads them apart.
    inline: Vec<&'a Expr>,
    scope: &'a HashSet<String>,
}

impl<'a> PathFilter<'a> {
    /// Whether a clause-level `WHERE` conjunct reads the path (as opposed
    /// to only the pattern's inline `WHERE` filtering the search).
    pub(super) fn has_clause_conjuncts(&self) -> bool {
        !self.conjuncts.is_empty()
    }

    /// The union the search must decide: the path-reading clause conjuncts,
    /// then the inline predicates — the pattern's `WHERE` and the end
    /// node's, one list chained once.
    fn predicates(&self) -> impl Iterator<Item = &'a Expr> + '_ {
        self.conjuncts
            .iter()
            .copied()
            .chain(self.inline.iter().copied())
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
    pub(super) fn split(
        path: &'p PathPattern,
        end: &'p NodePattern,
        residual: &'p Residual<'_>,
    ) -> Self {
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
        let (conjuncts, endpoint_only): (Vec<&Expr>, Vec<&Expr>) =
            residual.conjuncts.iter().partition(|conjunct| {
                let mut vars = HashSet::new();
                expr_vars(conjunct, &mut vars);
                vars.iter().any(|name| carriers.contains(name.as_str()))
            });
        let scope = residual.scope;
        // The two inline predicates — the pattern's own `WHERE` and the
        // end node's — are one role in the filter, so one list.
        let mut inline: Vec<&Expr> = Vec::new();
        if let Some(where_clause) = path.where_clause.as_deref() {
            inline.push(where_clause);
        }
        if let Some(where_clause) = end.where_clause.as_ref() {
            inline.push(where_clause);
        }
        let path_filter = (!conjuncts.is_empty() || !inline.is_empty()).then_some(PathFilter {
            conjuncts,
            inline,
            scope,
        });
        Self {
            path: path_filter,
            pair: (!endpoint_only.is_empty()).then_some(PairFilter {
                conjuncts: endpoint_only.into_iter().cloned().collect(),
                scope,
            }),
        }
    }
}

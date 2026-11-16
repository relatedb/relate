//! A non-seekable endpoint side, listed by its own `WHERE` (the sibling of
//! the point seek in [`super::anchor`]).
//!
//! An endpoint whose `WHERE` conjunct no index serves (`b.id + 0 = 7`, a
//! function over the property) is scan territory: the fan reaches every
//! node within the bound and builds a row and a path for each before the
//! residual drops it. The same predicate in a separate `MATCH` binds the
//! endpoint first, from a scan of its label, and runs the pair search — one
//! to two orders of magnitude less work when few nodes match.
//!
//! This module gives the route that second plan. The side's label
//! candidates are filtered by the endpoint-only conjuncts that read nothing
//! but the side's own variable, stopping at the first match past the pair
//! bound: at most [`SEEK_PAIR_LIMIT`] matches make a listed side the pair
//! product reads, more decline to the fan unchanged. Only the candidate set
//! narrows — the conjuncts stay in the residual, which still checks every
//! emitted row, so a node dropped here is exactly a node the residual would
//! have dropped after its partition was searched.
//!
//! The filter is exact only for a conjunct that decides the same way on
//! every evaluation, so it takes deterministic conjuncts only, and a
//! conjunct that raises on a candidate declines the whole side: the fan
//! evaluates conjuncts on the nodes it reaches, in its own order, and keeps
//! owning that error behavior. A side whose pattern or conjuncts read the
//! input row is not row-independent and is not filtered; one that is
//! (the common case) is filtered once per clause, whatever its row count.

use std::cell::RefCell;

use relate_core::{NodeId, Result};
use relate_ir::{Expr, NodePattern};

use crate::eval::{expr_vars, is_nondeterministic};
use crate::exec::gpm::gpm_bind_node;
use crate::exec::pushdown::conjuncts_hold;
use crate::pattern_walk::{node_candidates, Binding, NodeBinding};

use super::anchor::{Side, SEEK_PAIR_LIMIT};
use super::route::Route;

/// Which endpoint of the pattern a side is: the cache keeps one answer per
/// side, since the two patterns differ.
#[derive(Clone, Copy)]
pub(super) enum End {
    Start,
    End,
}

/// What the cache knows of one side.
enum Slot {
    /// Not asked yet.
    Unknown,
    /// Asked: the filter declined (nothing to filter by, too many matches,
    /// a conjunct raised) and the side stays unlisted.
    Declined,
    /// Asked: the side's matches, at most the pair bound.
    Listed(Vec<NodeId>),
}

/// The filtered sides of one clause's pattern, shared by its input rows:
/// a side that does not read the row filters once, not once per row. Lives
/// in the pattern's tier state beside the reach cache (so it never spans a
/// write).
pub(crate) struct EndFilterCache {
    slots: RefCell<[Slot; 2]>,
}

impl Default for EndFilterCache {
    fn default() -> Self {
        Self {
            slots: RefCell::new([Slot::Unknown, Slot::Unknown]),
        }
    }
}

impl EndFilterCache {
    fn index(end: End) -> usize {
        match end {
            End::Start => 0,
            End::End => 1,
        }
    }

    fn answer(&self, end: End) -> Option<Option<Side>> {
        match &self.slots.borrow()[Self::index(end)] {
            Slot::Unknown => None,
            Slot::Declined => Some(None),
            Slot::Listed(ids) => Some(Some(Side::Many(ids.clone()))),
        }
    }

    fn record(&self, end: End, ids: Option<Vec<NodeId>>) {
        self.slots.borrow_mut()[Self::index(end)] = match ids {
            Some(ids) => Slot::Listed(ids),
            None => Slot::Declined,
        };
    }
}

impl Route<'_> {
    /// `np`'s matches among the nodes its pattern admits, filtered by the
    /// endpoint-only conjuncts over its variable, as a listed side — or
    /// `None` (the side stays unlisted, the fan handles it) when there is
    /// nothing to filter by, more than [`SEEK_PAIR_LIMIT`] nodes match, a
    /// conjunct raises, or the side reads the input row.
    pub(super) fn filtered_side(
        &self,
        np: &NodePattern,
        class: NodeBinding<'_>,
        end: End,
    ) -> Result<Option<Side>> {
        let Binding::Unbound(v) = class else {
            return Ok(None);
        };
        // A pattern property map or inline `WHERE` may read the input row:
        // not one answer for every row.
        if np.properties.is_some() || np.where_clause.is_some() {
            return Ok(None);
        }
        let name = v.to_string();
        let usable: Vec<&Expr> = self
            .filters
            .endpoint
            .iter()
            .filter(|conjunct| self.reads_only(conjunct, &name))
            .collect();
        if usable.is_empty() {
            return Ok(None);
        }
        let cache = &self.reach.ends;
        if let Some(answer) = cache.answer(end) {
            return Ok(answer);
        }
        let listed = self.scan_matches(np, &usable)?;
        cache.record(end, listed.clone());
        Ok(listed.map(Side::Many))
    }

    /// Whether `conjunct` is a filter over `name` alone: it reads that
    /// variable and no other bound name, calls nothing nondeterministic and
    /// holds no subquery (whose interior reads its own scope).
    fn reads_only(&self, conjunct: &Expr, name: &str) -> bool {
        let mut vars = std::collections::HashSet::new();
        expr_vars(conjunct, &mut vars);
        vars.retain(|var| self.filters.scope.contains(var));
        vars.len() == 1
            && vars.contains(name)
            && !conjunct.any(&mut |expr| {
                matches!(expr, Expr::Function { name, .. } if name.with_str(is_nondeterministic))
                    || expr.is_scope_bearing()
            })
    }

    /// The candidates of `np` that hold every `usable` conjunct, ascending,
    /// or `None` past the pair bound or on a raising conjunct.
    fn scan_matches(&self, np: &NodePattern, usable: &[&Expr]) -> Result<Option<Vec<NodeId>>> {
        let graph = self.ctx.graph;
        let params = self.ctx.params;
        let candidates = node_candidates(graph, np, self.row, params, &[])?;
        let mut matches = Vec::new();
        for id in candidates {
            graph.cancel_tick()?;
            if !self.node_ok(id, np, self.row)? {
                continue;
            }
            let mut row = self.row.clone();
            if !gpm_bind_node(graph, np, id, &mut row) {
                continue;
            }
            match conjuncts_hold(graph, &row, usable, params) {
                Ok(true) => {
                    if matches.len() == SEEK_PAIR_LIMIT {
                        return Ok(None);
                    }
                    matches.push(id);
                }
                Ok(false) => {}
                // The fan owns the error behavior of the nodes it reaches.
                // A persistent condition (cancellation, a tripped budget)
                // raises again at the fan's own next tick, so declining
                // never hides one.
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(matches))
    }
}

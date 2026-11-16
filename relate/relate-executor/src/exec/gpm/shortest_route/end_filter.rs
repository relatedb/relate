//! A non-seekable endpoint side, listed by its own `WHERE` (the sibling of
//! the point seek in [`super::anchor`]).
//!
//! An endpoint whose `WHERE` conjunct no point seek serves (`b.id + 0 = 7`,
//! a function over the property) is scan territory: the fan reaches every
//! node within the bound and builds a row and a path for each before the
//! residual drops it. The same predicate in a separate `MATCH` binds the
//! endpoint first, from a scan of its label, and runs the pair search — one
//! to two orders of magnitude less work when few nodes match.
//!
//! This module gives the route that second plan. The side's candidates (a
//! served range seek narrows them, else the label scan) are filtered by the
//! endpoint-only conjuncts that read nothing but the side's own variable,
//! stopping at the first match past the pair bound: at most
//! [`SEEK_PAIR_LIMIT`] matches make a listed side the pair product reads,
//! more decline to the fan unchanged. Only the candidate set narrows — the
//! conjuncts stay in the residual, which still checks every emitted row, so
//! a node dropped here is exactly a node the residual would have dropped
//! after its partition was searched.
//!
//! The filter is exact only for a conjunct that decides the same way on
//! every evaluation, so it takes row-stable conjuncts only, and a conjunct
//! that raises on a candidate declines the whole side: the fan evaluates
//! conjuncts on the nodes it reaches, in its own order, and keeps owning
//! that error behavior. A side whose pattern or conjuncts read the input
//! row is not row-independent and is not filtered; one that is (the common
//! case) is filtered once per clause, whatever its row count.

use std::cell::RefCell;
use std::collections::HashSet;

use relate_core::{NodeId, Result};
use relate_ir::{Expr, NodePattern};

use crate::eval::{expr_vars, is_row_unstable_node};
use crate::exec::pushdown::conjuncts_hold;
use crate::pattern_walk::{node_candidates, Binding, NodeBinding};

use super::anchor::{Side, SEEK_PAIR_LIMIT};
use super::route::Route;

/// What one side's cell knows.
enum Slot {
    /// Not asked yet.
    Unknown,
    /// Asked: the side stays unlisted (nothing to filter by, too many
    /// matches, a conjunct raised) and the fan handles it.
    Unlisted,
    /// Asked: the side's matches, at most the pair bound.
    Listed(Vec<NodeId>),
}

/// One endpoint's filtered listing, computed at most once per clause: a
/// side that does not read the row filters once, not once per row.
pub(crate) struct SideCell(RefCell<Slot>);

impl Default for SideCell {
    fn default() -> Self {
        Self(RefCell::new(Slot::Unknown))
    }
}

impl SideCell {
    /// The cell's listing: the stored answer, or `scan`'s, stored. `None`
    /// is "unlisted".
    fn listed(
        &self,
        scan: impl FnOnce() -> Result<Option<Vec<NodeId>>>,
    ) -> Result<Option<Vec<NodeId>>> {
        match &*self.0.borrow() {
            Slot::Unknown => {}
            Slot::Unlisted => return Ok(None),
            Slot::Listed(ids) => return Ok(Some(ids.clone())),
        }
        let ids = scan()?;
        *self.0.borrow_mut() = match &ids {
            Some(ids) => Slot::Listed(ids.clone()),
            None => Slot::Unlisted,
        };
        Ok(ids)
    }
}

/// The filtered sides of one clause's pattern, shared by its input rows.
/// Lives in the pattern's tier state beside the reach cache (so it never
/// spans a write).
#[derive(Default)]
pub(crate) struct EndFilterCache {
    pub(super) start: SideCell,
    pub(super) end: SideCell,
}

impl Route<'_> {
    /// `np`'s matches among the nodes its pattern admits, filtered by the
    /// endpoint-only conjuncts over its variable, as a listed side — or
    /// `None` (the side stays unlisted, the fan handles it) when there is
    /// nothing to filter by, more than [`SEEK_PAIR_LIMIT`] nodes match, a
    /// conjunct raises, or the side reads the input row. `cell` is the
    /// side's own cache cell.
    pub(super) fn filtered_side(
        &self,
        np: &NodePattern,
        class: NodeBinding<'_>,
        cell: &SideCell,
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
        Ok(cell
            .listed(|| self.scan_matches(np, &usable))?
            .map(Side::Many))
    }

    /// Whether `conjunct` is a filter over `name` alone: it reads that
    /// variable and no other bound name, and is row-stable (no
    /// nondeterministic call, no subquery whose interior reads its own
    /// scope).
    fn reads_only(&self, conjunct: &Expr, name: &str) -> bool {
        let mut vars = HashSet::new();
        expr_vars(conjunct, &mut vars);
        vars.retain(|var| self.filters.scope.contains(var));
        vars.len() == 1 && vars.contains(name) && !conjunct.any(&mut is_row_unstable_node)
    }

    /// The candidates of `np` that hold every `usable` conjunct, ascending,
    /// or `None` past the pair bound or on a raising conjunct.
    fn scan_matches(&self, np: &NodePattern, usable: &[&Expr]) -> Result<Option<Vec<NodeId>>> {
        let graph = self.ctx.graph;
        let params = self.ctx.params;
        // The route's endpoint-only conjuncts ride the funnel as for every
        // other side, so a served range seek narrows the label scan.
        let candidates = node_candidates(graph, np, self.row, params, &self.filters.endpoint)?;
        let mut matches = Vec::new();
        for id in candidates {
            graph.cancel_tick()?;
            let Some(row) = self.bind_candidate(id, np)? else {
                continue;
            };
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

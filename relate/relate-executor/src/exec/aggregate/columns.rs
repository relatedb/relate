//! The groups' accumulator state, flat: one column per aggregate site (and
//! per shared percentile pool), one slot per group in each column.
//!
//! A group's state is a slot in each column, not a separately allocated
//! per-group vector of slots: what every group shares about a site (its
//! call, whether only an `ORDER BY` key or HAVING reads it, its seen-set
//! template) lives once in the column, and a group pays only the mutable
//! state. `count(*)` is eight bytes a group; a streaming fold is one
//! [`StreamFold`] and the bytes it holds; a retaining site (`collect`, a
//! DISTINCT percentile) is one `Vec<Value>`; a pooled percentile is one
//! [`PoolState`], shared by every call over its operand. Groups are
//! appended in landing order and never removed, so a group's index is its
//! slot in every column.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::Expr;

use crate::distinct_set::DistinctSet;
use crate::eval::{eval, StreamFold};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;
use crate::workspace::buffer_growth;

use super::eval::FoldedGroup;
use super::pool::{PoolSite, PoolState, SortedPool};
use super::stream::{finish_pooled, finish_retained, Call};

/// Groups whose lenient site failed to fold: the error is parked here and
/// surfaces when the site is read. Sparse — empty unless an `ORDER BY` key
/// or HAVING operand failed (see [`GroupSubclauses::having`] for the read's
/// outcome, ADR-0458 §7).
///
/// [`GroupSubclauses::having`]: super::group::GroupSubclauses
type Failed = BTreeMap<usize, Error>;

/// What one parked failure retains: its map entry, the error value and its
/// message. Billed once per site, so K sites parking one operand's error
/// pay K times.
fn parked_error_bytes(error: &Error) -> u64 {
    (size_of::<(usize, Error)>() as u64 + error.to_string().len() as u64)
        .saturating_add(BTREE_ENTRY_OVERHEAD)
}

/// Per-entry bookkeeping of a `BTreeMap` node, a conservative flat figure.
const BTREE_ENTRY_OVERHEAD: u64 = 16;

/// Whether an operand is admitted for `group`: true when there is no
/// seen-set (the call is not DISTINCT) or the value is first seen there.
fn admits(seen: &mut Option<Vec<DistinctSet>>, group: usize, value: &Value) -> Result<bool> {
    seen.as_mut()
        .map_or(Ok(true), |seen| seen[group].insert_value(value))
}

/// The one strict-raises/lenient-parks rule of an offer: a failure on a
/// lenient site (or a pool only lenient sites read) is handed back to be
/// parked, any other failure fails the statement.
fn parked(lenient: bool, offered: Result<()>) -> Result<Option<Error>> {
    match offered {
        Ok(()) => Ok(None),
        Err(error) if lenient => Ok(Some(error)),
        Err(error) => Err(error),
    }
}

/// One aggregate site's state across every group.
pub(super) enum SiteState<'e> {
    CountStar(Vec<i64>),
    /// A non-retaining aggregate: constant-size state per group,
    /// independent of the number of rows offered.
    Stream {
        call: Call<'e>,
        /// The fold a new group starts from.
        template: StreamFold,
        folds: Vec<StreamFold>,
        /// Per group, the peak payload bytes of the one value a `min`/`max`
        /// keeps, already charged.
        held: Vec<u64>,
        /// A DISTINCT call's per-group seen-set (`Some` for such a site):
        /// an operand folds only when first seen and is dropped at once.
        seen: Option<Vec<DistinctSet>>,
        failed: Failed,
    },
    /// `collect` and the DISTINCT percentiles: the operands are the state,
    /// charged to the aggregation's running total as they settle.
    Retain {
        call: Call<'e>,
        vals: Vec<Vec<Value>>,
        /// Per group, how many of `vals` are already charged.
        charged: Vec<usize>,
        /// A DISTINCT call's per-group seen-set: only first-seen operands
        /// reach `vals`.
        seen: Option<Vec<DistinctSet>>,
        failed: Failed,
    },
    /// A plain percentile call: its operands live in the shared pool
    /// column at this index, not here.
    Pooled {
        pool: usize,
        call: Call<'e>,
        /// The call's percentile argument, resolved when it was pooled.
        percentile: &'e Expr,
        failed: Failed,
    },
    /// An arity violation (a backtick-escaped `` `count`() `` bypasses the
    /// parser and front validation): raised when a group resolves, as the
    /// row-retaining fold did, so an empty keyed input still yields no
    /// error.
    Rejected(Error),
}

/// One aggregate call site: the call's identity (the evaluator asks for its
/// value by this reference), whether only an `ORDER BY` key or HAVING reads
/// it (a fold error parks instead of failing the statement), and its state.
pub(super) struct SiteColumn<'e> {
    pub(super) expr: &'e Expr,
    pub(super) lenient: bool,
    pub(super) state: SiteState<'e>,
}

struct PoolColumn<'e> {
    site: PoolSite<'e>,
    states: Vec<PoolState>,
}

/// Every group's accumulators.
pub(crate) struct AccTable<'e> {
    groups: usize,
    pools: Vec<PoolColumn<'e>>,
    sites: Vec<SiteColumn<'e>>,
    /// Bytes of parked lenient failures not yet charged; [`Self::settle`]
    /// bills them to the running total.
    parked_bytes: u64,
}

impl<'e> SiteColumn<'e> {
    /// The same site with no groups.
    fn empty_like(&self) -> Self {
        let state = match &self.state {
            SiteState::CountStar(_) => SiteState::CountStar(Vec::new()),
            SiteState::Stream {
                call,
                template,
                seen,
                ..
            } => SiteState::Stream {
                call: *call,
                template: template.clone(),
                folds: Vec::new(),
                held: Vec::new(),
                seen: seen.as_ref().map(|_| Vec::new()),
                failed: Failed::new(),
            },
            SiteState::Retain { call, seen, .. } => SiteState::Retain {
                call: *call,
                vals: Vec::new(),
                charged: Vec::new(),
                seen: seen.as_ref().map(|_| Vec::new()),
                failed: Failed::new(),
            },
            SiteState::Pooled {
                pool,
                call,
                percentile,
                ..
            } => SiteState::Pooled {
                pool: *pool,
                call: *call,
                percentile,
                failed: Failed::new(),
            },
            SiteState::Rejected(error) => SiteState::Rejected(error.clone()),
        };
        Self {
            expr: self.expr,
            lenient: self.lenient,
            state,
        }
    }

    /// The call's percentile argument, which the finish reads from the
    /// group's seed.
    pub(super) fn percentile(&self) -> Option<&'e Expr> {
        match &self.state {
            SiteState::Retain { call, .. } => call.percentile,
            SiteState::Pooled { percentile, .. } => Some(percentile),
            _ => None,
        }
    }

    /// Per-group bytes of this site's column slot.
    fn slot_bytes(&self) -> u64 {
        let distinct = |seen: &Option<Vec<DistinctSet>>| {
            seen.as_ref().map_or(0, |_| size_of::<DistinctSet>() as u64)
        };
        match &self.state {
            SiteState::CountStar(_) => size_of::<i64>() as u64,
            SiteState::Stream { seen, .. } => {
                (size_of::<StreamFold>() + size_of::<u64>()) as u64 + distinct(seen)
            }
            SiteState::Retain { seen, .. } => {
                (size_of::<Vec<Value>>() + size_of::<usize>()) as u64 + distinct(seen)
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => 0,
        }
    }

    fn open_group(&mut self) {
        match &mut self.state {
            SiteState::CountStar(counts) => counts.push(0),
            SiteState::Stream {
                template,
                folds,
                held,
                seen,
                ..
            } => {
                folds.push(template.clone());
                held.push(0);
                if let Some(seen) = seen {
                    seen.push(DistinctSet::default());
                }
            }
            SiteState::Retain {
                vals,
                charged,
                seen,
                ..
            } => {
                vals.push(Vec::new());
                charged.push(0);
                if let Some(seen) = seen {
                    seen.push(DistinctSet::default());
                }
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
    }

    /// Offer one input row to group `group`'s slot.
    fn offer(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        match &mut self.state {
            SiteState::CountStar(counts) => counts[group] += 1,
            SiteState::Stream {
                call,
                folds,
                seen,
                failed,
                ..
            } => {
                if failed.contains_key(&group) {
                    return Ok(());
                }
                graph.cancel_tick()?;
                let value = eval(graph, row, call.operand, params)?;
                if admits(seen, group, &value)? {
                    call.name.with_str(|name| folds[group].offer(value, name))?;
                }
            }
            SiteState::Retain {
                call,
                vals,
                seen,
                failed,
                ..
            } => {
                if failed.contains_key(&group) {
                    return Ok(());
                }
                graph.cancel_tick()?;
                let value = eval(graph, row, call.operand, params)?;
                if admits(seen, group, &value)? {
                    vals[group].push(value);
                }
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        Ok(())
    }

    /// A lenient site's operand failed for `group`: park the error and drop
    /// what the group's slot retained. The failure surfaces when the site
    /// is read. Returns the bytes the parked error retains, for the caller
    /// to bill.
    fn park_failure(&mut self, group: usize, error: Error) -> u64 {
        let bytes = parked_error_bytes(&error);
        match &mut self.state {
            SiteState::Stream { failed, .. } | SiteState::Pooled { failed, .. } => {
                failed.insert(group, error);
            }
            SiteState::Retain {
                vals,
                charged,
                failed,
                ..
            } => {
                vals[group] = Vec::new();
                charged[group] = 0;
                failed.insert(group, error);
            }
            SiteState::CountStar(_) | SiteState::Rejected(_) => return 0,
        }
        bytes
    }

    fn settle(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        match &mut self.state {
            SiteState::Retain {
                vals,
                charged,
                failed,
                ..
            } => {
                if failed.contains_key(&group) {
                    return Ok(());
                }
                for value in &vals[group][charged[group]..] {
                    let slot_bytes = buffer_growth(
                        charged[group],
                        charged[group] + 1,
                        size_of::<Value>() as u64,
                    );
                    total.add_bytes(graph, slot_bytes)?;
                    total.add_owned(graph, value)?;
                    charged[group] += 1;
                }
            }
            SiteState::Stream { folds, held, .. } => {
                if let Some(best) = folds[group].held_value() {
                    let now = crate::workspace::owned_heap(best);
                    if now > held[group] {
                        total.add_bytes(graph, now - held[group])?;
                        held[group] = now;
                    }
                }
            }
            SiteState::CountStar(_) | SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        Ok(())
    }

    /// Resolve this site for `group`, taking the group's state out.
    fn finish(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        pools: &[SortedPool],
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value> {
        match &mut self.state {
            SiteState::CountStar(counts) => Ok(Value::Int(counts[group])),
            SiteState::Stream { folds, failed, .. } => match failed.get(&group) {
                Some(error) => Err(error.clone()),
                None => std::mem::replace(&mut folds[group], StreamFold::Count(0)).finish(),
            },
            SiteState::Retain {
                call, vals, failed, ..
            } => match failed.get(&group) {
                Some(error) => Err(error.clone()),
                None => {
                    finish_retained(graph, std::mem::take(&mut vals[group]), *call, seed, params)
                }
            },
            SiteState::Pooled {
                pool,
                call,
                percentile,
                failed,
            } => match failed.get(&group) {
                Some(error) => Err(error.clone()),
                None => finish_pooled(graph, &pools[*pool], *call, percentile, seed, params),
            },
            SiteState::Rejected(error) => Err(error.clone()),
        }
    }
}

impl<'e> AccTable<'e> {
    pub(super) fn new(sites: Vec<SiteColumn<'e>>, pools: Vec<PoolSite<'e>>) -> Self {
        Self {
            groups: 0,
            parked_bytes: 0,
            pools: pools
                .into_iter()
                .map(|site| PoolColumn {
                    site,
                    states: Vec::new(),
                })
                .collect(),
            sites,
        }
    }

    /// The same sites and pools with no groups.
    pub(super) fn empty_like(&self) -> Self {
        Self {
            groups: 0,
            parked_bytes: 0,
            pools: self
                .pools
                .iter()
                .map(|pool| PoolColumn {
                    site: pool.site,
                    states: Vec::new(),
                })
                .collect(),
            sites: self.sites.iter().map(SiteColumn::empty_like).collect(),
        }
    }

    pub(super) fn sites(&self) -> &[SiteColumn<'e>] {
        &self.sites
    }

    /// The bytes the next group's slots add: each column's buffer growth
    /// when its length moves by one (the same reservation model every
    /// retained container is charged by). Read before [`Self::open_group`].
    pub(super) fn landing_bytes(&self) -> u64 {
        let n = self.groups;
        self.pools
            .iter()
            .map(|_| size_of::<PoolState>() as u64)
            .chain(self.sites.iter().map(SiteColumn::slot_bytes))
            .filter(|stride| *stride > 0)
            .map(|stride| buffer_growth(n, n + 1, stride))
            .fold(0, u64::saturating_add)
    }

    /// Append a fresh group's slot to every column; returns its index.
    pub(super) fn open_group(&mut self) -> usize {
        for pool in &mut self.pools {
            pool.states.push(PoolState::default());
        }
        for site in &mut self.sites {
            site.open_group();
        }
        self.groups += 1;
        self.groups - 1
    }

    /// Offer one input row to every site of group `group`. A streaming site
    /// evaluates its operand and folds it into its constant-size state; a
    /// retaining site keeps the value. What a site kept is charged by
    /// [`Self::settle`], once the spent row has let go of its share.
    pub(super) fn offer(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        for index in 0..self.pools.len() {
            let pool = &mut self.pools[index];
            let state = &mut pool.states[group];
            let offered = state.offer(&pool.site, graph, row, params);
            if let Some(error) = parked(pool.site.lenient, offered)? {
                // The pool's sites are all lenient: park the failure on
                // each and stop collecting.
                state.stop();
                for site in &mut self.sites {
                    if matches!(site.state, SiteState::Pooled { pool, .. } if pool == index) {
                        self.parked_bytes = self
                            .parked_bytes
                            .saturating_add(site.park_failure(group, error.clone()));
                    }
                }
            }
        }
        for site in &mut self.sites {
            let offered = site.offer(group, graph, row, params);
            if let Some(error) = parked(site.lenient, offered)? {
                self.parked_bytes = self
                    .parked_bytes
                    .saturating_add(site.park_failure(group, error));
            }
        }
        Ok(())
    }

    /// Charge what group `group`'s sites kept since the last settle to the
    /// aggregation's one running total: a retaining site's new operand slots
    /// and payloads, a pool's buffer growth, and the growth of the payload
    /// a `min`/`max` holds. The caller settles after the spent input row is
    /// dropped, so a value cloned from that row is the sole owner of its
    /// payload by now and is charged in full (a payload still shared with
    /// the store or a parameter adds nothing, as everywhere else). One total
    /// for every group: the groups' retained state is alive together, so it
    /// is bounded together.
    pub(super) fn settle(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        total.add_bytes(graph, std::mem::take(&mut self.parked_bytes))?;
        for pool in &mut self.pools {
            pool.states[group].settle(graph, total)?;
        }
        for site in &mut self.sites {
            site.settle(group, graph, total)?;
        }
        Ok(())
    }

    /// Resolve every site of group `group`, taking its state out. `seed` is
    /// the group's own first row — a percentile's second argument is read
    /// from it, and an empty group has no percentile to report.
    pub(super) fn finish_group(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
    ) -> FoldedGroup<'e> {
        let pools: Vec<SortedPool> = self
            .pools
            .iter_mut()
            .map(|pool| std::mem::take(&mut pool.states[group]).finish())
            .collect();
        FoldedGroup::new(
            self.sites
                .iter_mut()
                .map(|site| {
                    let value = site.finish(group, graph, &pools, seed, params);
                    (site.expr, value)
                })
                .collect(),
        )
    }
}

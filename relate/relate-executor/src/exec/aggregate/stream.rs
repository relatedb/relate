//! The grouped aggregate's streaming state: what a group keeps instead of
//! its input rows.
//!
//! [`discover`] finds every aggregate call an aggregating
//! projection can evaluate (items, `ORDER BY` keys, HAVING) by running the
//! one recursive evaluator in its discovery mode, so the sites are exactly
//! the calls the evaluator reaches. Each site gets one column of per-group
//! state ([`AccTable`], `columns`): a non-retaining aggregate (`count`,
//! `sum`, `avg`, `min`, `max`, `stDev`, `stDevP`, DISTINCT or not) keeps
//! constant-size state (plus a DISTINCT call's seen-set) and never an
//! operand value; only `collect` and the percentiles retain their operands,
//! charged to the expression budget and, when admission is on, to the
//! workspace ledger as one retaining container. The plain
//! percentile calls of one group over the same operand share one `f64`
//! buffer (`pool`) rather than each keeping its own `Value`s.
//! [`AccTable::finish_group`] resolves every site of a group once, and the
//! evaluator reads the results back from the `FoldedGroup` it resolves.

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::Expr;

use crate::overlay::ExecGraph;
#[cfg(test)]
use crate::overlay::RetainedTotal;
#[cfg(test)]
use crate::row::Row;

use super::columns::AccTable;
#[cfg(test)]
use super::eval::FoldedGroup;
use super::eval::{agg_eval, AggScope};
use super::pool::{shareable, PoolSite};
use super::site::Call;
use super::site::{SiteColumn, SiteState};

/// Discover the sites of an aggregating projection and return their
/// accumulator table with no groups: `items` are the projection items'
/// aggregate expressions, `lenient` the expressions only an `ORDER BY` key
/// or HAVING evaluates. A call an item also reads is strict.
pub(crate) fn discover<'e>(
    graph: &ExecGraph<'_>,
    items: impl IntoIterator<Item = &'e Expr>,
    lenient: impl IntoIterator<Item = &'e Expr>,
    params: &BTreeMap<String, Value>,
) -> Result<AccTable<'e>> {
    let mut sites: Vec<SiteColumn<'e>> = Vec::new();
    for (exprs, is_lenient) in [
        (items.into_iter().collect::<Vec<_>>(), false),
        (lenient.into_iter().collect::<Vec<_>>(), true),
    ] {
        let found = RefCell::new(Vec::new());
        for expr in exprs {
            agg_eval(graph, AggScope::discovering(&found), expr, params)?;
        }
        for expr in found.into_inner() {
            if sites.iter().all(|site| !std::ptr::eq(site.expr, expr)) {
                sites.push(SiteColumn::classify(expr, is_lenient)?);
            }
        }
    }
    let pools = pool_percentiles(&mut sites);
    Ok(AccTable::new(sites, pools))
}

/// One group's accumulators on their own: a table holding group 0.
#[cfg(test)]
pub(crate) struct GroupAccs<'e>(AccTable<'e>);

#[cfg(test)]
impl<'e> GroupAccs<'e> {
    /// The single-group harness over a freshly discovered table: group 0
    /// opened, ready to offer.
    pub(crate) fn open(table: AccTable<'e>) -> Self {
        let mut table = table;
        table.open_group();
        Self(table)
    }

    pub(crate) fn offer(
        &mut self,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        let mut bill = super::table::GroupBill::take_retained(total, graph)?;
        let result = self.0.offer(0, graph, row, params, &mut bill);
        bill.return_retained(total);
        result
    }

    pub(crate) fn settle(
        &mut self,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        let mut bill = super::table::GroupBill::take_retained(total, graph)?;
        let result = self.0.settle(0, graph, &mut bill);
        bill.return_retained(total);
        result
    }

    pub(crate) fn finish(
        mut self,
        graph: &ExecGraph<'_>,
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
    ) -> FoldedGroup<'e> {
        self.0.finish_group(0, graph, seed, params)
    }
}

/// Route every plain percentile call over a shareable operand to one pool
/// per distinct operand expression. A DISTINCT call keeps its own seen-set
/// and values, and `collect` keeps `Value`s: neither is pooled. A pool is
/// lenient only while every site reading it is, and carries the indices of
/// its reading sites — the reverse edge of the sites' pool reference, so a
/// pool's failure parks its readers without scanning every site.
fn pool_percentiles<'e>(sites: &mut [SiteColumn<'e>]) -> Vec<PoolSite<'e>> {
    let mut pools: Vec<PoolSite<'e>> = Vec::new();
    for (site_index, site) in sites.iter_mut().enumerate() {
        let SiteState::Retain { call, .. } = &site.state else {
            continue;
        };
        let Call::Percentile(percentile_call) = *call else {
            continue;
        };
        let operand = percentile_call.operand;
        if site.seen.is_some() || !shareable(operand) {
            continue;
        }
        let index = match pools.iter().position(|pool| pool.reads(operand)) {
            Some(index) => {
                pools[index].lenient &= site.lenient;
                index
            }
            None => {
                pools.push(PoolSite::new(operand, site.lenient));
                pools.len() - 1
            }
        };
        pools[index].readers.push(site_index);
        site.state = SiteState::Pooled {
            pool: index,
            call: percentile_call,
        };
    }
    pools
}

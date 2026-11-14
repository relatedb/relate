//! The groups' accumulator state, flat: one column per aggregate site (and
//! per shared percentile pool), one slot per group in each column. The
//! site itself — the typed call, its classification, its state and the
//! per-shape finish policy — lives in `site`; this module is the table
//! over the sites: the offer/settle/finish orchestration and the one
//! strict-raises/lenient-parks rule.
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

use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::workspace::buffer_growth;
use relate_ir::Expr;

use super::eval::FoldedGroup;
use super::pool::{shareable, Collecting, PoolSite, PoolState, SortedPool};
use super::site::{Call, SiteColumn, SiteState};
use super::table::GroupBill;

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

struct PoolColumn<'e> {
    site: PoolSite<'e>,
    states: Vec<PoolState>,
}

/// Every group's accumulators.
pub(crate) struct AccTable<'e> {
    groups: usize,
    pools: Vec<PoolColumn<'e>>,
    sites: Vec<SiteColumn<'e>>,
}

impl<'e> AccTable<'e> {
    /// Build the table over the discovered sites: one column per site, and
    /// the percentile pools with the sites that read them. The routing is
    /// the table's own construction — every plain percentile call over a
    /// shareable operand goes to one pool per distinct operand expression,
    /// and the pool carries its readers — so a site's `Pooled` state and
    /// the pool backing it are made in one place, where both columns live.
    /// A DISTINCT call keeps its own seen-set and values, and `collect`
    /// keeps `Value`s: neither is pooled.
    pub(super) fn new(mut sites: Vec<SiteColumn<'e>>) -> Self {
        let mut pools: Vec<PoolSite<'e>> = Vec::new();
        for (site_index, site) in sites.iter_mut().enumerate() {
            let SiteState::Retain {
                call: Call::Percentile(percentile_call),
                ..
            } = &site.state
            else {
                continue;
            };
            let operand = percentile_call.operand;
            if site.seen.is_some() || !shareable(operand) {
                continue;
            }
            let index = match pools.iter().position(|pool| pool.reads(operand)) {
                Some(index) => {
                    pools[index].readers.push(site_index);
                    index
                }
                None => {
                    pools.push(PoolSite::new(operand, site_index));
                    pools.len() - 1
                }
            };
            site.state = SiteState::Pooled {
                pool: index,
                call: *percentile_call,
            };
        }
        Self {
            groups: 0,
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

    /// The expressions the finish reads from a group's seed beyond the
    /// projection itself: every percentile's argument.
    pub(super) fn seed_reads(&self) -> impl Iterator<Item = &'e Expr> + '_ {
        self.sites.iter().filter_map(SiteColumn::percentile)
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

    /// Append a fresh group's slot to every column. The group's index is
    /// its slot in every column — the caller reads it where the group is
    /// created (`GroupTable::land` takes it from the index entry).
    pub(super) fn open_group(&mut self) {
        for pool in &mut self.pools {
            pool.states.push(PoolState::default());
        }
        for site in &mut self.sites {
            site.open_group();
        }
        self.groups += 1;
    }

    /// Offer one input row to every site of group `group`. A streaming site
    /// evaluates its operand and folds it into its constant-size state; a
    /// retaining site keeps the value. What a site kept is charged by
    /// [`Self::settle`], once the spent row has let go of its share; a
    /// lenient failure that parks is billed to `bill` here, as it parks.
    pub(super) fn offer(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        bill: &mut GroupBill,
    ) -> Result<()> {
        for pool in &mut self.pools {
            // A pool parks only while every site reading it is lenient —
            // the readers' own flags, asked once per row rather than kept
            // as a second spelling of the same fact on the pool.
            let lenient = pool
                .site
                .readers
                .iter()
                .all(|&reader| self.sites[reader].lenient);
            let state = &mut pool.states[group];
            let offered = state.offer(&pool.site, graph, row, params);
            if let Some(error) = parked(lenient, offered)? {
                // The pool's sites are all lenient: park the failure on
                // each through the readers the routing recorded — the
                // reverse edge of the sites' pool reference, no scan — and
                // end collecting the one way both endings share.
                state.stop_collecting(Collecting::Parked);
                for reader in &pool.site.readers {
                    self.sites[*reader].park_failure(group, error.clone(), graph, bill)?;
                }
            }
        }
        for site in &mut self.sites {
            let offered = site.offer(group, graph, row, params);
            if let Some(error) = parked(site.lenient, offered)? {
                site.park_failure(group, error, graph, bill)?;
            }
        }
        Ok(())
    }

    /// Charge what group `group`'s sites kept since the last settle to the
    /// aggregation's one bill: a retaining site's new operand slots and
    /// payloads, a pool's buffer growth, and the growth of the payload a
    /// `min`/`max` holds. The bill feeds that figure to the expression cap
    /// and, when admission is on, to the workspace ledger. The caller
    /// settles after the spent input row is dropped, so a value cloned from
    /// that row is the sole owner of its payload by now and is charged in
    /// full (a payload still shared with the store or a parameter adds
    /// nothing, as everywhere else). One total for every group: the groups'
    /// retained state is alive together, so it is bounded together.
    pub(super) fn settle(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        bill: &mut GroupBill,
    ) -> Result<()> {
        for pool in &mut self.pools {
            pool.states[group].settle(graph, bill)?;
        }
        for site in &mut self.sites {
            site.settle(group, graph, bill)?;
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

#[cfg(test)]
mod tests {
    use super::super::site::SiteState;
    use super::super::stream::discover;
    use super::super::table::GroupBill;
    use super::*;
    use crate::exec::test_support::{
        agg_fn_over, group_with_vals, percentile_fn_over, OPERAND_COLUMN,
    };
    use crate::graph::Graph;
    use relate_ir::Expr;
    use relate_names::SmallName;

    /// A property of the shared operand column, by key — the one
    /// property-expression builder the fixtures name their keys through.
    fn prop_of_x(key: &str) -> Expr {
        Expr::Property {
            key_id: Default::default(),
            expr: Box::new(Expr::Variable(OPERAND_COLUMN.into())),
            key: SmallName::from_name(key),
        }
    }

    /// A resolved group's slot keeps neither what it held nor a charge
    /// cursor pointing past it, whichever shape the site has: the cursor
    /// leaves with the values (a retaining site) or the fold (a streaming
    /// site's held payload), so a slot read after a resolve never claims
    /// bytes it no longer holds.
    #[test]
    fn a_resolved_group_leaves_no_charge_cursor_behind() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let params = BTreeMap::new();
        let calls = [
            agg_fn_over("collect", prop_of_x("p")),
            agg_fn_over("min", prop_of_x("s")),
        ];
        let mut table = discover(&graph, [], calls.iter(), &params).expect("discovered");
        table.open_group();
        let mut total = graph.retain_bytes(0).expect("a fresh total");
        // The production order the settle charge relies on: the offered row
        // is dropped before the settle, so the payloads the slots hold are
        // solely the group's.
        {
            let row = group_with_vals(&[Value::Map(
                BTreeMap::from([
                    ("p".to_string(), Value::Int(1)),
                    ("s".to_string(), Value::String("a held payload".into())),
                ])
                .into(),
            )])
            .remove(0);
            let mut bill = GroupBill::take_retained(&mut total, &graph).expect("a bill");
            table
                .offer(0, &graph, &row, &params, &mut bill)
                .expect("offered");
            drop(row);
            table.settle(0, &graph, &mut bill).expect("settled");
            bill.return_retained(&mut total);
        }
        let SiteState::Retain { vals, charged, .. } = &table.sites[0].state else {
            panic!("collect retains")
        };
        assert_eq!(
            (vals[0].len(), charged[0]),
            (1, 1),
            "the operand is kept and charged"
        );
        let SiteState::Stream { held, .. } = &table.sites[1].state else {
            panic!("min streams")
        };
        assert!(held[0] > 0, "the min site charged the payload it holds");
        table.finish_group(0, &graph, None, &params);
        let SiteState::Retain { vals, charged, .. } = &table.sites[0].state else {
            panic!("collect retains")
        };
        assert!(vals[0].is_empty(), "the resolve takes the operands out");
        assert_eq!(charged[0], 0, "and the cursor leaves with them");
        let SiteState::Stream { held, .. } = &table.sites[1].state else {
            panic!("min streams")
        };
        assert_eq!(held[0], 0, "the held-payload cursor leaves with the fold");
    }

    /// A lenient site that parks a failure releases what its group slot
    /// retained, whichever shape the site has (a streaming fold, retained
    /// operands, or a shared percentile pool), and every site answers the
    /// group with the parked error.
    #[test]
    fn a_parked_failure_releases_the_slot_in_every_site_shape() {
        let g = Graph::default();
        let graph = ExecGraph::read(&g);
        let params = BTreeMap::new();
        let calls = [
            agg_fn_over("count", prop_of_x("p")),
            agg_fn_over("collect", prop_of_x("p")),
            percentile_fn_over("percentileCont", prop_of_x("p"), 0.5),
            agg_fn_over("min", prop_of_x("s")),
        ];
        let mut table = discover(&graph, [], calls.iter(), &params).expect("discovered");
        table.open_group();
        let mut total = graph.retain_bytes(0).expect("a fresh total");
        let ok = Value::Map(
            BTreeMap::from([
                ("p".to_string(), Value::Int(1)),
                ("s".to_string(), Value::String("a held payload".into())),
            ])
            .into(),
        );
        // The production order the settle charge relies on: the offered row
        // is dropped before the settle, so the payload the fold holds is
        // solely the group's. The failing row is built apart and offered
        // after the assertions on the charged state.
        let fail_row = group_with_vals(&[Value::Int(5)]).remove(0);
        {
            let ok_row = group_with_vals(&[ok]).remove(0);
            let mut bill = GroupBill::take_retained(&mut total, &graph).expect("a bill");
            table
                .offer(0, &graph, &ok_row, &params, &mut bill)
                .expect("offered");
            drop(ok_row);
            table.settle(0, &graph, &mut bill).expect("settled");
            bill.return_retained(&mut total);
        }
        let SiteState::Retain { vals, .. } = &table.sites[1].state else {
            panic!("collect retains")
        };
        assert_eq!(vals[0].len(), 1);
        assert_eq!(table.pools[0].states[0].held(), 1);
        let SiteState::Stream { held, .. } = &table.sites[3].state else {
            panic!("min streams")
        };
        assert!(held[0] > 0, "the min site charged the payload it holds");
        let before = total.total();

        // `5.p` fails on every lenient site: parked, billed, released.
        let mut bill = GroupBill::take_retained(&mut total, &graph).expect("a bill");
        table
            .offer(0, &graph, &fail_row, &params, &mut bill)
            .expect("parked");
        bill.return_retained(&mut total);
        assert!(total.total() > before, "the parked errors are billed");
        let SiteState::Stream { folds, .. } = &table.sites[0].state else {
            panic!("count streams")
        };
        assert_eq!(
            folds[0].clone().finish().expect("a fresh count"),
            Value::Int(0)
        );
        let SiteState::Retain { vals, charged, .. } = &table.sites[1].state else {
            panic!("collect retains")
        };
        assert!(vals[0].is_empty());
        assert_eq!(charged[0], 0);
        let SiteState::Stream { held, .. } = &table.sites[3].state else {
            panic!("min streams")
        };
        assert_eq!(held[0], 0, "the held-payload charge drops with its value");
        assert_eq!(table.pools[0].states[0].held(), 0);
        assert!(table.sites.iter().all(|site| site.failed.contains_key(&0)));
        let folded = table.finish_group(0, &graph, Some(&fail_row), &params);
        for call in &calls {
            assert!(folded.value(call).is_err(), "{call:?}");
        }
    }
}

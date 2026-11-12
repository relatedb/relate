//! The grouped aggregate's streaming state: what a group keeps instead of
//! its input rows.
//!
//! [`AggSites::discover`] finds every aggregate call an aggregating
//! projection can evaluate (items, `ORDER BY` keys, HAVING) by running the
//! one recursive evaluator in its discovery mode, so the sites are exactly
//! the calls the evaluator reaches. Each site gets one column of per-group
//! state ([`AccTable`], `columns`): a non-retaining aggregate (`count`,
//! `sum`, `avg`, `min`, `max`, `stDev`, `stDevP`, DISTINCT or not) keeps
//! constant-size state (plus a DISTINCT call's seen-set) and never an
//! operand value; only `collect` and the percentiles retain their operands,
//! charged to the expression budget as a retaining container. The plain
//! percentile calls of one group over the same operand share one `f64`
//! buffer (`pool`) rather than each keeping its own `Value`s.
//! [`AccTable::finish_group`] resolves every site of a group once, and the
//! evaluator reads the results back from the [`FoldedGroup`].

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::{classify_aggregate, AggregateArity, AggregateKind, Expr};
use relate_names::SmallName;

use crate::eval::{eval, fold_aggregate, needs_numbers, FoldInput, Percentile, StreamFold};
use crate::overlay::ExecGraph;
#[cfg(test)]
use crate::overlay::RetainedTotal;
use crate::row::Row;

use super::columns::{AccTable, SiteColumn, SiteState};
use super::eval::{agg_eval, AggScope};
use super::pool::{shareable, PoolSite, SortedPool};

/// One aggregate call, validated: the borrowed pieces every offer and the
/// finish read.
#[derive(Clone, Copy)]
pub(super) struct Call<'e> {
    kind: AggregateKind,
    pub(super) name: &'e SmallName,
    pub(super) operand: &'e Expr,
    /// The percentile argument of a `PercentilePair` call.
    pub(super) percentile: Option<&'e Expr>,
}

/// The aggregate call sites of one aggregating projection, as an empty
/// accumulator table: every group's table is opened from it.
pub(crate) struct AggSites<'e> {
    blank: AccTable<'e>,
}

impl<'e> AggSites<'e> {
    /// Discover the sites of an aggregating projection: `items` are the
    /// projection items' aggregate expressions, `lenient` the expressions
    /// only an `ORDER BY` key or HAVING evaluates. A call an item also
    /// reads is strict.
    pub(crate) fn discover(
        graph: &ExecGraph<'_>,
        items: impl IntoIterator<Item = &'e Expr>,
        lenient: impl IntoIterator<Item = &'e Expr>,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<Self> {
        let mut sites: Vec<SiteColumn<'e>> = Vec::new();
        for (exprs, is_lenient) in [
            (items.into_iter().collect::<Vec<_>>(), false),
            (lenient.into_iter().collect::<Vec<_>>(), true),
        ] {
            let found = RefCell::new(Vec::new());
            for expr in exprs {
                agg_eval(
                    graph,
                    AggScope::discovering(&found),
                    expr,
                    params,
                    cypher_version,
                )?;
            }
            for expr in found.into_inner() {
                if sites.iter().all(|site| !std::ptr::eq(site.expr, expr)) {
                    sites.push(SiteColumn {
                        expr,
                        lenient: is_lenient,
                        state: site_state(expr)?,
                    });
                }
            }
        }
        let pools = pool_percentiles(&mut sites);
        Ok(Self {
            blank: AccTable::new(sites, pools),
        })
    }

    /// A fresh table with no groups: each landing group opens its slot in
    /// every column ([`AccTable::open_group`]).
    pub(super) fn open_table(&self) -> AccTable<'e> {
        self.blank.empty_like()
    }

    /// The expressions the finish reads from a group's seed beyond the
    /// projection itself: every percentile's argument.
    pub(super) fn seed_reads(&self) -> impl Iterator<Item = &'e Expr> + '_ {
        self.blank.sites().iter().filter_map(SiteColumn::percentile)
    }

    /// One group's accumulators on their own (the single-group harness the
    /// tests drive).
    #[cfg(test)]
    pub(crate) fn open_group(&self) -> GroupAccs<'e> {
        let mut table = self.open_table();
        table.open_group();
        GroupAccs(table)
    }
}

/// One group's accumulators on their own: a table holding group 0.
#[cfg(test)]
pub(crate) struct GroupAccs<'e>(AccTable<'e>);

#[cfg(test)]
impl<'e> GroupAccs<'e> {
    pub(crate) fn offer(
        &mut self,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<()> {
        self.0.offer(0, graph, row, params, cypher_version)
    }

    pub(crate) fn settle(
        &mut self,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        self.0.settle(0, graph, total)
    }

    pub(crate) fn finish(
        mut self,
        graph: &ExecGraph<'_>,
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> FoldedGroup<'e> {
        self.0.finish_group(0, graph, seed, params, cypher_version)
    }
}

/// Route every plain percentile call over a shareable operand to one pool
/// per distinct operand expression. A DISTINCT call keeps its own seen-set
/// and values, and `collect` keeps `Value`s: neither is pooled. A pool is
/// lenient only while every site reading it is.
fn pool_percentiles<'e>(sites: &mut [SiteColumn<'e>]) -> Vec<PoolSite<'e>> {
    let mut pools: Vec<PoolSite<'e>> = Vec::new();
    for site in sites {
        let SiteState::Retain {
            seen: None, call, ..
        } = &site.state
        else {
            continue;
        };
        let call = *call;
        let Some(percentile) = call.percentile else {
            continue;
        };
        if !shareable(call.operand) {
            continue;
        }
        let index = match pools.iter().position(|pool| pool.reads(call.operand)) {
            Some(index) => {
                pools[index].lenient &= site.lenient;
                index
            }
            None => {
                pools.push(PoolSite::new(call.operand, site.lenient));
                pools.len() - 1
            }
        };
        site.state = SiteState::Pooled {
            pool: index,
            call,
            percentile,
            failed: BTreeMap::new(),
        };
    }
    pools
}

/// Classify one discovered call once: its arity contract, and whether it
/// streams or retains.
fn site_state(expr: &Expr) -> Result<SiteState<'_>> {
    let (name, distinct, args) = match expr {
        Expr::CountStar => return Ok(SiteState::CountStar(Vec::new())),
        Expr::Function {
            name,
            distinct,
            args,
        } => (name, *distinct, args.as_slice()),
        _ => {
            return Err(Error::internal(
                "an aggregate call site is a count(*) or a function call",
            ))
        }
    };
    let kind = name.with_str(classify_aggregate).ok_or_else(|| {
        Error::internal("discovery records only calls the aggregate classification accepts")
    })?;
    // LABS-276 / ADR-0225/0217: every aggregate's arity contract comes from
    // the kind's row in the single `relate-ir` contracts table. `count` is
    // guarded here too — a backtick-escaped `` `count`() `` bypasses both
    // the parser's keyword path and front validation — and the percentiles
    // keep their own two-argument message.
    let arity = kind.arity();
    if args.len() != arity.required() {
        let rejection = name.with_str(|text| arity.rejection(text, args.len()));
        return Ok(SiteState::Rejected(match arity {
            AggregateArity::SingleInput => Error::syntax(rejection),
            AggregateArity::PercentilePair => Error::ty(rejection),
        }));
    }
    let call = Call {
        kind,
        name,
        operand: &args[0],
        percentile: args.get(1),
    };
    // `min`/`max` ignore DISTINCT (the extreme of the distinct values is the
    // extreme of the values), so they need no seen-set; every other
    // streaming DISTINCT call folds an operand when its seen-set admits it.
    let dedups = distinct && !matches!(kind, AggregateKind::Min | AggregateKind::Max);
    Ok(match StreamFold::for_kind(kind) {
        Some(template) => SiteState::Stream {
            call,
            template,
            folds: Vec::new(),
            held: Vec::new(),
            seen: dedups.then(Vec::new),
            failed: BTreeMap::new(),
        },
        None => SiteState::Retain {
            call,
            vals: Vec::new(),
            charged: Vec::new(),
            seen: distinct.then(Vec::new),
            failed: BTreeMap::new(),
        },
    })
}

/// The validated percentile argument of a call. The percentile depends only
/// on the group's first row; an empty group has no row to ask and no
/// percentile to report (`None`).
fn percentile_input(
    graph: &ExecGraph<'_>,
    call: Call<'_>,
    percentile: &Expr,
    seed: Option<&Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Option<Percentile>> {
    let Some(first) = seed else {
        return Ok(None);
    };
    let p = eval(graph, first, percentile, params, cypher_version)?;
    // Two distinct rejections: a non-numeric percentile is the
    // wrong-type class, an out-of-[0, 1] one the NumberOutOfRange
    // kind (ADR-0175's 22N03 mapping).
    let Some(p) = p.as_number() else {
        return Err(Error::invalid_argument_type("percentile must be a number"));
    };
    if !(0.0..=1.0).contains(&p) {
        return Err(Error::argument_value_kind(
            ErrorKind::NumberOutOfRange,
            "percentile",
        ));
    }
    call.name
        .with_str(|text| Percentile::new(call.kind, p, text))
        .map(Some)
}

/// A retaining site's result: the percentile argument is validated first,
/// and the shared per-kind fold finishes the operands (`collect` and
/// the DISTINCT percentiles).
pub(super) fn finish_retained(
    graph: &ExecGraph<'_>,
    vals: Vec<Value>,
    call: Call<'_>,
    seed: Option<&Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let input = match call.percentile {
        None => FoldInput::Many(call.kind),
        Some(percentile) => {
            match percentile_input(graph, call, percentile, seed, params, cypher_version)? {
                Some(percentile) => FoldInput::Percentile(percentile),
                None => return Ok(Value::Null),
            }
        }
    };
    call.name.with_str(|text| fold_aggregate(input, text, vals))
}

/// A pooled percentile's result: the same order of rejections as the
/// per-site fold (a parked evaluation failure is already a failed site, read
/// before this; then the percentile argument, then a non-numeric operand),
/// then the rank in the sample the group sorted once.
pub(super) fn finish_pooled(
    graph: &ExecGraph<'_>,
    pool: &SortedPool,
    call: Call<'_>,
    percentile: &Expr,
    seed: Option<&Row>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<Value> {
    let Some(percentile) = percentile_input(graph, call, percentile, seed, params, cypher_version)?
    else {
        return Ok(Value::Null);
    };
    if pool.non_numeric {
        return Err(call.name.with_str(needs_numbers));
    }
    Ok(percentile.rank_in_sorted(&pool.vals))
}

/// One group's resolved aggregate calls, read back by the evaluator.
pub(crate) struct FoldedGroup<'e> {
    values: Vec<(&'e Expr, Result<Value>)>,
}

impl<'e> FoldedGroup<'e> {
    pub(super) fn new(values: Vec<(&'e Expr, Result<Value>)>) -> Self {
        Self { values }
    }

    /// The value of the call at `expr` (by identity).
    pub(crate) fn value(&self, expr: &Expr) -> Result<Value> {
        self.values
            .iter()
            .find(|(site, _)| std::ptr::eq(*site, expr))
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| {
                Err(Error::internal(
                    "an aggregate call was not discovered before the group resolved",
                ))
            })
    }
}

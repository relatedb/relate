//! The grouped aggregate's streaming state: what a group keeps instead of
//! its input rows.
//!
//! [`AggSites::discover`] finds every aggregate call an aggregating
//! projection can evaluate (items, `ORDER BY` keys, HAVING) by running the
//! one recursive evaluator in its discovery mode, so the sites are exactly
//! the calls the evaluator reaches. Each site gets one accumulator per
//! group ([`GroupAccs`]): a non-retaining aggregate (`count`, `sum`, `avg`,
//! `min`, `max`, `stDev`, `stDevP`, DISTINCT or not) keeps constant-size state
//! (plus a DISTINCT call's seen-set) and never an operand value; only `collect` and the percentiles retain
//! their operands, charged to the expression budget as a retaining
//! container. [`GroupAccs::finish`] resolves every site once, and the
//! evaluator reads the results back from the [`FoldedGroup`].

use std::cell::RefCell;
use std::collections::BTreeMap;

use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::{classify_aggregate, AggregateArity, AggregateKind, Expr};
use relate_names::SmallName;

use crate::distinct_set::DistinctSet;
use crate::eval::{eval, fold_aggregate, FoldInput, StreamFold};
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;

use super::eval::{agg_eval, AggScope};

/// One aggregate call, validated: the borrowed pieces every offer and the
/// finish read.
#[derive(Clone, Copy)]
struct Call<'e> {
    kind: AggregateKind,
    name: &'e SmallName,
    operand: &'e Expr,
    /// The percentile argument of a `PercentilePair` call.
    percentile: Option<&'e Expr>,
}

/// One site's per-group state.
enum SiteAcc<'e> {
    CountStar(i64),
    /// A non-retaining aggregate: constant-size state, independent of the
    /// number of rows offered.
    Stream {
        fold: StreamFold,
        call: Call<'e>,
        /// The peak payload bytes of the one value a `min`/`max` keeps,
        /// already charged ([`GroupAccs::settle`]).
        held: u64,
        /// A DISTINCT call's seen-set: an operand folds only when it is
        /// first seen, and is dropped at once; the set's keys are the
        /// only per-distinct-value state.
        seen: Option<DistinctSet>,
    },
    /// `collect` and the percentiles (and their DISTINCT forms): the operands are the
    /// state, charged to the aggregation's running total as they settle.
    Retain {
        vals: Vec<Value>,
        /// How many of `vals` are already charged.
        charged: usize,
        /// A DISTINCT call's seen-set: only first-seen operands reach
        /// `vals`, so the state is proportional to the distinct values.
        seen: Option<DistinctSet>,
        call: Call<'e>,
    },
    /// A lenient site (one only an `ORDER BY` key or HAVING reads) whose
    /// operand failed to fold: the error is parked here and surfaces when
    /// the site is read. The read's outcome is stated once at the
    /// canonical contract ([`GroupSubclauses::having`], ADR-0458 §7); an
    /// ORDER BY key orders as null.
    Failed(Error),
    /// An arity violation (a backtick-escaped `` `count`() `` bypasses the
    /// parser and front validation): raised when the group resolves, as the
    /// row-retaining fold did, so an empty keyed input still yields no
    /// error.
    Rejected(Error),
}

struct Slot<'e> {
    /// The call's identity: the evaluator asks for its value by this
    /// reference.
    expr: &'e Expr,
    /// Whether only an `ORDER BY` key or HAVING reads this call: a fold
    /// error parks instead of failing the statement (see
    /// [`SiteAcc::Failed`]); the read's outcome is the canonical HAVING
    /// contract ([`GroupSubclauses::having`], ADR-0458 §7).
    lenient: bool,
    acc: SiteAcc<'e>,
}

impl<'e> SiteAcc<'e> {
    /// A fresh group's state from this template site: the same call, an
    /// empty accumulator (a DISTINCT call's seen-set starts empty).
    fn open(&self) -> SiteAcc<'e> {
        match self {
            Self::CountStar(_) => Self::CountStar(0),
            Self::Stream {
                fold,
                call,
                held,
                seen,
            } => Self::Stream {
                fold: fold.clone(),
                call: *call,
                held: *held,
                seen: seen.as_ref().map(|_| DistinctSet::default()),
            },
            Self::Retain { seen, call, .. } => Self::Retain {
                vals: Vec::new(),
                charged: 0,
                seen: seen.as_ref().map(|_| DistinctSet::default()),
                call: *call,
            },
            Self::Failed(error) => Self::Failed(error.clone()),
            Self::Rejected(error) => Self::Rejected(error.clone()),
        }
    }
}

impl SiteAcc<'_> {
    /// Offer one input row to this site.
    fn offer(
        &mut self,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<()> {
        match self {
            Self::CountStar(n) => *n += 1,
            Self::Stream {
                fold, call, seen, ..
            } => {
                graph.cancel_tick()?;
                let value = eval(graph, row, call.operand, params, cypher_version)?;
                if seen
                    .as_mut()
                    .map_or(Ok(true), |seen| seen.insert_value(&value))?
                {
                    call.name.with_str(|name| fold.offer(value, name))?;
                }
            }
            Self::Retain {
                vals, seen, call, ..
            } => {
                graph.cancel_tick()?;
                let value = eval(graph, row, call.operand, params, cypher_version)?;
                if seen
                    .as_mut()
                    .map_or(Ok(true), |seen| seen.insert_value(&value))?
                {
                    vals.push(value);
                }
            }
            Self::Failed(_) | Self::Rejected(_) => {}
        }
        Ok(())
    }
}

/// The aggregate call sites of one aggregating projection, with the
/// accumulator each group starts from.
pub(crate) struct AggSites<'e> {
    template: GroupAccs<'e>,
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
        let mut slots: Vec<Slot<'e>> = Vec::new();
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
                if slots.iter().all(|slot| !std::ptr::eq(slot.expr, expr)) {
                    slots.push(Slot {
                        expr,
                        lenient: is_lenient,
                        acc: site_acc(expr)?,
                    });
                }
            }
        }
        Ok(Self {
            template: GroupAccs { slots },
        })
    }

    /// A fresh group's accumulators.
    pub(crate) fn open_group(&self) -> GroupAccs<'e> {
        GroupAccs {
            slots: self
                .template
                .slots
                .iter()
                .map(|slot| Slot {
                    expr: slot.expr,
                    lenient: slot.lenient,
                    acc: slot.acc.open(),
                })
                .collect(),
        }
    }
}

/// Classify one discovered call once: its arity contract, and whether it
/// streams or retains.
fn site_acc(expr: &Expr) -> Result<SiteAcc<'_>> {
    let (name, distinct, args) = match expr {
        Expr::CountStar => return Ok(SiteAcc::CountStar(0)),
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
        return Ok(SiteAcc::Rejected(match arity {
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
        Some(fold) => SiteAcc::Stream {
            fold,
            call,
            held: 0,
            seen: dedups.then(DistinctSet::default),
        },
        None => SiteAcc::Retain {
            vals: Vec::new(),
            charged: 0,
            seen: distinct.then(DistinctSet::default),
            call,
        },
    })
}

/// One group's accumulators, one per site.
pub(crate) struct GroupAccs<'e> {
    slots: Vec<Slot<'e>>,
}

impl<'e> GroupAccs<'e> {
    /// The constant footprint of a group's accumulators — what the group
    /// map's retained total grows by when a group lands.
    pub(crate) fn footprint(&self) -> u64 {
        crate::workspace::buffer_bytes_saturating(self.slots.len(), size_of::<Slot<'e>>() as u64)
    }

    /// Offer one input row to every site. A streaming site evaluates its
    /// operand and folds it into its constant-size state; a retaining site
    /// keeps the value. What a site kept is charged by [`Self::settle`],
    /// once the spent row has let go of its share.
    pub(crate) fn offer(
        &mut self,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> Result<()> {
        for slot in &mut self.slots {
            let offered = slot.acc.offer(graph, row, params, cypher_version);
            match offered {
                Ok(()) => {}
                Err(error) if slot.lenient => slot.acc = SiteAcc::Failed(error),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Charge what the sites kept since the last settle to the
    /// aggregation's one running total: a retaining site's new operand slots
    /// and payloads, and the growth of the payload a `min`/`max` holds. The
    /// caller settles after the spent input row is dropped, so a value
    /// cloned from that row is the sole owner of its payload by now and is
    /// charged in full (a payload still shared with the store or a
    /// parameter adds nothing, as everywhere else). One total for every
    /// group: the groups' retained state is alive together, so it is
    /// bounded together.
    pub(crate) fn settle(
        &mut self,
        graph: &ExecGraph<'_>,
        total: &mut RetainedTotal,
    ) -> Result<()> {
        for slot in &mut self.slots {
            match &mut slot.acc {
                SiteAcc::Retain { vals, charged, .. } => {
                    for value in &vals[*charged..] {
                        let slot_bytes = crate::workspace::buffer_growth(
                            *charged,
                            *charged + 1,
                            size_of::<Value>() as u64,
                        );
                        total.add_bytes(graph, slot_bytes)?;
                        total.add_owned(graph, value)?;
                        *charged += 1;
                    }
                }
                SiteAcc::Stream { fold, held, .. } => {
                    if let Some(best) = fold.held_value() {
                        let now = crate::workspace::owned_heap(best);
                        if now > *held {
                            total.add_bytes(graph, now - *held)?;
                            *held = now;
                        }
                    }
                }
                SiteAcc::CountStar(_) | SiteAcc::Failed(_) | SiteAcc::Rejected(_) => {}
            }
        }
        Ok(())
    }

    /// Resolve every site. `seed` is the group's own first row — a
    /// percentile's second argument is read from it, and an empty group has
    /// no percentile to report.
    pub(crate) fn finish(
        self,
        graph: &ExecGraph<'_>,
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
        cypher_version: Option<u32>,
    ) -> FoldedGroup<'e> {
        FoldedGroup {
            values: self
                .slots
                .into_iter()
                .map(|slot| {
                    let value = match slot.acc {
                        SiteAcc::CountStar(n) => Ok(Value::Int(n)),
                        SiteAcc::Stream { fold, .. } => fold.finish(),
                        SiteAcc::Retain { vals, call, .. } => {
                            finish_retained(graph, vals, call, seed, params, cypher_version)
                        }
                        SiteAcc::Failed(error) | SiteAcc::Rejected(error) => Err(error),
                    };
                    (slot.expr, value)
                })
                .collect(),
        }
    }
}

/// A retaining site's result: the percentile argument is validated first,
/// and the shared per-kind fold finishes the operands (`collect` and
/// the percentiles).
fn finish_retained(
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
            // The percentile depends only on the group's first row; an empty
            // group has no row to ask and the fold yields null.
            let Some(first) = seed else {
                return Ok(Value::Null);
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
                .with_str(|text| FoldInput::percentile(call.kind, p, text))?
        }
    };
    call.name.with_str(|text| fold_aggregate(input, text, vals))
}

/// One group's resolved aggregate calls, read back by the evaluator.
pub(crate) struct FoldedGroup<'e> {
    values: Vec<(&'e Expr, Result<Value>)>,
}

impl FoldedGroup<'_> {
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

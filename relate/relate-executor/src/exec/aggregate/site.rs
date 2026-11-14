//! The aggregate site: the typed call shape, its classification (the one
//! construction path of a [`SiteColumn`]), the site's state across every
//! group, and the per-shape finish policy. `columns` keeps the flat table
//! over the sites and the offer/settle/finish orchestration.

use std::collections::BTreeMap;

use relate_core::{Error, ErrorKind, Result, Value};
use relate_ir::{classify_aggregate, AggregateArity, AggregateKind, Expr};
use relate_names::SmallName;

use crate::distinct_set::DistinctSet;
use crate::eval::{eval, fold_aggregate, needs_numbers, FoldInput, Percentile, StreamFold};
use crate::overlay::ExecGraph;
use crate::row::Row;
use crate::workspace::buffer_growth;

use super::pool::{SampleEnding, SortedPool};
use super::table::GroupBill;

/// Groups whose lenient site failed to fold: the error is parked here and
/// surfaces when the site is read. Sparse — empty unless an `ORDER BY` key
/// or HAVING operand failed (see [`GroupSubclauses::where_mode`] for the
/// outcome, ADR-0458 §7).
///
/// [`GroupSubclauses::where_mode`]: super::group::GroupSubclauses
pub(super) type Failed = BTreeMap<usize, Error>;

/// What one parked failure retains: its map entry, the error value and its
/// message. Billed once per site, so K sites parking one operand's error
/// pay K times.
fn parked_error_bytes(error: &Error) -> u64 {
    (size_of::<(usize, Error)>() as u64 + error.to_string().len() as u64)
        .saturating_add(BTREE_ENTRY_OVERHEAD)
}

/// Per-entry bookkeeping of a `BTreeMap` node, a conservative flat figure.
const BTREE_ENTRY_OVERHEAD: u64 = 16;

/// The `PercentilePair` call's payload: the four borrowed pieces one
/// percentile call is, carried whole wherever it goes — the enum's arm or
/// a pooled site's column — so no reader re-splits it into loose fields.
#[derive(Clone, Copy)]
pub(super) struct PercentileCall<'e> {
    pub(super) kind: AggregateKind,
    pub(super) name: &'e SmallName,
    pub(super) operand: &'e Expr,
    pub(super) percentile: &'e Expr,
}

/// One aggregate call, validated: the borrowed pieces every offer and the
/// finish read, in one typed shape — the kind's arity row in the
/// `relate-ir` contracts table decides the arm once, in
/// [`SiteColumn::classify`], so no reader re-derives whether the call carries
/// a percentile argument.
#[derive(Clone, Copy)]
pub(super) enum Call<'e> {
    /// A single-input call: `count`, `sum`, `avg`, `min`, `max`, a
    /// deviation, `collect`.
    Plain {
        kind: AggregateKind,
        name: &'e SmallName,
        operand: &'e Expr,
    },
    /// A `PercentilePair` call: the percentile argument rides in the arm's
    /// payload, non-optional.
    Percentile(PercentileCall<'e>),
}

impl<'e> Call<'e> {
    pub(super) fn name(&self) -> &'e SmallName {
        match self {
            Self::Plain { name, .. } => name,
            Self::Percentile(call) => call.name,
        }
    }

    fn operand(&self) -> &'e Expr {
        match self {
            Self::Plain { operand, .. } => operand,
            Self::Percentile(call) => call.operand,
        }
    }

    /// The percentile argument of a percentile call (`None` for a plain
    /// one) — a discriminant query for the readers that ask, never a
    /// re-derivation of the arity fact.
    pub(super) fn percentile(&self) -> Option<&'e Expr> {
        match self {
            Self::Percentile(call) => Some(call.percentile),
            Self::Plain { .. } => None,
        }
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
    },
    /// `collect` and the DISTINCT percentiles: the operands are the state,
    /// charged to the aggregation's running total as they settle.
    Retain {
        call: Call<'e>,
        vals: Vec<Vec<Value>>,
        /// Per group, how many of `vals` are already charged.
        charged: Vec<usize>,
    },
    /// A plain percentile call: its operands live in the shared pool
    /// column at this index, not here. The routing in
    /// [`super::columns::AccTable::new`] admits only percentile calls into
    /// this state, so the call rides whole — the plain-call shape has no
    /// spelling here.
    Pooled {
        pool: usize,
        call: PercentileCall<'e>,
    },
    /// An arity violation (a backtick-escaped `` `count`() `` bypasses the
    /// parser and front validation): raised when a group resolves, as the
    /// row-retaining fold did, so an empty keyed input still yields no
    /// error.
    Rejected(Error),
}

/// One aggregate call site: the call's identity (the evaluator asks for its
/// value by this reference), whether only an `ORDER BY` key or HAVING reads
/// it (a fold error parks instead of failing the statement), the groups
/// whose fold already failed, and its state.
pub(super) struct SiteColumn<'e> {
    pub(super) expr: &'e Expr,
    pub(super) lenient: bool,
    pub(super) failed: Failed,
    /// A DISTINCT call's per-group seen-set (`Some` for such a site): an
    /// operand folds only when first seen and is dropped at once. A
    /// property of the site, not of its fold shape — both the streaming
    /// and the retaining shapes keep one.
    pub(super) seen: Option<Vec<DistinctSet>>,
    pub(super) state: SiteState<'e>,
}

/// The operand `call` yields for `row` in `group`, when it is to be folded:
/// `None` for a group whose site already failed (nothing is evaluated any
/// more) and for a DISTINCT call's operand already seen there. The one
/// preamble every streaming and retaining offer shares.
pub(super) fn admitted_operand(
    failed: &Failed,
    seen: &mut Option<Vec<DistinctSet>>,
    group: usize,
    call: &Call<'_>,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<Option<Value>> {
    if failed.contains_key(&group) {
        return Ok(None);
    }
    graph.cancel_tick()?;
    let value = eval(graph, row, call.operand(), params)?;
    let admitted = seen
        .as_mut()
        .map_or(Ok(true), |seen| seen[group].insert_value(&value))?;
    Ok(admitted.then_some(value))
}

impl<'e> SiteColumn<'e> {
    /// Classify one discovered call once — the one construction path, so
    /// the state and the seen-set that must agree with it are built in
    /// one place and the impossible combinations (a seen-set on a pooled
    /// or `count(*)` site, a `min`/`max` site with one) have no spelling.
    ///
    /// LABS-276 / ADR-0225/0217: every aggregate's arity contract comes
    /// from the kind's row in the single `relate-ir` contracts table —
    /// the one place the call's typed shape is decided. `count` is
    /// guarded here too — a backtick-escaped `` `count`() `` bypasses
    /// both the parser's keyword path and front validation — and the
    /// percentiles keep their own two-argument message.
    pub(super) fn classify(expr: &'e Expr, lenient: bool) -> Result<Self> {
        let (name, distinct, args) = match expr {
            Expr::CountStar => {
                return Ok(Self::assemble(
                    expr,
                    lenient,
                    SiteState::CountStar(Vec::new()),
                    None,
                ))
            }
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
        let arity = kind.arity();
        if args.len() != arity.required() {
            let rejected = SiteState::Rejected(name.with_str(|text| arity.error(text, args.len())));
            return Ok(Self::assemble(expr, lenient, rejected, None));
        }
        let call = match arity {
            AggregateArity::SingleInput => Call::Plain {
                kind,
                name,
                operand: &args[0],
            },
            AggregateArity::PercentilePair => Call::Percentile(PercentileCall {
                kind,
                name,
                operand: &args[0],
                percentile: &args[1],
            }),
        };
        // `min`/`max` ignore DISTINCT (the extreme of the distinct values
        // is the extreme of the values), so they need no seen-set; every
        // other streaming DISTINCT call folds an operand when its seen-set
        // admits it.
        let dedups = distinct && !matches!(kind, AggregateKind::Min | AggregateKind::Max);
        let (state, keeps_seen) = match StreamFold::for_kind(kind) {
            Some(template) => (
                SiteState::Stream {
                    call,
                    template,
                    folds: Vec::new(),
                    held: Vec::new(),
                },
                dedups,
            ),
            None => (
                SiteState::Retain {
                    call,
                    vals: Vec::new(),
                    charged: Vec::new(),
                },
                distinct,
            ),
        };
        Ok(Self::assemble(
            expr,
            lenient,
            state,
            keeps_seen.then(Vec::new),
        ))
    }

    /// The packer behind [`Self::classify`]: a site is its identity,
    /// lenience, an empty failure map, its state, and the seen-set the
    /// classification agreed on — one place, beside the classification
    /// that decides the pair.
    fn assemble(
        expr: &'e Expr,
        lenient: bool,
        state: SiteState<'e>,
        seen: Option<Vec<DistinctSet>>,
    ) -> Self {
        Self {
            expr,
            lenient,
            failed: Failed::new(),
            seen,
            state,
        }
    }

    /// The call's percentile argument, which the finish reads from the
    /// group's seed.
    pub(super) fn percentile(&self) -> Option<&'e Expr> {
        // Every shape named: a streaming call also carries a `Call`, and
        // answering from it (not from the storage shape) keeps a new shape
        // deciding instead of silently reading as "no".
        match &self.state {
            SiteState::Retain { call, .. } | SiteState::Stream { call, .. } => call.percentile(),
            SiteState::Pooled { call, .. } => Some(call.percentile),
            SiteState::CountStar(_) | SiteState::Rejected(_) => None,
        }
    }

    /// Per-group bytes of this site's column slot.
    pub(super) fn slot_bytes(&self) -> u64 {
        let distinct = self
            .seen
            .as_ref()
            .map_or(0, |_| size_of::<DistinctSet>() as u64);
        match &self.state {
            SiteState::CountStar(_) => size_of::<i64>() as u64,
            SiteState::Stream { .. } => {
                (size_of::<StreamFold>() + size_of::<u64>()) as u64 + distinct
            }
            SiteState::Retain { .. } => {
                (size_of::<Vec<Value>>() + size_of::<usize>()) as u64 + distinct
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => 0,
        }
    }

    pub(super) fn open_group(&mut self) {
        match &mut self.state {
            SiteState::CountStar(counts) => counts.push(0),
            SiteState::Stream {
                template,
                folds,
                held,
                ..
            } => {
                folds.push(template.clone());
                held.push(0);
            }
            SiteState::Retain { vals, charged, .. } => {
                vals.push(Vec::new());
                charged.push(0);
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        if let Some(seen) = &mut self.seen {
            seen.push(DistinctSet::default());
        }
    }

    /// Offer one input row to group `group`'s slot.
    pub(super) fn offer(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        row: &Row,
        params: &BTreeMap<String, Value>,
    ) -> Result<()> {
        let Self {
            failed,
            seen,
            state,
            ..
        } = self;
        match state {
            SiteState::CountStar(counts) => counts[group] += 1,
            SiteState::Stream { call, folds, .. } => {
                if let Some(value) =
                    admitted_operand(failed, seen, group, call, graph, row, params)?
                {
                    call.name()
                        .with_str(|name| folds[group].offer(value, name))?;
                }
            }
            SiteState::Retain { call, vals, .. } => {
                if let Some(value) =
                    admitted_operand(failed, seen, group, call, graph, row, params)?
                {
                    vals[group].push(value);
                }
            }
            SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        Ok(())
    }

    /// Drop what group `group`'s slot retained: the operands (or the fold)
    /// with their charge cursors, and the DISTINCT seen-set — one reset
    /// over the site's own fields, whichever shape its state holds. A
    /// pooled site holds nothing here (its pool state releases the
    /// sample).
    fn release(&mut self, group: usize) {
        match &mut self.state {
            SiteState::Stream {
                template,
                folds,
                held,
                ..
            } => {
                let _ = take_stream_slot(template, folds, held, group);
            }
            SiteState::Retain { vals, charged, .. } => {
                let _ = take_retain_slot(vals, charged, group);
            }
            SiteState::CountStar(_) | SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        if let Some(seen) = &mut self.seen {
            seen[group] = DistinctSet::default();
        }
    }

    /// A lenient site's operand failed for `group`: bill what the parked
    /// error retains to `total`, release what the slot kept, and park the
    /// error. The failure surfaces when the site is read.
    pub(super) fn park_failure(
        &mut self,
        group: usize,
        error: Error,
        graph: &ExecGraph<'_>,
        bill: &mut GroupBill,
    ) -> Result<()> {
        bill.add(graph, parked_error_bytes(&error))?;
        self.release(group);
        self.failed.insert(group, error);
        Ok(())
    }

    pub(super) fn settle(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        bill: &mut GroupBill,
    ) -> Result<()> {
        if self.failed.contains_key(&group) {
            return Ok(());
        }
        match &mut self.state {
            SiteState::Retain { vals, charged, .. } => {
                for value in &vals[group][charged[group]..] {
                    let slot_bytes = buffer_growth(
                        charged[group],
                        charged[group] + 1,
                        size_of::<Value>() as u64,
                    );
                    bill.add(graph, slot_bytes)?;
                    bill.add(graph, crate::workspace::owned_heap(value))?;
                    charged[group] += 1;
                }
            }
            SiteState::Stream { folds, held, .. } => {
                if let Some(best) = folds[group].held_value() {
                    let now = crate::workspace::owned_heap(best);
                    if now > held[group] {
                        bill.add(graph, now - held[group])?;
                        held[group] = now;
                    }
                }
            }
            SiteState::CountStar(_) | SiteState::Pooled { .. } | SiteState::Rejected(_) => {}
        }
        Ok(())
    }

    /// Resolve this site for `group`, taking the group's state out.
    pub(super) fn finish(
        &mut self,
        group: usize,
        graph: &ExecGraph<'_>,
        pools: &[SortedPool],
        seed: Option<&Row>,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value> {
        if let Some(error) = self.failed.get(&group) {
            return Err(error.clone());
        }
        // Each retaining shape resolves through the one slot reset its
        // taker performs — the same reset a release performs — so the slot
        // a resolved group leaves behind holds neither its values nor a
        // cursor claiming bytes for them.
        match &mut self.state {
            SiteState::CountStar(counts) => Ok(Value::Int(counts[group])),
            SiteState::Stream {
                template,
                folds,
                held,
                ..
            } => take_stream_slot(template, folds, held, group).finish(),
            SiteState::Retain {
                call,
                vals,
                charged,
            } => finish_retained(
                graph,
                take_retain_slot(vals, charged, group),
                *call,
                seed,
                params,
            ),
            SiteState::Pooled { pool, call } => {
                finish_pooled(graph, &pools[*pool], *call, seed, params)
            }
            SiteState::Rejected(error) => Err(error.clone()),
        }
    }
}

/// The one stream-slot reset: group `group`'s fold taken out, the site's
/// template back in its place, and the held-payload cursor zeroed with the
/// value it described. One policy for the release a parked failure
/// performs and the resolve a finish takes, so the two cannot disagree.
fn take_stream_slot(
    template: &StreamFold,
    folds: &mut [StreamFold],
    held: &mut [u64],
    group: usize,
) -> StreamFold {
    held[group] = 0;
    std::mem::replace(&mut folds[group], template.clone())
}

/// The one retain-slot reset: group `group`'s operands taken out and the
/// charge cursor zeroed with them.
fn take_retain_slot(vals: &mut [Vec<Value>], charged: &mut [usize], group: usize) -> Vec<Value> {
    charged[group] = 0;
    std::mem::take(&mut vals[group])
}

/// The validated percentile argument of a percentile call. The percentile
/// depends only on the group's first row; an empty group has no row to ask
/// and no percentile to report (`None`).
pub(super) fn percentile_input(
    graph: &ExecGraph<'_>,
    call: PercentileCall<'_>,
    seed: Option<&Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Option<Percentile>> {
    let Some(first) = seed else {
        return Ok(None);
    };
    let p = eval(graph, first, call.percentile, params)?;
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
) -> Result<Value> {
    let input = match call {
        Call::Plain { kind, .. } => FoldInput::Many(kind),
        Call::Percentile(p) => match percentile_input(graph, p, seed, params)? {
            Some(percentile) => FoldInput::Percentile(percentile),
            None => return Ok(Value::Null),
        },
    };
    call.name()
        .with_str(|text| fold_aggregate(input, text, vals))
}

/// A pooled percentile's result: the same order of rejections as the
/// per-site fold (a parked evaluation failure is already a failed site, read
/// before this; then the percentile argument, then a non-numeric operand),
/// then the rank in the sample the group sorted once.
pub(super) fn finish_pooled(
    graph: &ExecGraph<'_>,
    pool: &SortedPool,
    call: PercentileCall<'_>,
    seed: Option<&Row>,
    params: &BTreeMap<String, Value>,
) -> Result<Value> {
    let Some(percentile) = percentile_input(graph, call, seed, params)? else {
        return Ok(Value::Null);
    };
    // The sample's own ending, matched where it leaves the pool: only a
    // non-numeric operand is this rejection. A parked pool has no ending
    // here — its sites answer with the parked error before this.
    match pool.ending {
        SampleEnding::NonNumeric => return Err(call.name.with_str(needs_numbers)),
        SampleEnding::Open => {}
    }
    Ok(percentile.rank_in_sorted(&pool.vals))
}

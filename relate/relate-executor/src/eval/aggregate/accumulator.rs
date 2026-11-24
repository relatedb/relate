//! The one aggregate folds shared by every aggregation path — the
//! sum()/avg() accumulator (numeric and duration accumulation behind one
//! strict offer), the per-kind fold over an aggregate call's operand list,
//! and the strict numeric extraction the number-only aggregates share.

use relate_core::temporal::{self, Temporal};
use relate_core::{Error, Result, Value};
use relate_ir::AggregateKind;

use crate::eval::{extreme_offer, Extreme};

/// ADR-0205: the one sum()/avg() accumulator shared by the grouped path
/// and the horizontal rewrite (both through [`fold_aggregate`]) and the
/// one-hop fold ([`crate::exec::agg_fold::StreamAgg`]) — three call sites,
/// one behavior:
/// nulls are skipped, numbers accumulate int/float, durations accumulate
/// component-wise, and mixing numbers with durations is a type error
/// (F-3-1: the copies had already drifted — `sum([0, duration])` was
/// silently accepted on the horizontal path but rejected elsewhere).
/// The accumulator mode (F-6-1): empty until the first non-null offer,
/// then either numeric or duration for the rest of its life. The
/// no-mixing invariant (F-3-2) is the match on this state at each
/// transition — a numeric offer against `Dur` errors, a duration offer
/// against `Num` errors — not a separate guard probing side fields.
#[derive(Clone)]
enum SumState {
    Empty,
    Num {
        int: i64,
        float: f64,
        used_float: bool,
        /// Overflow-safe running mean, used by avg() only when the float
        /// sum below is not finite. `mean - mean/n + value/n` is
        /// algebraically `mean + (value - mean)/n` but never overflows the
        /// intermediate `value - mean`, so two `1.7976931348623157E308`
        /// values average to the max value rather than to infinity.
        /// (Neo4j's own `AvgFunction` uses the unguarded difference; the
        /// pinned corpus is the more forgiving form — `[max, -max]` pins
        /// `0.0`, which the sum path below already yields.)
        mean: f64,
        /// Non-null numeric values seen (avg needs the divisor).
        n: usize,
    },
    Dur {
        acc: Temporal,
        /// Non-null duration values seen (avg needs the divisor).
        n: usize,
    },
}

#[derive(Clone)]
pub(crate) struct SumAcc {
    state: SumState,
}

impl SumAcc {
    pub(crate) fn new() -> Self {
        Self {
            state: SumState::Empty,
        }
    }

    /// Offer one value with the strict sum()/avg() contract: nulls skipped,
    /// anything that is neither number nor duration is a type error.
    /// `name` ("sum"/"avg") labels the error. One offer for both aggregates,
    /// so the grouped, horizontal and fold paths cannot drift apart.
    pub(crate) fn offer(&mut self, v: &Value, name: &str) -> Result<()> {
        match v {
            Value::Int(i) => self.offer_int(*i, name),
            Value::Float(f) => self.offer_float(*f, name),
            Value::Temporal(t @ Temporal::Duration { .. }) => self.offer_duration(t, name),
            Value::Null => Ok(()),
            _ => Err(needs_numbers(name)),
        }
    }

    fn offer_int(&mut self, i: i64, name: &str) -> Result<()> {
        match &mut self.state {
            state @ SumState::Empty => {
                *state = SumState::Num {
                    int: i,
                    float: 0.0,
                    used_float: false,
                    mean: i as f64,
                    n: 1,
                };
            }
            SumState::Num {
                int,
                float,
                used_float,
                mean,
                n,
            } => {
                // sum() promotes to a float once the i64 integer sum leaves
                // its range (Neo4j's SumAggregator): `sum([max, 1])` is
                // 9.223372036854776E18, not a wrapped negative. The partial
                // integer sum folds into the float accumulator at the
                // crossing, so later integers keep accumulating exactly
                // until they too overflow.
                match int.checked_add(i) {
                    Some(sum) => *int = sum,
                    None => {
                        *float += *int as f64 + i as f64;
                        *used_float = true;
                        *int = 0;
                    }
                }
                *n += 1;
                mean_offer(mean, i as f64, *n);
            }
            SumState::Dur { .. } => return Err(Self::mix(name)),
        }
        Ok(())
    }

    fn offer_float(&mut self, f: f64, name: &str) -> Result<()> {
        match &mut self.state {
            state @ SumState::Empty => {
                *state = SumState::Num {
                    int: 0,
                    float: f,
                    used_float: true,
                    mean: f,
                    n: 1,
                };
            }
            SumState::Num {
                float,
                used_float,
                mean,
                n,
                ..
            } => {
                *float += f;
                *used_float = true;
                *n += 1;
                mean_offer(mean, f, *n);
            }
            SumState::Dur { .. } => return Err(Self::mix(name)),
        }
        Ok(())
    }

    /// A duration value: one accumulation arm for both offers (F-1-11/12/16:
    /// the strict and lenient offers shared one copy of this arm each; the
    /// take-add-replace dance would drift the way the three
    /// pre-consolidation copies did).
    fn offer_duration(&mut self, t: &Temporal, name: &str) -> Result<()> {
        match &mut self.state {
            state @ SumState::Empty => {
                *state = SumState::Dur {
                    acc: t.clone(),
                    n: 1,
                };
            }
            SumState::Dur { acc, n } => {
                *acc = temporal::add_duration(acc, t)?;
                *n += 1;
            }
            SumState::Num { .. } => return Err(Self::mix(name)),
        }
        Ok(())
    }

    fn mix(name: &str) -> Error {
        Error::invalid_argument_type(format!("{name}() cannot mix numbers and durations"))
    }

    /// sum(): durations accumulate component-wise; over zero non-null
    /// values the result is 0, matching the reference engine.
    pub(crate) fn finish_sum(self) -> Value {
        match self.state {
            SumState::Dur { acc, .. } => Value::Temporal(acc),
            SumState::Empty => Value::Int(0),
            SumState::Num {
                int,
                float,
                used_float: true,
                ..
            } => Value::Float(float + int as f64),
            SumState::Num { int, .. } => Value::Int(int),
        }
    }

    /// avg(): durations average via the accumulated duration scaled by
    /// 1/n; numbers average as a float; over zero non-null values the
    /// result is null. The numeric result is the running [`SumState::Num`]
    /// mean, never a divided sum — see that field for why.
    pub(crate) fn finish_avg(self) -> Result<Value> {
        match self.state {
            SumState::Dur { acc, n } => Ok(Value::Temporal(temporal::scale_duration(
                &acc,
                1.0 / n as f64,
            )?)),
            SumState::Empty => Ok(Value::Null),
            SumState::Num {
                int,
                float,
                mean,
                n,
                ..
            } => {
                // The plain divided sum is the reference formula and is
                // bit-exact for every non-overflowing input. When that sum
                // overflowed to infinity the running mean takes over (see
                // its field docs) — but only when the mean is itself
                // finite: an input that is already `±Infinity` or NaN makes
                // the mean NaN, and there the divided sum carries the
                // pinned answer (`[-Infinity, 1]` averages to `-Infinity`,
                // `[Infinity, -Infinity]` to NaN).
                let sum = float + int as f64;
                if sum.is_finite() {
                    Ok(Value::Float(sum / n as f64))
                } else if mean.is_finite() {
                    Ok(Value::Float(mean))
                } else {
                    Ok(Value::Float(sum / n as f64))
                }
            }
        }
    }
}

/// Advance the overflow-safe running mean by one value. Equivalent to
/// `mean + (value - mean) / n` but computed as `mean - mean/n + value/n`,
/// whose intermediates stay in range whenever the mean itself is finite.
fn mean_offer(mean: &mut f64, value: f64, n: usize) {
    *mean = *mean - *mean / n as f64 + value / n as f64;
}

/// The one strict numeric extraction for the number-only aggregates
/// (`stDev`, `stDevP`, `percentileCont`, `percentileDisc`): nulls are
/// skipped and every other non-number is the typed `{name}() needs numbers`
/// error. This is Neo4j's contract, not a local choice —
/// `StdevAggregator`'s `failType` raises
/// `onlyNumericalValuesOrNullAllowed(...)`, and the percentile aggregators
/// run every operand through `CypherCoercions.asNumberValue`, which raises
/// for a non-number — so an operand is never silently dropped from the
/// sample or the divisor.
/// A non-null operand that is not a number: the one rejection of the strict
/// numeric extraction, which each caller words with its own call name
/// ([`needs_numbers`]).
pub(crate) struct NotNumeric;

/// The one strict numeric extraction over an aggregate operand: `None` for
/// a null (skipped, as every aggregate skips nulls), the number otherwise,
/// or [`NotNumeric`] — never a silently dropped value. The f64 sample and
/// the value-retaining percentile path both extract through it.
fn operand_number(value: &Value) -> std::result::Result<Option<f64>, NotNumeric> {
    if value.is_null() {
        return Ok(None);
    }
    value.as_number().map(Some).ok_or(NotNumeric)
}

/// The shared percentile pool's operand sample: the strict numeric
/// extraction ([`operand_number`]: nulls skipped, every other non-number
/// rejected, never silently dropped) into the eight-byte keys the pool keeps
/// and the one ascending sort over them. The pooled path is
/// `percentileCont`-only; a retained percentile keeps its values instead
/// ([`Percentile::rank_values`]).
#[derive(Default)]
pub(crate) struct PercentileSample {
    nums: Vec<f64>,
}

impl PercentileSample {
    /// Keep one operand: nothing for a null, the number otherwise.
    pub(crate) fn offer(&mut self, value: &Value) -> std::result::Result<(), NotNumeric> {
        if let Some(number) = operand_number(value)? {
            self.nums.push(number);
        }
        Ok(())
    }

    /// How many numbers the sample holds.
    pub(crate) fn len(&self) -> usize {
        self.nums.len()
    }

    /// The sample in ascending order, through the engine's one total order
    /// ([`Value::order_by_cmp`], whose NaN rank sorts every NaN — either
    /// sign bit — after every number), so the pooled and retained
    /// percentile samples cannot disagree on a position.
    pub(crate) fn into_sorted(mut self) -> Vec<f64> {
        self.nums
            .sort_by(|a, b| Value::Float(*a).order_by_cmp(&Value::Float(*b)));
        self.nums
    }
}

/// The one wrong-type sentence both number-only paths emit — the strict
/// sum()/avg() offer and the strict numeric extraction — so the wording
/// cannot drift between them.
pub(crate) fn needs_numbers(name: &str) -> Error {
    Error::ty(format!("{name}() needs numbers"))
}

/// The one input of [`fold_aggregate`]: a plain aggregate kind, or a
/// percentile paired with its already-validated argument. The pairing is a
/// variant rather than an `Option` beside the kind, so a percentile fold
/// cannot run without its argument and no other kind can carry one.
pub(crate) enum FoldInput {
    Many(AggregateKind),
    Percentile(Percentile),
}

/// A percentile kind paired with its validated argument in [0, 1]. The rank
/// pick hangs off this type, not [`FoldInput`], so it exists only for the two
/// kinds that have one.
#[derive(Clone, Copy)]
pub(crate) enum Percentile {
    Cont(f64),
    Disc(f64),
}

impl Percentile {
    /// The one pairing of a percentile kind with its validated argument:
    /// the arity class says a call carries a percentile, but only these two
    /// kinds own one. The match enumerates every kind — the percentile kinds
    /// build their variant, every other kind is the internal contract
    /// violation — so the pairing lives beside the vocabulary and no caller
    /// re-derives it with a wildcard match over `kind`.
    pub(crate) fn new(kind: AggregateKind, p: f64, name: &str) -> Result<Self> {
        match kind {
            AggregateKind::PercentileCont => Ok(Self::Cont(p)),
            AggregateKind::PercentileDisc => Ok(Self::Disc(p)),
            AggregateKind::Count
            | AggregateKind::Collect
            | AggregateKind::Sum
            | AggregateKind::Avg
            | AggregateKind::Min
            | AggregateKind::Max
            | AggregateKind::StDev
            | AggregateKind::StDevP => Err(Error::internal(format!(
                "{name}() has the percentile arity but not a percentile kind"
            ))),
        }
    }

    /// The percentile over the pooled ascending `f64` sample
    /// ([`PercentileSample::into_sorted`]), shared by every call reading the
    /// pool. Only `percentileCont` is pooled: a discrete percentile answers
    /// an input value, which an f64 image cannot reproduce above 2^53, so
    /// `percentileDisc` stays on the retained path
    /// ([`Self::rank_values`]). An empty sample has no percentile to report
    /// (null); the [0, 1] argument was validated before the fold.
    pub(crate) fn rank_pooled(self, sorted: &[f64]) -> Result<Value> {
        if sorted.is_empty() {
            return Ok(Value::Null);
        }
        match self {
            Self::Cont(p) => Ok(rank_cont(sorted.len(), p, |i| sorted[i])),
            Self::Disc(_) => Err(Error::internal(
                "percentileDisc is not pooled: its answer is an operand value",
            )),
        }
    }

    /// The percentile over the retained operand values — the path for
    /// DISTINCT calls and every `percentileDisc`. The retained vector is
    /// filtered and sorted **in place**, through the one ORDER BY total
    /// order ([`Value::order_by_cmp`]): the retaining site already charged
    /// exactly these operands, and a mirrored `(number, value)` copy would
    /// hold them twice while charging once. The continuous kind reads the
    /// bracketing numbers, and the discrete pick answers an input value: an
    /// INTEGER keeps its identity above 2^53 instead of coming back through
    /// an f64 image. An empty sample is null.
    fn rank_values(self, name: &str, mut vals: Vec<Value>) -> Result<Value> {
        // Validation first, so the typed rejection propagates before any
        // compaction; then `retain` drops the nulls in place.
        for value in &vals {
            operand_number(value).map_err(|NotNumeric| needs_numbers(name))?;
        }
        vals.retain(|value| !value.is_null());
        if vals.is_empty() {
            return Ok(Value::Null);
        }
        vals.sort_by(Value::order_by_cmp);
        match self {
            Self::Cont(p) => Ok(rank_cont(vals.len(), p, |i| value_number(&vals[i]))),
            Self::Disc(p) => Ok(rank_disc(&vals, p)),
        }
    }
}

/// The numeric value of one retained percentile operand, whose `as_number`
/// the pre-pass already accepted.
fn value_number(value: &Value) -> f64 {
    value
        .as_number()
        .expect("the percentile sample holds numbers only")
}

/// The one per-kind fold over an aggregate call's already-evaluated operand
/// values, shared by the grouped evaluator (through
/// [`StreamFold`]) and the horizontal rewrite
/// ([`super::horizontal::horizontal_aggregate`]) so count/collect/min/
/// max/sum/avg/stDev/percentile cannot drift into two behaviours again.
/// Nulls are skipped by every kind — count counts the non-null operands,
/// collect drops them, and min/max report null when no non-null value was
/// seen. `name` is the call's spelling and labels the wrong-type messages;
/// `input` is the typed selector, with the percentile's argument living in
/// the variant that owns it.
pub(crate) fn fold_aggregate(input: FoldInput, name: &str, vals: Vec<Value>) -> Result<Value> {
    match input {
        // ADR-0462: collect() is a dynamic container construction — a
        // chained `WITH collect(a) AS a` adds one level per clause — so it
        // goes through the checked seam like every other constructor.
        FoldInput::Many(AggregateKind::Collect) => {
            Value::checked_list(vals.into_iter().filter(|value| !value.is_null()).collect())
        }
        // Every other plain kind is its streaming accumulator driven over
        // the list — the grouped path folds through the same accumulator row
        // by row, so the two cannot drift. A kind with none (a percentile
        // kind built without its percentile) is a contract violation,
        // reported typed rather than panicking.
        FoldInput::Many(kind) => {
            let Some(mut fold) = StreamFold::for_kind(kind) else {
                return Err(Error::internal(format!(
                    "{name}() has no plain fold: a percentile kind needs its percentile"
                )));
            };
            for value in vals {
                fold.offer(value, name)?;
            }
            fold.finish()
        }
        FoldInput::Percentile(percentile) => percentile.rank_values(name, vals),
    }
}

/// Which standard deviation a [`StreamFold::Deviation`] reports: `stDev`
/// divides by `n - 1`, `stDevP` by `n`.
#[derive(Clone, Copy)]
pub(crate) enum DeviationKind {
    Sample,
    Population,
}

impl DeviationKind {
    /// The divisor of `n` (at least 2 for a sample) squared deviations.
    fn divisor(self, n: usize) -> usize {
        match self {
            Self::Sample => n - 1,
            Self::Population => n,
        }
    }
}

/// One aggregate call's streaming accumulator: the constant-size state a
/// non-retaining aggregate keeps per group instead of its operand values —
/// a counter for `count`, the numeric/duration accumulator for `sum` and
/// `avg`, the best value for `min`/`max`, Welford's `(n, mean, M2)` for the
/// two deviations. Nulls are skipped by every kind. `collect` and the
/// percentiles genuinely retain their operands and have no accumulator
/// ([`StreamFold::for_kind`] is `None`), as does any DISTINCT call (its
/// seen-set is the caller's to hold).
#[derive(Clone)]
pub(crate) enum StreamFold {
    Count(i64),
    Sum(SumAcc),
    Avg(SumAcc),
    Extreme {
        extreme: Extreme,
        best: Option<Value>,
    },
    Deviation {
        kind: DeviationKind,
        n: usize,
        mean: f64,
        m2: f64,
    },
}

impl StreamFold {
    /// The accumulator for `kind`, or `None` for a kind that retains its
    /// operands. The match is exhaustive: a new kind must choose.
    pub(crate) fn for_kind(kind: AggregateKind) -> Option<Self> {
        match kind {
            AggregateKind::Count => Some(Self::Count(0)),
            AggregateKind::Sum => Some(Self::Sum(SumAcc::new())),
            AggregateKind::Avg => Some(Self::Avg(SumAcc::new())),
            AggregateKind::Min => Some(Self::extreme(Extreme::Min)),
            AggregateKind::Max => Some(Self::extreme(Extreme::Max)),
            AggregateKind::StDev => Some(Self::deviation(DeviationKind::Sample)),
            AggregateKind::StDevP => Some(Self::deviation(DeviationKind::Population)),
            AggregateKind::Collect
            | AggregateKind::PercentileCont
            | AggregateKind::PercentileDisc => None,
        }
    }

    fn extreme(extreme: Extreme) -> Self {
        Self::Extreme {
            extreme,
            best: None,
        }
    }

    fn deviation(kind: DeviationKind) -> Self {
        Self::Deviation {
            kind,
            n: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    /// The one operand value a `min`/`max` keeps — the only value any
    /// streaming kind retains, so the caller can charge its payload.
    pub(crate) fn held_value(&self) -> Option<&Value> {
        match self {
            Self::Extreme { best, .. } => best.as_ref(),
            Self::Count(_) | Self::Sum(_) | Self::Avg(_) | Self::Deviation { .. } => None,
        }
    }

    /// Fold one operand value in. `name` labels the wrong-type message of
    /// the number-only kinds; the error is raised at the offending value.
    pub(crate) fn offer(&mut self, value: Value, name: &str) -> Result<()> {
        // Every kind skips a null operand: the one skip, before the arms.
        if value.is_null() {
            return Ok(());
        }
        match self {
            Self::Count(n) => *n += 1,
            Self::Sum(acc) | Self::Avg(acc) => acc.offer(&value, name)?,
            Self::Extreme { extreme, best } => {
                extreme_offer(best, *extreme, value, Value::order_by_cmp);
            }
            Self::Deviation { n, mean, m2, .. } => {
                let x = value.as_number().ok_or_else(|| needs_numbers(name))?;
                *n += 1;
                if *n == 1 {
                    *mean = x;
                    *m2 = 0.0;
                } else {
                    // Welford's online update, not `sum((x - mean)^2)`:
                    // squaring values near `f64::MAX` overflows to +Inf and a
                    // naive sum reports Inf where Cypher reports NaN (and
                    // where a `[MAX, MAX]` sample must stay 0.0). Welford keeps
                    // the mean bounded by the inputs' magnitude, and for a
                    // `[MAX, -MAX]` sample its M2 goes negative infinite, so
                    // the final `sqrt` yields the NaN.
                    let delta = x - *mean;
                    *mean += delta / *n as f64;
                    *m2 += delta * (x - *mean);
                }
            }
        }
        Ok(())
    }

    /// The group's result. Empty-group semantics per kind: `count` and
    /// `sum` are 0, `avg`/`min`/`max`/the deviations are null; a single
    /// sample's `stDev` is 0.0 (its zero divisor must not turn 0/0 into
    /// NaN), ADR-0205.
    pub(crate) fn finish(self) -> Result<Value> {
        match self {
            Self::Count(n) => Ok(Value::Int(n)),
            Self::Sum(acc) => Ok(acc.finish_sum()),
            Self::Avg(acc) => acc.finish_avg(),
            Self::Extreme { best, .. } => Ok(best.unwrap_or(Value::Null)),
            Self::Deviation { n: 0, .. } => Ok(Value::Null),
            Self::Deviation {
                kind: DeviationKind::Sample,
                n: 1,
                ..
            } => Ok(Value::Float(0.0)),
            Self::Deviation { kind, n, m2, .. } => {
                Ok(Value::Float((m2 / kind.divisor(n) as f64).sqrt()))
            }
        }
    }
}

/// `percentileCont` over `len` non-empty ascending operand values, whose
/// number at index `i` is `num_at(i)` — the pooled sample's `f64` buffer and
/// the retained sample's number-beside-value pairs each answer through the
/// one accessor, so the rank arithmetic is stated once. `p` is the
/// already-validated percentile in [0, 1]. Linear interpolation between the
/// two nearest ranks.
///
/// A degenerate interpolation weight selects its endpoint instead of
/// multiplying it in: `f == 0.0` makes `lo == hi` (the single-sample case
/// included), so the formula's other term is `sample * 0.0` — NaN for
/// ±Infinity, which would turn `[inf]` and `[1.0, 2.0, inf]` at `p == 1.0`
/// into NaN rather than ±Infinity.
fn rank_cont(len: usize, p: f64, num_at: impl Fn(usize) -> f64) -> Value {
    let h = p * (len - 1) as f64;
    let lo = h.floor() as usize;
    let hi = h.ceil() as usize;
    let f = h - lo as f64;
    let value = if f == 0.0 {
        num_at(lo)
    } else {
        num_at(lo) * (1.0 - f) + num_at(hi) * f
    };
    Value::Float(value)
}

/// `percentileDisc` over the non-empty ascending operand values; `p` is the
/// already-validated percentile in [0, 1]. The
/// nearest rank at or below the percentile position, answered as the picked
/// input value itself (the discrete percentile returns one of its operands,
/// type included).
///
/// The rank arithmetic is total: `saturating_sub(1)` yields index 0 at
/// `p == 0.0` (where `(0.0f64).ceil() as usize` is 0 and would otherwise
/// underflow the subtraction, wrapping to the last element in release), and
/// the same index as before for every other `p` in [0, 1]. With `p <= 1.0`
/// the ceiling never exceeds `values.len()`, so no further clamp is needed.
fn rank_disc(values: &[Value], p: f64) -> Value {
    let idx = (((values.len() as f64) * p).ceil() as usize).saturating_sub(1);
    values[idx].clone()
}

//! One key's accumulator while its label builds: count, minimum, maximum,
//! sum, a distinct sketch and a heavy-hitter summary.

use std::cmp::Ordering;

use relate_core::Value;
use relate_model::canonical_key::cmp_i64_f64;
use relate_planner::{KeyStats, Provenance};

use super::gate::{Admission, KeyClass};
use super::sketch::{Hll, TopK};
use crate::graph::declared::{order_key, OrderKey};

/// What a key's order-comparable values share, so a minimum and a maximum
/// mean something: numbers compare with numbers, a `DATE` with a `DATE`.
/// Strings and keyless values (points, uuids, references) have no range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Bool,
    Number,
    Date,
    LocalTime,
    Time,
    LocalDateTime,
    DateTime,
    Duration,
}

/// A number as the range orders it: an integer or a float, compared
/// exactly. Nothing is cast between a value and its order key, so two
/// integers above `2^53` stay distinct and an integer and a float order by
/// their exact values.
#[derive(Clone, Copy, Debug)]
pub(super) enum Num {
    Int(i64),
    Float(f64),
}

/// The invariant every `Num` comparison rests on: the type gate declines a
/// NaN before any range sees it, so every pair is ordered, and `-0.0` ties
/// with `0.0` and with `Int(0)` alike.
const NAN_FREE: &str = "the type gate declines a NaN before any range";

impl Num {
    pub(super) fn order(self, other: Num) -> Ordering {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a.cmp(&b),
            (Num::Float(a), Num::Float(b)) => a.partial_cmp(&b).expect(NAN_FREE),
            (Num::Int(i), Num::Float(f)) => cmp_i64_f64(i, f).expect(NAN_FREE),
            (Num::Float(f), Num::Int(i)) => cmp_i64_f64(i, f).expect(NAN_FREE).reverse(),
        }
    }
}

impl PartialEq for Num {
    fn eq(&self, other: &Self) -> bool {
        self.order(*other) == Ordering::Equal
    }
}

impl PartialOrd for Num {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.order(*other))
    }
}

/// The comparable key of a value within its [`Family`]. Two keys are only
/// ever compared inside one family, which fixes the variant.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
enum RangeKey {
    Number(Num),
    Other(OrderKey),
}

/// The family and key of an order-comparable value; `None` for a value
/// with no range (a string, a point, a uuid, a reference).
fn range_key(value: &Value) -> Option<(Family, RangeKey)> {
    let key = match value {
        Value::Int(i) => return Some((Family::Number, RangeKey::Number(Num::Int(*i)))),
        Value::Float(f) => return Some((Family::Number, RangeKey::Number(Num::Float(*f)))),
        _ => order_key(value)?,
    };
    let family = match key {
        OrderKey::Bool(_) => Family::Bool,
        OrderKey::Date(_) => Family::Date,
        OrderKey::LocalTime(_) => Family::LocalTime,
        OrderKey::Time(_) => Family::Time,
        OrderKey::LocalDateTime(_) => Family::LocalDateTime,
        OrderKey::DateTime(_) => Family::DateTime,
        OrderKey::Duration(..) => Family::Duration,
        // Numbers are keyed from their value above, and a string has no
        // range: neither reaches this match from a `Value` that is not an
        // `Int` or a `Float`.
        OrderKey::Int(_) | OrderKey::Float(_) | OrderKey::String(_) => return None,
    };
    Some((family, RangeKey::Other(key)))
}

/// The range of a key: tracking one family, or switched off once a value
/// without a comparable key, or of another family, arrived.
enum Range {
    Empty,
    Tracking {
        family: Family,
        min: (RangeKey, Value),
        max: (RangeKey, Value),
    },
    Off,
}

impl Range {
    fn observe(&mut self, value: &Value) {
        let Some((family, key)) = range_key(value) else {
            *self = Range::Off;
            return;
        };
        match self {
            Range::Empty => {
                *self = Range::Tracking {
                    family,
                    min: (key.clone(), value.clone()),
                    max: (key, value.clone()),
                };
            }
            Range::Tracking {
                family: tracked,
                min,
                max,
            } => {
                if *tracked != family {
                    *self = Range::Off;
                } else if key < min.0 {
                    *min = (key, value.clone());
                } else if key > max.0 {
                    *max = (key, value.clone());
                }
            }
            Range::Off => {}
        }
    }
}

/// One key's running statistics over a label's live nodes.
pub(super) struct Accum {
    /// Admitted values that were text: a key with any is a text key for
    /// the budget's priority.
    text_values: u64,
    admitted: u64,
    hll: Hll,
    top: TopK,
    range: Range,
    numeric: u64,
    int_sum: i128,
    float_sum: f64,
}

impl Accum {
    pub(super) fn new() -> Self {
        Self {
            text_values: 0,
            admitted: 0,
            hll: Hll::new(),
            top: TopK::default(),
            range: Range::Empty,
            numeric: 0,
            int_sum: 0,
            float_sum: 0.0,
        }
    }

    /// The key's budget priority: structured until one text value arrives.
    pub(super) fn class(&self) -> KeyClass {
        if self.text_values == 0 {
            KeyClass::Structured
        } else {
            KeyClass::Text
        }
    }

    pub(super) fn admitted(&self) -> u64 {
        self.admitted
    }

    pub(super) fn observe(&mut self, value: &Value, admission: Admission, hash: u64) {
        self.admitted += 1;
        self.text_values += u64::from(admission == Admission::Text);
        self.hll.insert(hash);
        self.top.observe(hash, value);
        self.range.observe(value);
        match value {
            Value::Int(i) => {
                self.numeric += 1;
                self.int_sum = self.int_sum.saturating_add(i128::from(*i));
            }
            Value::Float(f) => {
                self.numeric += 1;
                self.float_sum += f;
            }
            _ => {}
        }
    }

    /// The published form: `declined` values of this key were not read.
    pub(super) fn finish(self, declined: u64) -> KeyStats {
        let (min, max) = match self.range {
            Range::Tracking { min, max, .. } => (Some(min.1), Some(max.1)),
            Range::Empty | Range::Off => (None, None),
        };
        KeyStats {
            provenance: Provenance::Sketched,
            count: self.admitted,
            declined,
            distinct: self.hll.estimate().min(self.admitted),
            min,
            max,
            numeric_count: self.numeric,
            int_sum: self.int_sum,
            float_sum: self.float_sum,
            heavy: self.top.heavy(self.admitted),
        }
    }
}

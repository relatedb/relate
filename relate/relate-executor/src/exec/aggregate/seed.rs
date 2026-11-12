//! What a group keeps of its first input row (its *seed*).
//!
//! After the fold, the seed is read by exactly five consumers — the
//! inventory lives in [`SeedPlan::plan`]'s doc, the one spelling of it. A
//! projection item that is
//! itself a grouping key needs only the key's first-seen *value* — already
//! evaluated for the key, so the group keeps that one value and not the
//! columns it was computed from. [`SeedPlan::plan`] finds the variables
//! every other consumer reads once, at plan time, so a group keeps only
//! those columns of its first row — often none, never the row's other
//! columns, however wide the input is.
//!
//! The analysis is exact for expressions it can see through. It declines —
//! [`SeedPlan::Whole`], an explicit typed mode, never a silent fallback —
//! and keeps the whole first row for the two shapes whose reads it cannot
//! enumerate:
//!
//! * a `*` projection item (or key), which carries every in-scope variable
//!   through to the output row;
//! * a pattern predicate, pattern comprehension, `shortestPath`, or
//!   `EXISTS`/`COUNT`/`COLLECT` subquery in any finish-time expression: it
//!   opens its own scope that can capture outer variables by name.

use std::collections::HashSet;
use std::sync::Arc;

use relate_core::{Error, Node, Result, Value};
use relate_ir::{classify_aggregate, Expr, MapProjectionItem};

use crate::row::{Row, RowNames};

use super::key::KeyValues;

/// A kept value in 24 bytes, not a 48-byte [`Value`]: the dominant seed
/// values (null, boolean, integer, float, string, node) are held inline or
/// by their shared pointer; every other value is boxed.
#[derive(Default)]
pub(super) enum Kept {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Arc<str>),
    Node(Arc<Node>),
    Other(Box<Value>),
}

impl Kept {
    fn new(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(b),
            Value::Int(i) => Self::Int(i),
            Value::Float(f) => Self::Float(f),
            Value::String(s) => Self::Str(s),
            Value::Node(n) => Self::Node(n),
            other => Self::Other(Box::new(other)),
        }
    }

    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(i) => Value::Int(i),
            Self::Float(f) => Value::Float(f),
            Self::Str(s) => Value::String(s),
            Self::Node(n) => Value::Node(n),
            Self::Other(boxed) => *boxed,
        }
    }

    /// Heap beyond the inline slot: the box of an `Other`, plus the
    /// payload the group solely owns (a shared payload adds nothing).
    fn heap_bytes(value: &Value) -> u64 {
        let boxed = match value {
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::String(_)
            | Value::Node(_) => 0,
            _ => size_of::<Value>() as u64,
        };
        boxed + crate::workspace::owned_heap(value)
    }
}

/// What a finished group's seed hands back: the narrowed first row (`None`
/// for the keyless group of an empty input, which never landed one) and the
/// first-seen values of the grouping keys the projection items read.
pub(super) struct Taken {
    pub(super) row: Option<Row>,
    pub(super) keys: Vec<Value>,
}

/// The decision for one aggregation.
#[derive(Debug)]
pub(crate) enum SeedPlan {
    /// Keep the first row whole (the conservative mode; triggers in the
    /// module docs).
    Whole,
    /// Keep only the columns of the first row named here (those it has).
    Columns(HashSet<String>),
}

/// The finish-time reads, accumulated over every consumer expression.
struct Reads {
    names: HashSet<String>,
    whole: bool,
}

impl Reads {
    /// The variables `expr` reads outside its aggregate calls: an
    /// aggregate call's operands were evaluated against each input row
    /// while it streamed and are not read again. Comprehension-bound
    /// names are collected too (an over-approximation that only retains a
    /// column that exists under that name).
    fn leaves(&mut self, expr: &Expr) {
        match expr {
            Expr::Star => self.whole = true,
            Expr::CountStar => {}
            Expr::Function { name, args, .. } => {
                if name.with_str(classify_aggregate).is_none() {
                    args.iter().for_each(|arg| self.leaves(arg));
                }
            }
            Expr::Variable(name) => {
                self.names.insert(name.to_string());
            }
            Expr::MapProjection { expr, items } => {
                for item in items {
                    if let MapProjectionItem::Variable { name } = item {
                        self.names.insert(name.to_string());
                    }
                }
                self.leaves(expr);
                for item in items {
                    if let MapProjectionItem::KeyValue { value, .. } = item {
                        self.leaves(value);
                    }
                }
            }
            scoped if scoped.is_scope_bearing() => {
                self.whole = true;
            }
            other => other.each_child(&mut |child| self.leaves(child)),
        }
    }
}

impl SeedPlan {
    /// The plan for an aggregation whose seed is read by `finish_reads`.
    /// The consumer inventory — the one spelling of it, which the module
    /// doc points at: the non-aggregate leaves of the projection items, a
    /// percentile's second argument, the `ORDER BY` keys, HAVING, and the
    /// plain post-aggregation WHERE (the predicate the projection applies
    /// to the emitted rows, so its reads must survive the narrowing) —
    /// must arrive whole in `finish_reads`; a consumer the chain omits
    /// loses its columns. `star` is true when a `*` item or key is in
    /// play.
    pub(crate) fn plan<'e>(star: bool, finish_reads: impl IntoIterator<Item = &'e Expr>) -> Self {
        if star {
            return Self::Whole;
        }
        let mut reads = Reads {
            names: HashSet::new(),
            whole: false,
        };
        for expr in finish_reads {
            reads.leaves(expr);
        }
        if reads.whole {
            Self::Whole
        } else {
            Self::Columns(reads.names)
        }
    }
}

/// The columns a narrowed seed keeps, fixed by the first row that lands.
pub(super) struct Resolved {
    names: RowNames,
    /// The kept column names, in the first row's column order.
    columns: Vec<String>,
}

/// Every group's seed, flat: one `Kept` buffer of `stride` slots per group —
/// the first-seen values of the grouping keys an item reads, then the kept
/// first-row columns — (or one whole `Row` per group in whole-row mode),
/// indexed by group.
pub(super) enum SeedStore {
    Whole(Vec<Row>),
    Columns {
        wanted: HashSet<String>,
        resolved: Option<Resolved>,
        /// Which of the key's values (by key position) each group keeps.
        key_slots: Vec<usize>,
        values: Vec<Kept>,
    },
}

impl SeedStore {
    pub(super) fn new(plan: SeedPlan, key_slots: Vec<usize>) -> Self {
        match plan {
            SeedPlan::Whole => Self::Whole(Vec::new()),
            SeedPlan::Columns(wanted) => Self::Columns {
                wanted,
                resolved: None,
                key_slots,
                values: Vec::new(),
            },
        }
    }

    /// Bytes the next landing adds: the slot growth of the flat buffer
    /// plus the payloads the kept values solely own. Read before the row
    /// is offered to the accumulators, while it still solely owns its
    /// payload (the same order the old whole-row charge used).
    pub(super) fn landing_bytes(&self, groups: usize, keys: &KeyValues, row: &Row) -> Result<u64> {
        use crate::workspace::{buffer_growth, retained_rows_bytes, row_retained_heap};
        match self {
            Self::Whole(_) => {
                Ok(row_retained_heap(row.values()) + retained_rows_bytes(1, row.values().len()))
            }
            Self::Columns {
                wanted,
                resolved,
                key_slots,
                ..
            } => {
                let cols: Vec<&Value> = match resolved {
                    Some(resolved) => resolved
                        .columns
                        .iter()
                        .map(|name| {
                            row.get(name).ok_or_else(|| {
                                Error::internal(
                                    "an aggregation input row lost a column its first row had",
                                )
                            })
                        })
                        .collect::<Result<_>>()?,
                    None => row
                        .iter()
                        .filter(|(name, _)| wanted.contains(*name))
                        .map(|(_, value)| value)
                        .collect(),
                };
                let stride = key_slots.len() + cols.len();
                Ok(buffer_growth(
                    groups * stride,
                    (groups + 1) * stride,
                    size_of::<Kept>() as u64,
                ) + key_slots
                    .iter()
                    .map(|slot| Kept::heap_bytes(keys.get(*slot)))
                    .chain(cols.iter().map(|value| Kept::heap_bytes(value)))
                    .sum::<u64>())
            }
        }
    }

    /// Move the landing group's key values and first-row columns in as its
    /// seed.
    pub(super) fn land(&mut self, mut keys: KeyValues, mut row: Row) -> Result<()> {
        match self {
            Self::Whole(rows) => rows.push(row),
            Self::Columns {
                wanted,
                resolved,
                key_slots,
                values,
            } => {
                let resolved = resolved.get_or_insert_with(|| {
                    let columns: Vec<String> = row
                        .keys()
                        .filter(|name| wanted.contains(*name))
                        .cloned()
                        .collect();
                    Resolved {
                        names: RowNames::new(&columns),
                        columns,
                    }
                });
                for slot in key_slots.iter() {
                    values.push(Kept::new(keys.take(*slot)));
                }
                for name in &resolved.columns {
                    values.push(Kept::new(row.take_value(name).ok_or_else(|| {
                        Error::internal("an aggregation input row lost a column its first row had")
                    })?));
                }
            }
        }
        Ok(())
    }

    /// Take group `group`'s seed out (its slots are left empty). The row is
    /// `None` when the store holds no seed for it: the keyless group of an
    /// empty input, which never landed one.
    pub(super) fn take(&mut self, group: usize) -> Taken {
        match self {
            Self::Whole(rows) => Taken {
                row: rows.get_mut(group).map(std::mem::take),
                keys: Vec::new(),
            },
            Self::Columns {
                resolved,
                key_slots,
                values,
                ..
            } => {
                let Some(resolved) = resolved.as_ref() else {
                    return Taken {
                        row: None,
                        keys: Vec::new(),
                    };
                };
                let stride = key_slots.len() + resolved.columns.len();
                let mut slots = values[group * stride..(group + 1) * stride]
                    .iter_mut()
                    .map(|kept| std::mem::take(kept).into_value());
                let keys = slots.by_ref().take(key_slots.len()).collect();
                let columns = slots.collect();
                Taken {
                    row: Some(Row::from_names(&resolved.names, columns)),
                    keys,
                }
            }
        }
    }
}

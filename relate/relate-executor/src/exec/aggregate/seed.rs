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
//! and keeps the whole first row for the one shape whose reads it cannot
//! enumerate: a `*` projection item (or key), which carries every in-scope
//! variable through to the output row.
//!
//! A nested scope in a finish-time expression narrows instead of declining:
//! an `EXISTS`/`COUNT`/`COLLECT` body keeps its *correlated* reads — the
//! names it reads and does not bind ([`relate_ir::correlated_reads`]) — so a
//! body that re-uses a projection-dropped name binds it fresh (REL8-2023
//! issue 64) while one that reads an enclosing name still finds it in the
//! seed row; a pattern-led scope (pattern predicate, pattern comprehension,
//! `shortestPath`) keeps the whole row, its reads not being enumerated.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashSet;
use std::sync::Arc;

use relate_core::{Error, Node, Result, Value};
use relate_ir::{is_agg_call, map_projection_shorthand_names, Expr, Projection};

use crate::eval::is_agg_expr;
use crate::row::{Row, RowNames};
use crate::workspace::{buffer_growth, owned_heap, row_entry_bytes};

use super::super::scope::is_star_item;
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

    /// Whether `value` is held boxed: the partition [`Self::heap_bytes`]
    /// bills by. [`Self::new`]'s inline arms and catch-all construct the
    /// same partition in lockstep — the two lists are held together by
    /// `kept_boxes_exactly_the_values_billed_as_boxed`, which fails the
    /// moment a value moves between them.
    fn boxes(value: &Value) -> bool {
        !matches!(
            value,
            Value::Null
                | Value::Bool(_)
                | Value::Int(_)
                | Value::Float(_)
                | Value::String(_)
                | Value::Node(_)
        )
    }

    /// Heap beyond the inline slot: the box of an `Other`, plus the
    /// payload the group solely owns (a shared payload adds nothing).
    fn heap_bytes(value: &Value) -> u64 {
        let boxed = if Self::boxes(value) {
            size_of::<Value>() as u64
        } else {
            0
        };
        boxed + owned_heap(value)
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
            // The one `*` decline: the item carries every in-scope variable
            // through to the output row.
            Expr::Star => self.whole = true,
            // An aggregate call's operands were folded while the rows
            // streamed; every other call rides the one child walk below.
            call if is_agg_call(call) => {}
            Expr::Variable(name) => {
                self.names.insert(name.to_string());
            }
            // The shorthand items are reads no child holds; the base
            // expression and every `KeyValue` value ride the one child walk.
            mp @ Expr::MapProjection { items, .. } => {
                for name in map_projection_shorthand_names(items) {
                    self.names.insert(name.to_string());
                }
                mp.each_child(&mut |child| self.leaves(child));
            }
            // A nested scope's *correlated* reads are this row's: an
            // aggregating projection ends its input names' scope, and the
            // body of a subquery expression is executed against the emitted
            // row, so it needs the enclosing names it reads seeded. A name
            // the body's own patterns bind is not seeded — the body binds it
            // fresh instead of matching the group's first-row value
            // (Neo4j-verified, REL8-2023 issue 64), which a seeded column
            // would turn into a pre-bound match. The pattern-less scopes
            // (a pattern predicate, a pattern comprehension, a shortest
            // path) keep the whole row: their reads are not enumerated here.
            scoped if scoped.is_scope_bearing() => match scoped.subquery_body() {
                Some(query) => {
                    // The one IR derivation: the body's reads minus the
                    // names its own patterns bind, so a body that re-uses a
                    // projection-dropped name binds it fresh (issue 64)
                    // while one that reads an enclosing name still finds it.
                    let mut reads = HashSet::new();
                    relate_ir::correlated_reads(query, &mut reads);
                    self.names.extend(reads);
                }
                // The pattern-led scopes keep the whole row: their reads are
                // not enumerated.
                _ => self.whole = true,
            },
            other => other.each_child(&mut |child| self.leaves(child)),
        }
    }
}

impl SeedPlan {
    /// The plan for an aggregation whose seed is read by `finish_reads`.
    /// The consumer inventory — the one spelling of it, which the module
    /// doc points at: the non-aggregate leaves of the projection items (a
    /// `*` item among them, which declines the narrowing), a percentile's
    /// second argument, the `ORDER BY` keys, HAVING, and the plain
    /// post-aggregation WHERE (the predicate the projection applies to the
    /// emitted rows, so its reads must survive the narrowing) — must
    /// arrive whole in `finish_reads`; a consumer the chain omits loses
    /// its columns.
    fn plan<'e>(finish_reads: impl IntoIterator<Item = &'e Expr>) -> Self {
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

/// How the finish answers one projection item, classified once per item
/// with its final meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemRole {
    /// The `*` item: its variables are already in the seed row.
    Star,
    /// An aggregate expression: answered by the group's folded sites; its
    /// non-aggregate leaves (`sum(x) + tax`) still read the seed.
    Aggregate,
    /// A pure non-aggregate item that is a grouping key: answered by the
    /// key's first-seen value; the payload is the item's kept key slot
    /// (items restating one key share the slot).
    Key(usize),
    /// Any other item: evaluated against the seed row.
    Leaf,
}

/// The columns a narrowed seed keeps, fixed by the first row that lands.
pub(super) struct Resolved {
    /// The kept columns, in the first row's column order.
    names: RowNames,
}

impl Resolved {
    /// Resolve the kept columns from a group's first row.
    fn from_first_row(wanted: &HashSet<String>, row: &Row) -> Self {
        Self {
            names: RowNames::new(&kept_columns(wanted, row)),
        }
    }

    /// The kept column names, in the first row's column order.
    fn columns(&self) -> &[String] {
        self.names.names()
    }
}

/// The invariant error: every input row carries the columns its first row
/// had.
fn lost_column() -> Error {
    Error::internal("an aggregation input row lost a column its first row had")
}

/// Every group's seed, flat: one `Kept` buffer of `stride` slots per group —
/// the first-seen values of the grouping keys an item reads, then the kept
/// first-row columns — (or one whole `Row` per group in whole-row mode),
/// indexed by group.
pub(super) enum SeedStore {
    Whole(Vec<Row>),
    Columns {
        wanted: HashSet<String>,
        /// Resolved once, by whichever of the landing bill or the landing
        /// sees the first row first.
        resolved: OnceCell<Resolved>,
        /// The key positions whose first-seen values a group keeps, one per
        /// kept slot — the dedup the classification builds as items
        /// restate keys.
        key_slots: Vec<usize>,
        values: Vec<Kept>,
    },
}

/// The item-kind precedence, spelled once: a `*` item, then an aggregate
/// expression, then the residual a grouping key may answer (`None`).
pub(super) fn base_role(item: &Projection) -> Option<ItemRole> {
    match (is_star_item(item), is_agg_expr(&item.expr)) {
        (true, _) => Some(ItemRole::Star),
        (_, true) => Some(ItemRole::Aggregate),
        _ => None,
    }
}

/// The kept slots one group holds in the flat buffer: the key values its
/// items read, then `columns` first-row columns. The one spelling of the
/// stride — the landing bill and the take both read it, so what is billed
/// and what is sliced out cannot disagree.
fn slot_stride(key_slots: &[usize], columns: usize) -> usize {
    key_slots.len() + columns
}

/// The expression's position in the grouping keys, `None` when it restates
/// none — the one spelling of the key relation, consulted by the seed-read
/// filter and the kept-slot assignment alike.
fn key_position(key_exprs: &[Cow<'_, Expr>], expr: &Expr) -> Option<usize> {
    key_exprs.iter().position(|key| **key == *expr)
}

/// The columns of a group's first row the seed keeps — those of `wanted`
/// the row has, in its column order. The one selection both the landing
/// bill and the landing itself read, so what is billed and what is kept
/// cannot disagree.
fn kept_columns(wanted: &HashSet<String>, first_row: &Row) -> Vec<String> {
    first_row
        .keys()
        .filter(|name| wanted.contains(*name))
        .cloned()
        .collect()
}

impl SeedStore {
    /// Plan the seed of one aggregation and classify its items — each role
    /// built once with its final meaning. A grouping key an item restates
    /// is answered by the key's first-seen value (kept once per group, so
    /// the kept-slot assignment runs inside the classification); the
    /// finish reads the seed only for the remaining items and the
    /// `consumers` (`ORDER BY` keys, HAVING, the plain WHERE, the
    /// percentile arguments — see [`SeedPlan::plan`]) — and keeps the
    /// whole row only where the analysis declines, in which case no item
    /// is answered by a kept key value at all.
    pub(super) fn planned<'e>(
        items: &'e [Projection],
        key_exprs: &[Cow<'_, Expr>],
        consumers: impl IntoIterator<Item = &'e Expr>,
    ) -> (Self, Vec<ItemRole>) {
        // A key's first-seen value answers its items, so the seed-read
        // analysis skips the items a key answers — planned before the
        // roles are built, so no role is ever rewritten.
        let plan = SeedPlan::plan(
            items
                .iter()
                .map(|item| &item.expr)
                .filter(|expr| key_position(key_exprs, expr).is_none())
                .chain(consumers),
        );
        let SeedPlan::Columns(mut wanted) = plan else {
            // The whole row answers: no item reads a kept key value, so no
            // item is answered by one.
            let roles = items
                .iter()
                .map(|item| base_role(item).unwrap_or(ItemRole::Leaf))
                .collect();
            return (Self::Whole(Vec::new()), roles);
        };
        // A bare-variable grouping key is in scope *by its own name* inside a
        // nested body of the projection's expressions (`RETURN a, COUNT {
        // RETURN a.p + 1 + count(a) }` — GroupByAcceptance 696), which the
        // item's own evaluation scope supplies and a sibling item's does not,
        // so the group keeps it whatever the consumers read. A property key
        // (`WITH n.g AS g, count(*)`) exposes no such name: `n` is out of
        // scope there, and a body that re-uses it binds it fresh
        // (REL8-2023 issue 64).
        for key in key_exprs {
            if let Expr::Variable(name) = &**key {
                wanted.insert(name.to_string());
            }
        }
        // Two items restating one key share its kept slot; the slot is
        // assigned here, in the classification, and read nowhere else.
        let mut key_slots: Vec<usize> = Vec::new();
        let roles = items
            .iter()
            .map(|item| {
                base_role(item).unwrap_or_else(|| {
                    key_position(key_exprs, &item.expr).map_or(ItemRole::Leaf, |key| {
                        let slot = key_slots
                            .iter()
                            .position(|kept| *kept == key)
                            .unwrap_or_else(|| {
                                key_slots.push(key);
                                key_slots.len() - 1
                            });
                        ItemRole::Key(slot)
                    })
                })
            })
            .collect();
        let store = Self::Columns {
            wanted,
            resolved: OnceCell::new(),
            key_slots,
            values: Vec::new(),
        };
        (store, roles)
    }

    /// Bytes the next landing adds: the slot growth of the flat buffer
    /// plus the payloads the kept values solely own. Read before the row
    /// is offered to the accumulators, while it still solely owns its
    /// payload (the same order the old whole-row charge used).
    pub(super) fn landing_bytes(&self, groups: usize, keys: &KeyValues, row: &Row) -> Result<u64> {
        match self {
            Self::Whole(_) => Ok(row_entry_bytes(row.values())),
            Self::Columns {
                wanted,
                resolved,
                key_slots,
                ..
            } => {
                // The first row resolves the columns once; `land` and every
                // later row read the fixed set.
                let resolved = resolved.get_or_init(|| Resolved::from_first_row(wanted, row));
                let cols: Vec<&Value> = resolved
                    .columns()
                    .iter()
                    .map(|name| row.get(name).ok_or_else(lost_column))
                    .collect::<Result<_>>()?;
                let stride = slot_stride(key_slots, cols.len());
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
                let resolved = resolved.get_or_init(|| Resolved::from_first_row(wanted, &row));
                // Materialize the whole slot run first, then push: a missing
                // column leaves the buffer at a whole number of strides.
                let mut slots =
                    Vec::with_capacity(slot_stride(key_slots, resolved.columns().len()));
                for slot in key_slots.iter() {
                    slots.push(Kept::new(keys.take(*slot)));
                }
                for name in resolved.columns() {
                    slots.push(Kept::new(row.take_value(name).ok_or_else(lost_column)?));
                }
                values.extend(slots);
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
                let Some(resolved) = resolved.get() else {
                    return Taken {
                        row: None,
                        keys: Vec::new(),
                    };
                };
                let stride = slot_stride(key_slots, resolved.columns().len());
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

#[cfg(test)]
mod tests {
    use relate_ir::Expr;

    use super::*;
    use crate::exec::test_support::{agg_fn_over, node_value};

    fn var(name: &str) -> Expr {
        Expr::Variable(name.into())
    }

    fn columns(plan: SeedPlan) -> Vec<String> {
        let SeedPlan::Columns(names) = plan else {
            panic!("a narrowed plan")
        };
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        names
    }

    /// An aggregate call's operands are evaluated while the rows stream;
    /// the finish never reads them again.
    #[test]
    fn aggregate_operands_are_not_seed_reads() {
        let count_x = agg_fn_over("count", var("x"));
        assert!(columns(SeedPlan::plan([&count_x])).is_empty());
        let mixed = Expr::Binary {
            op: relate_ir::BinaryOp::Add,
            left: Box::new(agg_fn_over("sum", var("x"))),
            right: Box::new(var("tax")),
        };
        assert_eq!(columns(SeedPlan::plan([&mixed])), ["tax"]);
    }

    /// The `*` item declines the narrowing; a subquery expression keeps its
    /// *correlated* reads and drops the names its own patterns bind (a body
    /// that re-uses a projection-dropped name binds it fresh, REL8-2023
    /// issue 64); a pattern-only scope keeps the whole row.
    #[test]
    fn star_declines_and_captured_scopes_narrow_to_their_row_reads() {
        assert!(matches!(SeedPlan::plan([&Expr::Star]), SeedPlan::Whole));
        // A body whose only names are its own pattern's binds nothing from
        // the row: `a` must bind fresh, not match the first row's value.
        let query =
            relate_front::parse_and_lower("RETURN EXISTS { (a)-[:E]->() } AS x").expect("parses");
        let Some(relate_ir::Clause::Return { items, .. }) = query.parts[0].clauses.last() else {
            panic!("a RETURN")
        };
        assert!(columns(SeedPlan::plan([&items[0].expr])).is_empty());
        // The subquery body's correlated read `k` is the group row's; the
        // name its own pattern binds (`a`) is not seeded, so the body binds
        // it fresh instead of matching the first row's value.
        let query = relate_front::parse_and_lower(
            "WITH 1 AS k RETURN COUNT { MATCH (a) WHERE a.p = k RETURN a } AS x",
        )
        .expect("parses");
        let Some(relate_ir::Clause::Return { items, .. }) = query.parts[0].clauses.last() else {
            panic!("a RETURN")
        };
        assert_eq!(columns(SeedPlan::plan([&items[0].expr])), ["k"]);
    }

    /// The one partition: a value is billed a box exactly when it is kept
    /// in one.
    #[test]
    fn kept_boxes_exactly_the_values_billed_as_boxed() {
        let list = Value::List(Arc::new(vec![Value::Int(1)]));
        for (value, boxed) in [
            (Value::Null, false),
            (Value::Bool(true), false),
            (Value::Int(1), false),
            (Value::Float(1.5), false),
            (Value::String("s".into()), false),
            (node_value(1), false),
            (list, true),
        ] {
            assert_eq!(Kept::boxes(&value), boxed, "{value:?}");
            assert_eq!(
                matches!(Kept::new(value), Kept::Other(_)),
                boxed,
                "boxing disagrees with the billing partition"
            );
        }
    }

    /// A landing that finds a column missing fails whole: the flat buffer
    /// stays at a whole number of strides, key slots included.
    #[test]
    fn a_failed_landing_leaves_the_buffer_whole() {
        let mut store = SeedStore::Columns {
            wanted: ["a", "b"].map(String::from).into_iter().collect(),
            resolved: OnceCell::new(),
            key_slots: vec![0],
            values: Vec::new(),
        };
        let mut first = Row::new();
        first.insert("a".into(), Value::Int(1));
        first.insert("b".into(), Value::Int(2));
        store
            .land(KeyValues::One(Value::Int(9)), first)
            .expect("first");
        let mut short = Row::new();
        short.insert("a".into(), Value::Int(3));
        assert!(store.land(KeyValues::One(Value::Int(9)), short).is_err());
        let SeedStore::Columns { values, .. } = &store else {
            panic!("a narrowed store")
        };
        assert_eq!(values.len(), 3);
    }
}

//! `UNWIND` row carry.
//!
//! Expanding a list copies the input row once per element. When that row
//! still holds the list (`UNWIND file.symbols AS row` while `file` contains
//! `symbols`), each copy is another full list and a file of N symbols costs
//! N². The list is removed from the carried row when the rest of the clause
//! list does not read it. Sibling fields (`file.path`, `file.language`) stay.
//! A later read of the list keeps the copy, so `RETURN file.symbols` is
//! unchanged.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use relate_core::{Result, Value};
use relate_ir::{var_prop_owned, walk_clauses, Clause, Expr, MapProjectionItem};
use relate_names::SmallName;

use crate::ctx::ExecCtx;
use crate::eval::{eval, into_items, lazy_range, Items, RangeValues};
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::feed::StreamedFeed;
use super::profile_tick::StreamedStats;
use super::scope::call_scope_reads;

pub(super) enum Source {
    /// `UNWIND xs AS x`: drop the `xs` column.
    Column(String),
    /// `UNWIND file.symbols AS row`: drop that key from the map, node, or
    /// relationship bound to `var`.
    Key { var: String, key: String },
}

/// Reads of `source` in one clause suffix. A property base (`file` in
/// `file.path`) is one variable occurrence and one base, so it does not by
/// itself keep the list. A bare `file` is a variable occurrence with no
/// matching base.
struct Hits {
    reads: bool,
    vars: u32,
    bases: u32,
}

impl Source {
    /// Strip decision for this `UNWIND`. `None` means every carried row keeps
    /// the source. The suffix walk runs once; applying it does not.
    pub(super) fn plan(expression: &Expr, suffix: &[Clause]) -> Option<Self> {
        let source = source(expression)?;
        if suffix_keeps(&source, suffix) {
            None
        } else {
            Some(source)
        }
    }

    pub(super) fn strip(&self, row: &mut Row) {
        match self {
            Source::Column(name) => {
                row.shift_remove(name);
            }
            Source::Key { var, key } => {
                if let Some(value) = row.get_mut(var) {
                    clear_key(value, key);
                }
            }
        }
    }

    fn var(&self) -> &str {
        match self {
            Source::Column(name) => name,
            Source::Key { var, .. } => var,
        }
    }

    fn reads_property(&self, key: &str) -> bool {
        match self {
            Source::Column(_) => true,
            Source::Key { key: want, .. } => key == want,
        }
    }

    fn reads_projection(&self, items: &[MapProjectionItem]) -> bool {
        match self {
            Source::Column(_) => true,
            Source::Key { key, .. } => items.iter().any(|item| match item {
                MapProjectionItem::AllProperties => true,
                MapProjectionItem::Property { key: item_key } => item_key == key.as_str(),
                // A variable selector reads a row variable. `note` counts
                // that whether or not this projection's base is the source.
                MapProjectionItem::Variable { .. } | MapProjectionItem::KeyValue { .. } => false,
            }),
        }
    }
}

fn source(expression: &Expr) -> Option<Source> {
    match expression {
        Expr::Variable(name) => Some(Source::Column(name.to_string())),
        other => var_prop_owned(other).map(|(var, key)| Source::Key { var, key }),
    }
}

fn clear_key(value: &mut Value, key: &str) {
    match value {
        Value::Map(map) => {
            // Shared with a parameter or another row (ADR-0170). Copy the
            // map only then, so the removal stays on this row.
            Arc::make_mut(map).remove(key);
        }
        // Shared with another row (the value is an `Arc`). Copy the entity
        // only when this removal is not the sole owner.
        Value::Node(node) => {
            Arc::make_mut(&mut Arc::make_mut(node).properties).remove(key);
        }
        Value::Relationship(rel) => {
            Arc::make_mut(&mut Arc::make_mut(rel).properties).remove(key);
        }
        _ => {}
    }
}

fn suffix_keeps(source: &Source, suffix: &[Clause]) -> bool {
    // A `CALL { … }` clause reads the row without any expression naming
    // it — tracked beside the expression accumulator (the two visitors
    // capture different locals, so both are plain `&mut` closures).
    let mut row_read = false;
    let mut hits = Hits {
        reads: false,
        vars: 0,
        bases: 0,
    };
    walk_clauses(
        suffix,
        &mut |clause| row_read |= call_scope_reads_row(clause, source),
        &mut |expr| note(expr, source, &mut hits),
    );
    row_read || hits.reads || hits.vars > hits.bases
}

/// Whether a clause reads the row without an expression naming it: the
/// naked and `(*)` `CALL { … }` scopes take the whole row, and an import
/// list must find every name it names — the one [`call_scope_reads`] rule
/// [`super::pushdown::live_from`] applies to the same clause.
fn call_scope_reads_row(clause: &Clause, source: &Source) -> bool {
    let Clause::CallSubquery { scope, .. } = clause else {
        return false;
    };
    call_scope_reads(scope).is_none_or(|vars| vars.iter().any(|name| name == source.var()))
}

fn note(expr: &Expr, source: &Source, hits: &mut Hits) {
    let var = source.var();
    match expr {
        Expr::Star => hits.reads = true,
        Expr::Variable(name) if name == var => hits.vars += 1,
        Expr::Property { expr, key, .. } => {
            if note_base(expr, var, hits) && key.with_str(|text| source.reads_property(text)) {
                hits.reads = true;
            }
        }
        Expr::DynamicProperty { expr, .. } => {
            if note_base(expr, var, hits) {
                hits.reads = true;
            }
        }
        Expr::MapProjection { expr, items } => {
            // A variable selector reads that name from the row. It is not a
            // child expression, and the projection base is often a different
            // variable (`mm { file }`).
            if items
                .iter()
                .any(|item| matches!(item, MapProjectionItem::Variable { name } if name == var))
            {
                hits.reads = true;
            }
            if note_base(expr, var, hits) && source.reads_projection(items) {
                hits.reads = true;
            }
        }
        _ => {}
    }
}

/// Count one base when `expr` is exactly the source variable.
fn note_base(expr: &Expr, var: &str, hits: &mut Hits) -> bool {
    match expr {
        Expr::Variable(name) if name == var => {
            hits.bases += 1;
            true
        }
        _ => false,
    }
}

/// The elements one input row's `UNWIND` source yields, walked one at a
/// time: a `range(...)` call is a counter (no list is ever built), an
/// evaluated list moves or clones its elements as the walk reaches them, and
/// a non-list scalar is its own single element.
pub(super) enum Elements {
    Range(RangeValues),
    List(Items),
    One(Option<Value>),
}

impl Elements {
    /// Elements not yet yielded.
    pub(super) fn remaining(&self) -> Result<usize> {
        match self {
            Elements::Range(range) => range.remaining(),
            Elements::List(items) => Ok(items.len()),
            Elements::One(value) => Ok(usize::from(value.is_some())),
        }
    }
}

impl Iterator for Elements {
    type Item = Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Elements::Range(range) => range.next(),
            Elements::List(items) => items.next().map(Ok),
            Elements::One(value) => value.take().map(Ok),
        }
    }
}

/// Evaluate `expression` for one input row without materializing a
/// generator source: `range(...)` becomes a counter, everything else is
/// evaluated once (a list is then walked, never copied wholesale).
pub(super) fn open_source(
    graph: &ExecGraph<'_>,
    row: &Row,
    expression: &Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Elements> {
    if let Expr::Function { name, args, .. } = expression {
        let plan = name.with_str(|text| lazy_range(graph, row, params, text, args))?;
        if let Some(plan) = plan {
            return Ok(Elements::Range(plan.values()));
        }
    }
    Ok(match eval(graph, row, expression, params)? {
        Value::List(xs) => Elements::List(into_items(xs)),
        // A null source yields no rows; any other scalar is its own
        // single element.
        Value::Null => Elements::One(None),
        other => Elements::One(Some(other)),
    })
}

/// The `UNWIND` producer: the [`FeedSource`](super::feed::FeedSource) the
/// shared spine drives — the generator form of the clause, its rows
/// produced one at a time and never retained, so a consumer that folds
/// rows as they arrive (an aggregation) holds constant state however long
/// the source is, and a consumer that stops early never generates the
/// rest. Owns the expanded expression, the alias, the strip decision (the
/// row carry this module's head documents), and the input row being
/// expanded with its remaining elements.
pub(crate) struct UnwindSource<'a> {
    expression: &'a Expr,
    alias: &'a SmallName,
    strip: Option<Source>,
    /// The input row being expanded, with its remaining elements.
    current: Option<(Row, Elements)>,
}

impl<'a> UnwindSource<'a> {
    pub(super) fn new(tail: &[Clause], expression: &'a Expr, alias: &'a SmallName) -> Self {
        Self {
            expression,
            alias,
            strip: Source::plan(expression, tail),
            current: None,
        }
    }
}

impl super::feed::FeedSource for UnwindSource<'_> {
    fn open(&mut self, ctx: ExecCtx<'_>, row: Row) -> Result<bool> {
        let elements = open_source(ctx.graph, &row, self.expression, ctx.params)?;
        let mut template = row;
        // A list source sheds the source column from the carried row; a
        // scalar keeps its row whole.
        if !matches!(elements, Elements::One(_)) {
            if let Some(source) = &self.strip {
                source.strip(&mut template);
            }
        }
        self.current = Some((template, elements));
        Ok(true)
    }

    fn charge_facts(&self) -> Result<(usize, usize)> {
        let (template, elements) = self
            .current
            .as_ref()
            .expect("charge_facts is read on an open source");
        Ok((elements.remaining()?, template.len().saturating_add(1)))
    }

    fn pull(&mut self, ctx: ExecCtx<'_>) -> Option<Result<Row>> {
        let (template, elements) = self.current.as_mut()?;
        match elements.next() {
            Some(Ok(value)) => {
                if let Err(error) = ctx.graph.cancel_tick() {
                    return Some(Err(error));
                }
                let mut row = template.clone();
                row.insert(self.alias.to_string(), value);
                Some(Ok(row))
            }
            Some(Err(error)) => {
                self.current = None;
                Some(Err(error))
            }
            None => {
                self.current = None;
                None
            }
        }
    }
}

/// The clause's feed over its source: the constructor both the parked
/// (fused) and the materializing form build (`load_csv_feed`'s shape).
pub(super) fn unwind_feed<'a>(
    ctx: ExecCtx<'a>,
    tail: &'a [Clause],
    inputs: Vec<Row>,
    expression: &'a Expr,
    alias: &'a SmallName,
    stats: Rc<StreamedStats>,
) -> StreamedFeed<'a> {
    StreamedFeed::new(
        ctx,
        inputs,
        Box::new(UnwindSource::new(tail, expression, alias)),
        stats,
    )
}

#[cfg(test)]
mod strip_plan_tests {
    use super::*;
    use relate_ir::{CallScope, Query, SingleQuery};

    fn call(scope: CallScope) -> Clause {
        Clause::CallSubquery {
            query: Box::new(Query {
                parts: vec![SingleQuery { clauses: vec![] }],
                union_all: vec![],
                next: None,
            }),
            optional: false,
            scope,
            in_transactions: None,
        }
    }

    #[test]
    fn plan_strips_when_nothing_reads_the_source() {
        let plan = Source::plan(&Expr::Variable("xs".into()), &[]);
        assert!(matches!(plan, Some(Source::Column(name)) if name == "xs"));
    }

    #[test]
    fn plan_keeps_for_the_three_call_scope_shapes() {
        // The shapes this module grew to keep: the naked and `(*)` scopes
        // take the whole row, and an import list naming the source reads
        // it without an expression.
        let shapes = [
            CallScope::None,
            CallScope::Wildcard,
            CallScope::Vars(vec!["xs".into()]),
        ];
        for scope in shapes {
            let suffix = [call(scope)];
            assert!(Source::plan(&Expr::Variable("xs".into()), &suffix).is_none());
        }
    }

    #[test]
    fn plan_strips_when_an_import_list_names_another_var() {
        let suffix = [call(CallScope::Vars(vec!["other".into()]))];
        let plan = Source::plan(&Expr::Variable("xs".into()), &suffix);
        assert!(matches!(plan, Some(Source::Column(name)) if name == "xs"));
    }
}

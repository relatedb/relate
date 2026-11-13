//! Row scope helpers: `RETURN *` column names, `CALL { … }` read rules,
//! and post-write row refresh.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use relate_core::Value;
use relate_ir::{
    CallScope, Clause, Expr, OrderItem, PathPattern, PatternElement, Projection, SingleQuery,
};
use relate_names::SmallName;

use crate::eval::expr_vars;
use crate::overlay::ExecGraph;
use crate::row::Row;

/// Internal/synthetic row column names start with `__` (e.g. the `__agg_*`
/// bindings aggregation seeds, or `anon_name` temporaries); they are never
/// projected to users.
pub(crate) fn is_internal_column(name: &str) -> bool {
    name.starts_with("__")
}

/// `base` lengthened with `_` until no name in `names` starts with it: the
/// one freshening policy for a synthetic `__` column prefix (parked sort
/// keys, aggregate child bindings), so a user spelling of an internal name
/// can never be shadowed or read back as a synthetic one. One pass over the
/// names, linear in their total length (a name can be as long as the query
/// cap): the prefix needs one more `_` than the longest run of them any
/// name carries right after `base`.
pub(crate) fn uncolliding_prefix<'n>(
    base: &str,
    names: impl IntoIterator<Item = &'n str>,
) -> String {
    let longest_run = names
        .into_iter()
        .filter_map(|name| name.strip_prefix(base))
        .map(|rest| rest.bytes().take_while(|&byte| byte == b'_').count())
        .max();
    let mut prefix = String::from(base);
    if let Some(run) = longest_run {
        prefix.extend(std::iter::repeat_n('_', run + 1));
    }
    prefix
}

/// The output names of a projection's non-`*` items, in item order —
/// the one walk the parked-prefix check, the sink's output names and the
/// result-column models read.
pub(crate) fn projection_names(items: &[Projection]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .filter(|(_, p)| !is_star_item(p))
        .map(|(i, p)| p.output_name(i))
        .collect()
}

/// A result column of a rectangular projection row: every projection path
/// binds each result column name — and, at projection time, every parked
/// `__ord*` sort key ([`park_order_keys`]) — on every row by contract, so a
/// missing name is a bug, not a null. The one place that invariant is
/// asserted: the result materialization ([`return_result`]), the DISTINCT
/// key extraction in [`project_rows`], the sink's output construction and
/// the parked-key readback ([`parked_keys`]).
pub(crate) fn result_column<'r>(row: &'r Row, name: &str) -> &'r Value {
    row.get(name)
        .expect("projection rows carry every result column")
}

/// Whether the projection produces `name` as one of its output columns.
pub(crate) fn projects_name(items: &[Projection], name: &SmallName) -> bool {
    // The projected output names are the String rendering domain; the
    // IR variable probes them through the text bridge.
    name.with_str(|text| projects_name_text(items, text))
}

fn projects_name_text(items: &[Projection], name: &str) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| !is_star_item(item) && item.output_name(index) == name)
}

/// One projection's (`RETURN` / `WITH`) borrowed field bundle, so helpers
/// over a projection read named fields instead of positional parameters
/// (three of which are the identical `&Option<Expr>`). `where_clause` is
/// `None` for `RETURN`, the `WITH`'s own filter otherwise.
#[derive(Clone, Copy)]
pub(crate) struct ProjectionFields<'a> {
    pub distinct: bool,
    pub items: &'a [Projection],
    pub group_by: Option<&'a [Expr]>,
    pub order: &'a [OrderItem],
    pub skip: Option<&'a Expr>,
    pub limit: Option<&'a Expr>,
    pub where_clause: Option<&'a Expr>,
}

impl<'a> ProjectionFields<'a> {
    /// The one `RETURN`/`WITH` field extraction: the clause's projection
    /// fields in the shapes every consumer reads (group-by keys and the
    /// optional expressions as borrows), `None` for any other clause. All
    /// cap/sink/liveness sites consult this instead of re-destructuring the
    /// same seven fields.
    pub(crate) fn from_clause(clause: &'a Clause) -> Option<Self> {
        match clause {
            Clause::Return {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
            } => Some(Self {
                distinct: *distinct,
                items,
                group_by: group_by.as_deref(),
                order,
                skip: skip.as_ref(),
                limit: limit.as_ref(),
                where_clause: None,
            }),
            Clause::With {
                distinct,
                group_by,
                items,
                order,
                skip,
                limit,
                where_clause,
                ..
            } => Some(Self {
                distinct: *distinct,
                items,
                group_by: group_by.as_deref(),
                order,
                skip: skip.as_ref(),
                limit: limit.as_ref(),
                where_clause: where_clause.as_ref(),
            }),
            _ => None,
        }
    }
}

pub(crate) fn push_scope_name(names: &mut Vec<String>, name: impl Into<String>) {
    let name = name.into();
    if !is_internal_column(&name) && !names.iter().any(|n| n == &name) {
        names.push(name);
    }
}

/// The one walk of the names a path pattern binds: its path names (the outer
/// path variable and a named parenthesized subpath), every declared node and
/// relationship variable, then the element names sunk in document order
/// through the canonical [`for_each_gpm_element_name`] walk (which includes
/// inline-predicate variables, sorted, and recurses into quantified
/// sub-paths).
/// Every sink — null-binding, scope-set insertion, ordered column push —
/// differs only in what it does with the name.
pub(crate) fn for_each_pattern_bound_name(pattern: &PathPattern, sink: &mut impl FnMut(&str)) {
    // The names ride the IR's own representation; each sink call renders
    // through the allocation-free text read.
    for name in pattern.path_names() {
        name.with_str(|text| sink(text));
    }
    for node in &pattern.nodes {
        if let Some(variable) = &node.variable {
            variable.with_str(|text| sink(text));
        }
    }
    for rel in &pattern.rels {
        if let Some(variable) = &rel.variable {
            variable.with_str(|text| sink(text));
        }
    }
    // The element phase is the canonical walk, sunk in DOCUMENT order (the
    // pre-refactor contract): the caller's sink is order-sensitive —
    // `RETURN *` / `WITH *` column order and subquery export columns — so
    // the dedup is an order-preserving seen-set, never a HashSet iteration.
    let mut seen = HashSet::new();
    for_each_gpm_element_name(&pattern.elements, &mut |name| {
        if seen.insert(name.to_string()) {
            sink(name);
        }
    });
}

/// The one walk of the element-bound names: each element's own variable,
/// then its inline-predicate variables (sorted — their source is a set with
/// no document order), recursing into quantified sub-paths. Every consumer
/// (the ordered bound-name sink above, the scope-set inserter below) differs
/// only in what it does with the name.
pub(crate) fn for_each_gpm_element_name(elements: &[PatternElement], sink: &mut impl FnMut(&str)) {
    fn predicate_vars(predicate: &Expr, sink: &mut impl FnMut(&str)) {
        let mut vars = HashSet::new();
        expr_vars(predicate, &mut vars);
        let mut ordered: Vec<&String> = vars.iter().collect();
        ordered.sort();
        for name in ordered {
            sink(name);
        }
    }
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if let Some(name) = &node.variable {
                    name.with_str(|text| sink(text));
                }
                if let Some(predicate) = &node.where_clause {
                    predicate_vars(predicate, sink);
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(name) = &rel.variable {
                    name.with_str(|text| sink(text));
                }
                if let Some(predicate) = &rel.where_clause {
                    predicate_vars(predicate, sink);
                }
            }
            PatternElement::Quantified(qpp) => {
                if let Some(name) = &qpp.subpath_variable {
                    name.with_str(|text| sink(text));
                }
                if let Some(predicate) = &qpp.where_clause {
                    predicate_vars(predicate, sink);
                }
                for_each_gpm_element_name(&qpp.elements, sink);
            }
        }
    }
}

pub(crate) fn add_gpm_element_names(elements: &[PatternElement], out: &mut HashSet<String>) {
    for_each_gpm_element_name(elements, &mut |name| {
        out.insert(name.to_string());
    });
}

pub(crate) fn add_path_scope(names: &mut Vec<String>, p: &PathPattern) {
    for_each_pattern_bound_name(p, &mut |name| {
        push_scope_name(names, name.to_string());
    });
}

/// The names a `RETURN *`/`WITH *` expands when there is no row to read them
/// off — the declared scope, so the empty table still has the right columns.
/// It walks the arms that bind a name before the projection: MATCH/MERGE/
/// CREATE patterns, `UNWIND` aliases, `CALL … YIELD` columns, and the
/// preceding `WITH`.
///
/// This is the executor's runtime stand-in for an empty table, not the
/// front's admission set: the front's `clause_introduced_names` also folds
/// the catalog commands' output columns, the `LOAD CSV` alias and a nested
/// `CALL { … }`'s exports, which this walk does not model.
pub(crate) fn star_scope_names(clauses: &[Clause]) -> Vec<String> {
    let mut names = Vec::new();
    for c in clauses {
        match c {
            Clause::Match { patterns, .. } | Clause::Create { patterns } => {
                for p in patterns {
                    add_path_scope(&mut names, p);
                }
            }
            Clause::Merge { pattern, .. } => add_path_scope(&mut names, pattern),
            Clause::Unwind { alias, .. } => push_scope_name(&mut names, alias.to_string()),
            Clause::Call { yield_items, .. } => {
                for name in yield_items.effective_names() {
                    push_scope_name(&mut names, name.to_string());
                }
            }
            Clause::With { items, .. } => {
                // A `WITH` replaces the scope; under `WITH *` it keeps the
                // names bound so far and adds the explicit items.
                let carried = std::mem::take(&mut names);
                names = return_columns(items, move || carried);
            }
            Clause::Return { .. } => break,
            _ => {}
        }
    }
    names.sort();
    names
}

/// Whether one projection item is the `*` item — the one spelling of the
/// per-item predicate every use site derives from.
pub(crate) fn is_star_item(item: &Projection) -> bool {
    matches!(item.expr, Expr::Star)
}

/// Whether a projection's item list carries a `*` item — the one spelling
/// of the list-level predicate, consulted by [`return_columns`] (which
/// expands it), the `WITH` arm of [`star_scope_names`] (which replaces the
/// carried scope) and the liveness walk ([`super::pushdown`], which gives
/// up on it).
pub(crate) fn has_star_item(items: &[Projection]) -> bool {
    items.iter().any(is_star_item)
}

/// The terminal `RETURN` of one UNION arm: its clause list (the declared
/// scope a `*` item expands) and its projection items. `None` when the arm
/// does not conclude in a `RETURN` (a `WHEN` body, `FINISH`, a write). The
/// one probe of that fact — [`return_columns`]'s callers
/// (`stage_export_columns`, `failed_batch_row`) read it instead of each
/// re-opening `clauses.last()`.
pub(crate) fn terminal_return_items(part: &SingleQuery) -> Option<(&[Clause], &[Projection])> {
    match part.clauses.last()? {
        Clause::Return { items, .. } => Some((part.clauses.as_slice(), items.as_slice())),
        _ => None,
    }
}

/// The `__` namespace reservation (OPLANE_REQ-00328162): no user-authored
/// output name (alias) may fall in the internal `__` namespace on an
/// optimized path. The match sink and the aggregate folds are those paths —
/// each declines a colliding alias so the ordinary projection, which treats
/// every name as a user column, answers it. Parked `ORDER BY` keys are not a
/// reason to decline: their prefix is chosen to miss every result column
/// (`super::order::OrderKeys`).
pub(crate) fn projection_collides_with_internal_names(items: &[Projection]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| is_internal_column(&item.output_name(index)))
}

/// Whether two items resolve to the same output column name.
///
/// The front-end's `ColumnNameConflict` check compares parser-level names
/// (verbatim source text), while [`Projection::output_name`] canonicalizes —
/// so `n.name` and `` n.`name` `` pass that check yet resolve alike. A path
/// that writes items over one row by name would collapse them; the match sink
/// declines such a projection and leaves it to the ordinary projection.
pub(crate) fn projection_repeats_output_name(items: &[Projection]) -> bool {
    let mut seen = HashSet::new();
    items
        .iter()
        .enumerate()
        .any(|(index, item)| !seen.insert(item.output_name(index)))
}

/// The result columns of a `RETURN`/`WITH` item list: the names its `*`
/// item expands, followed by every non-star item's output name. `star` is
/// called only when the list has a `*` item, so each caller picks the
/// expansion source (the first row's visible keys, the declared scope for
/// an empty table, the `WITH`'s carried-over scope). Dedup-preserving: the
/// first occurrence wins, so an explicit item repeating a star name keeps
/// the star's place.
///
/// The one implementation of that policy: the projection's result columns
/// ([`super::project::projection_result_columns`]) and the `CALL { … }`
/// export set ([`super::call::call_export_columns`]) both call it, so a star
/// can never silently drop its sibling items.
pub(crate) fn return_columns(
    items: &[Projection],
    star: impl FnOnce() -> Vec<String>,
) -> Vec<String> {
    let mut columns = if has_star_item(items) {
        star()
    } else {
        Vec::new()
    };
    for (index, item) in items.iter().enumerate() {
        if is_star_item(item) {
            continue;
        }
        let name = item.output_name(index);
        if !columns.contains(&name) {
            columns.push(name);
        }
    }
    columns
}

/// The names a `CALL { … }` clause reads from the enclosing row, or `None`
/// when its scope reads the whole row (the naked `CALL { … }` and
/// `CALL (*) { … }` forms). The one statement of that rule, at the
/// granularity both liveness walks need: the `UNWIND` source strip asks
/// whether a given column is among them ([`super::unwind`]), the expression
/// liveness pass takes the whole set ([`super::pushdown::live_from`]).
pub(crate) fn call_scope_reads(scope: &CallScope) -> Option<&[SmallName]> {
    match scope {
        CallScope::Vars(vars) => Some(vars),
        CallScope::None | CallScope::Wildcard => None,
    }
}

pub(crate) fn visible_keys(row: &Row) -> Vec<String> {
    row.keys()
        .filter(|k| !is_internal_column(k))
        .cloned()
        .collect()
}

/// The seed row of a nested body that sees the enclosing row as its whole
/// scope — the implicit-import forms `CALL { … }`, `CALL (*) { … }` and the
/// expression subqueries `EXISTS { … }` / `COUNT { … }` / `COLLECT { … }`:
/// the enclosing row without the clause engine's internal columns. A
/// `__`-prefixed slot (the match sink's parked `__ord*` sort keys, the
/// `__agg_*` seeds, other clause temporaries) describes the *enclosing*
/// clause and never the body's own state; a body that inherited one reads
/// it as its own and skips items it never evaluated.
///
/// Not for the imperative bodies (`exec_conditional`'s CIP-135 `WHEN`/
/// `ELSE` branches, `exec_foreach`'s per-element body): they run on the
/// caller's row unchanged — a sink projection only ever feeds the clause
/// immediately after its MATCH, so a `Conditional`/`Foreach` body can never
/// receive sink-parked `__ord*`/`__agg_*` rows, and no strip is needed.
pub(crate) fn nested_seed_row(row: &Row) -> Row {
    let mut seed = row.clone();
    seed.retain(|name, _| !is_internal_column(name));
    seed
}

/// Re-attach the importing-subquery scope bindings a `WITH` projection
/// dropped: a binding already present in the row is the body's own value
/// and wins. The keep-visible half of the importing-subquery scope policy
/// (the seed half is [`nested_seed_row`]), shared by the `WITH` arm and the
/// `CALL { … }` NEXT-stage recursion. See `run_nested_clauses_scoped`.
pub(crate) fn carry_scope_bindings(rows: &mut [Row], scope: &Row) {
    for row in rows {
        for (name, value) in scope {
            row.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
}

/// Fold a write clause's effects back into the rows that drove it: a bound
/// entity the write updated is re-read from the store, and one the write
/// deleted becomes an **empty tombstone** — the same id, no labels, no
/// properties (a returned deleted node renders `()`).
/// The tombstone keeps the identity a later pattern needs: the clause
/// engine still matches a deleted-but-bound variable through it
/// (`node_ok`'s snapshot path), while a fresh scan of the store finds
/// nothing. A deleted entity is never refetched.
pub(crate) fn refresh_rows(graph: &ExecGraph<'_>, rows: &mut [Row]) {
    for row in rows {
        let keys: Vec<_> = row.keys().cloned().collect();
        for k in keys {
            match row.get(&k) {
                Some(Value::Node(n)) => {
                    if let Some(fresh) = graph.node(n.id) {
                        row.insert(k, Value::from_node(fresh));
                    } else if graph.is_deleted_node(n.id) {
                        row.insert(
                            k,
                            Value::from_node(relate_core::Node {
                                id: n.id,
                                labels: Arc::new(Vec::new()),
                                properties: relate_core::PropertyMap::share(BTreeMap::new()),
                            }),
                        );
                    }
                }
                Some(Value::Relationship(r)) => {
                    if let Some(fresh) = graph.rel(r.id) {
                        row.insert(k, Value::from_relationship(fresh));
                    } else if graph.is_deleted_rel(r.id) {
                        // Same tombstone contract as the node arm above: the
                        // identity survives (id, endpoints, type — what a
                        // later pattern matches on), the properties do not.
                        // A property read on the tombstone — `r.since` or
                        // the `r {.*}` projection — raises the one
                        // DeletedEntityAccess rejection, never the pre-delete
                        // map and never a silent `{}`.
                        row.insert(
                            k,
                            Value::from_relationship(relate_core::Relationship {
                                id: r.id,
                                start: r.start,
                                end: r.end,
                                rel_type: r.rel_type.clone(),
                                properties: relate_core::PropertyMap::share(BTreeMap::new()),
                            }),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

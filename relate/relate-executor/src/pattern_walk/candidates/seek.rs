//! The seek vocabulary and its recognizers: which single-index lookup
//! (if any) answers a node pattern's residual predicate — TEXT contains /
//! prefix, RANGE bounds, SPATIAL, and the `id()`/`elementId()` direct
//! seeks — the family the parent module's funnel (`leaf_seek`), the
//! exec engine's far-node prefilter, and the rel twin's decode
//! ([`id_fn_conversion`]) consult.

use std::collections::BTreeMap;

use relate_core::{NodeId, Result, Value};
use relate_ir::{BinaryOp, Expr};
use relate_names::SmallName;

use crate::eval::{eval, orient};
use crate::graph::{RangeSeek, SpatialSeek};
use crate::overlay::ExecGraph;
use crate::row::Row;

#[derive(Clone)]
pub(crate) enum StringSeek {
    Contains(String),
    Prefix(String),
}

/// One index-answerable predicate a conjunct places on the node being
/// matched. Collected from every conjunct, ranked, and the first one an
/// index serves wins; the residual re-checks the predicate either way.
#[derive(Clone)]
pub(crate) enum Seek {
    /// `id(n) = …` / `id(n) IN …` / `n IN [nodes…]` (LABS-250): ids
    /// answer directly from storage or the list, so this needs no index
    /// and no static label conjunction.
    Ids(Vec<NodeId>),
    /// `CONTAINS` / `ENDS WITH` (TEXT index) and `STARTS WITH` (RANGE index).
    Text(String, StringSeek),
    /// `=`, `IN`, and comparisons (RANGE index).
    Range(String, RangeSeek),
    /// `point.distance` / `point.withinBBox` (POINT index).
    Spatial(String, SpatialSeek),
}

impl Seek {
    /// Preference when several conjuncts seek: a direct id lookup, then
    /// text, range, and spatial. Stable, so conjunct order breaks ties.
    pub(super) fn rank(&self) -> u8 {
        match self {
            Seek::Ids(_) => 0,
            Seek::Text(..) => 1,
            Seek::Range(..) => 2,
            Seek::Spatial(..) => 3,
        }
    }

    /// Whether this seek is usable at all — the funnels' ONE per-seek
    /// eligibility call, none re-assembles the compound guard. Two
    /// criteria: an index-backed seek is only servable under a static
    /// label conjunction (a non-indexed seek — the scan fallback — is
    /// usable either way), and ADR-0493's crossover (a majority range
    /// window is cheaper as the scan the funnel falls through to).
    pub(crate) fn servable(
        &self,
        graph: &ExecGraph<'_>,
        labels: &[String],
        label_seekable: bool,
    ) -> bool {
        (!self.indexed() || label_seekable) && !self.crossover_declines(graph, labels)
    }

    /// Served by a declared index over the pattern labels. `Ids` is
    /// answered by storage and is valid under any label expression —
    /// the same classification tags the served answer
    /// [`ServedIds::Exact`] (the ids seek answers exactly) or
    /// [`ServedIds::Superset`] (every index serve can over-answer).
    pub(crate) fn indexed(&self) -> bool {
        !matches!(self, Seek::Ids(_))
    }

    /// Candidate ids tagged exact-or-superset, or `None` when no index
    /// serves this seek (the TEXT index also declines an empty
    /// `CONTAINS` needle).
    pub(crate) fn serve(&self, graph: &ExecGraph<'_>, labels: &[String]) -> Option<ServedIds> {
        let ids = match self {
            Seek::Ids(ids) => ids
                .iter()
                .copied()
                .filter(|&id| graph.node_exists(id))
                .collect(),
            Seek::Text(key, StringSeek::Contains(n)) => {
                graph.lookup_string_contains(labels, key, n)?
            }
            Seek::Text(key, StringSeek::Prefix(n)) => graph.lookup_string_prefix(labels, key, n)?,
            Seek::Range(key, seek) => graph.lookup_range(labels, key, seek)?,
            Seek::Spatial(key, seek) => graph.lookup_spatial(labels, key, seek)?,
        };
        Some(if self.indexed() {
            ServedIds::Superset(ids)
        } else {
            ServedIds::Exact(ids)
        })
    }

    /// `Some(n)` is an upper bound on the ids this seek would materialize.
    /// `None` when no index serves it (text and spatial seeks included).
    pub(crate) fn cardinality(&self, graph: &ExecGraph<'_>, labels: &[String]) -> Option<usize> {
        match self {
            Seek::Ids(ids) => Some(ids.len()),
            Seek::Range(key, seek) => graph.lookup_range_cardinality(labels, key, seek),
            Seek::Text(..) | Seek::Spatial(..) => None,
        }
    }

    /// ADR-0493's crossover, this seek's own arm: a range window at or
    /// above a quarter of the scan population it replaces — the
    /// smallest label the pattern names (the label conjunction the
    /// funnel's scan arm serves), the one population rule the
    /// planner's ladder gates with too, so plan narration and
    /// execution agree on multi-label nodes. `pattern_label_rows`
    /// sizes an absent label at 0: the scan the seek would replace is
    /// empty, so declining is the same rule applied to a measurement,
    /// not an unknown. A window the index cannot size, or an empty
    /// label set, keeps the seek (no measurement, no gate). Every
    /// other seek kind never declines here.
    fn crossover_declines(&self, graph: &ExecGraph<'_>, labels: &[String]) -> bool {
        let Seek::Range(..) = self else {
            return false;
        };
        let Some(window) = self.cardinality(graph, labels) else {
            return false;
        };
        // The planner's one population fold and gate (every label is
        // measured here — an absent label sizes 0).
        let scan_rows = relate_planner::crossover_scan_rows(
            // The planner's fold rides the name representation; the label
            // rows probe reads through the free text bridge.
            labels
                .iter()
                .map(|label| SmallName::from_name(label.as_str())),
            |l| l.with_str(|text| Some(graph.base().pattern_label_rows(text))),
        );
        relate_planner::range_seek_gated(window as u64, scan_rows)
    }
}

/// One served seek answer, tagged by exactness — the tag the funnel's
/// one narrowing gate reads (LABS-811, stellar F-2-2: the guard had been
/// spelled twice with different policies). [`Exact`](ServedIds::Exact):
/// the ids are the conjunct's answer — the ids seek is answered by
/// storage, nothing can over-answer it. [`Superset`](ServedIds::Superset):
/// an index serve whose members can over-answer — a RANGE equality over
/// zoned datetimes matches both DST halves at one instant, a TEXT
/// contains serve answers ENDS WITH — so the pattern's exact
/// label/property equality still narrows them. The residual re-applies
/// every predicate either way; the tag only decides whether the funnel
/// pays the narrowing lookup.
pub(crate) enum ServedIds {
    Exact(Vec<NodeId>),
    Superset(Vec<NodeId>),
}

impl ServedIds {
    /// The ids, narrowed to the pattern's exact label/property equality
    /// when (and only when) the serve is a superset — the ONE narrowing
    /// site every served answer the funnel returns flows through. The
    /// property map arrives as a lazy thunk so an exact serve never
    /// evaluates it (a malformed pattern-property expression keeps
    /// surfacing at the per-candidate check instead, exactly as before).
    /// `pub(super)` — the funnel tier: every caller is this module's
    /// parent (`candidates/mod.rs`), no other walk narrows a served
    /// answer.
    pub(super) fn narrowed(
        self,
        graph: &ExecGraph<'_>,
        labels: &[String],
        props: impl FnOnce() -> Result<BTreeMap<String, Value>>,
    ) -> Result<Vec<NodeId>> {
        match self {
            ServedIds::Exact(ids) => Ok(ids),
            ServedIds::Superset(mut ids) => {
                let props = props()?;
                if props.is_empty() {
                    return Ok(ids);
                }
                // props non-empty: lookup_nodes always serves (its only
                // None is the empty-seek case). Its answer is ascending
                // and deduplicated, so the membership test is a binary
                // search on the sorted slice — no per-statement hash set
                // (LABS-803's `make_hash<i64>` churn).
                let eq = graph
                    .lookup_nodes(labels, &props)
                    .expect("props non-empty: lookup_nodes always serves");
                ids.retain(|id| eq.binary_search(id).is_ok());
                Ok(ids)
            }
        }
    }

    /// The composite of two served answers under one Boolean op's id-set
    /// fold (`union_ids` for OR, `intersect_ids` for AND): exact only
    /// when both are — one superset arm keeps the composite a superset.
    /// `pub(crate)`, beside [`into_ids`](ServedIds::into_ids): since
    /// LABS-803 the exec far probe's materialize folds its served
    /// answers through this same spelling cross-module — the one
    /// composite every served-set consumer reads. [`narrowed`](ServedIds::narrowed)
    /// stays `pub(super)`: only the funnel narrows.
    pub(crate) fn folded(
        self,
        other: ServedIds,
        fold: fn(Vec<NodeId>, Vec<NodeId>) -> Vec<NodeId>,
    ) -> ServedIds {
        let exact = matches!(self, ServedIds::Exact(_)) && matches!(other, ServedIds::Exact(_));
        let ids = fold(self.into_ids(), other.into_ids());
        if exact {
            ServedIds::Exact(ids)
        } else {
            ServedIds::Superset(ids)
        }
    }

    /// The ids without the tag — for consumers that filter by set
    /// membership only (the far probe's intersection). `pub(crate)`,
    /// deliberately wider than its `pub(super)` siblings: the exec
    /// engine's far probe (`exec/far_probe.rs`) reads this projection
    /// cross-module — the one ServedIds member with a caller outside
    /// the candidates tier.
    pub(crate) fn into_ids(self) -> Vec<NodeId> {
        match self {
            ServedIds::Exact(ids) | ServedIds::Superset(ids) => ids,
        }
    }
}

/// The seek one conjunct expresses on `var`, if any. The four recognizers
/// are disjoint: an ids seek has `id()`/`elementId()` or the bare
/// variable itself (LABS-250's `n IN [nodes…]`) on the left, a text or
/// range seek a property, a spatial seek a point function.
pub(crate) fn leaf_seek(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<Seek> {
    if let Some(ids) = ids_seek_expr(var, expr, graph, row, params, cypher_version) {
        return Some(Seek::Ids(ids));
    }
    if let Some((key, seek)) = string_seek_expr(var, expr, graph, row, params, cypher_version) {
        return Some(Seek::Text(key, seek));
    }
    if let Some((key, seek)) = range_seek_expr(var, expr, graph, row, params, cypher_version) {
        return Some(Seek::Range(key, seek));
    }
    point_seek_expr(var, expr, graph, row, params, cypher_version)
        .map(|(key, seek)| Seek::Spatial(key, seek))
}

fn string_seek_expr(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<(String, StringSeek)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    // Classify once, binding the constructor before any paid work: the
    // `_` arm is the decline gate for every other op.
    let variant: fn(String) -> StringSeek = match op {
        BinaryOp::Contains | BinaryOp::EndsWith => StringSeek::Contains,
        BinaryOp::StartsWith => StringSeek::Prefix,
        _ => return None,
    };
    // The var's property on the left — the one canonical recognizer.
    let key = prop_key(left, var)?;
    let val = eval(graph, row, right, params, cypher_version).ok()?;
    let s = val.as_string()?.to_string();
    // Servability has one owner: the gram index itself declines an empty
    // needle (lookup_string_contains returns None), and node_candidates
    // falls through to its shared scan tail — `CONTAINS ''` stays correct.
    Some((key, variant(s)))
}

fn range_seek_expr(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<(String, RangeSeek)> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    let (key, bound_expr, op) = orient(|e| prop_key(e, var), left, right, *op)?;
    let bound = eval(graph, row, bound_expr, params, cypher_version).ok()?;
    // A null bound is never servable. A bound that references the pattern
    // variable itself (`c.key = c.cluster`) or a variable bound later in
    // the pattern evaluates against a row where that variable is unbound —
    // null — and a served seek silently renders the whole pattern empty
    // instead of declining (the LABS-328 regression: the entity-resolution
    // report cell's `WHERE c.key = c.cluster` returned 0 rows once the
    // constraint's backing index materialized). Null never satisfies an
    // equality or a comparison, so declining to the scan tail plus the
    // residual re-applying the predicate per candidate row is the same
    // answer, correctly reached.
    if bound.is_null() {
        return None;
    }
    match op {
        BinaryOp::Eq => Some((key, RangeSeek::Eq(bound))),
        BinaryOp::In => {
            let Value::List(xs) = bound else {
                return None;
            };
            Some((key, RangeSeek::In(xs)))
        }
        BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Lte | BinaryOp::Gte => {
            Some((key, RangeSeek::Cmp { op, bound }))
        }
        _ => None,
    }
}

/// The var's own property key — `key_of` in owned form.
fn prop_key(expr: &Expr, var: &SmallName) -> Option<String> {
    // The seek's property key is the planner-IR name; it renders once at
    // this recognition (index lookup text, not a per-row read).
    crate::eval::key_of(expr, var).map(|key| key.to_string())
}

/// The one call-shape test: `expr` is a call to `name` (case-insensitive)
/// with exactly `arity` arguments, returning them. Every seek shape and the
/// id-conversion reader consult it instead of re-spelling the name/arity
/// conjunction.
fn named_call<'a>(expr: &'a Expr, name: &str, arity: usize) -> Option<&'a [Expr]> {
    match expr {
        Expr::Function {
            name: actual, args, ..
        } if actual.with_str(|text| text.eq_ignore_ascii_case(name)) && args.len() == arity => {
            Some(args)
        }
        _ => None,
    }
}

fn point_seek_expr(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<(String, SpatialSeek)> {
    if let Some(args) = named_call(expr, "point.withinbbox", 3) {
        let key = prop_key(&args[0], var)?;
        let lo = eval(graph, row, &args[1], params, cypher_version).ok()?;
        let hi = eval(graph, row, &args[2], params, cypher_version).ok()?;
        return Some((key, SpatialSeek::BBox { lo, hi }));
    }
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    let (func, bound_expr, inclusive) = distance_cmp(left, right, *op)?;
    let args = named_call(func, "point.distance", 2)?;
    let (key, origin) =
        point_prop_and_origin(var, &args[0], &args[1], graph, row, params, cypher_version)?;
    let max = eval(graph, row, bound_expr, params, cypher_version)
        .ok()?
        .as_number()?;
    Some((
        key,
        SpatialSeek::Distance {
            origin,
            max,
            inclusive,
        },
    ))
}

fn distance_cmp<'a>(
    left: &'a Expr,
    right: &'a Expr,
    op: BinaryOp,
) -> Option<(&'a Expr, &'a Expr, bool)> {
    match op {
        BinaryOp::Lt => Some((left, right, false)),
        BinaryOp::Lte => Some((left, right, true)),
        BinaryOp::Gt => Some((right, left, false)),
        BinaryOp::Gte => Some((right, left, true)),
        _ => None,
    }
}

fn point_prop_and_origin(
    var: &SmallName,
    a: &Expr,
    b: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<(String, Value)> {
    // The distance functions are operand-symmetric — no op to mirror, so
    // only the side-selection core applies: the side carrying the var's
    // property decides, the other side is the origin.
    let (key, origin_expr) = crate::eval::var_key_side(var, a, b)?;
    let origin = eval(graph, row, origin_expr, params, cypher_version).ok()?;
    Some((key.to_string(), origin))
}

/// An id conversion selected by a seek recognizer's left shape: a
/// value to a node or rel id (`NodeId`/`RelId` are both `i64`). The
/// three known conversions name their shape — a typed enum, not a
/// boxed closure, so a seek classification allocates nothing
/// (LABS-950).
pub(crate) enum IdConversion {
    /// `id(var)`: the value's integer form.
    Int,
    /// `elementId(var)`: the engine's self-encoding element-id string,
    /// through the caller's namespace picker.
    ElementId(fn(&str) -> Option<i64>),
    /// The bare node variable (LABS-250): the value's node form.
    NodeValue,
}

impl IdConversion {
    /// The one application of a conversion — every consumer (the ids
    /// recognizer, its `IN` tail, the rel twin's decode) reads this
    /// spelling, never a re-derived per-variant match.
    pub(crate) fn apply(&self, value: &Value) -> Option<i64> {
        match self {
            IdConversion::Int => value.as_int(),
            IdConversion::ElementId(parse) => value.as_string().and_then(*parse),
            IdConversion::NodeValue => crate::pattern_walk::node_id(value),
        }
    }
}

/// The left-shape dispatch shared by the node and rel id seeks: a
/// single-argument `id(var)` or `elementId(var)` call selects the id
/// conversion — integer for `id()`, the engine's self-encoding element
/// id string for `elementId()` (`parse_element_id` picks the node or
/// rel namespace). Any other left shape declines.
pub(crate) fn id_fn_conversion(
    var: &SmallName,
    left: &Expr,
    parse_element_id: fn(&str) -> Option<i64>,
) -> Option<IdConversion> {
    let (args, conversion) = if let Some(args) = named_call(left, "id", 1) {
        (args, IdConversion::Int)
    } else if let Some(args) = named_call(left, "elementid", 1) {
        (args, IdConversion::ElementId(parse_element_id))
    } else {
        return None;
    };
    let Expr::Variable(v) = &args[0] else {
        return None;
    };
    if v != var {
        return None;
    }
    Some(conversion)
}

/// The comparison an ids-seek serves, narrowed from `BinaryOp` up
/// front: `=` (the right side is the one id) or `IN` (the right side
/// is the list of ids). The narrowing lets the recognizer's collect
/// step match exhaustively — no dead arm re-splitting the op fact the
/// front match already decided.
enum IdsCmp {
    /// `=`: the right side is the one id.
    Eq,
    /// `IN`: the right side is the list of ids.
    In,
}

/// The ids recognizer: one seek whose id conversion is selected by the
/// left shape — `id(n) =/IN …` (integer), `elementId(n) =/IN …` (the
/// `4:relate-neo4j:n<id>` string form), or the bare node variable
/// (`n = node` / `n IN [nodes…]`, LABS-250, the node-value conversion).
/// The shapes are disjoint, so at most one conversion applies per
/// conjunct.
fn ids_seek_expr(
    var: &SmallName,
    expr: &Expr,
    graph: &ExecGraph<'_>,
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Option<Vec<NodeId>> {
    let Expr::Binary { op, left, right } = expr else {
        return None;
    };
    // One classification of `op`: every other op declines before paying
    // an eval. The narrowed `cmp` keeps the collect step below
    // exhaustive.
    let cmp = match op {
        BinaryOp::Eq => IdsCmp::Eq,
        BinaryOp::In => IdsCmp::In,
        _ => return None,
    };
    // The left-shape dispatch is pure: the bare node variable is the
    // one special case (its node form, for `=` and `IN` alike —
    // LABS-250); every other left goes to the canonical call-shape
    // owner, which declines what it does not recognize. The op
    // classification is read only by the collect step below.
    let to_id = match left.as_ref() {
        Expr::Variable(v) if v == var => IdConversion::NodeValue,
        _ => id_fn_conversion(var, left, relate_core::parse_node_element_id)?,
    };
    let val = eval(graph, row, right, params, cypher_version).ok()?;
    match cmp {
        IdsCmp::Eq => to_id.apply(&val).map(|id| vec![id]),
        IdsCmp::In => in_list_ids(&val, &to_id),
    }
}

/// The `IN`-seek tail shared by every left shape (id()/elementId() and the
/// bare node variable, LABS-250): a list value mapped through the id
/// conversion, deduped — a duplicate list element is a semantic regression
/// (a duplicate row), not a harmless extra. The conversion is total over
/// the list, the same contract as the scalar arm: one element it cannot
/// read declines the seek, and the residual re-evaluates the predicate —
/// the same answer, since an unreadable element can never equal the
/// variable's id. A non-list declines the seek the same way; the residual
/// re-evaluates the predicate anyway — null matches nothing, any other
/// non-list errors with InvalidArgumentType.
fn in_list_ids(val: &Value, to_id: &IdConversion) -> Option<Vec<NodeId>> {
    let Value::List(xs) = val else {
        return None;
    };
    let mut ids: Vec<NodeId> = xs
        .iter()
        .map(|x| to_id.apply(x))
        .collect::<Option<Vec<_>>>()?;
    ids.sort_unstable();
    ids.dedup();
    Some(ids)
}

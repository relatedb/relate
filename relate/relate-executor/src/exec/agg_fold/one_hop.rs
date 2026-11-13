//! The one-hop aggregate fold (ADR-0036): the dispatch gate
//! `try_fold_one_hop_aggs` and the fold's running state
//! [`OneHopFold`] — the per-hop offer, the (start, rel) accept, and the
//! start-binding preamble as methods; the scan loop as `run`; the
//! output loop as `finish` — beside their siblings (the streaming
//! accumulators in `acc`, the multi-hop path fold in `path`, its
//! compile half in `plan`). The group table the hop offers accumulate
//! into rides the fold, so the phases never thread it; the shared
//! scaffolding both folds read — the RETURN gate's [`FoldOutput`], the
//! one [`Rank`] classification vocabulary with the shared
//! `project_ranked_row` projection over it — lives in the parent
//! module.

use std::collections::{BTreeMap, HashSet};

use indexmap::map::Entry;
use indexmap::IndexMap;
use relate_core::{NodeId, RelId, Result, Value};
use relate_ir::{Expr, PathPattern, Projection};

use crate::eval::{eval, expr_vars, is_agg_expr, predicate_holds};
use crate::group_key::GroupKey;
use crate::overlay::ExecGraph;
use crate::pattern_walk::{
    bind_node_stub, bind_trail, binding, node_candidates, rel_edge_ok, RelFilter,
};
use crate::row::{Row, RowRung};
use crate::store::RelTypes;

use super::super::candidates::{node_ok, oriented_start, resolve_rel_seek, SeekedRel};
use super::super::match_folds::FoldCtx;
use super::super::pushdown::{flatten_and_owned, match_scope};
use super::super::scope::is_internal_column;
use super::super::WalkTallies;
use super::{
    expr_within, finish_folded_rows, fold_hop_vars, fold_return, project_ranked_row, CachedKey,
    FoldOutput, FoldedReturn, Rank, StreamAgg,
};

/// A residual the one-hop fold cannot decide — a named path, or a conjunct
/// whose scope variables are not on the input row or the hop — keeps the
/// full-match path.
fn where_fold_ready(where_clause: Option<&Expr>, path: &PathPattern, rows: &[Row]) -> bool {
    let Some(expr) = where_clause else {
        return true;
    };
    let hop = fold_hop_vars(path);
    let mut used = HashSet::new();
    expr_vars(expr, &mut used);
    if path
        .path_names()
        .any(|name| name.with_str(|text| used.contains(text)))
    {
        return false;
    }
    let patterns = std::slice::from_ref(path);
    for row in rows {
        let scope = match_scope(row, patterns);
        let mut available = hop.clone();
        for key in row.keys() {
            if !is_internal_column(key) {
                available.insert(key.clone());
            }
        }
        for conjunct in flatten_and_owned(expr) {
            let mut vars = HashSet::new();
            expr_vars(&conjunct, &mut vars);
            vars.retain(|var| scope.contains(var));
            if vars.iter().any(|var| !available.contains(var)) {
                return false;
            }
        }
    }
    true
}

/// The fold's seek outcome — its own vocabulary, exhaustive without the
/// walk-only `RelSeek::Forced` (exec/expand.rs): an accepted fold hop
/// is always anchored (the row-bound start stays the anchor), so the
/// fold never yields Forced.
enum FoldSeek {
    /// No seek applies (no rel variable, no equality conjunct, or a shape
    /// the seek does not serve) — the adjacency scan serves the row.
    Decline,
    /// Resolved but matches nothing (not visible, type-filtered, or not
    /// incident to the row-bound start) — the pattern matches nothing; the
    /// row is skipped, never scanned.
    Gone,
    /// The one anchored hop to offer.
    Anchored(NodeId, RelId),
}

/// LABS-161, the fold's twin of the walk's `first_rel_seek`: an
/// `elementId(r) = …` / `id(r) = …` conjunct (or a row-bound `r`) on the
/// one-hop pattern's rel variable resolves the single hop directly.
/// The decode core — row binding, `rel_seek_expr` over the conjuncts,
/// `rel_ends` visibility, the static type filter — is the shared
/// [`resolve_rel_seek`]; the fold-local policy is only the orientation
/// decline (an undirected pattern matches each rel in both orientations;
/// one anchor would drop the reverse hop, so the scan keeps serving it)
/// and the row-bound start incidence check. The WHERE clause is
/// re-evaluated on the bound row either way, so the seek narrows the
/// search, never the result.
fn fold_rel_seek(
    graph: &ExecGraph<'_>,
    path: &PathPattern,
    types: &RelTypes,
    conjuncts: &[Expr],
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> FoldSeek {
    let rp = &path.rels[0];
    let (start, end, rid) = match resolve_rel_seek(graph, rp, types, conjuncts, row, params) {
        SeekedRel::Decline => return FoldSeek::Decline,
        SeekedRel::Gone => return FoldSeek::Gone,
        SeekedRel::Resolved { start, end, rid } => (start, end, rid),
    };
    // Pick the direction-oriented start of the rel — the one
    // `oriented_start` spelling; the undirected decline lives there.
    let Some(sid) = oriented_start(rp, start, end) else {
        return FoldSeek::Decline;
    };
    // A row-bound start stays the anchor: the hop counts only when the
    // rel is incident to it. A null or non-node binding matches nothing,
    // like the scan's empty candidate set — one accept predicate,
    // `Binding::accepts`.
    if !binding(&path.nodes[0], row).accepts(&sid) {
        return FoldSeek::Gone;
    }
    FoldSeek::Anchored(sid, rid)
}

/// Bind a pattern node seat's variable to its node stub, keeping an
/// existing binding (the seat's accept check guarantees it is this very
/// id) — the shape `match_node` spells for the walk, stated once for both
/// the fold's seats.
fn bind_node_seat(np: &relate_ir::NodePattern, row: &mut Row, graph: &ExecGraph<'_>, id: NodeId) {
    if let Some(v) = &np.variable {
        // Text render: the row schema's names are String.
        row.entry(v.to_string())
            .or_insert_with(|| bind_node_stub(graph, id));
    }
}

/// One one-hop-fold aggregate seat: the prototype paired with its
/// validated operand (`None` for `count(*)`). The pair stays together so
/// the offer site never zips two same-length vectors back by index.
type StreamSeat<'e> = (StreamAgg, Option<&'e Expr>);

/// Streamable prototypes for the aggregate half of the projection, in
/// item order; `None` when any aggregate item is not streamable — the
/// caller declines the fold in the same pass.
fn fresh_stream_aggs<'e>(agg_items: &[&'e Projection]) -> Option<Vec<StreamSeat<'e>>> {
    agg_items.iter().map(|p| StreamAgg::new(&p.expr)).collect()
}

/// ADR-0036: fold one-hop MATCH + streamable RETURN aggregates (no row
/// per edge). The eligibility gates and the one `Rank` classification
/// pass run here; the accepted shape hands its rows to
/// [`OneHopFold`] — the phases the gate inline-spelled before the
/// extraction: the hop offer, the (start, rel) accept, and the
/// start-binding preamble as methods, the scan loop as `run`, the
/// output loop as `finish`. The dispatcher tries the path fold
/// (exec/agg_fold/path.rs) first: it serves count, count-distinct, and
/// single-node predicates; this one-hop fold serves exactly what it
/// declined. Sum, CASE, and predicates that mention both ends stay here
/// on the scratch loop.
pub(crate) fn try_fold_one_hop_aggs(ctx: &FoldCtx<'_>) -> Result<Option<FoldedReturn>> {
    let (graph, rows, params) = (&ctx.graph, ctx.rows, ctx.params);
    let (patterns, where_clause, suffix) = (ctx.patterns, ctx.where_clause, ctx.tail);
    // CIP-236: an explicit GROUP BY supplies the grouping keys, so folding
    // by the non-aggregating items would build the wrong groups.
    let Some(output) = fold_return(suffix, params) else {
        return Ok(None);
    };
    let items = output.items;
    // One pass classifies each item once — which half it belongs to and
    // its rank within that half — into the one per-item table `ranks`
    // over the shared `Rank` vocabulary, the same vocabulary the path
    // fold's plan ranks its items with. The halves are projections of
    // the same pass, and every consumer (the hop-vars guard here, the
    // fold's group keys, per-item offers, and output projection) reads
    // them — no site re-classifies, and a new aggregate/key kind
    // updates this one pass only.
    let mut key_items: Vec<&Projection> = Vec::new();
    let mut agg_items: Vec<&Projection> = Vec::new();
    let mut ranks: Vec<Rank> = Vec::with_capacity(items.len());
    for p in items {
        let rank = if is_agg_expr(&p.expr) {
            let rank = Rank::Agg(agg_items.len());
            agg_items.push(p);
            rank
        } else {
            let rank = Rank::Key(key_items.len());
            key_items.push(p);
            rank
        };
        ranks.push(rank);
    }
    // Build the streamable-aggregate prototype in one pass; a
    // non-streamable aggregate declines the fold.
    let Some(proto_aggs) = fresh_stream_aggs(&agg_items) else {
        return Ok(None);
    };
    if patterns.len() != 1 || patterns[0].rels.len() != 1 {
        return Ok(None);
    }
    let path = &patterns[0];
    let rp = &path.rels[0];
    if path.needs_gpm()
        || rp.var_length
        || rp
            .type_expr
            .as_ref()
            .is_some_and(|_| !rp.type_expr_is_static_types())
        || path.nodes.iter().any(|n| {
            n.label_expr
                .as_ref()
                .is_some_and(|e| !e.is_static_conjunction())
        })
    {
        return Ok(None);
    }
    let hop_vars = fold_hop_vars(path);
    for p in &key_items {
        if !expr_within(&p.expr, &hop_vars) {
            return Ok(None);
        }
    }
    if !where_fold_ready(where_clause, path, rows) {
        return Ok(None);
    }
    let mut fold = OneHopFold {
        graph,
        output: &output,
        path,
        where_clause,
        ranks: &ranks,
        key_items: &key_items,
        // LABS-801: the pattern's filter resolves once for the whole
        // fold; every row's seek and every start's walk read it.
        rel_filter: RelFilter::resolve(graph, &path.rels[0]),
        proto_aggs,
        groups: IndexMap::new(),
        // ADR-0463 (LABS-781): the group table's guard starts empty and
        // grows with every landing `offer_hop` accepts.
        retained: graph.retain_bytes(0)?,
        tallies: WalkTallies::default(),
    };
    fold.run(rows)?;
    // LABS-805: the run's whole walk count lands as one flush on the
    // fold's drop, never a thread-local access per hop.
    let out = fold.finish()?;
    Ok(Some(finish_folded_rows(graph, &output, out)?))
}

/// The one-hop fold's running state: the graph, the pattern (the one
/// rel derived from it at each use site — the pattern is the single
/// source, never a field beside it), the WHERE re-evaluation, the
/// gated output (the projection items and the run's params and Cypher
/// version in the one shared [`FoldOutput`], never transcribed into
/// parallel fields beside it), the one per-item rank table with its
/// key-half projection (derived once by the dispatch gate's classify
/// pass, which reads the same halves for its own declines), the
/// streamable-aggregate prototype seats, and the group table the hop
/// offers accumulate into.
struct OneHopFold<'a> {
    graph: &'a ExecGraph<'a>,
    output: &'a FoldOutput<'a>,
    path: &'a PathPattern,
    where_clause: Option<&'a Expr>,
    ranks: &'a [Rank],
    key_items: &'a [&'a Projection],
    /// The one rel's resolved filter (LABS-801): the pattern stays the
    /// single source of the written list; this is its interned-id form
    /// plus the type expression's staticness verdict, resolved once at
    /// construction instead of per row.
    rel_filter: RelFilter,
    proto_aggs: Vec<StreamSeat<'a>>,
    groups: IndexMap<Vec<GroupKey>, (Vec<Value>, Vec<StreamSeat<'a>>)>,
    /// ADR-0463 (LABS-781): the group table's retaining-total guard —
    /// every new group's landing (map entry, key, retained key-half
    /// values) grows it, enforced at the per-value cap while the groups
    /// land, the same protocol the match-then-aggregate group map
    /// follows.
    retained: crate::overlay::RetainedTotal,
    /// The run's walk tallies (LABS-805): `accept` counts locally, the
    /// flush lands on the fold's drop after `run`.
    tallies: WalkTallies,
}

impl<'a> OneHopFold<'a> {
    /// One accepted hop (rel property filter, endpoint checks, the whole
    /// WHERE re-evaluated on the bound row, then the aggregate offer) —
    /// shared by the adjacency scan and the LABS-161 rel seek, which must
    /// filter identically. `rung` is the row's binding-schema rung for the
    /// rel bind (built once per row in [`Self::run`]) — the bind stamps it
    /// instead of rebuilding a schema per hop.
    fn offer_hop(&mut self, base: &Row, rid: RelId, eid: NodeId, rung: &RowRung) -> Result<()> {
        let Self {
            graph,
            path,
            where_clause,
            output,
            key_items,
            proto_aggs,
            groups,
            retained,
            rel_filter,
            ..
        } = self;
        // The one rel, derived from the pattern at its use site (the
        // pattern is the single source).
        let rp = &path.rels[0];
        let params = output.params;
        // The canonical rel-edge policy (type expression and properties,
        // pattern_walk) — the seek must filter identically to the scan.
        // The staticness verdict is the fold's resolved fact (LABS-801),
        // not a per-hop re-derivation.
        let hop = crate::pattern_walk::RelHop {
            pattern: rp,
            filter: rel_filter,
        };
        if !rel_edge_ok(graph, rid, hop, base, params)? {
            return Ok(());
        }
        // A row-bound end must be this hop's far node; a null or
        // non-node binding matches nothing — the same accept predicate
        // as the fold's row-bound start check, `Binding::accepts`.
        if !binding(&path.nodes[1], base).accepts(&eid) {
            return Ok(());
        }
        if !node_ok(graph, eid, &path.nodes[1], base, params)? {
            return Ok(());
        }
        let mut scratch = base.clone();
        // The canonical pre-bound rel-variable reconciliation
        // (`bind_trail`, pattern_walk): a row-bound rel variable
        // accepts only its own id — a differing rid refuses the hop
        // instead of overwriting the binding.
        if !bind_trail(&mut scratch, rp, &[rid], graph, 0, false, rung) {
            return Ok(());
        }
        bind_node_seat(&path.nodes[1], &mut scratch, graph, eid);
        // The canonical MATCH-predicate truthiness (`predicate_holds`):
        // a null or non-boolean answer does not hold.
        if let Some(where_clause) = where_clause {
            if !predicate_holds(graph, &scratch, where_clause, params)? {
                return Ok(());
            }
        }
        let vals = key_items
            .iter()
            .map(|p| eval(graph, &scratch, &p.expr, params))
            .collect::<Result<Vec<_>>>()?;
        let CachedKey { key, vals } = CachedKey::from_vals(vals);
        // ADR-0463 (LABS-781): the group-table landing charge. An
        // occupied landing retains nothing new (this row's key is
        // dropped by the lookup and its vals with it); a new group's
        // landing pays the one `add_group_entry` charge ahead of the
        // insert.
        let map_slots = groups.len();
        let entry = match groups.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                retained.add_group_entry(graph, map_slots, entry.key(), vals.len())?;
                // The per-group seats vector the landing clones (its
                // aggregate state is inline counters, so the slot buffer
                // is the whole figure).
                retained.add_bytes(
                    graph,
                    crate::workspace::buffer_bytes(
                        proto_aggs.len(),
                        size_of::<StreamSeat>() as u64,
                    ),
                )?;
                entry.insert((vals, proto_aggs.clone()))
            }
        };
        for (agg, operand) in &mut entry.1 {
            agg.offer(graph, &scratch, *operand, params, retained)?;
        }
        Ok(())
    }

    /// One enumerated (start, rel) pair — the walk tally, the far node,
    /// then the per-hop offer — shared by the adjacency scan (per
    /// neighbor) and the LABS-161 rel seek (one pair per row). The
    /// fold's counting event is the enumerated pair, tallied before the
    /// hop resolves and the offer's admission runs — the fold's own
    /// contract where the walkers tally post-admission (see
    /// `count_neighbor_walks`' doc for the per-engine events).
    fn accept(&mut self, base: &Row, sid: NodeId, rid: RelId, rung: &RowRung) -> Result<()> {
        let graph = self.graph;
        graph.cancel_tick()?;
        self.tallies.walk();
        let Some(eid) = graph.hop(sid, rid, self.path.rels[0].direction) else {
            return Ok(());
        };
        self.offer_hop(base, rid, eid, rung)
    }

    /// The start-binding preamble both paths share: the start node's
    /// label/property checks, then the row clone with the node stub
    /// bound. `None` declines the start.
    fn start_base(&self, row: &Row, sid: NodeId) -> Result<Option<Row>> {
        let Self {
            graph,
            path,
            output,
            ..
        } = self;
        if !node_ok(graph, sid, &path.nodes[0], row, output.params)? {
            return Ok(None);
        }
        let mut base = row.clone();
        bind_node_seat(&path.nodes[0], &mut base, graph, sid);
        Ok(Some(base))
    }

    /// The scan loop: one input row at a time — its binding-schema rung
    /// for the rel bind (the row's schema plus the start seat's and the
    /// rel variable's appends, each seat's absence a schema fact; built
    /// once per row, before its scan, every hop's bind_trail stamping
    /// it), the LABS-161 seek ([`fold_rel_seek`]) when it applies, else
    /// the start-node scan feeding [`Self::accept`] per neighbor.
    fn run(&mut self, rows: &[Row]) -> Result<()> {
        let conjuncts = self.where_clause.map(flatten_and_owned).unwrap_or_default();
        // LABS-801: the fold's resolved filter, shared with the walk's
        // closure — a run-local clone (an Arc share, never a
        // re-resolution): the closure borrows it while `accept` borrows
        // self mutably.
        let rel_types = self.rel_filter.types.clone();
        for row in rows {
            // Text render: the rung build reads bind names as &str.
            let node_var = self.path.nodes[0]
                .variable
                .as_ref()
                .map(|name| name.to_string());
            let rel_var = self.path.rels[0]
                .variable
                .as_ref()
                .map(|name| name.to_string());
            let rung = RowRung::past(row, [node_var.as_deref(), rel_var.as_deref()]);
            // LABS-161: an elementId()/id() equality on the rel variable (or
            // a row-bound rel) seeks the rel directly instead of scanning
            // every start node's adjacency per row.
            match fold_rel_seek(
                self.graph,
                self.path,
                &rel_types,
                &conjuncts,
                row,
                self.output.params,
            ) {
                FoldSeek::Decline => {}
                FoldSeek::Gone => continue,
                FoldSeek::Anchored(sid, rid) => {
                    if let Some(base) = self.start_base(row, sid)? {
                        // LABS-448: the walk phase — the seek route's hop runs
                        // the same shared `accept` (the `tallies.walk()`
                        // bump and `graph.hop` read the `Rels` kind counts), so
                        // it takes the same span the scan arm's hops do: a
                        // hop's phase coverage never depends on which arm of
                        // the seek match produced it.
                        crate::ctx::phase_walk(|| self.accept(&base, sid, rid, &rung))?;
                    }
                    continue;
                }
            }
            // LABS-448: the seek (the fold's pattern starts) is stamped inside
            // the one counted funnel; the walk (the neighbor traversal each
            // start feeds) is one span per arm — inclusive of the nested
            // phases inside.
            let starts = node_candidates(
                self.graph,
                &self.path.nodes[0],
                row,
                self.output.params,
                &[],
            )?;
            for sid in starts {
                self.graph.cancel_tick()?;
                let Some(base) = self.start_base(row, sid)? else {
                    continue;
                };
                let graph = self.graph;
                let rp = &self.path.rels[0];
                crate::ctx::phase_walk(|| {
                    graph.try_for_each_neighbor_resolved(sid, rp.direction, &rel_types, |rid| {
                        self.accept(&base, sid, rid, &rung)
                    })
                })?;
            }
        }
        Ok(())
    }

    /// The output loop: the accumulated groups as the folded rows, each
    /// projected through the shared `project_ranked_row` over the one
    /// rank table (the fold supplying only its aggregate finisher) —
    /// with the no-accepted-hop tail: `groups` stayed empty (the only
    /// inserts happen inside the hop offer), an all-aggregate projection
    /// answers from the seeded empty-key group, and any key column
    /// falls through to the empty row set the empty `groups` already
    /// produces — the path fold's tail shape, one exit, no compensating
    /// early return.
    fn finish(mut self) -> Result<Vec<Row>> {
        if self.groups.is_empty() && self.key_items.is_empty() {
            // ADR-0463 (LABS-781): the keyless phantom group lands
            // through the same `add_group_entry` charge every row-loop
            // landing pays (an empty key carries no payload).
            self.retained.add_group_entry(self.graph, 0, &[], 0)?;
            let proto_aggs = std::mem::take(&mut self.proto_aggs);
            self.groups.insert(Vec::new(), (Vec::new(), proto_aggs));
        }
        let Self {
            output,
            ranks,
            groups,
            ..
        } = self;
        Ok(groups
            .into_values()
            .map(|(key_vals, aggs)| {
                project_ranked_row(output.items, ranks, &key_vals, |agg| aggs[agg].0.finish())
            })
            .collect())
    }
}

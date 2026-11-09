//! Compile half of the fixed-length path fold: the plan types and the
//! helpers that lower a pattern list into a [`PathPlan`]. The runtime fold
//! lives in [`super::path`].

use std::collections::HashSet;

use relate_ir::{
    classify_aggregate, AggregateKind, BinaryOp, Expr, PathPattern, Projection, UnaryOp,
};
use relate_names::{NamedToken, SmallName};

use crate::eval::{expr_vars, is_agg_expr, orient};

use super::super::pushdown::flatten_and_owned;
use super::{fold_hop_vars, Rank};

/// A node in the folded patterns. Shared variables use one slot.
/// Compile-time only: the slot assignments flow into the plan's steps.
struct Occ {
    pattern: usize,
    node: usize,
    slot: usize,
}

/// One hop the fold walks: which relationship of which pattern, and the
/// slots its endpoints bind. The shape's steps are data — the runtime driver
/// threads a slot→id binding map so each step anchors on the slots earlier
/// steps bound.
pub(super) struct HopStep {
    pub(super) pattern: usize,
    pub(super) rel: usize,
    pub(super) start_slot: usize,
    pub(super) end_slot: usize,
}

pub(super) enum KeyPlan {
    None,
    One(usize),
    Mixed,
}

pub(super) enum PathPred {
    Cmp {
        key: NamedToken,
        op: BinaryOp,
        other: Expr,
    },
    Null {
        key: NamedToken,
        is_null: bool,
    },
}

/// The one count-seat vocabulary (ADR-0485): `count(*)` is the `Star`
/// row count, a property operand reads the slot's property, and a bare
/// node variable counts the slot's bound endpoint — the whole node. The
/// distinct flag rides inside the `Prop`/`Whole` variants, so a
/// distinct star count is unrepresentable. The seat is its own value
/// source: the runtime fold clones it into the aggregate template and
/// matches it directly, with no second source vocabulary beside it. The
/// zero-argument backtick-escaped `` `count`() `` is not an operand:
/// the fold declines it, so the general path states the contract's
/// arity rejection exactly as the one-hop fold's classification does.
#[derive(Clone)]
pub(super) enum CountSeat {
    Star,
    Prop {
        distinct: bool,
        slot: usize,
        prop: NamedToken,
    },
    Whole {
        distinct: bool,
        slot: usize,
    },
}

/// One projection item of the folded path, as `path_item` classifies it:
/// a group key, or a count seat. Compile-time only — `compile_path`
/// splits the classified items into the plan's key list, count seats,
/// and per-item ranks, and no later site re-walks this vocabulary.
enum PathItem {
    Key { slot: usize, prop: NamedToken },
    Count(CountSeat),
}

/// A slot's pushed-down conjuncts: the compiled predicates beside the
/// original expressions, so an unbound start can seek.
#[derive(Default)]
pub(super) struct SlotConjuncts {
    pub(super) preds: Vec<PathPred>,
    pub(super) exprs: Vec<Expr>,
}

pub(super) struct PathPlan {
    pub(super) nslots: usize,
    pub(super) conjuncts: Vec<SlotConjuncts>,
    /// The key half of the classified items: one `(slot, prop)` per key
    /// item, in item order. The runtime key derivation reads this list
    /// directly — no per-binding re-filter of the items.
    pub(super) keys: Vec<(usize, NamedToken)>,
    /// The count half of the classified items: one seat per count item,
    /// in item order — the fold's aggregate template.
    pub(super) seats: Vec<CountSeat>,
    /// Per projection item, its rank within its half.
    pub(super) ranks: Vec<Rank>,
    pub(super) key_plan: KeyPlan,
    pub(super) steps: Vec<HopStep>,
}

fn hop_names(patterns: &[PathPattern]) -> HashSet<String> {
    patterns.iter().flat_map(fold_hop_vars).collect()
}

fn mentions_any(expr: &Expr, names: &HashSet<String>) -> bool {
    let mut used = HashSet::new();
    expr_vars(expr, &mut used);
    used.iter().any(|name| names.contains(name))
}

/// The same relationship variable in two positions is an equijoin. The fold
/// walks each hop independently, so that pattern stays on match-then-aggregate.
fn repeats_rel_variable(patterns: &[PathPattern]) -> bool {
    let mut seen = HashSet::<&SmallName>::new();
    for path in patterns {
        for rel in &path.rels {
            if let Some(name) = &rel.variable {
                if !seen.insert(name) {
                    return true;
                }
            }
        }
    }
    false
}

fn pattern_streams(path: &PathPattern, names: &HashSet<String>) -> bool {
    !path.needs_gpm()
        && !path.has_path_name()
        && !path.is_shortest_path_function
        && path.nodes.len() == path.rels.len() + 1
        && path.rels.iter().all(|rel| {
            !rel.var_length
                && rel
                    .type_expr
                    .as_ref()
                    .is_none_or(|_| rel.type_expr_is_static_types())
                && rel
                    .properties
                    .as_ref()
                    .is_none_or(|expr| !mentions_any(expr, names))
        })
        && path.nodes.iter().all(|node| {
            node.label_expr
                .as_ref()
                .is_none_or(|expr| expr.is_static_conjunction())
                && node
                    .properties
                    .as_ref()
                    .is_none_or(|expr| !mentions_any(expr, names))
        })
}

fn occurrences(patterns: &[PathPattern]) -> (Vec<Occ>, usize) {
    let mut occ = Vec::new();
    let mut nslots = 0usize;
    // The first-seen slot table, keyed on the name domain: a variable's
    // first occurrence fixes its slot, later occurrences share it.
    let mut first: Vec<(&SmallName, usize)> = Vec::new();
    for (pattern_idx, pattern) in patterns.iter().enumerate() {
        for (node_idx, node) in pattern.nodes.iter().enumerate() {
            let slot = if let Some(name) = &node.variable {
                if let Some((_, slot)) = first.iter().find(|(seen, _)| *seen == name) {
                    *slot
                } else {
                    let slot = nslots;
                    nslots += 1;
                    first.push((name, slot));
                    slot
                }
            } else {
                let slot = nslots;
                nslots += 1;
                slot
            };
            occ.push(Occ {
                pattern: pattern_idx,
                node: node_idx,
                slot,
            });
        }
    }
    (occ, nslots)
}

/// The slot a pattern node variable occupies — the one canonical
/// variable-name → slot lookup over the occurrence table, shared by every
/// site that needs it.
fn slot_of(occ: &[Occ], patterns: &[PathPattern], name: &SmallName) -> Option<usize> {
    occ.iter().find_map(|item| {
        patterns[item.pattern].nodes[item.node]
            .variable
            .as_ref()
            .filter(|var| *var == name)
            .map(|_| item.slot)
    })
}

/// The path-fold slot of a `var.prop` access — `var_prop` mapped through
/// the occurrence table.
fn node_prop_slot(
    expr: &Expr,
    occ: &[Occ],
    patterns: &[PathPattern],
) -> Option<(usize, NamedToken)> {
    let (name, key) = relate_ir::var_prop_named(expr)?;
    let slot = slot_of(occ, patterns, name)?;
    Some((slot, key))
}

/// The path-fold slot of a bare node variable (ADR-0485's whole-node
/// value source): `var_prop` matches only `var.prop`, so a bare
/// `Expr::Variable` resolves its slot here instead of declining.
fn node_slot(expr: &Expr, occ: &[Occ], patterns: &[PathPattern]) -> Option<usize> {
    let Expr::Variable(name) = expr else {
        return None;
    };
    slot_of(occ, patterns, name)
}

/// The comparison-operator vocabulary every predicate consumer shares —
/// the folds push down (`path_conjunct` below), the batch filter
/// (ADR-0468's scan-filter chain, `batch.rs`), the kernels program
/// (`kernels/program.rs`), and the scan-predicate specialization
/// (`predicate_spec.rs`, whose compiled gates evaluate the same
/// operators through the shared `binary`) — one table, every consumer,
/// never a second list at any site.
pub(in crate::exec) fn is_pred_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Neq
            | BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Lte
            | BinaryOp::Gte
            | BinaryOp::In
            | BinaryOp::StartsWith
            | BinaryOp::EndsWith
            | BinaryOp::Contains
            | BinaryOp::Regex
    )
}

fn path_conjunct(
    expr: &Expr,
    names: &HashSet<String>,
    occ: &[Occ],
    patterns: &[PathPattern],
) -> Option<(usize, PathPred)> {
    match expr {
        Expr::Unary { op, expr } => {
            let is_null = match op {
                UnaryOp::IsNull => true,
                UnaryOp::IsNotNull => false,
                _ => return None,
            };
            let (slot, key) = node_prop_slot(expr, occ, patterns)?;
            Some((slot, PathPred::Null { key, is_null }))
        }
        Expr::Binary { op, left, right } if is_pred_op(*op) => {
            // orient owns the mirror policy: the property on the left
            // keeps the op, a property on the right of anything
            // operand-asymmetric (In, the string predicates) declines the
            // fold here.
            let ((slot, key), other, op) =
                orient(|e| node_prop_slot(e, occ, patterns), left, right, *op)?;
            if mentions_any(other, names) {
                return None;
            }
            Some((
                slot,
                PathPred::Cmp {
                    key,
                    op,
                    other: other.clone(),
                },
            ))
        }
        _ => None,
    }
}

fn path_item(expr: &Expr, occ: &[Occ], patterns: &[PathPattern]) -> Option<PathItem> {
    match expr {
        Expr::CountStar => Some(PathItem::Count(CountSeat::Star)),
        // The typed aggregate contract (ADR-0217): classify the name once
        // and match the kind — other aggregates fall through to the
        // aggregate-decline arm, non-aggregates to the key arm.
        Expr::Function {
            name,
            distinct,
            args,
        } if name.with_str(classify_aggregate) == Some(AggregateKind::Count) => {
            let seat = match args.as_slice() {
                [arg] => match node_prop_slot(arg, occ, patterns) {
                    Some((slot, prop)) => CountSeat::Prop {
                        distinct: *distinct,
                        slot,
                        prop,
                    },
                    // ADR-0485: a bare node variable counts the slot's
                    // bound endpoint — the group's binding count, or the
                    // distinct set keyed on the node id.
                    None => CountSeat::Whole {
                        distinct: *distinct,
                        slot: node_slot(arg, occ, patterns)?,
                    },
                },
                _ => return None,
            };
            Some(PathItem::Count(seat))
        }
        other if !is_agg_expr(other) => {
            let (slot, prop) = node_prop_slot(other, occ, patterns)?;
            Some(PathItem::Key { slot, prop })
        }
        _ => None,
    }
}

/// The slot of one pattern node's occurrence — every node gets one, so
/// every step endpoint resolves directly.
fn occ_slot(occ: &[Occ], pattern: usize, node: usize) -> usize {
    occ.iter()
        .find(|item| item.pattern == pattern && item.node == node)
        .expect("occurrence built for every node")
        .slot
}

/// One step of one pattern's hop: rel `rel` connects nodes `rel` and
/// `rel + 1` (pattern_streams guarantees nodes == rels + 1), so the
/// endpoint slots come straight from the occurrence table.
fn hop_step(occ: &[Occ], pattern: usize, rel: usize) -> HopStep {
    HopStep {
        pattern,
        rel,
        start_slot: occ_slot(occ, pattern, rel),
        end_slot: occ_slot(occ, pattern, rel + 1),
    }
}

pub(super) fn compile_path(
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    items: &[Projection],
) -> Option<PathPlan> {
    if repeats_rel_variable(patterns) {
        return None;
    }
    let names = hop_names(patterns);
    let (occ, nslots) = occurrences(patterns);
    // Shape gates: every pattern streams; a single pattern takes one or
    // two hops; the comma shape takes two one-hop patterns with exactly
    // one shared node variable (the second pattern anchors on the slot it
    // bound). Wider or unshared shapes decline the fold.
    let shape_ok = patterns.iter().all(|path| pattern_streams(path, &names))
        && match patterns {
            [path] => matches!(path.rels.len(), 1 | 2),
            [first, second] => {
                first.rels.len() == 1
                    && second.rels.len() == 1
                    && shared_node(first, second).is_some()
            }
            _ => false,
        };
    if !shape_ok {
        return None;
    }
    // Every accepted shape compiles the same way: one step per
    // relationship of each pattern, in pattern order.
    let occ = &occ;
    let steps = patterns
        .iter()
        .enumerate()
        .flat_map(|(p, path)| (0..path.rels.len()).map(move |r| hop_step(occ, p, r)))
        .collect();
    let mut conjuncts: Vec<SlotConjuncts> = (0..nslots).map(|_| SlotConjuncts::default()).collect();
    if let Some(where_clause) = where_clause {
        for conjunct in flatten_and_owned(where_clause) {
            let (slot, pred) = path_conjunct(&conjunct, &names, occ, patterns)?;
            let entry = &mut conjuncts[slot];
            entry.preds.push(pred);
            entry.exprs.push(conjunct);
        }
    }
    // One pass splits the classified items into their key and count
    // halves and records each item's rank within its half: the key
    // derivation, the aggregate template, and the output loop all read
    // these tables — nothing re-walks the `PathItem` vocabulary.
    let mut keys: Vec<(usize, NamedToken)> = Vec::new();
    let mut seats: Vec<CountSeat> = Vec::new();
    let mut ranks: Vec<Rank> = Vec::with_capacity(items.len());
    for item in items {
        match path_item(&item.expr, occ, patterns)? {
            PathItem::Key { slot, prop } => {
                ranks.push(Rank::Key(keys.len()));
                keys.push((slot, prop));
            }
            PathItem::Count(seat) => {
                ranks.push(Rank::Agg(seats.len()));
                seats.push(seat);
            }
        }
    }
    let key_plan = key_plan_of(&keys);
    Some(PathPlan {
        nslots,
        conjuncts,
        keys,
        seats,
        ranks,
        key_plan,
        steps,
    })
}

/// The key-slot classification of the key list: no keys, keys all
/// reading the same slot (one key column), or keys reading different
/// slots.
fn key_plan_of(keys: &[(usize, NamedToken)]) -> KeyPlan {
    match keys {
        [] => KeyPlan::None,
        keys if keys.iter().all(|(slot, _)| *slot == keys[0].0) => KeyPlan::One(keys[0].0),
        _ => KeyPlan::Mixed,
    }
}

fn shared_node<'a>(first: &'a PathPattern, second: &'a PathPattern) -> Option<&'a SmallName> {
    let mut shared = None;
    for node in &first.nodes {
        let Some(name) = node.variable.as_ref() else {
            continue;
        };
        if second
            .nodes
            .iter()
            .any(|other| other.variable.as_ref() == Some(name))
        {
            if shared.is_some() {
                return None;
            }
            shared = Some(name);
        }
    }
    shared
}

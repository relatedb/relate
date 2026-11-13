//! The contracted kernels' structural matcher (ADR-0473): the plan-time
//! recognizer that decides, linear in pattern size, whether a `MATCH`
//! clause's patterns are one of the contracted families — and compiles
//! everything the kernel runs on. Everything it cannot prove it declines,
//! and the per-row walker keeps the clause unchanged, answers identical.
//!
//! The two traversal families (ADR-0473 §Decision):
//!
//! - **Unrolled fixed-k chain** — one pattern of 2..=4 fixed single hops
//!   with per-hop predicate programs. The planner has already oriented the
//!   path's selective end to `nodes[0]` (ADR-0218's `reverse_path`), so
//!   the kernel's scan segment sits there and the hops walk forward as
//!   written.
//! - **Hub-star** — exactly two patterns, each 1..=2 fixed single hops,
//!   sharing exactly one **hub** node variable at an end of both patterns
//!   (the q19/q01/q26 family), optionally sharing a **terminal** variable
//!   at the opposite ends (a bounded cycle — q30's shared creator). The
//!   kernel enumerates hub candidates and expands both sides per hub,
//!   intersecting locally.
//!
//! The scan-shaped single node is the third family, and it already runs as
//! the scan-filter chain (`exec/batch.rs`, LABS-359): the chain kernel's
//! scan segment is that chain over id slots, so what this matcher
//! recognizes is the two traversal families, each with its scan segment
//! inside.
//!
//! Structural proofs shared by both families ([`plain_pattern`]): no GPM
//! features, no named path, no var-length or quantified hop, static type
//! lists, no property maps, and a static label conjunction per node.
//! Variable proofs: no repeated rel variable (the walker's pre-bound
//! reconciliation semantics), no node variable repetition beyond the
//! hub/terminal sharing the hub-star family is defined by. Predicate
//! proofs: every flattened `WHERE` conjunct decodes into the closed
//! vocabulary (`program`) onto exactly one pattern node variable, at most
//! one per variable.

use std::collections::{BTreeMap, HashSet};

use relate_ir::{
    pattern_prop_key, Direction, Expr, MatchMode, NodePattern, PathPattern, PatternElement,
    RelPattern,
};
use relate_names::SmallName;
use relate_planner::seed_on_rel_start;

use crate::pattern_walk::label_index_seekable;
use crate::row::Row;

use super::program::{bound_is_row_invariant, decode_conjunct, NodeTarget, Prog};

/// The most hops a chain kernel unrolls (ADR-0473's bounded set — a longer
/// chain keeps the recursive walker, whose trail machinery is the right
/// tool past a handful of hops).
pub(crate) const MAX_CHAIN_HOPS: usize = 4;

/// One matched family with everything its kernel runs on.
pub(crate) enum Contract<'a> {
    /// (a) The unrolled fixed-k chain — boxed: a contract is built once
    /// per clause and matched once, never in a hot loop, and the compiled
    /// programs carry `String`s and `Expr`s that dwarf the enum's tags.
    Chain(Box<Chain<'a>>),
    /// (b) The hub-star candidate intersection, boxed for the same reason.
    HubStar(Box<HubStar<'a>>),
}

impl Contract<'_> {
    /// The rung this contract runs (LABS-1117): the chain with its hop
    /// count, or the hub-star. The one derivation both the runtime
    /// engagement record and the PROFILE fact read.
    pub(crate) fn rung(&self) -> crate::ctx::KernelRung {
        match self {
            Contract::Chain(chain) => crate::ctx::KernelRung::Chain {
                hops: chain.hops.len(),
            },
            Contract::HubStar(_) => crate::ctx::KernelRung::HubStar,
        }
    }
}

/// One hop of a kernel's walk: the rel pattern, the traversal direction in
/// the kernel's walk order, the target node's admission, and the
/// degree-estimator's facts (the hop's seed-side node pattern and whether
/// the driving end rides the rel's start side at this hop).
pub(crate) struct Hop<'a> {
    pub(crate) rel: &'a RelPattern,
    /// The traversal direction of this hop in walk order (a walk against
    /// the written direction flips it).
    pub(crate) traverse: Direction,
    /// The hop's arrival node (the end the walk arrives at).
    pub(crate) target: NodeTarget<'a>,
    /// The hop's seed-side node pattern (the end the walk leaves).
    pub(crate) seed: &'a NodePattern,
    /// Whether the driving seed rides the rel's start side at this hop —
    /// the degree estimator's one derived fact (LABS-426's tail choice).
    pub(crate) seed_on_start: bool,
}

impl Hop<'_> {
    /// This hop scaled from the driving end by the planner's degree tail:
    /// `rows` rows sitting at the hop's seed side become the hop's
    /// arrival-side row estimate. A hop the walk model cannot price (an
    /// untyped rel, an unlabeled seed side) rides the any-label tail
    /// instead of passing the rows through (LABS-991).
    pub(crate) fn scaled_from_seed(
        &self,
        rows: u128,
        stats: &dyn relate_planner::PatternStats,
    ) -> u128 {
        relate_planner::apply_hop_bound(rows, self.seed, self.rel, self.seed_on_start, stats)
    }
}

/// The chain contract: the scan segment's subject plus the unrolled hops.
pub(crate) struct Chain<'a> {
    /// The scan segment's subject — `nodes[0]`, the planner-oriented
    /// selective end (ADR-0218's `reverse_path`).
    pub(crate) start: NodeTarget<'a>,
    /// Hops in walk order; hop `j`'s target is `nodes[j + 1]`.
    pub(crate) hops: Vec<Hop<'a>>,
}

impl Chain<'_> {
    /// The final hop (tallied in count mode, halt-capped in rows mode)
    /// and the hops before it. The compile's rel gate enforces
    /// `2..=MAX_CHAIN_HOPS` hops, so the split states an invariant — the
    /// one spelling both entry points share, never a reachable case.
    pub(crate) fn split_last_hop(&self) -> (&Hop<'_>, &[Hop<'_>]) {
        self.hops
            .split_last()
            .expect("the compile enforces 2..=MAX_CHAIN_HOPS hops")
    }
}

/// The hub-star contract: two sides that meet at a shared hub.
pub(crate) struct HubStar<'a> {
    /// The hub node target of each side's pattern, pattern order. The
    /// candidate funnel reads side 0's pattern (its labels drive the label
    /// index); side 1's program re-checks — the walker binds the hub once
    /// and reconciles, so both patterns' label conjunctions apply, and the
    /// kernel spells that as two admissions. The hub variable's own
    /// compiled conjunct (at most one) attaches to BOTH slots — the same
    /// predicate checked twice on the same node, idempotent, exactly like
    /// the labels.
    pub(crate) hub: [NodeTarget<'a>; 2],
    /// The two sides, pattern order, hops oriented hub → terminal.
    pub(crate) sides: [Vec<Hop<'a>>; 2],
    /// Whether each pattern's hub sits at its index 0 (`true`) or its
    /// last index — the walk-start fact's backward flag, and the degree
    /// estimator's orientation when a side is scaled from its terminal
    /// instead of the hub.
    pub(crate) hub_at_start: [bool; 2],
    /// Both sides end at the same named variable — the per-hub
    /// intersection key (a bounded cycle, q30's shape). `false` is the
    /// open star (q19/q01/q26): the sides only both have to be non-empty.
    pub(crate) shared_terminal: bool,
}

/// Recognize a contracted family over the planned patterns — the
/// plan-time half of engagement (ADR-0473). `None` declines: the shape is
/// not one of the families, the driving row carries bindings (a mid-statement
/// clause keeps the walker's per-row semantics), or the mode is not
/// `DIFFERENT RELATIONSHIPS` (relationship uniqueness would not apply).
///
/// Linear in pattern size: one pass over nodes and rels per pattern, plus
/// one pass over the conjuncts. Nothing here reads the graph.
pub(crate) fn compile<'a>(
    patterns: &'a [PathPattern],
    match_mode: MatchMode,
    row: &Row,
    conjuncts: &[Expr],
) -> Option<Contract<'a>> {
    if match_mode != MatchMode::DifferentRelationships || !row.is_empty() {
        return None;
    }
    match patterns.len() {
        1 => compile_chain(&patterns[0], conjuncts).map(|c| Contract::Chain(Box::new(c))),
        2 => compile_hub_star(patterns, conjuncts).map(|star| Contract::HubStar(Box::new(star))),
        _ => None,
    }
}

/// The rel-spelling facts the pattern gates share: no quantified
/// spelling, no inline property map, no inline `WHERE`. `plain_pattern`
/// declines var-length hops outright; `unroll_fixed` expands the exact
/// `*k` subset — both consult this one predicate for the spelling facts
/// they share, never a second derivation.
fn rel_plain(rel: &RelPattern) -> bool {
    !rel.quantified && rel.properties.is_none() && rel.where_clause.is_none()
}

/// The per-pattern proofs both families share. Every condition is a proof,
/// not a heuristic: a shape the kernels' loops cannot reproduce exactly
/// keeps the per-row walker.
fn plain_pattern(path: &PathPattern) -> Option<()> {
    // GPM-only features (path modes, selectors, pattern/element WHERE),
    // the shapes that clear the compact form, and named paths.
    if path.needs_gpm() || path.is_shortest_path_function || path.has_path_name() {
        return None;
    }
    if path.nodes.len() != path.rels.len() + 1 {
        return None;
    }
    for node in &path.nodes {
        // An inline element predicate is GPM (caught above; re-stated for
        // the compact form). A property map is compiled — the planner
        // folds every `WHERE var.key = value` into one, so declining maps
        // would decline the family's own queries.
        if node.where_clause.is_some() {
            return None;
        }
        // A dynamic label expression keeps the walker's label evaluation.
        if !label_index_seekable(node) {
            return None;
        }
    }
    for rel in &path.rels {
        // Var-length and quantified hops keep the recursive walker; a
        // rel property map keeps the walker's `rel_edge_ok`; a non-1
        // bound is a quantified spelling. A static single type keeps the
        // `types` list the kernel's adjacency reads (the synthesized
        // `type_expr` of every typed rel, like the nodes' `label_expr`);
        // a dynamic — or disjunctive — type expression keeps the walker,
        // and the single-type bound is what the degree estimator prices.
        if rel.var_length
            || !rel_plain(rel)
            || !rel
                .type_expr
                .as_ref()
                .is_none_or(|expr| expr.is_static_conjunction())
            || rel.min_hops != 1
            || rel.max_hops.is_some_and(|max| max != 1)
        {
            return None;
        }
    }
    Some(())
}

/// The pattern with every exact `*k` relationship spelled out as `k`
/// anonymous single hops (`(a)-[:R*3]->(b)` becomes
/// `(a)-[:R]->()-[:R]->()-[:R]->(b)`), or `None` when no relationship is
/// var-length or a spelling is not exactly unrollable: a named variable
/// (it binds a list), a property map or inline `WHERE` (the shared
/// `rel_plain` facts), a quantified, ranged or zero-length hop, or an
/// unrolled hop count past the chain bound. The unrolled pattern has the same
/// default-mode (trail) semantics — one relationship per hop, pairwise
/// distinct.
///
/// It lives here, beside [`rel_plain`] and [`plain_pattern`]: the three
/// are the pattern-spelling gates, and this one shares its rel-spelling
/// facts with the proof that declines var-length outright. Its consumer
/// is the chain count fold, which preprocesses the exact-`*k` spelling
/// into a fixed-hop pattern before compiling — `compile` itself never
/// calls it. The walker-facing var-length semantics stay
/// `relate-front`'s `quantify_var_length` (ADR-0471), which lowers the
/// parser's spellings into these bounds.
pub(super) fn unroll_fixed(path: &PathPattern) -> Option<PathPattern> {
    if !path.rels.iter().any(|rel| rel.var_length) || path.rels.len() + 1 != path.nodes.len() {
        return None;
    }
    // The per-rel unroll counts, derived once: the total (not each `k`
    // alone: one fixed hop beside a `*4` unrolls past the bound a lone
    // `*4` does not) must land in the chain's own range — one gate
    // before any allocation.
    let copies: Vec<u32> = path.rels.iter().map(fixed_copies).collect::<Option<_>>()?;
    let total = copies
        .iter()
        .fold(0usize, |sum, &c| sum.saturating_add(c as usize));
    if !(2..=MAX_CHAIN_HOPS).contains(&total) {
        return None;
    }
    let mut nodes = vec![path.nodes[0].clone()];
    let mut rels = Vec::new();
    for ((rel, next), &count) in path.rels.iter().zip(&path.nodes[1..]).zip(&copies) {
        // `count - 1` anonymous hops, then the written end node — the
        // unrolled chain's real shape, no loop index special-casing the
        // last copy (`fixed_copies` guarantees `count >= 1`).
        let mut single = rel.clone();
        single.var_length = false;
        single.min_hops = 1;
        single.max_hops = Some(1);
        for _ in 0..count - 1 {
            rels.push(single.clone());
            nodes.push(NodePattern::anonymous());
        }
        rels.push(single);
        nodes.push(next.clone());
    }
    // The built pattern is closed with the same shape proof the compile
    // applies, so "unrolled implies chain-shaped" holds at the return,
    // never by the downstream compile's grace (no behaviour change:
    // `compile` already applies exactly this proof to this pattern).
    let mut elements = vec![PatternElement::Node(nodes[0].clone())];
    for (rel, node) in rels.iter().zip(&nodes[1..]) {
        elements.push(PatternElement::Relationship(rel.clone()));
        elements.push(PatternElement::Node(node.clone()));
    }
    let unrolled = PathPattern {
        elements,
        nodes,
        rels,
        ..path.clone()
    };
    plain_pattern(&unrolled).is_some().then_some(unrolled)
}

/// One rel's fixed-hop count when the spelling unrolls exactly: `k` for a
/// var-length hop, `1` for a fixed one, `None` for a spelling that does
/// not unroll — a zero-length hop (`*0`, which would unroll to nothing
/// and silently drop its end node's variable, labels and property map), a
/// named variable (it binds a list), a ranged hop (max not the exact
/// `k`), or the shared `rel_plain` facts.
fn fixed_copies(rel: &RelPattern) -> Option<u32> {
    if !rel.var_length {
        return Some(1);
    }
    let k = rel.min_hops;
    (k != 0 && rel.max_hops == Some(k) && rel.variable.is_none() && rel_plain(rel)).then_some(k)
}

/// The pattern's compiled node programs: every conjunct decodes onto one
/// of the patterns' node variables, at most one program per variable. The
/// map is keyed by variable name; an anonymous node has no entry and no
/// program beyond its labels.
fn node_programs(patterns: &[PathPattern], conjuncts: &[Expr]) -> Option<BTreeMap<String, Prog>> {
    let node_vars = node_var_set(patterns);
    let mut progs = BTreeMap::new();
    for conjunct in conjuncts {
        let (var, prog) = decode_conjunct(conjunct)?;
        // A conjunct naming anything but one of these patterns' node
        // variables (an outer variable, a rel variable, an unbound name)
        // keeps the per-row path: its evaluation semantics are the
        // residual's, not a compiled program's.
        // The map's keys borrow the patterns (they outlive the compile),
        // so the membership check takes the decoded name and the insert
        // takes the pattern's own spelling of it.
        // Text render: the compiled program table is keyed by the
        // variable's String spelling (sorted iteration stays text order).
        let name = node_vars.get(var.as_str())?.to_string();
        if progs.contains_key(&name) {
            // Two predicates on one variable: outside the closed
            // vocabulary (ADR-0473's one-program bound).
            return None;
        }
        progs.insert(name, prog);
    }
    Some(progs)
}

/// The set of node variable names across `patterns`.
fn node_var_set(patterns: &[PathPattern]) -> HashSet<String> {
    let mut vars = HashSet::new();
    for path in patterns {
        for node in &path.nodes {
            if let Some(var) = node.variable.as_ref() {
                vars.insert(var.to_string());
            }
        }
    }
    vars
}

/// One node's compiled admission: the pattern's property map folded into
/// the vocabulary — a single row-invariant entry is exactly an equality
/// program (the planner folds `WHERE var.key = value` into this shape),
/// an empty map is nothing, and anything richer (two entries, a
/// row-dependent value) declines — plus the node's own `WHERE` conjunct
/// program, when the fold left one (the non-foldable predicates). Two
/// programs on one variable is outside the closed vocabulary's
/// one-predicate bound, so it declines.
///
/// `pub(crate)`: the frontier matcher arm (exec/kernels/frontier_match.rs)
/// folds the boundary nodes' admission through this same compile — one
/// canonical spelling of the closed vocabulary's node fold for both
/// kernels.
pub(crate) fn compile_node(
    pattern: &NodePattern,
    conjunct: Option<Prog>,
) -> Option<NodeTarget<'_>> {
    let map_prog = match &pattern.properties {
        None => None,
        Some(Expr::Map(entries)) => match entries.as_slice() {
            [] => None,
            [(key, value)] => {
                if !bound_is_row_invariant(value) {
                    return None;
                }
                Some(Prog::Cmp {
                    key: pattern_prop_key(&pattern.prop_keys, key),
                    op: relate_ir::BinaryOp::Eq,
                    bound: value.clone(),
                })
            }
            // A two-entry map is two predicates on one variable.
            _ => return None,
        },
        // A property map is always the lowered `Map` form; anything else
        // is a shape the walker's `map_from_expr` serves and this layer
        // does not re-derive.
        Some(_) => return None,
    };
    let prog = match (map_prog, conjunct) {
        (None, None) => Prog::Bare,
        (Some(prog), None) | (None, Some(prog)) => prog,
        (Some(_), Some(_)) => return None,
    };
    Some(NodeTarget { pattern, prog })
}

/// No rel variable repeats — within a pattern (the walker's pre-bound
/// reconciliation in `bind_trail`), across the pair (the same
/// reconciliation), or against any node variable. A named rel binds one
/// relationship; the kernels bind one rel per hop and cannot reproduce a
/// repeated name's reconciliation.
fn rel_vars_unique(patterns: &[PathPattern]) -> bool {
    let mut seen: HashSet<&SmallName> = HashSet::new();
    let node_vars = node_var_set(patterns);
    for path in patterns {
        for rel in &path.rels {
            if let Some(var) = rel.variable.as_ref() {
                if !seen.insert(var) || var.with_str(|text| node_vars.contains(text)) {
                    return false;
                }
            }
        }
    }
    true
}

/// The chain family: one pattern, 2..=4 fixed single hops, every node
/// variable occurring once, every conjunct compiled.
fn compile_chain<'a>(path: &'a PathPattern, conjuncts: &[Expr]) -> Option<Chain<'a>> {
    plain_pattern(path)?;
    if path.rels.len() < 2 || path.rels.len() > MAX_CHAIN_HOPS {
        return None;
    }
    // Every node variable exactly once: a repetition inside the pattern is
    // the walker's pre-bound reconciliation, not a chain.
    let mut seen: HashSet<&SmallName> = HashSet::new();
    for node in &path.nodes {
        if let Some(var) = node.variable.as_ref() {
            if !seen.insert(var) {
                return None;
            }
        }
    }
    if !rel_vars_unique(std::slice::from_ref(path)) {
        return None;
    }
    let progs = node_programs(std::slice::from_ref(path), conjuncts)?;
    let target = |index: usize| {
        compile_node(
            &path.nodes[index],
            path.nodes[index]
                .variable
                .as_ref()
                .and_then(|var| progs.get(&var.to_string()).cloned()),
        )
    };
    let start = target(0)?;
    let hops = path
        .rels
        .iter()
        .enumerate()
        .map(|(j, rel)| {
            Some(Hop {
                rel,
                // The chain walks the pattern as written, from the
                // planner-oriented selective end.
                traverse: rel.direction,
                target: target(j + 1)?,
                seed: &path.nodes[j],
                seed_on_start: seed_on_rel_start(rel, false),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Chain { start, hops })
}

/// One occurrence of a node variable: which pattern, which node index.
type Occurrence = (usize, usize);

/// The hub-star family: exactly two patterns of 1..=2 fixed single hops,
/// sharing exactly one hub variable at an end of both patterns, optionally
/// a second shared variable at the opposite ends (the terminal — a bounded
/// cycle), and no other sharing.
///
/// Which shared variable is the hub: pattern 0's start variable when it is
/// shared (the q30 shape — the comment end), else the one shared variable.
/// The opposite orientation (enumerating from the other shared end) stays
/// with the walker, whose cheapest-start machinery may already prefer it.
fn compile_hub_star<'a>(patterns: &'a [PathPattern], conjuncts: &[Expr]) -> Option<HubStar<'a>> {
    for path in patterns {
        plain_pattern(path)?;
        // 1..=2 hops per side (ADR-0473's bounded side). The floor is a
        // proof, not the ceiling's mirror: a one-node pattern (zero hops)
        // passed the ceiling alone and left the family with a side that
        // has no hops — a shape the per-hub side expansion cannot walk
        // and the cost gate's terminal estimate cannot read (`side.last`
        // on an empty side). It keeps the per-row walker, which handles
        // the bare scan pattern exactly as it always has.
        if !(2..=3).contains(&path.nodes.len()) {
            return None;
        }
    }
    if !rel_vars_unique(patterns) {
        return None;
    }
    // Occurrence accounting over node variables, name-sorted for a
    // deterministic walk: a variable may occur once (any position — a
    // middle node of a 2-hop side), or exactly twice — once per pattern,
    // both times at an end. The twice-occurring variables are the hub
    // (exactly one, required) and optionally the terminal.
    let mut occurrences: BTreeMap<String, Vec<Occurrence>> = BTreeMap::new();
    for (pi, path) in patterns.iter().enumerate() {
        for (ni, node) in path.nodes.iter().enumerate() {
            if let Some(var) = node.variable.as_ref() {
                occurrences
                    .entry(var.to_string())
                    .or_default()
                    .push((pi, ni));
            }
        }
    }
    let mut shared: Vec<&str> = Vec::new();
    for (var, occ) in &occurrences {
        match occ.as_slice() {
            [_] => {}
            [(p0, _), (p1, _)] => {
                if p0 == p1 {
                    // Twice inside one pattern: a repeat, not a shared end.
                    return None;
                }
                for &(pi, ni) in occ {
                    let last = patterns[pi].nodes.len() - 1;
                    // The hub must sit at an end for the per-hub side
                    // expansion; a middle occurrence is not an end.
                    if ni != 0 && ni != last {
                        return None;
                    }
                }
                shared.push(var.as_str());
            }
            // More than twice: not a shape the family defines.
            _ => return None,
        }
    }
    // Exactly one shared variable is required (the hub); a second (the
    // terminal) must sit at the opposite end of the hub in both patterns.
    let hub_var = match shared.as_slice() {
        [] => return None, // no shared variable: not a hub-star
        [only] => *only,
        [a, b] => {
            for pi in 0..2 {
                let a_at = occurrence_index(patterns, pi, a);
                let b_at = occurrence_index(patterns, pi, b);
                let last = patterns[pi].nodes.len() - 1;
                let a_end = a_at == 0 || a_at == last;
                let b_end = b_at == 0 || b_at == last;
                if !a_end || !b_end || (a_at == 0) == (b_at == 0) {
                    return None;
                }
            }
            // Pattern 0's start variable when shared (the q30 shape);
            // otherwise the name-sorted first.
            if occurrence_index(patterns, 0, b) == 0 {
                *b
            } else {
                *a
            }
        }
        _ => return None,
    };
    let shared_terminal = shared.len() == 2;
    let progs = node_programs(patterns, conjuncts)?;
    let node_target = |pi: usize, ni: usize| {
        compile_node(
            &patterns[pi].nodes[ni],
            patterns[pi].nodes[ni]
                .variable
                .as_ref()
                .and_then(|var| progs.get(&var.to_string()).cloned()),
        )
    };
    // The hub's own WHERE conjunct (and its folded map — one program per
    // variable, whichever the planner left) attaches to BOTH slots: the
    // same predicate checked twice on the same node, idempotent, the
    // walker's bind-once-and-reconcile spelled as two admissions.
    let hub = [
        node_target(0, occurrence_index(patterns, 0, hub_var))?,
        node_target(1, occurrence_index(patterns, 1, hub_var))?,
    ];
    let mut sides = [Vec::new(), Vec::new()];
    let mut hub_at_start = [false; 2];
    for pi in 0..2 {
        let path = &patterns[pi];
        let count = path.rels.len();
        hub_at_start[pi] = occurrence_index(patterns, pi, hub_var) == 0;
        for step in 0..count {
            // Oriented hub → terminal: from the hub's end, hop by hop.
            // A hub-at-start side walks the rels forward as written; a
            // hub-at-last side walks them backward, flipped.
            let (j, traverse, reversed) = if hub_at_start[pi] {
                (step, path.rels[step].direction, false)
            } else {
                let j = count - 1 - step;
                (j, path.rels[j].direction.flip(), true)
            };
            let rel = &path.rels[j];
            // Walking forward leaves node `j` and arrives at `j + 1`;
            // walking backward leaves `j + 1` and arrives at `j`.
            let (seed, arrival) = if hub_at_start[pi] {
                (&path.nodes[j], j + 1)
            } else {
                (&path.nodes[j + 1], j)
            };
            sides[pi].push(Hop {
                rel,
                traverse,
                target: node_target(pi, arrival)?,
                seed,
                seed_on_start: seed_on_rel_start(rel, reversed),
            });
        }
    }
    Some(HubStar {
        hub,
        sides,
        hub_at_start,
        shared_terminal,
    })
}

/// The node index of `var`'s occurrence in `patterns[pi]`.
fn occurrence_index(patterns: &[PathPattern], pi: usize, var: &str) -> usize {
    patterns[pi]
        .nodes
        .iter()
        .position(|node| node.variable.as_ref().is_some_and(|v| v == var))
        .unwrap_or_else(|| panic!("a shared variable occurs in every pattern: {var}"))
}

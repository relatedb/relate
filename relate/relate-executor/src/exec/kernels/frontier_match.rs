//! The frontier family's plan-time matcher arm (ADR-0478 §2): the proofs
//! that decide whether one `MATCH` clause's quantified pattern is
//! frontier-eligible, and the compiled artifact the BFS runner and its
//! wiring (this feature's `frontier-core` / `rider-integration` nodes)
//! consume. Every gate is a proof, never a heuristic — linear in pattern
//! size plus the NFA compile (itself linear in the unrolled state
//! count), nothing reads the graph beyond the interner the NFA interns
//! through and the planner's estimate machinery the cost gate reads —
//! and every decline keeps the unchanged per-row DFS: answers identical,
//! so the decline is always safe.
//!
//! Three gate halves:
//!
//! - **Structural** ([`compile_frontier`]'s pattern proofs plus the NFA
//!   compiler, [`super::nfa`]): one pattern with exactly one top-level
//!   `Quantified` element — the element every var-length spelling lowers
//!   to since ADR-0471, so every spelling reaches this arm — whose inner
//!   elements are plain rel hops over static type lists, nested bounded
//!   quantifiers per the compiler's soundness rule. **The compiler's own
//!   declines are this arm's declines** ([`FrontierDecline::NfaShape`]):
//!   a second top-level quantified element, a subpath variable, a pattern
//!   WHERE or a selector, rel-predicate inner hops, admission-bearing
//!   interior nodes, the 64-state ceiling, a nested unbounded quantifier,
//!   broken alternation, an uninterned type name, overflowing bounds. The
//!   boundary nodes' own admission rides the kernel layer's closed
//!   predicate vocabulary ([`NodeTarget`] — the chain/hub-star matcher's
//!   own fold, [`super::matcher::compile_node`]), plus one pair
//!   comparison between the two boundary variables ([`BoundaryPair`],
//!   `n <> p`) that the distinct-endpoint consumer decides per
//!   (seed, endpoint) pair.
//! - **Answer-shape** ([`Consumer`] and [`decode_consumer`]): the
//!   semantic half, as much a proof as the structural one — the frontier
//!   serves exactly the consumers whose answers are functions of the
//!   (node, state) *pair*, not of the *path*. ADR-0478 Decision 2's
//!   fourth shape — bounded WALK mode — is carried as the pool fact
//!   ([`FrontierContract::pool`]), not a consumer variant: WALK
//!   changes which witnesses are exact (the frontier's native semantics
//!   for in-bounds arrivals, the re-arrival rule's walk-length check for
//!   below-min candidates, ADR-0478 §3), never which consumers are
//!   pair-functions, and full enumeration declines under every mode.
//! - **Cost** ([`frontier_engages`], in [`super::frontier_cost`]): engage
//!   when the quantifier is unbounded (the DFS cannot halt below its
//!   rel-count cap, so a cyclic type system makes the enumeration
//!   exponential while the answer stays pair-shaped), or when the
//!   measured per-unit costs say the DFS's trails cost more than the
//!   frontier's pairs and probes; under that break-even the DFS keeps
//!   the clause (short bounds, the bounded acyclic-schema recursions). The diagnostic
//!   switch's forced rung (ADR-0519, `RELATE_PATTERN_ENGINE=nfa`)
//!   bypasses this gate alone — every other proof stands.

use std::collections::{BTreeMap, HashSet};

use relate_core::{NodeId, Value};
use relate_ir::{
    classify_aggregate, AggregateKind, BinaryOp, Clause, Expr, MatchMode, NodePattern, PathMode,
    PathPattern, PatternElement, QuantifiedPathPattern,
};
use relate_names::SmallName;
use relate_planner::PatternStats;

use crate::eval::expr_vars;
use crate::frontier_decline::FrontierDecline;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::pattern_engine::PatternEngine;
use crate::pattern_walk::label_index_seekable;
use crate::row::Row;

use super::super::match_cap::return_with_suffix;
use super::super::pattern_policies::{gpm_group_vocabulary, inlined_all_reduces};
use super::frontier_cost::frontier_engages;
use super::matcher::compile_node;
use super::nfa::{self, FrontierNfa, Pool};
use super::program::{decode_conjunct, NodeTarget, Prog};

/// The answer shapes the frontier serves (ADR-0478 Decision 2): the
/// consumers whose answers are functions of the (node, state) pair, not
/// of the path. Constructed by the wiring site — the EXISTS probe arm
/// builds [`Consumer::Exists`] directly; everything row-shaped goes
/// through [`decode_consumer`], the statement-suffix decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consumer {
    /// An EXISTS probe (or boolean-count fold) arm: a verdict, nothing
    /// consumed — a pair reaching an accepting state is a witness.
    Exists,
    /// A distinct-endpoint answer: `count(DISTINCT y)`, `RETURN DISTINCT
    /// y`, `collect(DISTINCT y)`, `min`/`max` over the endpoint — one row
    /// per accepting arrival over the boundary variables, running to
    /// fixpoint or `max` (a riding `LIMIT` only truncates the endpoint
    /// rows afterwards, a contract-free prefix either way). Carries the
    /// clause's [`BoundaryPair`]: only this consumer's answer is the pair
    /// set the comparison filters, so no other consumer can hold one.
    DistinctEndpoints { pair: BoundaryPair },
    /// A walk count (`count(*)`/`count(<boundary var>)`, no DISTINCT)
    /// under the bounded WALK pool with an unfiltered target — the
    /// counting arm (`frontier_count`): the walk multiset contracts to
    /// per-level (node, state) multiplicities and the last leg to a
    /// degree sum, so no walk is enumerated. Constructed by that fold's
    /// own wiring, never by [`decode_consumer`] (a row-shaped rung has
    /// no row to emit for it).
    WalkCount,
    /// Un-ordered capped emission (`LIMIT` without `ORDER BY`, the cap
    /// rider's `Plain` shape): at most `want` witnesses reconstructed
    /// output-sensitively after the BFS, the path variable and group
    /// lists consumed through them. BFS-discovery order replaces DFS
    /// order (ADR-0478 Decision 4 — LIMIT without ORDER BY has no order
    /// contract).
    Capped { want: usize },
}

/// The boundary-pair comparison a `WHERE` conjunct between the two
/// boundary variables states (`n <> p`): the one vocabulary entry beyond
/// the per-variable property predicates, decided per (seed, endpoint)
/// pair at arrival. Only `<>` is admitted — the pair set a `<>` filters is
/// exactly the pairs the BFS reports (the walker's row filter over the
/// same trail endpoints), the self pair dropped. Carried only by the
/// distinct-endpoint consumer, whose answer is the pair set itself: every
/// other consumer declines the conjunct (`Conjunct`), as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundaryPair {
    /// No conjunct between the boundary variables: every admitted
    /// endpoint answers.
    Unconstrained,
    /// `start <> target`: an endpoint that is the seed itself — the seed
    /// re-reached by a cycle or a self-loop, or the zero-length arrival
    /// of a `min = 0` quantifier — does not answer.
    Distinct,
}

impl BoundaryPair {
    /// Whether the endpoint answers for its seed. Identity, not value
    /// equality: the walker's `<>` over two node values compares node
    /// identity.
    pub(crate) fn admits(self, seed: NodeId, endpoint: NodeId) -> bool {
        match self {
            BoundaryPair::Unconstrained => true,
            BoundaryPair::Distinct => seed != endpoint,
        }
    }
}

impl Consumer {
    /// The boundary-pair comparison this consumer's answer filters by:
    /// the distinct-endpoint consumer's payload, [`BoundaryPair::Unconstrained`]
    /// for every consumer that cannot carry one.
    pub(crate) fn pair(self) -> BoundaryPair {
        match self {
            Consumer::DistinctEndpoints { pair } => pair,
            Consumer::Exists | Consumer::WalkCount | Consumer::Capped { .. } => {
                BoundaryPair::Unconstrained
            }
        }
    }
}

/// The plan-time artifact the matcher returns: everything the BFS runner
/// (`frontier-core`) and the wiring (`rider-integration`) run on. Built
/// once per clause and shared by every seed of the run — the capped
/// consumer's per-seed remaining want rides the emission's own
/// parameter, never a contract copy.
pub(crate) struct FrontierContract<'a> {
    /// The pattern the NFA compiled from — the row-binding vocabulary
    /// (boundary variables, group lists, subpath and path variables) the
    /// witness reconstruction reads.
    pub(crate) pattern: &'a PathPattern,
    /// The compiled NFA (ADR-0478 Decision 1): states, transitions, hop
    /// bounds — the BFS runner's input.
    pub(crate) nfa: FrontierNfa,
    /// The seed-side boundary admission (labels plus at most one
    /// predicate program) — the scan segment's target, the single-seed
    /// frontier's start.
    pub(crate) start: NodeTarget<'a>,
    /// The target-side boundary admission — the arrival check.
    pub(crate) target: NodeTarget<'a>,
    /// The answer shape the runner serves (the answer-shape gate's
    /// survivor).
    pub(crate) consumer: Consumer,
    /// The pool a run of this contract answers in (ADR-0478 §3), set
    /// once here from the match mode and the written path mode — the
    /// typed fact every driver reads, never re-derived: `Walk` for the
    /// bounded WALK spelling (the frontier's native semantics for
    /// in-bounds arrivals, the re-arrival rule's walk-length check for
    /// its below-min candidates), `Default` for everything else (the
    /// default pool's trail exactness is the rule's jurisdiction).
    pub(crate) pool: Pool,
}

/// The frontier family's matcher arm: the plan-time proofs that decide
/// whether one `MATCH` clause's quantified pattern is frontier-eligible,
/// returning the compiled artifact. `Err` names the gate that declined —
/// the caller keeps the unchanged per-row DFS, answers identical.
///
/// Call with the same entering row and flattened MATCH-WHERE conjuncts
/// the GPM expansion site sees: the allReduce gate reads the inliner's
/// own extraction against that row, so its answer is exact there.
pub(crate) fn compile_frontier<'a>(
    graph: &Graph,
    patterns: &'a [PathPattern],
    match_mode: MatchMode,
    row: &Row,
    conjuncts: &[Expr],
    consumer: Consumer,
    stats: &dyn PatternStats,
) -> Result<FrontierContract<'a>, FrontierDecline> {
    // One pattern: the frontier is a single-pattern, single-quantifier
    // family (each UNION arm is its own clause).
    let [pattern] = patterns else {
        return Err(FrontierDecline::MultiplePatterns);
    };
    let (start_node, target_node) = boundary(pattern).ok_or(FrontierDecline::NfaShape)?;
    // The entering row may bind exactly the seed-side variable — the
    // single-seed frontier's premise. Anything else is a prebound pin
    // (the legacy exact-trail rule binds a group name to a list) or a
    // mid-statement clause's outer binding.
    let seed_var = start_node.variable.as_ref();
    if row
        .keys()
        .any(|name| seed_var.is_none_or(|var| var != name.as_str()))
    {
        return Err(FrontierDecline::CarryingRow);
    }
    // The mode proofs. The default pool's per-witness mode checks are
    // the emission's (`frontier_emit`): an explicit TRAIL rides the
    // default pool exactly like the unwritten mode, and the CAPPED
    // consumer carries the per-witness ACYCLIC/SIMPLE cycle checks —
    // every other consumer of an explicit mode stays per-path (full
    // enumeration: two trails to one node are two answers;
    // distinct-endpoint: an acyclic endpoint set is not a pair
    // function), and the REPEATABLE pool has no version of a
    // trail-enforcing mode at all.
    if match_mode == MatchMode::RepeatableElements
        && matches!(
            pattern.mode,
            Some(PathMode::Trail | PathMode::Acyclic | PathMode::Simple)
        )
    {
        return Err(FrontierDecline::Mode);
    }
    if matches!(
        pattern.mode,
        Some(PathMode::Trail | PathMode::Acyclic | PathMode::Simple)
    ) && !matches!(consumer, Consumer::Capped { .. })
    {
        return Err(FrontierDecline::Mode);
    }
    // The pool a run answers in (ADR-0478 §3): the bounded WALK
    // spelling — the REPEATABLE pool with no trail-enforcing path mode,
    // the one spelling whose rel reuse is legal. Its in-bounds arrivals
    // are the frontier's native pair answers, and its below-min
    // candidates are answered by the re-arrival rule's walk-length
    // check — the candidate rule the walk pool shares with the default
    // pool's own candidate rule. Everything else answers in the default
    // pool.
    let pool = if match_mode == MatchMode::RepeatableElements
        && pattern.mode.is_none_or(|mode| mode == PathMode::Walk)
    {
        Pool::Walk
    } else {
        Pool::Default
    };
    if pool == Pool::Walk
        && pattern
            .elements
            .iter()
            .any(|element| matches!(element, PatternElement::Quantified(q) if q.max.is_none()))
    {
        // Unbounded REPEATABLE ELEMENTS keeps its authored rejection
        // (ADR-0478 §5) — declining keeps the DFS path that raises it.
        return Err(FrontierDecline::Mode);
    }
    // The allReduce conflict (ADR-0478 §2, hard decline): the inliner's
    // own extraction, against this row — a spec it returns is a group of
    // this quantification (its qualification rules require exactly that).
    if !inlined_all_reduces(pattern, row, conjuncts).is_empty() {
        return Err(FrontierDecline::AllReduceGroup);
    }
    if !names_unique(pattern) {
        return Err(FrontierDecline::PinnedBinding);
    }
    // The spine gate (the emission's premise, `frontier_emit`): the one
    // quantified element's inner chain must be plain one-hop rels. A
    // var-length inner rel or a nested quantified group unrolls the
    // spine (variable round length) or accumulates list-of-list groups,
    // and the fixed-round reconstruction cannot bind those rows — the
    // DFS keeps the shape, answers identical.
    if pattern
        .elements
        .iter()
        .any(|element| matches!(element, PatternElement::Quantified(qpp) if spine_unrolls(qpp)))
    {
        return Err(FrontierDecline::NestedSpine);
    }
    // The boundary conjuncts: every flattened WHERE conjunct decodes into
    // the closed predicate vocabulary onto one boundary variable, at most
    // one per variable (the property-map fold's own bound).
    // Text render: the compiled boundary programs are keyed by the
    // variable's String spelling (sorted iteration stays text order).
    let mut progs: BTreeMap<String, Prog> = BTreeMap::new();
    let mut consumer = consumer;
    for conjunct in conjuncts {
        if is_boundary_pair_neq(conjunct, start_node, target_node) {
            // The pair comparison rides only the consumer whose answer is
            // the pair set; the others keep their old decline.
            let Consumer::DistinctEndpoints { pair } = &mut consumer else {
                return Err(FrontierDecline::Conjunct);
            };
            *pair = BoundaryPair::Distinct;
            continue;
        }
        let Some((var, prog)) = decode_conjunct(conjunct) else {
            return Err(FrontierDecline::Conjunct);
        };
        let Some(name) = boundary_vars(start_node, target_node)
            .into_iter()
            .flatten()
            .find(|name| *name == var.as_str())
        else {
            return Err(FrontierDecline::Conjunct);
        };
        if progs.insert(name.to_string(), prog).is_some() {
            // Two predicates on one boundary variable: the closed
            // vocabulary's one-program bound.
            return Err(FrontierDecline::Conjunct);
        }
    }
    let start = boundary_target(start_node, &progs).ok_or(FrontierDecline::Conjunct)?;
    let target = boundary_target(target_node, &progs).ok_or(FrontierDecline::Conjunct)?;
    // The diagnostic switch (ADR-0519), resolved once here for the two
    // facts it decides: the NFA compiler's rung (the forced rung compiles
    // the gap classes) and the cost gate below.
    let engine = graph.pattern_engine();
    // The structural proof engine: the NFA compiler's declines are this
    // arm's declines.
    let nfa = nfa::compile(graph, pattern, engine).ok_or(FrontierDecline::NfaShape)?;
    // A predicate-guarded NFA (the forced rung's class F — a quantified
    // group whose WHERE references its own pattern variables) compiles,
    // and the runner evaluates its guards, but no consumer proof covers
    // serving it: the pair-dedup answers these shapes approximately (the
    // predicate reads each referenced variable's last binding on the
    // first-arrival chain), so every consumer declines and the DFS
    // answers exactly. Per-state guards (class K) carry no such fact —
    // a node either satisfies the position's pattern or not — and serve.
    if nfa.predicates.iter().any(Option::is_some) {
        return Err(FrontierDecline::GuardedGroup);
    }
    // The cost gate, forced off by the diagnostic switch (ADR-0519):
    // `RELATE_PATTERN_ENGINE=nfa` takes every frontier-compilable
    // clause — every proof above (and the allocation ceiling and the
    // run-time budget below) still stands, and a decline still fails
    // through.
    // The walk count needs no cost gate: its level multiplicities touch
    // each (node, state) pair once per level the DFS would reach it at
    // least once on, it allocates no span tables, and the DFS it
    // replaces enumerates every walk.
    if engine != PatternEngine::Nfa
        && consumer != Consumer::WalkCount
        // An unbounded spine engages on shape alone: no gate to run.
        && nfa.max_hops.is_some()
        && !frontier_engages(
            stats,
            u128::try_from(graph.next_node_id())
                .expect("node ids are allocator-issued upward from zero"),
            conjuncts,
            start_node,
            target_node,
            &pattern.elements,
        )
    {
        return Err(FrontierDecline::Cost);
    }
    Ok(FrontierContract {
        pattern,
        nfa,
        start,
        target,
        consumer,
        pool,
    })
}

/// Whether `conjunct` is `<>` between the two boundary variables, either
/// order — the one pair comparison the closed vocabulary admits
/// ([`BoundaryPair::Distinct`]). Both sides must be exactly the pattern's
/// two named boundary variables: a comparison against an interior, outer
/// or group variable, or against the same variable twice, is not this
/// shape and keeps the `Conjunct` decline.
fn is_boundary_pair_neq(conjunct: &Expr, start: &NodePattern, target: &NodePattern) -> bool {
    let Expr::Binary {
        op: BinaryOp::Neq,
        left,
        right,
    } = conjunct
    else {
        return false;
    };
    let (Expr::Variable(left), Expr::Variable(right)) = (left.as_ref(), right.as_ref()) else {
        return false;
    };
    let [Some(start), Some(target)] = boundary_vars(start, target) else {
        return false;
    };
    (left == start && right == target) || (left == target && right == start)
}

/// The consumer of a `MATCH`'s rows as the statement's suffix proves it —
/// the answer-shape gate's plan-time half over the first suffix clause
/// (the same parse the cap riders read, `match_cap::return_with_suffix`).
/// Conservative everywhere the shape is only approximately knowable: a
/// suffix outside the frontier-sound vocabulary declines, and the DFS
/// answers identically.
pub(crate) fn decode_consumer(
    suffix: &[Clause],
    pattern: &PathPattern,
    graph: &ExecGraph<'_>,
    params: &BTreeMap<String, Value>,
) -> Result<Consumer, FrontierDecline> {
    let Some(clause) = suffix.first() else {
        // No suffix at all cannot happen for a planned statement; this is
        // the defensive arm of "not a RETURN/WITH" below.
        return Err(FrontierDecline::FullEnumeration);
    };
    let Some(s) = return_with_suffix(clause) else {
        // A suffix that starts with anything but a RETURN/WITH feeds the
        // rows onward — every row is the answer.
        return Err(FrontierDecline::FullEnumeration);
    };
    // Grouped output is not one row per match (CIP-236 — every per-match
    // cap declines it, and so does the pair-shaped answer); a filtered
    // WITH re-filters rows the enumeration would have to produce whole.
    if s.group_by.is_some() || s.where_clause.is_some() {
        return Err(FrontierDecline::FullEnumeration);
    }
    let (start, target) = boundary(pattern).ok_or(FrontierDecline::NfaShape)?;
    // Every name the clause consumes: the projection items and the order
    // keys (an `ORDER BY` over a path shape consumes it as surely as a
    // projected item does).
    let mut referenced: HashSet<String> = HashSet::new();
    for item in s.items {
        expr_vars(&item.expr, &mut referenced);
    }
    for order in s.order {
        expr_vars(&order.expr, &mut referenced);
    }
    let names_boundary_only =
        |referenced: &HashSet<String>| names_boundary_only(referenced, start, target);

    // The aggregate calls: an endpoint fold is sound exactly when the
    // aggregate is idempotent over duplicates (min/max) or collapses them
    // itself (`count`/`collect` with DISTINCT); everything else folds the
    // path multiset.
    let mut calls: Vec<(AggregateKind, bool)> = Vec::new();
    let mut count_star = false;
    for item in s.items {
        count_star |= aggregate_calls(&item.expr, &mut calls);
    }
    if count_star {
        // `count(*)` counts rows — paths, not endpoints.
        return Err(FrontierDecline::PathCount);
    }
    if !calls.is_empty() {
        let endpoint_fold = |(kind, distinct): &(AggregateKind, bool)| {
            matches!(kind, AggregateKind::Min | AggregateKind::Max)
                || (matches!(kind, AggregateKind::Count | AggregateKind::Collect) && *distinct)
        };
        if !calls.iter().all(endpoint_fold) {
            return Err(FrontierDecline::PathCount);
        }
        if !names_boundary_only(&referenced) {
            // An endpoint fold over the path variable or a group list
            // (`min(length(p))`) reads bindings only witnesses carry.
            return Err(FrontierDecline::PathConsumer);
        }
        return Ok(Consumer::DistinctEndpoints {
            pair: BoundaryPair::Unconstrained,
        });
    }
    if s.distinct {
        if !names_boundary_only(&referenced) {
            return Err(FrontierDecline::PathConsumer);
        }
        return Ok(Consumer::DistinctEndpoints {
            pair: BoundaryPair::Unconstrained,
        });
    }
    // The capped shape: `LIMIT` without `ORDER BY`, the cap rider's own
    // `Plain` want (`SKIP + LIMIT`). Any projection rides — the ≤ want
    // reconstructed witnesses bind everything the DFS's rows bind.
    let want = match s.skip_limit_want(graph, params) {
        Ok(want) => want,
        // A LIMIT that errors here declines; the DFS path's own cap
        // construction (`match_limit_cap`) raises the identical error.
        Err(_) => return Err(FrontierDecline::FullEnumeration),
    };
    let Some(want) = want else {
        // No LIMIT (or an overflowing SKIP+LIMIT): every row is consumed.
        return Err(FrontierDecline::FullEnumeration);
    };
    if !s.order.is_empty() {
        return Err(FrontierDecline::OrderedCap);
    }
    if want == 0 {
        // `LIMIT 0`: the cap rider declines it too (nothing to halt on) —
        // keep the DFS path's own behavior.
        return Err(FrontierDecline::FullEnumeration);
    }
    Ok(Consumer::Capped { want })
}

/// Whether the quantified spine's inner chain is anything but plain
/// one-hop rels — a var-length rel or a nested quantified group: the
/// shape the emission's fixed-round reconstruction (`frontier_emit`)
/// cannot bind rows for, the `NestedSpine` decline's predicate.
fn spine_unrolls(qpp: &QuantifiedPathPattern) -> bool {
    qpp.elements.iter().any(|element| match element {
        PatternElement::Relationship(rel) => rel.var_length,
        PatternElement::Quantified(_) => true,
        PatternElement::Node(_) => false,
    })
}

/// The pattern's boundary node patterns: the first and last elements —
/// the seed side and the target side of the walk. `None` when the
/// elements do not start and end with a node (the alternation the NFA
/// compile itself requires and declines). `pub(crate)`: the emission's
/// test suite builds its boundary node targets through it.
pub(crate) fn boundary(pattern: &PathPattern) -> Option<(&NodePattern, &NodePattern)> {
    pattern.boundary_nodes()
}

/// The boundary pair's variable names, seed side first — the one
/// spelling of the boundary-variable fact every re-derivation site
/// consults: the conjunct decode's name lookup (here, in
/// [`compile_frontier`]), the consumer decode's boundary-only rule
/// ([`decode_consumer`]), and the exists arm's counted-expression gate
/// (`frontier_run::try_frontier_exists`) — both of the latter through
/// [`names_boundary_only`].
pub(crate) fn boundary_vars<'a>(
    start: &'a NodePattern,
    target: &'a NodePattern,
) -> [Option<&'a SmallName>; 2] {
    [start.variable.as_ref(), target.variable.as_ref()]
}

/// Whether every name in `referenced` is one of the boundary pair's
/// variables — the one closed-boundary rule: the frontier binds only the
/// boundary pair (a full node value each), so an expression naming
/// anything else (the path variable, a group list, an interior or outer
/// variable) reads a binding only a witness carries.
pub(crate) fn names_boundary_only(
    referenced: &HashSet<String>,
    start: &NodePattern,
    target: &NodePattern,
) -> bool {
    let boundary = boundary_vars(start, target);
    referenced.iter().all(|name| {
        boundary
            .into_iter()
            .flatten()
            .any(|var| var == name.as_str())
    })
}

/// One boundary node's compiled admission: the canonical fold of the
/// pattern's property map and its decoded conjunct (the chain/hub-star
/// matcher's own `compile_node`), over the boundary facts that fold
/// covers. An inline element predicate (`WHERE` on the node itself) and
/// a dynamic label expression decline here — `compile_node` reads
/// neither, so riding one would silently drop the check (the NFA
/// compiler's own rule for interior nodes, `plain_node`; the walker
/// keeps the shape, answers identical).
fn boundary_target<'a>(
    node: &'a NodePattern,
    progs: &BTreeMap<String, Prog>,
) -> Option<NodeTarget<'a>> {
    if node.where_clause.is_some() || !label_index_seekable(node) {
        return None;
    }
    compile_node(
        node,
        node.variable
            .as_ref()
            .and_then(|var| progs.get(&var.to_string()).cloned()),
    )
}

/// The top-level element names are unique: every node element's variable
/// (the boundary pair and the interior context nodes) and every plain
/// rel's variable occurs exactly once, the quantified groups' inner
/// names bind each name once (an inner node variable colliding with an
/// inner rel variable is a pin, not a merge), and the groups' accumulated
/// names (group lists, subpath variables — the canonical
/// `gpm_group_vocabulary` spelling) collide with none of them — the tail
/// check owns that last fact, so no mid-scan copy of it exists. A
/// repeat is the walker's pre-bound reconciliation, which the
/// pair-dedup cannot reproduce.
fn names_unique(pattern: &PathPattern) -> bool {
    // The plain-element half: one per-scope scan over the top-level
    // elements — the same skeleton every quantified group's inner scan
    // below runs.
    let Some((nodes, rels)) = named_element_scan(&pattern.elements) else {
        return false;
    };
    let mut groups: HashSet<String> = HashSet::new();
    for element in &pattern.elements {
        let PatternElement::Quantified(qpp) = element else {
            continue;
        };
        // One group's inner vocabulary binds each name once per kind —
        // the same per-scope scan the top level runs. The scan is
        // kind-aware because the accumulated vocabulary
        // (`gpm_group_vocabulary`, below) merges both kinds into one
        // name set and would hide the collision.
        if named_element_scan(&qpp.elements).is_none() {
            return false;
        }
        // The group's whole vocabulary — inner element names plus the
        // subpath variable — from the one spelling every consumer reads.
        let mut vocabulary = Vec::new();
        gpm_group_vocabulary(qpp, &mut vocabulary);
        groups.extend(vocabulary);
    }
    // The collected group vocabulary is String-keyed; each entry is
    // classified into the name domain once, here, to probe the pattern's
    // own element-name sets.
    groups.iter().all(|name| {
        let probe = SmallName::from_name(name.as_str());
        !nodes.contains(&probe) && !rels.contains(&probe)
    })
}

/// One scope's plain-element name scan: the node elements' variables and
/// the plain rels' variables, each an insert-or-reject against the
/// sibling kind, `None` the collision verdict — the one skeleton the
/// top-level element scan and every quantified group's inner scan run
/// (`names_unique`). A quantified element is passed by: its names are
/// the caller's to accumulate (the top level's `groups` set; a nested
/// group contributes only its subpath variable, through the enclosing
/// group's `gpm_group_vocabulary` — its inner names are never checked
/// here, and a nested quantified shape is declined before the frontier
/// by the spine gate).
fn named_element_scan<'s>(
    elements: &'s [PatternElement],
) -> Option<(HashSet<&'s SmallName>, HashSet<&'s SmallName>)> {
    let mut nodes: HashSet<&'s SmallName> = HashSet::new();
    let mut rels: HashSet<&'s SmallName> = HashSet::new();
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if let Some(var) = node.variable.as_ref() {
                    if !insert_or_reject(&mut nodes, &rels, var) {
                        return None;
                    }
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(var) = rel.variable.as_ref() {
                    if !insert_or_reject(&mut rels, &nodes, var) {
                        return None;
                    }
                }
            }
            PatternElement::Quantified(_) => {}
        }
    }
    Some((nodes, rels))
}

/// One scope's insert-or-reject: `var` binds in `own`'s kind — a repeat
/// is the pre-bound pin — and must not already bind in the sibling kind.
/// Both of `named_element_scan`'s kinds consult the one spelling,
/// symmetrically.
fn insert_or_reject<'s>(
    own: &mut HashSet<&'s SmallName>,
    sibling: &HashSet<&'s SmallName>,
    var: &'s SmallName,
) -> bool {
    own.insert(var) && !sibling.contains(var)
}

/// The aggregate calls inside `expr`, each as its typed kind plus the
/// call's DISTINCT flag; the return value is whether a `count(*)` rides.
fn aggregate_calls(expr: &Expr, calls: &mut Vec<(AggregateKind, bool)>) -> bool {
    let mut count_star = false;
    expr.visit(&mut |expr| match expr {
        Expr::CountStar => count_star = true,
        Expr::Function { name, distinct, .. } => {
            if let Some(kind) = name.with_str(classify_aggregate) {
                calls.push((kind, *distinct));
            }
        }
        _ => {}
    });
    count_star
}

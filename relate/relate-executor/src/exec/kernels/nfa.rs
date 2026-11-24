//! The frontier-NFA compiler (ADR-0478 Decision 1): the plan-time
//! Thompson construction that lowers the one `Quantified` element of a
//! path pattern — the element every var-length spelling lowers to since
//! ADR-0471, so every spelling reaches this compiler — into a small NFA
//! over interned rel-type ids: the shape the frontier BFS runs on, the
//! production form of the research prototype's hand-built `Pattern`
//! (`graph/qpp.rs`, ADR-0476).
//!
//! Model (the prototype's, generalized):
//!
//! - states `0..states`, a `start` state, and rel-consuming transitions
//!   `(from --type-set--> to)` carrying the per-hop direction tag
//!   (`Direction::Outgoing/Incoming/Both`, honored per transition, never
//!   globally — the prototype is undirected-only);
//! - `accepting[s]`: the walk can complete at `s` (the final state, or a
//!   state that epsilon-reaches it); each transition's `accepts` flag is
//!   that table hoisted onto the transition — the prototype's hot-loop
//!   shape, derived from the one table, never a second fact;
//! - epsilon edges — rel-free skips — exist only where bounded
//!   quantifiers unroll with optional copies (or a zero-copy
//!   pass-through); the prototype's two families compile with none;
//! - hop bounds `{min, max}` over the whole run, `max = None` running to
//!   pair-saturation fixpoint.
//!
//! Compile families:
//!
//! - **flat** (`-[r:T*m..n]-`, the `(A|B)+` union): one self-loop state
//!   over the interned type union — the prototype's `Pattern::hops`;
//! - **group** (`((a)-[:A]-(b)-[:B]-(c)){m,n}`): the chained cycle, state
//!   *i* --step *i*--> *i+1* with the last step closing back to the entry
//!   state, hop bounds `rounds × steps.len()` — the prototype's
//!   `Pattern::rounds`;
//! - **concatenation context** (plain sibling rels around the quantified
//!   element, the scan+segment classes): one more transition in the same
//!   chain, the bounds offset by the fixed context hops;
//! - **nested quantifiers** (a var-length rel inside a group — the front
//!   end quantifies top level only, `quantify_var_length`, so a nested
//!   group spelling keeps its `Relationship` element): bounded ones
//!   unroll structurally, exact copies with the optional tail skipped by
//!   epsilon — never cycles.
//!
//! The soundness rule every form shares: the global hop interval may pin
//! the count of at most ONE cycle, and only when every traversal of that
//! cycle is the same fixed length (n rounds are then exactly n·L hops,
//! so the interval recovers the round count). A nested unbounded loop,
//! or a variable-length round under an unbounded outer quantifier,
//! would leave some instance's count unpinned — one instance skimping
//! compensated by another's excess — so both decline and the per-row DFS
//! keeps the pattern, answers identical.
//!
//! Every decline is structural, never an assert: state count above the
//! 64-state bitmask ceiling (the prototype's `assert!` is a decline
//! here), selectors, subpath variables, predicates on inner rels,
//! non-static type lists, a type name the store never interned, and
//! anything that breaks the node/hop alternation. An *empty* type list is
//! not a decline: it is the untyped hop, and it compiles as the any-type
//! set (see `NfaTransition::types`). The two admission
//! declines are switch-gated (ADR-0519): under `auto` an interior or
//! inner node carrying admission facts declines exactly as before (the
//! v1 NFA has no node-admission surface; the boundary nodes' admission
//! is the caller's), and so does a quantified group's own `WHERE`; under
//! the forced `nfa` rung both gap classes compile instead —
//!
//! - **class K, per-state guards**: an interior node's labels, label
//!   expression, and property map become the guard of the state its
//!   position occupies — the node pattern itself, checked at every
//!   arrival by the walker's own `node_ok` (exact pair-level semantics:
//!   a node either satisfies the position's pattern or not). A group's
//!   boundary positions alias — the body's first and last nodes share
//!   their state with the enclosing walk's nodes — so the guarded group
//!   compiles to a **product form**: the boundary is split into an
//!   `open` state (every round's start, the body's first pattern) and a
//!   `close` state (every round's end, the body's last pattern),
//!   stitched into the cycle by epsilon edges — the Thompson
//!   construction's own glue, consuming neither hop nor rel — and a
//!   guarded post-group node takes its own `exit` state, entered by
//!   epsilon from the close once the group is finished; so every state
//!   carries one pattern and the per-state check stays exact.
//! - **class F, transition guards**: a quantified group whose `WHERE`
//!   references its own pattern variables (the fraud f03 shape)
//!   compiles with the predicate riding the body's round-closing
//!   transition, read at each arrival against the referenced variables'
//!   **last bindings** — the arrival node, the departure node, or the
//!   first-arrival chain's most recent cell at the variable's state.
//!   The compile admits only plain one-hop-rel bodies here (the unroll
//!   path's copy stitching has no guard positions), and the serving
//!   gates keep every predicate-guarded clause on the DFS
//!   (`frontier_match`): the pair-dedup answers these shapes
//!   approximately, so no consumer proof covers serving them.

use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;
use relate_ir::{
    Direction, NodePattern, PathPattern, PatternElement, QuantifiedPathPattern, RelPattern,
};

mod builder;
mod guards;

use builder::Builder;
use guards::{interior_guard, node_element, InteriorGuard, TransitionPredicate};

/// The state ceiling: the frontier's dedup set is one `u64` bitmask per
/// node slot (the prototype's `seen: Vec<u64>`), so an NFA that would
/// need more than 64 states declines — ADR-0478 Decision 1 turns the
/// prototype's `assert!(states <= 64)` into a structural decline.
pub(crate) const STATE_CEILING: u32 = 64;

/// The pool a frontier run answers in — the family's shared vocabulary,
/// set once by the matcher on the contract
/// (`FrontierContract::pool`, the typed field) and read by every driver:
/// `Walk` for the bounded WALK spelling (the frontier's native semantics
/// for in-bounds arrivals, the re-arrival rule's walk-length check for
/// its below-min candidates, ADR-0478 §3), `Default` for everything else
/// (the default pool's trail exactness is the rule's jurisdiction,
/// ADR-0478 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pool {
    /// Bounded WALK mode: rel reuse is legal, every in-bounds accepting
    /// arrival is an endpoint — the frontier's native semantics — and a
    /// below-min candidate is an endpoint iff the walk-length check
    /// finds an in-bounds walk.
    Walk,
    /// The default pool (DIFFERENT RELATIONSHIPS ⇒ TRAIL): accepting
    /// arrivals are admitted only through the rule's proofs.
    Default,
}

/// One rel-consuming transition: the prototype's shape (from, to, the
/// interned type union, the hoisted accepts flag) plus the per-hop
/// direction tag the production transitions carry.
pub(crate) struct NfaTransition {
    pub(crate) from: u32,
    pub(crate) to: u32,
    /// Interned rel-type ids of the hop's static disjunction — the
    /// `(A|B)` union rides one transition, never an alternation arm.
    ///
    /// **Empty means every type matches** — the untyped hop (`-[*1..]-`,
    /// `-[]-`), not "no type". This is the one place the encoding is
    /// declared: `arms` resolves the empty list to `RelTypes::Any`
    /// through `RelTypes::from_interned` → `RelTypes::only`, whose
    /// empty-list arm is `Any` (LABS-1434). A reader that treats an
    /// empty vec as "matches nothing" would silently drop the hop.
    pub(crate) types: Vec<u32>,
    /// This hop's traversal direction, as written in the pattern.
    pub(crate) direction: Direction,
    /// Whether arrival via this transition can accept: `accepting[to]`,
    /// hoisted out of the table for the BFS hot loop.
    pub(crate) accepts: bool,
}

/// A quantified path pattern as a small NFA plus hop bounds — the
/// production form of the prototype's `Pattern` (`graph/qpp.rs`).
pub(crate) struct FrontierNfa {
    pub(crate) transitions: Vec<NfaTransition>,
    /// Rel-free `(from, to)` skips: the optional-copy shortcuts and
    /// zero-copy pass-throughs of unrolled bounded quantifiers, plus the
    /// guarded group's boundary stitching (the product form's `open` and
    /// `close` states). Empty for the prototype's two families; the
    /// runner expands them without consuming a hop.
    pub(crate) epsilons: Vec<(u32, u32)>,
    /// [`Self::epsilons`] indexed by from state: the runner's one view of
    /// the skip list, resolved once with the automaton (the clause), not
    /// per run.
    pub(crate) eps_from: Vec<Vec<u32>>,
    /// Per-state epsilon closure (`{s}` plus epsilon-reachable states),
    /// closed once with the automaton over [`Self::eps_from`]: the
    /// re-arrival search's zero-cost positions, the runner's
    /// `close_epsilons` in to-indexed form.
    pub(crate) eps_closure: Vec<Vec<u32>>,
    /// `accepting[s]`: the walk can complete at `s` — `s` is the final
    /// state or epsilon-reaches it. The canonical acceptance fact; the
    /// transitions' `accepts` flags are derived from it. `accepting
    /// [start]` with `min_hops == 0` is the zero-length-path endpoint.
    pub(crate) accepting: Vec<bool>,
    /// Per-state arrival guards (the forced rung's class K): the interior
    /// node patterns whose positions occupy each state, checked at every
    /// arrival and epsilon closure by the walker's own `node_ok`. Empty
    /// per state under the auto rung — the plain-node decline keeps those
    /// clauses on the DFS.
    pub(crate) state_guards: Vec<Vec<NodePattern>>,
    /// Per-transition group predicates (the forced rung's class F): the
    /// quantified group's `WHERE`, riding the body's round-closing
    /// transition, each referenced variable resolved to the state its
    /// body position occupies. `None` per transition under the auto rung.
    pub(crate) predicates: Vec<Option<TransitionPredicate>>,
    pub(crate) start: u32,
    pub(crate) states: u32,
    /// Hop bounds over the whole run; `max = None` runs to fixpoint.
    pub(crate) min_hops: u32,
    pub(crate) max_hops: Option<u32>,
}

/// The one-cycle shape fact the compile requires and the ladder rung's
/// cheap pre-gate (`frontier_run::try_frontier_rows`) shares: exactly one
/// top-level `Quantified` element — a juxtaposed second group (the parser
/// allows `(x)((a)-->(b))*((c)-->(d))*`) would be a second cycle the hop
/// interval cannot pin. One pass over the elements, never the graph.
pub(crate) fn exactly_one_quantified(elements: &[PatternElement]) -> bool {
    elements
        .iter()
        .filter(|element| matches!(element, PatternElement::Quantified(_)))
        .count()
        == 1
}

/// The alternating-chain shape fact both NFA walks require: a strictly
/// alternating element chain that starts and ends with a node — nodes at
/// the even positions, hops (plain rels and quantified groups) at the odd
/// ones — and therefore at least one hop (a chain shorter than three
/// elements has no interior to alternate over). `compile` and
/// `Builder::chain` decline on the same fact through this one predicate.
pub(crate) fn alternating_chain(elements: &[PatternElement]) -> bool {
    if elements.len() < 3 || elements.len().is_multiple_of(2) {
        return false;
    }
    elements
        .iter()
        .enumerate()
        .all(|(i, element)| match element {
            PatternElement::Node(_) => i.is_multiple_of(2),
            PatternElement::Relationship(_) | PatternElement::Quantified(_) => !i.is_multiple_of(2),
        })
}

/// Compile `pattern` into the frontier NFA, or `None` on any shape out
/// of the vocabulary above — the decline the caller turns into "keep the
/// per-row DFS". `engine` is the rung the caller resolved
/// ([`PatternEngine::Nfa`] compiles the gap classes; any other value the
/// v1 vocabulary) — the compiler never reads the switch itself.
pub(crate) fn compile(
    graph: &Graph,
    pattern: &PathPattern,
    engine: PatternEngine,
) -> Option<FrontierNfa> {
    // Pattern-level GPM facts the NFA cannot represent: a selector is the
    // shortest machinery's contract, a pattern WHERE is per-path
    // semantics, the shortest-path function spelling is its own route.
    // The path mode (`WALK`/`TRAIL`/…), the path variable, and a
    // pattern-level named subpath (`p = (q = …)`) are the caller's and
    // the witness reconstruction's business — they ride; the emission
    // binds the path value under every name (pattern_walk's
    // bind_path_names).
    if pattern.selector.is_some()
        || pattern.where_clause.is_some()
        || pattern.is_shortest_path_function
    {
        return None;
    }
    // A strictly alternating element chain that starts and ends with a
    // node, and exactly one `Quantified` element at a hop position — the
    // one cycle the hop interval may pin. A juxtaposed second group (the
    // parser allows `(x)((a)-->(b))*((c)-->(d))*`) would be a second
    // cycle: decline.
    let elements = &pattern.elements;
    if !alternating_chain(elements) {
        return None;
    }
    // The Quantified group (exactly one, by the gate below) sits at an
    // odd position — `alternating_chain` already establishes that, so it
    // needs no separate spine parity check.
    if !exactly_one_quantified(elements) {
        return None;
    }

    let mut builder = Builder::new(graph, engine == PatternEngine::Nfa);
    // State 0 is the pattern's first node — the walk's seed side.
    let mut current = builder.alloc()?;
    // The plain context hops either side of the spine: only their count
    // widens the hop window.
    let mut context = 0u32;
    let mut spine_len = None;
    for (i, element) in elements.iter().enumerate() {
        match element {
            PatternElement::Node(node) if i.is_multiple_of(2) => {
                // The first and last nodes are the pattern's boundary —
                // their admission (labels, properties, predicates) is the
                // caller's, through the kernel layer's `NodeTarget`
                // machinery. An interior node is a context node the auto
                // rung cannot check (its facts decline); the forced rung
                // compiles them as the per-state guard of the state the
                // position occupies — for a guarded node after the
                // quantified group that state is the spine's own exit
                // state past the round boundary (`split_exit`), never the
                // close every round ends at.
                if i != 0 && i + 1 != elements.len() {
                    builder.admit_interior(node, current)?;
                }
            }
            PatternElement::Relationship(rel) if !i.is_multiple_of(2) => {
                // A plain one-hop context rel (a top-level var-length rel
                // is out of vocabulary: the front end quantifies every
                // top-level var-length spelling, so one here is not
                // lowered IR — decline defensively).
                if !plain_rel(rel) {
                    return None;
                }
                let to = builder.alloc()?;
                let types = builder.interned(rel)?;
                builder.push_transition(current, to, types, rel.direction);
                context += 1;
                current = to;
            }
            PatternElement::Quantified(quantified) if !i.is_multiple_of(2) => {
                // The node after the group carries the caller's guard on
                // the spine's exit state (the loop attaches it there) —
                // the spine only needs the presence fact, to allocate
                // that state; the guard itself compiles once, in
                // `admit_interior` on the next iteration.
                let exit_guarded = if builder.forced && i + 2 < elements.len() {
                    matches!(
                        interior_guard(node_element(elements, i + 1)?)?,
                        InteriorGuard::Guard(_)
                    )
                } else {
                    false
                };
                let (close, len) = builder.spine(quantified, current, exit_guarded)?;
                current = close;
                spine_len = Some(len);
            }
            // A node at a hop position, a rel or group at a node
            // position: the alternation is broken.
            _ => return None,
        }
    }
    let spine_len = spine_len
        .expect("the quantified element at the odd index found above reaches the spine arm");
    // An overflowing bound declines (the `?`), never silently turns into
    // the unbounded `None` — that would relax the pattern's semantics.
    let min_hops = spine_len.min.checked_add(context)?;
    let max_hops = match spine_len.max {
        None => None,
        Some(max) => Some(max.checked_add(context)?),
    };
    Some(builder.finish(current, min_hops, max_hops))
}

/// The epsilon edges indexed by from state.
fn epsilon_index(states: u32, epsilons: &[(u32, u32)]) -> Vec<Vec<u32>> {
    let mut eps_from = vec![Vec::new(); states as usize];
    for &(from, to) in epsilons {
        eps_from[from as usize].push(to);
    }
    eps_from
}

/// Each state's epsilon closure over the from-indexed skip list: the
/// state itself plus everything reachable through skips.
fn epsilon_closure(eps_from: &[Vec<u32>]) -> Vec<Vec<u32>> {
    (0..eps_from.len() as u32)
        .map(|s| {
            let mut reach = vec![s];
            let mut pending = vec![s];
            while let Some(t) = pending.pop() {
                for &to in &eps_from[t as usize] {
                    if !reach.contains(&to) {
                        reach.push(to);
                        pending.push(to);
                    }
                }
            }
            reach
        })
        .collect()
}

/// The plain one-hop rel of the vocabulary: a static type disjunction
/// (the `(A|B)` union is one type list, not alternation IR), no
/// predicates, exactly one hop. Direction and the variable name ride the
/// transition and the row binding — neither is constrained here.
///
/// An empty type list is the untyped hop (`-[*1..]-`), and it compiles:
/// the transition carries no interned id, and `arms` resolves that empty
/// list to `RelTypes::Any` — the store's own encoding for "every type
/// matches", which is what an untyped hop means. Nothing has to
/// enumerate the interned vocabulary, so the decline this used to carry
/// (LABS-1434) is gone.
fn plain_rel(rel: &RelPattern) -> bool {
    !(rel.var_length
        || rel.min_hops != 1
        || rel.max_hops != Some(1)
        || rel.properties.is_some()
        || rel.where_clause.is_some()
        // The synthesized `type_expr` of every typed rel: static
        // disjunctions are exactly what the `types` list already carries
        // (single label or `Or`-chain of labels); anything dynamic,
        // negated, or conjunctive keeps the walker's type evaluation.
        || !rel.types_are_complete_filter())
}

/// The quantifier admissibility every quantifier consults — the spine's
/// own gate and both nested spellings' (the chain's nested group arm and
/// the nested var-length rel's normalized form), one spelling: no
/// subpath variable, no per-iteration WHERE (the forced rung admits the
/// spine's own — it compiles to a transition guard; every nested one
/// keeps the decline), and `max >= min` when bounded. Boundedness itself
/// is the caller's fact (the spine may run unbounded; a nested quantifier
/// may not).
fn quantified_admissible(quantified: &QuantifiedPathPattern, allow_where: bool) -> bool {
    quantified.subpath_variable.is_none()
        && (allow_where || quantified.where_clause.is_none())
        && quantified.max.is_none_or(|max| max >= quantified.min)
}

/// The quantified form of a nested var-length rel — the same
/// normalization the front end applies to top-level var-length rels
/// (`quantify_var_length`, ADR-0471): one clamped plain rel between two
/// implicit anonymous boundary nodes, the `*m..n` bounds moved onto the
/// quantifier. The front end quantifies top level only (a var-length
/// rel nested inside a group keeps its `Relationship` element), so the
/// compiler applies the normalization here — one quantifier code path
/// for both spellings, and [`quantified_admissible`] reads it like any
/// other quantifier.
fn var_length_quantified(rel: &RelPattern) -> QuantifiedPathPattern {
    QuantifiedPathPattern {
        subpath_variable: None,
        elements: vec![
            PatternElement::Node(NodePattern::anonymous()),
            PatternElement::Relationship(RelPattern {
                variable: rel.variable.clone(),
                types: rel.types.clone(),
                type_expr: rel.type_expr.clone(),
                direction: rel.direction,
                // The clamped rel: the bounds live on the quantifier.
                properties: None,
                prop_keys: Vec::new(),
                var_length: false,
                min_hops: 1,
                max_hops: Some(1),
                where_clause: None,
                quantified: rel.quantified,
            }),
            PatternElement::Node(NodePattern::anonymous()),
        ],
        where_clause: None,
        min: rel.min_hops,
        max: rel.max_hops,
    }
}

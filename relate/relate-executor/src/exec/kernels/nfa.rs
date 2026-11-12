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

use relate_ir::{
    Direction, NodePattern, PathPattern, PatternElement, QuantifiedPathPattern, RelPattern,
};
use relate_names::SmallName;

use crate::graph::Graph;
use crate::pattern_engine::PatternEngine;

mod guards;

use guards::{
    body_unrolls, compile_transition_predicate, interior_guard, node_element, ChainEdges,
    InteriorGuard, TransitionPredicate,
};

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

/// The hop-length range of a compiled segment: fixed iff
/// `max == Some(min)`, unbounded when `max` is `None`.
struct Len {
    min: u32,
    max: Option<u32>,
}

impl Len {
    /// The range of `self` followed by `other` (checked — a bound that
    /// overflows `u32` declines rather than wraps).
    fn plus(self, other: Len) -> Option<Len> {
        Some(Len {
            min: self.min.checked_add(other.min)?,
            max: match (self.max, other.max) {
                (Some(a), Some(b)) => Some(a.checked_add(b)?),
                _ => None,
            },
        })
    }
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
    if elements.len() < 3 || elements.len() % 2 == 0 {
        return None;
    }
    let spine = elements
        .iter()
        .position(|element| matches!(element, PatternElement::Quantified(_)))?;
    if spine % 2 == 0 {
        return None;
    }
    if !exactly_one_quantified(elements) {
        return None;
    }

    let mut builder = Builder::new(graph, engine == PatternEngine::Nfa);
    // State 0 is the pattern's first node — the walk's seed side.
    let mut current = builder.alloc()?;
    let mut leading = 0u32;
    let mut trailing = 0u32;
    let mut spine_len = None;
    for (i, element) in elements.iter().enumerate() {
        match element {
            PatternElement::Node(node) if i % 2 == 0 => {
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
            PatternElement::Relationship(rel) if i % 2 == 1 => {
                // A plain one-hop context rel (a top-level var-length rel
                // is out of vocabulary: the front end quantifies every
                // top-level var-length spelling, so one here is not
                // lowered IR — decline defensively).
                plain_rel(rel)?;
                let to = builder.alloc()?;
                let types = builder.interned(rel)?;
                builder.push_transition(current, to, types, rel.direction);
                if i < spine {
                    leading += 1;
                } else {
                    trailing += 1;
                }
                current = to;
            }
            PatternElement::Quantified(quantified) if i % 2 == 1 => {
                // The node after the group carries the caller's guard on
                // the spine's exit state (the loop attaches it there) —
                // the spine allocates that state, so it needs the fact
                // now.
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
    let spine_len = spine_len?;
    let accepting_state = current;
    let built = builder.finish();
    let (mut transitions, epsilons, states) = (built.transitions, built.epsilons, built.states);

    // Acceptance: the final state, plus every state that epsilon-reaches
    // it (an optional-copy skip or a zero-copy pass-through completes the
    // walk). Reverse reachability from the final state over the epsilon
    // edges; tiny graphs (≤ 64 states), so the edge scan per state is
    // the whole cost.
    let mut accepting = vec![false; states as usize];
    accepting[accepting_state as usize] = true;
    let mut work = vec![accepting_state];
    while let Some(state) = work.pop() {
        for &(from, to) in &epsilons {
            if to == state && !accepting[from as usize] {
                accepting[from as usize] = true;
                work.push(from);
            }
        }
    }
    for transition in &mut transitions {
        transition.accepts = accepting[transition.to as usize];
    }

    let context = leading.checked_add(trailing)?;
    // An overflowing bound declines (the `?`), never silently turns into
    // the unbounded `None` — that would relax the pattern's semantics.
    let max_hops = match spine_len.max {
        None => None,
        Some(max) => Some(max.checked_add(context)?),
    };
    Some(FrontierNfa {
        transitions,
        epsilons,
        accepting,
        state_guards: built.state_guards,
        predicates: built.predicates,
        start: 0,
        states,
        min_hops: spine_len.min.checked_add(context)?,
        max_hops,
    })
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
fn plain_rel(rel: &RelPattern) -> Option<()> {
    if rel.var_length
        || rel.min_hops != 1
        || rel.max_hops != Some(1)
        || rel.properties.is_some()
        || rel.where_clause.is_some()
        // The synthesized `type_expr` of every typed rel: static
        // disjunctions are exactly what the `types` list already carries
        // (single label or `Or`-chain of labels); anything dynamic,
        // negated, or conjunctive keeps the walker's type evaluation.
        || !rel
            .type_expr
            .as_ref()
            .is_none_or(|_| rel.type_expr_is_static_types())
    {
        return None;
    }
    Some(())
}

/// A node the auto rung cannot check must carry no admission facts: a
/// variable name binds per-iteration group lists (the witness
/// reconstruction's business, not the NFA's), but labels, label
/// expressions, property maps, and element predicates would be silently
/// unchecked — the auto NFA has no node-admission surface, so they
/// decline. The forced rung's arrival-time node checks (ADR-0478 §2's
/// extension, the gap class K) are [`guards::interior_guard`]'s
/// vocabulary instead, checked per (node, state) arrival; the one
/// admission policy over both is `Builder::admit_interior`.
fn plain_node(node: &NodePattern) -> Option<()> {
    if !node.labels.is_empty()
        || node.label_expr.is_some()
        || node.properties.is_some()
        || node.where_clause.is_some()
    {
        return None;
    }
    Some(())
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

/// The compilation builder: allocates states under the ceiling and
/// collects transitions, epsilon edges, guards, and predicates. The
/// spine emits a body speculatively in the cycle form and rolls the
/// emission back when the body's length turns out variable — one
/// vocabulary path, no parallel dry analysis walk to keep in sync.
struct Builder<'a> {
    graph: &'a Graph,
    /// The forced rung (ADR-0519): interior admission compiles to guards
    /// and the group WHERE to a transition predicate. Under `auto` the
    /// builder emits exactly the v1 vocabulary — every guard decline
    /// keeps the DFS.
    forced: bool,
    transitions: Vec<NfaTransition>,
    epsilons: Vec<(u32, u32)>,
    /// Guards as `(state, pattern)` pairs, scattered per state at
    /// `finish` — the builder allocates states incrementally, so a dense
    /// table cannot be indexed before it exists.
    guards: Vec<(u32, NodePattern)>,
    /// Predicates as `(transition, predicate)` pairs, scattered per
    /// transition the same way.
    predicates: Vec<(usize, TransitionPredicate)>,
    next_state: u32,
}

/// A builder snapshot: every append-only vector's length plus the state
/// counter, so `rollback` restores the whole emission state, not just
/// the transition tables.
struct Mark {
    transitions: usize,
    epsilons: usize,
    guards: usize,
    predicates: usize,
    next_state: u32,
}

/// One builder emission's collected form — the fields of the
/// [`FrontierNfa`] the compile assembles, guards and predicates already
/// scattered to their dense per-state and per-transition tables.
struct Built {
    transitions: Vec<NfaTransition>,
    epsilons: Vec<(u32, u32)>,
    states: u32,
    state_guards: Vec<Vec<NodePattern>>,
    predicates: Vec<Option<TransitionPredicate>>,
}

impl<'a> Builder<'a> {
    fn new(graph: &'a Graph, forced: bool) -> Self {
        Self {
            graph,
            forced,
            transitions: Vec::new(),
            epsilons: Vec::new(),
            guards: Vec::new(),
            predicates: Vec::new(),
            next_state: 0,
        }
    }

    /// One state under the ceiling; `None` = the ceiling decline.
    fn alloc(&mut self) -> Option<u32> {
        if self.next_state >= STATE_CEILING {
            return None;
        }
        let state = self.next_state;
        self.next_state += 1;
        Some(state)
    }

    fn push_transition(&mut self, from: u32, to: u32, types: Vec<u32>, direction: Direction) {
        self.transitions.push(NfaTransition {
            from,
            to,
            types,
            direction,
            accepts: false,
        });
    }

    /// Interned ids of `rel`'s type list — `None` when a name was never
    /// interned (the store carries no such type; the caller's decline
    /// keeps the DFS, whose answer for the type is the same none).
    fn interned(&self, rel: &RelPattern) -> Option<Vec<u32>> {
        rel.types
            .iter()
            .map(|(name, token)| {
                token.id_or_else(|| name.with_str(|text| self.graph.rel_type_id(text)))
            })
            .collect()
    }

    /// The builder snapshot a probe rolls back to — every append-only
    /// vector plus the state counter, so no probe fact can survive the
    /// rollback (a stale guard would land on a state a later emission
    /// reuses, or index past the kept `next_state` at `finish`).
    fn mark(&self) -> Mark {
        Mark {
            transitions: self.transitions.len(),
            epsilons: self.epsilons.len(),
            guards: self.guards.len(),
            predicates: self.predicates.len(),
            next_state: self.next_state,
        }
    }

    fn rollback(&mut self, mark: Mark) {
        self.transitions.truncate(mark.transitions);
        self.epsilons.truncate(mark.epsilons);
        self.guards.truncate(mark.guards);
        self.predicates.truncate(mark.predicates);
        self.next_state = mark.next_state;
    }

    fn finish(self) -> Built {
        let mut state_guards = vec![Vec::new(); self.next_state as usize];
        for (state, guard) in self.guards {
            state_guards[state as usize].push(guard);
        }
        let mut predicates = vec![None; self.transitions.len()];
        for (transition, predicate) in self.predicates {
            predicates[transition] = Some(predicate);
        }
        Built {
            transitions: self.transitions,
            epsilons: self.epsilons,
            states: self.next_state,
            state_guards,
            predicates,
        }
    }

    /// The pattern's one top-level quantified element, entered at
    /// `entry` (the state of the node before it). Two sound forms:
    ///
    /// - **fixed round length** — the cycle form, the prototype's
    ///   `hops`/`rounds`: the body chain closes back to its entry state
    ///   and the global interval pins the round count exactly (n rounds
    ///   are n·L hops);
    /// - **variable round length** — a bounded outer quantifier unrolls
    ///   its copies structurally; an unbounded one declines (the
    ///   interval would pin only the total, and one short round can be
    ///   compensated by a long one).
    fn spine(
        &mut self,
        quantified: &QuantifiedPathPattern,
        entry: u32,
        exit_guarded: bool,
    ) -> Option<(u32, Len)> {
        if !quantified_admissible(quantified, self.forced) {
            return None;
        }
        // The forced rung's boundary facts (the gap classes): the body's
        // own first and last node guards, computed here — the one
        // spelling the chain's split decision and the zero-round decline
        // below read. A zero-round group with a guarded round end or
        // post-group node declines: the zero-round pass-through lands the
        // pre-group node at the post-group position, and the round-end
        // pattern would apply to a node no round visited.
        let edges = if self.forced {
            let body = &quantified.elements;
            let last_index = body.len().checked_sub(1)?;
            let first = interior_guard(node_element(body, 0)?)?.into_pattern();
            let last = interior_guard(node_element(body, last_index)?)?.into_pattern();
            if quantified.min == 0 && (last.is_some() || exit_guarded) {
                return None;
            }
            ChainEdges {
                first,
                last,
                entry_guarded: self.state_guarded(entry),
                predicate: quantified.where_clause.as_ref(),
                zero_rounds: quantified.min == 0,
            }
        } else {
            ChainEdges::plain()
        };
        // Emit the body closing back to its entry — the cycle form every
        // fixed-length round takes (the prototype's `hops`/`rounds`: a
        // plain last rel targets the entry directly, a nested last
        // quantifier lands there through its exit target). The emission
        // is also the probe: its hop-length range decides whether the
        // cycle stands — exactly n·L hops per n rounds, so the global
        // interval recovers the round count — or the body must be
        // unrolled, in which case the speculative emission is rolled
        // back. One vocabulary path, no parallel dry walk, and no probe
        // state ever over-allocates past what the kept form needs.
        let mark = self.mark();
        let (close, body) = self.chain(&quantified.elements, entry, Some(entry), edges)?;
        let (close, len) = if body.max == Some(body.min) {
            let rounds = body.min;
            // An overflowing bound declines, never silently unbounds.
            let max = match quantified.max {
                None => None,
                Some(bound) => Some(bound.checked_mul(rounds)?),
            };
            (
                close,
                Len {
                    min: quantified.min.checked_mul(rounds)?,
                    max,
                },
            )
        } else {
            // Variable round length: a bounded spine unrolls; an
            // unbounded one cannot pin each round's count — decline.
            self.rollback(mark);
            let max = quantified.max?;
            self.unroll(&quantified.elements, quantified.min, max, entry, None)?
        };
        // The post-group node's position, past the group boundary in
        // both forms.
        Some((self.split_exit(close, exit_guarded)?, len))
    }

    /// Unroll `min..=max` structural copies of `elements` from `entry`,
    /// the last state landing at `exit_target` when given: `min`
    /// mandatory copies, then `max - min` optional copies each skipped
    /// by an epsilon edge. Every bounded nested quantifier compiles this
    /// way, never to a cycle — a cycle inside a repeating region would
    /// need the global interval to pin each instance's count, and the
    /// interval pins only the total.
    fn unroll(
        &mut self,
        elements: &[PatternElement],
        min: u32,
        max: u32,
        entry: u32,
        exit_target: Option<u32>,
    ) -> Option<(u32, Len)> {
        if min == 0 && max == 0 {
            // Zero copies: the segment is a pass-through, an epsilon hop
            // to the caller's target when it moved.
            let close = exit_target.unwrap_or(entry);
            if close != entry {
                self.epsilons.push((entry, close));
            }
            return Some((
                close,
                Len {
                    min: 0,
                    max: Some(0),
                },
            ));
        }
        let mut current = entry;
        let mut copy_len = None;
        for i in 0..min {
            // The last mandatory copy exits at the caller's target only
            // when nothing optional follows.
            let exit = if i + 1 == min && max == min {
                exit_target
            } else {
                None
            };
            let (close, len) = self.chain(elements, current, exit, ChainEdges::plain())?;
            current = close;
            copy_len = Some(len);
        }
        for j in 0..(max - min) {
            // Optional copy: skip (epsilon) or run, both landing at the
            // next state — the caller's target for the last one.
            let next = if j + 1 == max - min {
                exit_target
            } else {
                None
            }
            .or_else(|| self.alloc())?;
            self.epsilons.push((current, next));
            let (close, len) = self.chain(elements, current, Some(next), ChainEdges::plain())?;
            current = close;
            copy_len = Some(len);
        }
        let copy_len = copy_len?;
        Some((
            current,
            Len {
                min: min.checked_mul(copy_len.min)?,
                max: Some(max.checked_mul(copy_len.max?)?),
            },
        ))
    }

    /// Compile one node/hop/node chain — a quantified group's inner
    /// sequence, or a synthesized var-length group — from `entry`, the
    /// state of the chain's first node. `close`, when given, is the
    /// state the chain's final node must sit at (the cycle form's
    /// return-to-entry, the unrolled copy's exit); the returned state is
    /// that final node's state, with the chain's hop-length range. Every
    /// node in a chain is an inner node — `plain_node`'s vocabulary
    /// under the auto rung, `interior_guard`'s under the forced one —
    /// and every hop a plain one-hop rel or a nested quantifier.
    ///
    /// Under the forced rung a boundary-carrying chain takes the
    /// **product form**: an `open` state (every round's start, the
    /// body's first pattern as its guard) entered by epsilon from
    /// `entry` and re-entered by epsilon from the `close` state, and the
    /// `close` state itself (every round's end, the body's last pattern)
    /// as the last hop's target — so every state carries one node
    /// pattern and the per-state guard stays exact, where the plain
    /// form's shared boundary state would alias the enclosing walk's
    /// nodes with the round's own.
    fn chain(
        &mut self,
        elements: &[PatternElement],
        entry: u32,
        close: Option<u32>,
        edges: ChainEdges<'_>,
    ) -> Option<(u32, Len)> {
        if elements.len() < 3 || elements.len() % 2 == 0 {
            return None;
        }
        // Guards and the group predicate ride only a plain one-hop-rel
        // body: a var-length or nested quantified element unrolls the
        // emission into copies whose stitching has no guard positions.
        if edges.carries_round_facts() && body_unrolls(elements) {
            return None;
        }
        // The round boundary: the split form's dedicated open and close
        // states, or `entry` and the caller's `close` as-is — the plain
        // form, which allocates the final node's state below when none
        // was given (the v1 vocabulary's layout).
        let (open, last_target) = self.split_boundary(entry, close, &edges)?;
        let mut current = open;
        let mut len = Len {
            min: 0,
            max: Some(0),
        };
        // The body's node positions as `(variable, state)` — the
        // predicate's variable-to-state map, collected on the same walk
        // the emission runs.
        let mut node_states: Vec<(Option<SmallName>, u32)> = Vec::new();
        let mut last_rel_transition = None;
        for (i, element) in elements.iter().enumerate() {
            // The final hop sits at the second-to-last position — the
            // chain always ends with a node. It is the hop the close
            // target serves.
            let last_hop = i + 2 == elements.len();
            match element {
                PatternElement::Node(node) if i % 2 == 0 => {
                    // Under the forced rung the boundary positions' guards
                    // were attached by the split above (or carry none);
                    // under the auto rung every chain node is an inner
                    // node of the v1 vocabulary. The interior ones attach
                    // here, to the state the preceding hop allocated for
                    // the position.
                    let interior = i != 0 && i + 1 != elements.len();
                    if interior || !self.forced {
                        self.admit_interior(node, current)?;
                    }
                    node_states.push((node.variable.clone(), current));
                }
                PatternElement::Relationship(rel) if i % 2 == 1 => {
                    let exit = if last_hop { last_target } else { None };
                    if rel.var_length {
                        // The nested var-length rel: predicates on the
                        // traversed edges decline before the
                        // normalization drops them.
                        if rel.properties.is_some() || rel.where_clause.is_some() {
                            return None;
                        }
                        let quantified = var_length_quantified(rel);
                        if !quantified_admissible(&quantified, false) {
                            return None;
                        }
                        // A nested quantifier is bounded only — an
                        // unbounded nested loop's count would be pinned
                        // by the global interval, which pins only totals.
                        let max = quantified.max?;
                        let (hop_exit, hop_len) =
                            self.unroll(&quantified.elements, quantified.min, max, current, exit)?;
                        current = hop_exit;
                        len = len.plus(hop_len)?;
                    } else {
                        plain_rel(rel)?;
                        let to = if last_hop {
                            last_target.or_else(|| self.alloc())
                        } else {
                            self.alloc()
                        }?;
                        let types = self.interned(rel)?;
                        self.push_transition(current, to, types, rel.direction);
                        last_rel_transition = Some(self.transitions.len() - 1);
                        current = to;
                        len = len.plus(Len {
                            min: 1,
                            max: Some(1),
                        })?;
                    }
                }
                PatternElement::Quantified(quantified) if i % 2 == 1 => {
                    // A nested quantified group — unconstructible by the
                    // parser (the `NestedQuantification` rule), but the IR
                    // shape exists; the same nested rules apply, its own
                    // WHERE included (only the spine's group predicate
                    // compiles).
                    if !quantified_admissible(quantified, false) {
                        return None;
                    }
                    // A nested quantifier is bounded only — an
                    // unbounded nested loop's count would be pinned by
                    // the global interval, which pins only totals.
                    let max = quantified.max?;
                    let exit = if last_hop { last_target } else { None };
                    let (hop_exit, hop_len) =
                        self.unroll(&quantified.elements, quantified.min, max, current, exit)?;
                    current = hop_exit;
                    len = len.plus(hop_len)?;
                }
                // A node at a hop position, a rel or group at a node
                // position: the alternation is broken.
                _ => return None,
            }
        }
        // The group WHERE (class F), compiled against the positions the
        // walk above collected, riding the round-closing hop.
        if let Some(predicate) = edges.predicate {
            let compiled =
                compile_transition_predicate(predicate, node_states, last_rel_transition)?;
            self.predicates.push(compiled);
        }
        Some((current, len))
    }
}

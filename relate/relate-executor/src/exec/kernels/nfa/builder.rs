//! The frontier-NFA compilation builder: state allocation under the
//! ceiling, and the transition/epsilon/guard/predicate emission the
//! compile ([`super::compile`]) and the guarded forms ([`super::guards`])
//! drive. The spine emits its body speculatively in the cycle form and
//! rolls the emission back when the body's length turns out variable —
//! one vocabulary path, no parallel dry analysis walk to keep in sync.

use relate_ir::{Direction, NodePattern, PatternElement, QuantifiedPathPattern, RelPattern};
use relate_names::SmallName;

use crate::graph::Graph;

use super::guards::{
    body_unrolls, compile_transition_predicate, interior_guard, node_element, plain_node,
    ChainEdges, InteriorGuard, TransitionPredicate,
};
use super::{
    alternating_chain, epsilon_closure, epsilon_index, plain_rel, quantified_admissible,
    var_length_quantified, FrontierNfa, NfaTransition, STATE_CEILING,
};

/// The hop-length range of a compiled segment: fixed iff
/// `max == Some(min)`, unbounded when `max` is `None`.
pub(super) struct Len {
    pub(super) min: u32,
    pub(super) max: Option<u32>,
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

/// The compilation builder: allocates states under the ceiling and
/// collects transitions, epsilon edges, guards, and predicates. The
/// spine emits a body speculatively in the cycle form and rolls the
/// emission back when the body's length turns out variable — one
/// vocabulary path, no parallel dry analysis walk to keep in sync.
pub(super) struct Builder<'a> {
    graph: &'a Graph,
    /// The forced rung (ADR-0519): interior admission compiles to guards
    /// and the group WHERE to a transition predicate. Under `auto` the
    /// builder emits exactly the v1 vocabulary — every guard decline
    /// keeps the DFS.
    pub(super) forced: bool,
    tables: Tables,
    next_state: u32,
}

/// The emission's append-only tables, with their probe snapshot as a
/// method of the same struct: a vector added here sits one line from the
/// snapshot that must restore it, so `mark`/`rollback` cannot drift from
/// the state they carry.
#[derive(Default)]
struct Tables {
    transitions: Vec<NfaTransition>,
    epsilons: Vec<(u32, u32)>,
    /// Guards as `(state, pattern)` pairs, scattered per state at
    /// `finish` — the builder allocates states incrementally, so a dense
    /// table cannot be indexed before it exists.
    guards: Vec<(u32, NodePattern)>,
    /// Predicates as `(transition, predicate)` pairs, scattered per
    /// transition the same way.
    predicates: Vec<(usize, TransitionPredicate)>,
}

/// A tables snapshot: every append-only vector's length, so `rollback`
/// restores the whole emission state, not just the transition tables.
struct TablesMark {
    transitions: usize,
    epsilons: usize,
    guards: usize,
    predicates: usize,
}

/// A builder snapshot: the tables' snapshot plus the state counter.
struct Mark {
    tables: TablesMark,
    next_state: u32,
}

impl Tables {
    fn mark(&self) -> TablesMark {
        TablesMark {
            transitions: self.transitions.len(),
            epsilons: self.epsilons.len(),
            guards: self.guards.len(),
            predicates: self.predicates.len(),
        }
    }

    fn rollback(&mut self, mark: TablesMark) {
        self.transitions.truncate(mark.transitions);
        self.epsilons.truncate(mark.epsilons);
        self.guards.truncate(mark.guards);
        self.predicates.truncate(mark.predicates);
    }
}

impl<'a> Builder<'a> {
    pub(super) fn new(graph: &'a Graph, forced: bool) -> Self {
        Self {
            graph,
            forced,
            tables: Tables::default(),
            next_state: 0,
        }
    }

    /// One state under the ceiling; `None` = the ceiling decline.
    pub(super) fn alloc(&mut self) -> Option<u32> {
        if self.next_state >= STATE_CEILING {
            return None;
        }
        let state = self.next_state;
        self.next_state += 1;
        Some(state)
    }

    pub(super) fn push_transition(
        &mut self,
        from: u32,
        to: u32,
        types: Vec<u32>,
        direction: Direction,
    ) {
        self.tables.transitions.push(NfaTransition {
            from,
            to,
            types,
            direction,
            accepts: false,
        });
    }

    /// One epsilon (rel-free skip) edge.
    pub(super) fn push_epsilon(&mut self, from: u32, to: u32) {
        self.tables.epsilons.push((from, to));
    }

    /// Attach `guard` to `state` — a second guard on one state is the
    /// conjunction of every pattern whose position occupies the state.
    pub(super) fn guard(&mut self, state: u32, guard: NodePattern) {
        self.tables.guards.push((state, guard));
    }

    /// Whether `state` carries a guard yet — the split decision's entry
    /// fact.
    pub(super) fn state_guarded(&self, state: u32) -> bool {
        self.tables.guards.iter().any(|(at, _)| *at == state)
    }

    /// Interned ids of `rel`'s type list — `None` when a name was never
    /// interned (the store carries no such type; the caller's decline
    /// keeps the DFS, whose answer for the type is the same none).
    pub(super) fn interned(&self, rel: &RelPattern) -> Option<Vec<u32>> {
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
            tables: self.tables.mark(),
            next_state: self.next_state,
        }
    }

    fn rollback(&mut self, mark: Mark) {
        self.tables.rollback(mark.tables);
        self.next_state = mark.next_state;
    }

    /// The collected automaton: the tables scattered to their dense
    /// per-state and per-transition forms, the epsilon facts derived
    /// once, and acceptance — a state accepts iff its closure reaches
    /// `accepting_state` — stamped onto the transitions. The hop window
    /// is complete at construction, so the returned automaton never
    /// claims a bound the pattern does not.
    pub(super) fn finish(
        self,
        accepting_state: u32,
        min_hops: u32,
        max_hops: Option<u32>,
    ) -> FrontierNfa {
        let mut state_guards = vec![Vec::new(); self.next_state as usize];
        for (state, guard) in self.tables.guards {
            state_guards[state as usize].push(guard);
        }
        let mut predicates = vec![None; self.tables.transitions.len()];
        for (transition, predicate) in self.tables.predicates {
            predicates[transition] = Some(predicate);
        }
        let eps_from = epsilon_index(self.next_state, &self.tables.epsilons);
        let eps_closure = epsilon_closure(&eps_from);
        let accepting: Vec<bool> = (0..self.next_state as usize)
            .map(|s| eps_closure[s].contains(&accepting_state))
            .collect();
        let mut transitions = self.tables.transitions;
        for transition in &mut transitions {
            transition.accepts = accepting[transition.to as usize];
        }
        FrontierNfa {
            transitions,
            epsilons: self.tables.epsilons,
            eps_from,
            eps_closure,
            accepting,
            state_guards,
            predicates,
            start: 0,
            states: self.next_state,
            min_hops,
            max_hops,
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
    pub(super) fn spine(
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
                self.push_epsilon(entry, close);
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
            self.push_epsilon(current, next);
            let (close, len) = self.chain(elements, current, Some(next), ChainEdges::plain())?;
            current = close;
            copy_len = Some(len);
        }
        // The zero-copy case returned above, so at least one copy ran and
        // this is an invariant, never a decline.
        let copy_len = copy_len.expect("a non-zero copy count emits at least one copy");
        Some((
            current,
            Len {
                min: min.checked_mul(copy_len.min)?,
                max: Some(max.checked_mul(copy_len.max?)?),
            },
        ))
    }

    /// One nested quantifier's lowering — one spelling for both of the
    /// shapes a chain's hop position admits (a var-length rel, after
    /// [`var_length_quantified`], is a `QuantifiedPathPattern` too).
    /// Bounded only: an unbounded nested loop's count would be pinned by
    /// the global interval, which pins only totals.
    fn nest(
        &mut self,
        quantified: &QuantifiedPathPattern,
        current: u32,
        exit: Option<u32>,
        len: Len,
    ) -> Option<(u32, Len)> {
        if !quantified_admissible(quantified, false) {
            return None;
        }
        let max = quantified.max?;
        let (hop_exit, hop_len) =
            self.unroll(&quantified.elements, quantified.min, max, current, exit)?;
        Some((hop_exit, len.plus(hop_len)?))
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
        if !alternating_chain(elements) {
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
            let exit = if last_hop { last_target } else { None };
            match element {
                PatternElement::Node(node) if i.is_multiple_of(2) => {
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
                PatternElement::Relationship(rel) if !i.is_multiple_of(2) => {
                    if rel.var_length {
                        // The nested var-length rel: predicates on the
                        // traversed edges decline before the
                        // normalization drops them.
                        if rel.properties.is_some() || rel.where_clause.is_some() {
                            return None;
                        }
                        (current, len) =
                            self.nest(&var_length_quantified(rel), current, exit, len)?;
                    } else {
                        if !plain_rel(rel) {
                            return None;
                        }
                        let to = if last_hop {
                            last_target.or_else(|| self.alloc())
                        } else {
                            self.alloc()
                        }?;
                        let types = self.interned(rel)?;
                        self.push_transition(current, to, types, rel.direction);
                        last_rel_transition = Some(self.tables.transitions.len() - 1);
                        current = to;
                        len = len.plus(Len {
                            min: 1,
                            max: Some(1),
                        })?;
                    }
                }
                PatternElement::Quantified(quantified) if !i.is_multiple_of(2) => {
                    // A nested quantified group — unconstructible by the
                    // parser (the `NestedQuantification` rule), but the IR
                    // shape exists; the same nested rules apply, its own
                    // WHERE included (only the spine's group predicate
                    // compiles).
                    (current, len) = self.nest(quantified, current, exit, len)?;
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
            self.tables.predicates.push(compiled);
        }
        Some((current, len))
    }
}

/// The builder's guard emission — the forced rung's half of the
/// construction, beside the admission rules it applies.
impl Builder<'_> {
    /// The interior-node admission policy, one spelling for the pattern
    /// walk and the chain walk: under the forced rung the node compiles
    /// to the guard of `state`, the state its position occupies (a plain
    /// node to none); under the auto rung it must carry no admission
    /// facts at all (`plain_node` — the v1 NFA has no node-admission
    /// surface). `None` declines.
    pub(super) fn admit_interior(&mut self, node: &NodePattern, state: u32) -> Option<()> {
        if !self.forced {
            return plain_node(node);
        }
        if let InteriorGuard::Guard(guard) = interior_guard(node)? {
            self.guard(state, guard);
        }
        Some(())
    }

    /// The round boundary's states for one chain emission entered at
    /// `entry`: the round's `open` state and the last hop's target.
    /// Under the **product form** (a boundary-carrying chain) `open` is
    /// a fresh state carrying the body's first pattern, entered by
    /// epsilon from `entry`, and the last hop targets a fresh `close`
    /// state carrying the body's last pattern, stitched back to `open`
    /// by the repeat epsilon (and from `entry` by the zero-round
    /// pass-through when the quantifier admits zero rounds). Otherwise
    /// `open` is `entry` itself and the target is the caller's `close`
    /// as given — the plain form's layout, unchanged.
    pub(super) fn split_boundary(
        &mut self,
        entry: u32,
        close: Option<u32>,
        edges: &ChainEdges<'_>,
    ) -> Option<(u32, Option<u32>)> {
        let split_close = edges.needs_split_close();
        let open = if edges.first.is_some() || split_close {
            let open = self.alloc()?;
            if let Some(guard) = &edges.first {
                self.guard(open, guard.clone());
            }
            debug_assert_ne!(open, entry, "the split's open state is freshly allocated");
            self.push_epsilon(entry, open);
            open
        } else {
            entry
        };
        if !split_close {
            return Some((open, close));
        }
        let close_state = self.alloc()?;
        if let Some(guard) = &edges.last {
            self.guard(close_state, guard.clone());
        }
        // The repeat and the zero-round pass-through, the two ways the
        // walk leaves the round boundary: the next round departs the open
        // state (its guard re-checked on this same node), and a
        // zero-round group skips straight to the close.
        self.push_epsilon(close_state, open);
        if edges.zero_rounds {
            self.push_epsilon(entry, close_state);
        }
        Some((open, Some(close_state)))
    }

    /// The state the post-group node occupies, given the group's `close`
    /// (the state every round ends at). A guarded post-group node gets a
    /// fresh exit state, entered by epsilon from `close`, so its guard is
    /// checked once the group is finished — on `close` itself it would
    /// be checked at every round's end and drop each multi-round walk
    /// whose intermediate round ends fail it. An unguarded one stays on
    /// `close`.
    pub(super) fn split_exit(&mut self, close: u32, exit_guarded: bool) -> Option<u32> {
        if !exit_guarded {
            return Some(close);
        }
        let exit = self.alloc()?;
        self.push_epsilon(close, exit);
        Some(exit)
    }
}

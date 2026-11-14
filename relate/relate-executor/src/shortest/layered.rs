//! The layered predecessor-DAG search: a single-source BFS that keeps
//! shortest-path predecessors, trail enumeration over them, the shortest
//! closed trails through one node (ADR-0186, ADR-0189), and the
//! bidirectional all-shortest pair search (ADR-0601), which grows two such
//! DAGs with the same one layer step and joins them where they meet.

use std::collections::hash_map::Entry;

use relate_core::{NodeId, RelId, Result};

use crate::hash_seed::IdMap;
use crate::overlay::ExecGraph;

use super::Search;

/// A layered search's result: the shortest-path predecessors of every node
/// it reached.
pub(crate) struct Dag {
    origin: NodeId,
    /// Depth and first predecessor of each reached node other than the
    /// origin. The origin is never a member: a hop back to it is ignored
    /// at admission, so every entry carries a real first predecessor.
    first: IdMap<NodeId, (u32, (NodeId, RelId))>,
    /// Later same-depth predecessors; all-path searches only.
    more: IdMap<NodeId, Vec<(NodeId, RelId)>>,
}

/// One completed layer of a layered search, handed to the `on_layer`
/// consumer of [`Search::dag_layers`]: the nodes just reached, in discovery
/// order, over the search's settled predecessors. Its [`Layer::trails`]
/// reads only the reported nodes — a node's predecessors are final once its
/// layer completes, so a caller can read and emit the layer's trails at
/// once, before the deeper layers expand.
pub(crate) struct Layer<'a> {
    dag: &'a Dag,
    nodes: &'a [NodeId],
}

impl Layer<'_> {
    /// This layer's nodes, in discovery order.
    pub(crate) fn nodes(&self) -> &[NodeId] {
        self.nodes
    }

    /// Every trail the search holds from its origin to `target`, origin
    /// first: all predecessor chains of an all-path search, the first chain
    /// otherwise. Empty when `target` was not reached. `target` must be one
    /// of this layer's nodes, whose predecessors are settled.
    pub(crate) fn trails(&self, graph: &ExecGraph<'_>, target: NodeId) -> Result<Vec<Vec<RelId>>> {
        debug_assert!(
            self.nodes.contains(&target),
            "Layer::trails: {target} is not a node of this layer"
        );
        self.dag.trails(graph, target)
    }
}

/// A layered search in progress: its DAG so far, the last layer it
/// completed (the next to expand) and that layer's depth.
struct Grown {
    dag: Dag,
    frontier: Vec<NodeId>,
    depth: u32,
}

impl Grown {
    fn new(origin: NodeId) -> Self {
        Self {
            // Dense engine-id keys: the dense-id hasher rule (ADR-0355,
            // LABS-337), `hash_seed::IdMap`.
            dag: Dag {
                origin,
                first: IdMap::default(),
                more: IdMap::default(),
            },
            frontier: vec![origin],
            depth: 0,
        }
    }
}

impl Dag {
    /// Whether the search reached `node`: its origin or a node it keeps
    /// predecessors for.
    fn holds(&self, node: NodeId) -> bool {
        node == self.origin || self.first.contains_key(&node)
    }

    /// Every trail the search holds from its origin to `target`, origin
    /// first: all predecessor chains of an all-path search, the first chain
    /// otherwise. Empty when `target` was not reached.
    pub(crate) fn trails(&self, graph: &ExecGraph<'_>, target: NodeId) -> Result<Vec<Vec<RelId>>> {
        let mut out = Vec::new();
        if !self.holds(target) {
            return Ok(out);
        }
        let mut stack = vec![(target, Vec::new())];
        while let Some((at, mut rev)) = stack.pop() {
            graph.cancel_tick()?;
            let Some(&(_, (prev, rid))) = self.first.get(&at) else {
                rev.reverse();
                out.push(rev);
                continue;
            };
            for &(p, r) in self.more.get(&at).into_iter().flatten() {
                let mut longer = rev.clone();
                longer.push(r);
                stack.push((p, longer));
            }
            rev.push(rid);
            stack.push((prev, rev));
        }
        Ok(out)
    }
}

impl Search<'_, '_> {
    /// Layered BFS from `origin`, at most `max` relationships deep and never
    /// over `skip`. Each node is expanded once, at its first depth; with
    /// `all`, later arrivals at that same depth are kept as extra
    /// predecessors. Arrivals back at `origin` are ignored, so every
    /// predecessor chain is a shortest path and hence a trail.
    ///
    /// `stop(node, depth)` sees each node as it is reached, the origin first
    /// at depth 0. After the first layer in which it returns true the search
    /// stops; one that keeps only first predecessors stops at once. A
    /// fallible stop (the end-pattern admission, whose property-map
    /// evaluation errors, LABS-668) aborts the search with its error.
    pub(crate) fn dag(
        &self,
        origin: NodeId,
        stop: impl FnMut(NodeId, u32) -> Result<bool>,
    ) -> Result<Dag> {
        self.walk(origin, stop, |_| Ok(false))
    }

    /// [`Self::dag`] reporting each layer as it completes (ADR-0555): after
    /// the origin (depth 0, alone) and after every layer of newly reached
    /// nodes, `on_layer` gets that layer as a [`Layer`]. A node's
    /// predecessors are final once its layer completes (later arrivals are
    /// one layer deeper), so the layer's trails can be read and emitted at
    /// once; the deeper layers are never expanded when `on_layer` returns
    /// true, which is how a halted consumer stops the search itself. An
    /// empty layer is never reported: a search whose frontier empties simply
    /// ends.
    pub(crate) fn dag_layers(
        &self,
        origin: NodeId,
        on_layer: impl FnMut(Layer<'_>) -> Result<bool>,
    ) -> Result<Dag> {
        self.walk(origin, |_, _| Ok(false), on_layer)
    }

    /// The layered search's one body. [`Self::dag`] passes its per-node
    /// `stop` and a no-op layer channel; [`Self::dag_layers`] passes a no-op
    /// `stop` and its layer channel — each entry point carries exactly one
    /// stopping protocol.
    fn walk(
        &self,
        origin: NodeId,
        mut stop: impl FnMut(NodeId, u32) -> Result<bool>,
        mut on_layer: impl FnMut(Layer<'_>) -> Result<bool>,
    ) -> Result<Dag> {
        let mut grown = Grown::new(origin);
        let mut stopped = stop(origin, 0)?;
        let mut halted = on_layer(Layer {
            dag: &grown.dag,
            nodes: &[origin],
        })?;
        while !stopped && !halted && grown.depth < self.max {
            stopped = self.grow(&mut grown, &mut stop)?;
            // The layer is complete (or, for a first-predecessor search
            // stopped inside it, final for every node it reached).
            // An empty layer reports nothing: the search simply ends.
            if grown.frontier.is_empty() {
                break;
            }
            halted = on_layer(Layer {
                dag: &grown.dag,
                nodes: &grown.frontier,
            })?;
        }
        Ok(grown.dag)
    }

    /// Expand `grown`'s frontier by one layer: each frontier node once,
    /// the layer it reaches becoming the new frontier. Returns whether
    /// `stop` fired on a node of the new layer; a first-predecessor search
    /// (`all` unset) ends the layer at that node. The one layer step,
    /// shared by [`Self::walk`] and the bidirectional
    /// [`Self::all_shortest_trails`].
    fn grow(
        &self,
        grown: &mut Grown,
        stop: &mut impl FnMut(NodeId, u32) -> Result<bool>,
    ) -> Result<bool> {
        let origin = grown.dag.origin;
        let next_depth = grown.depth + 1;
        let mut next = Vec::new();
        let mut stopped = false;
        for &cur in &grown.frontier {
            self.graph.cancel_tick()?;
            self.each_hop(cur, |n, rid| {
                if Some(rid) == self.skip || n == origin {
                    return Ok(());
                }
                match grown.dag.first.entry(n) {
                    Entry::Vacant(v) => {
                        v.insert((next_depth, (cur, rid)));
                        next.push(n);
                        stopped |= stop(n, next_depth)?;
                    }
                    Entry::Occupied(o) if self.all && o.get().0 == next_depth => {
                        grown.dag.more.entry(n).or_default().push((cur, rid));
                    }
                    Entry::Occupied(_) => {}
                }
                Ok(())
            })?;
            if stopped && !self.all {
                break;
            }
        }
        grown.frontier = next;
        grown.depth = next_depth;
        Ok(stopped)
    }

    /// Every shortest trail from `start` to `end` (distinct nodes) of at
    /// most `max` relationships, each start first: the same set the
    /// one-sided all-path `dag(start, |n, _| Ok(n == end))` search's
    /// [`Dag::trails`] holds, found by growing a layered DAG from both
    /// ends (ADR-0034's balanced rule, all-path form) so a distance-`d`
    /// pair expands two balls of radius about `d / 2` instead of one of
    /// radius `d`. The order of the trails is unspecified.
    ///
    /// The side with the smaller frontier grows one whole layer at a time.
    /// With no node common to both sides yet, the distance exceeds the
    /// sum of the two depths; so the first layer to contain a node the
    /// other side holds lands exactly on the shortest length, and the
    /// other side holds that node at its deepest layer. Every shortest
    /// trail crosses the grown layer at exactly one such node, so the
    /// trails are, per meeting node, the product of its start-side chains
    /// and end-side chains — disjoint across nodes, and each a shortest
    /// path, hence a trail.
    ///
    /// Precondition: `start` ≠ `end`, `min` ≤ 1, `all` set, no `skip`.
    pub(crate) fn all_shortest_trails(
        &self,
        start: NodeId,
        end: NodeId,
    ) -> Result<Vec<Vec<RelId>>> {
        debug_assert!(
            start != end && self.min <= 1 && self.all && self.skip.is_none(),
            "all_shortest_trails needs distinct ends, min_hops <= 1 and an all-path search"
        );
        let back = self.reversed();
        let mut from_start = Grown::new(start);
        let mut from_end = Grown::new(end);
        loop {
            if from_start.frontier.is_empty() || from_end.frontier.is_empty() {
                return Ok(Vec::new());
            }
            // The next layer's meet would make a trail this long.
            let next = u64::from(from_start.depth) + u64::from(from_end.depth) + 1;
            if next > u64::from(self.max) {
                return Ok(Vec::new());
            }
            // Smaller frontier first; a tie grows the shallower side, so
            // two unit frontiers still meet in the middle.
            let start_first = (from_start.frontier.len(), from_start.depth)
                <= (from_end.frontier.len(), from_end.depth);
            let (search, side, other) = if start_first {
                (self, &mut from_start, &from_end)
            } else {
                (&back, &mut from_end, &from_start)
            };
            let meets = search.grow_to_meets(side, other)?;
            if !meets.is_empty() {
                return self.join_at(&from_start, &from_end, &meets);
            }
        }
    }

    /// Grow `side` one all-path layer and return the new layer's nodes the
    /// `other` side already holds.
    fn grow_to_meets(&self, side: &mut Grown, other: &Grown) -> Result<Vec<NodeId>> {
        self.grow(side, &mut |_, _| Ok(false))?;
        Ok(side
            .frontier
            .iter()
            .copied()
            .filter(|&n| other.dag.holds(n))
            .collect())
    }

    /// The start-to-end trails through `meets`: per node, every start-side
    /// chain followed by every end-side chain read backwards. The result
    /// holds one entry per shortest trail — the same retention as the
    /// one-sided [`Dag::trails`] it replaces (ADR-0601 defers streaming).
    fn join_at(
        &self,
        from_start: &Grown,
        from_end: &Grown,
        meets: &[NodeId],
    ) -> Result<Vec<Vec<RelId>>> {
        let mut out = Vec::new();
        for &at in meets {
            let heads = from_start.dag.trails(self.graph, at)?;
            let tails = from_end.dag.trails(self.graph, at)?;
            for head in &heads {
                for tail in &tails {
                    let mut trail = head.clone();
                    trail.extend(tail.iter().rev());
                    out.push(trail);
                }
            }
        }
        Ok(out)
    }

    /// The shortest closed trails through `s`, of length 1 to `max`: a first
    /// relationship `r` out of `s`, then a shortest way back to `s` that
    /// avoids `r`. A single first-arrival BFS from `s` would miss an
    /// undirected cycle that closes between two first-hop branches (a
    /// triangle) and could return over the relationship it left by
    /// (ADR-0186 Consequences). Once a length is found, later searches are
    /// capped at it (ties, with `self.all`) or below it.
    pub(crate) fn closed(&self, s: NodeId) -> Result<Vec<Vec<RelId>>> {
        let mut firsts = self.hops(s)?;
        // Self-loops first: no closed trail is shorter.
        firsts.sort_by_key(|&(x, _)| x != s);
        // The longest closed trail still wanted.
        let mut cap = self.max;
        let mut out: Vec<Vec<RelId>> = Vec::new();
        for (x, first) in firsts {
            if cap == 0 {
                break;
            }
            let back = Search {
                max: cap - 1,
                skip: Some(first),
                ..self.clone()
            };
            let rests = back.dag(x, |n, _| Ok(n == s))?.trails(self.graph, s)?;
            let Some(len) = rests.first().map(|rest| rest.len() + 1) else {
                continue;
            };
            if out.first().is_some_and(|t| t.len() > len) {
                out.clear();
            }
            out.extend(
                rests
                    .into_iter()
                    .map(|rest| std::iter::once(first).chain(rest).collect::<Vec<_>>()),
            );
            cap = if self.all { len as u32 } else { len as u32 - 1 };
        }
        Ok(out)
    }
}

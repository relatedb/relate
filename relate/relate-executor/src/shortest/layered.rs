//! The layered predecessor-DAG search: a single-source BFS that keeps
//! shortest-path predecessors, trail enumeration over them, and the
//! shortest closed trails through one node (ADR-0186, ADR-0189).

use std::collections::hash_map::Entry;

use relate_core::{NodeId, RelId, Result};

use crate::hash_seed::FxMap;
use crate::overlay::ExecGraph;

use super::Search;

/// A layered search's result: the shortest-path predecessors of every node
/// it reached.
pub(crate) struct Dag {
    origin: NodeId,
    /// Depth and first predecessor of each reached node other than the
    /// origin. The origin is never a member: a hop back to it is ignored
    /// at admission, so every entry carries a real first predecessor.
    first: FxMap<NodeId, (u32, (NodeId, RelId))>,
    /// Later same-depth predecessors; all-path searches only.
    more: FxMap<NodeId, Vec<(NodeId, RelId)>>,
}

impl Dag {
    /// Every trail the search holds from its origin to `target`, origin
    /// first: all predecessor chains of an all-path search, the first chain
    /// otherwise. Empty when `target` was not reached.
    pub(crate) fn trails(&self, graph: &ExecGraph<'_>, target: NodeId) -> Result<Vec<Vec<RelId>>> {
        let mut out = Vec::new();
        if target != self.origin && !self.first.contains_key(&target) {
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
        all: bool,
        skip: Option<RelId>,
        stop: impl FnMut(NodeId, u32) -> Result<bool>,
    ) -> Result<Dag> {
        self.dag_layers(origin, all, skip, stop, |_, _| Ok(false))
    }

    /// [`Self::dag`] reporting each layer as it completes (ADR-0555): after
    /// the origin (depth 0, alone) and after every layer of newly reached
    /// nodes, `on_layer` gets the search so far and that layer's nodes in
    /// discovery order (the search keeps no list of every reached node). A node's
    /// predecessors are final once its layer completes (later arrivals are
    /// one layer deeper), so the layer's trails can be read and emitted at
    /// once; the deeper layers are never expanded when `on_layer` returns
    /// true, which is how a halted consumer stops the search itself.
    pub(crate) fn dag_layers(
        &self,
        origin: NodeId,
        all: bool,
        skip: Option<RelId>,
        mut stop: impl FnMut(NodeId, u32) -> Result<bool>,
        mut on_layer: impl FnMut(&Dag, &[NodeId]) -> Result<bool>,
    ) -> Result<Dag> {
        // Dense engine-id keys: the dense-id hasher rule (ADR-0355,
        // LABS-337), `hash_seed::FxMap`.
        let mut dag = Dag {
            origin,
            first: FxMap::default(),
            more: FxMap::default(),
        };
        let mut stopped = stop(origin, 0)?;
        let mut halted = on_layer(&dag, &[origin])?;
        let mut frontier = vec![origin];
        let mut depth = 0;
        while !stopped && !halted && !frontier.is_empty() && depth < self.max {
            let next_depth = depth + 1;
            let mut next = Vec::new();
            for &cur in &frontier {
                self.graph.cancel_tick()?;
                self.each_hop(cur, |n, rid| {
                    if Some(rid) == skip || n == origin {
                        return Ok(());
                    }
                    match dag.first.entry(n) {
                        Entry::Vacant(v) => {
                            v.insert((next_depth, (cur, rid)));
                            next.push(n);
                            stopped |= stop(n, next_depth)?;
                        }
                        Entry::Occupied(o) if all && o.get().0 == next_depth => {
                            dag.more.entry(n).or_default().push((cur, rid));
                        }
                        Entry::Occupied(_) => {}
                    }
                    Ok(())
                })?;
                if stopped && !all {
                    break;
                }
            }
            // The layer is complete (or, for a first-predecessor search
            // stopped inside it, final for every node it reached).
            // An empty layer reports nothing: the search simply ends.
            if !next.is_empty() {
                halted = on_layer(&dag, &next)?;
            }
            frontier = next;
            depth = next_depth;
        }
        Ok(dag)
    }

    /// The shortest closed trails through `s`, of length 1 to `max`: a first
    /// relationship `r` out of `s`, then a shortest way back to `s` that
    /// avoids `r`. A single first-arrival BFS from `s` would miss an
    /// undirected cycle that closes between two first-hop branches (a
    /// triangle) and could return over the relationship it left by
    /// (ADR-0186 Consequences). Once a length is found, later searches are
    /// capped at it (ties, with `all`) or below it.
    pub(crate) fn closed(&self, s: NodeId, all: bool) -> Result<Vec<Vec<RelId>>> {
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
                ..self.clone()
            };
            let rests = back
                .dag(x, all, Some(first), |n, _| Ok(n == s))?
                .trails(self.graph, s)?;
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
            cap = if all { len as u32 } else { len as u32 - 1 };
        }
        Ok(out)
    }
}

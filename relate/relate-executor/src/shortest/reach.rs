//! The reach set: every node a search can arrive at from one origin within
//! its hop bound, kept without predecessors. It answers "is `end` within
//! `max` hops of `origin`" for any number of ends from one breadth-first
//! pass, which is all a per-pair search that comes back empty needs.

use relate_core::{NodeId, Result};

use crate::hash_seed::IdSet;

use super::Search;

impl Search<'_, '_> {
    /// The nodes other than `origin` this search reaches from `origin`
    /// within `max` relationships, or `None` once more than `cap` of them
    /// are found (the set never holds more than `cap + 1`, so its memory is
    /// bounded by `cap`, never by the component).
    ///
    /// A first arrival is a shortest walk, and a shortest walk repeats no
    /// node and so no relationship: membership is exactly "a trail of
    /// length 1 to `max` from `origin` ends there", the question
    /// [`super::pair`] answers for one end.
    pub(crate) fn reach(&self, origin: NodeId, cap: usize) -> Result<Option<IdSet<NodeId>>> {
        let mut seen = IdSet::default();
        let mut frontier = vec![origin];
        let mut depth = 0;
        while !frontier.is_empty() && depth < self.max {
            let mut next = Vec::new();
            for &cur in &frontier {
                self.graph.cancel_tick()?;
                self.each_hop(cur, |n, _| {
                    // Stop inserting at `cap + 1`: the set never holds
                    // more, whatever a hub's degree.
                    if seen.len() <= cap && n != origin && seen.insert(n) {
                        next.push(n);
                    }
                    Ok(())
                })?;
                if seen.len() > cap {
                    return Ok(None);
                }
            }
            frontier = next;
            depth += 1;
        }
        Ok(Some(seen))
    }
}

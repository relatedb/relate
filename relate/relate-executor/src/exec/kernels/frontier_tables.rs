//! The frontier family's table vocabulary (ADR-0478 Decision 6's
//! allocation half, in the sparse layout): the tables one frontier run
//! reads and writes — the per-touched-node records (the dedup bitmask
//! and the endpoint/candidate marks) and the first-arrival pair ledger
//! carrying each pair's parent — the run's output records those tables'
//! readers consume (the accepted [`Endpoint`] with its [`Arrival`], the
//! reconstructed [`Chain`]), and the allocation ceiling that bounds them
//! ([`FRONTIER_TABLE_BUDGET`], [`within_table_budget`]). Split from the
//! BFS runner (`super::frontier`) by the file-size rule: the runner owns
//! the level loop and the run types (`RunOutcome` and the run it serves,
//! never named here); this module owns the shapes the loop writes and
//! every consumer — the capped emission's halt probe, the re-arrival
//! rule's chain proof, the witness reconstruction — reads.
//!
//! **Sparse, never span-sized.** Every table is sized by the (node,
//! state) pairs the run touched, never by the store's node-id span: the
//! node records live in a [`NodeMap`] (open-addressed, generation-stamped
//! — O(1) reset), the pairs in one ledger vector, and a node's pairs are
//! linked newest-first through the ledger from its record. Nothing is
//! allocated before the first arrival, and no untouched memory is ever
//! written.

use std::mem::size_of;

use relate_core::{NodeId, RelId};

use crate::overlay::ExecGraph;
use crate::store::RelTypes;

use super::nfa::FrontierNfa;
use super::node_map::NodeMap;

/// The per-run table-allocation ceiling (ADR-0478 Decision 6's
/// allocation half, the ADR-0395 fixed-constant family): the exact bytes
/// a run's tables hold — [`FrontierTables::heap_bytes`] plus the
/// re-arrival rule's
/// [`RuleBuffers::heap_bytes`](super::rearival::RuleBuffers::heap_bytes)
/// — are checked against it at every level boundary, beside the walk
/// budget, and a run that crosses it declines at run time
/// (`FrontierRunDecline::TableBudget`): its buffered rows are discarded and
/// the clause falls through to the DFS, which has its own caps. Decline
/// over an authored error is deliberate — the family's fail-through
/// direction. The sparse tables allocate nothing up front, so the store's
/// span no longer decides anything; what the ceiling bounds is a run that
/// genuinely touches more pairs than the tables should hold. Sized so
/// every measured class fits with headroom: a run touching every node of
/// a ~10⁶-node store at the workload's 1–3 states sits an order under it.
pub(crate) const FRONTIER_TABLE_BUDGET: u64 = 1 << 30;

/// Whether a run's tables, at `bytes` of exact allocation, sit within
/// the ceiling — the BFS runner's level-boundary check. The decline is
/// the DFS fall-through, never an error; the test seam
/// (`frontier_table_budget_override`) pins the decline on a small graph
/// instead of building a run whose tables cross the production constant.
pub(crate) fn within_table_budget(bytes: usize) -> bool {
    #[cfg(test)]
    let budget = super::frontier_table_budget_override().unwrap_or(FRONTIER_TABLE_BUDGET);
    #[cfg(not(test))]
    let budget = FRONTIER_TABLE_BUDGET;
    bytes as u64 <= budget
}

/// How one first arrival happened: over a relationship, or over an
/// epsilon edge (an unrolled bounded quantifier's optional-copy skip —
/// same node, no hop, no rel). The rel-id column (Decision 1: one
/// `RelId` per pair) rides [`Step::Rel`] in place: the traversed rel is
/// exactly what the witness reconstruction needs beside the predecessor
/// pair, and the ledger's entry layout already carried everything else
/// that column requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// A rel-consuming transition, over this relationship.
    Rel(RelId),
    /// A rel-free skip between automaton states of the same node.
    Epsilon,
}

/// The recorded parent of one (node, state) pair's first arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Parent {
    /// The predecessor node.
    pub(crate) node: NodeId,
    /// The predecessor automaton state.
    pub(crate) state: u32,
    /// Whether the arrival consumed a rel or was an epsilon skip.
    pub(crate) step: Step,
}

/// The ledger's end-of-chain link: a node record's head, or a pair
/// entry's `next`, holding this names no further pair. The largest
/// ledger index a record's packed head word can carry
/// ([`NodeRecord::link`]): the table ceiling bounds the ledger orders
/// below it.
const NO_PAIR: u32 = (1 << 30) - 1;

/// The record's endpoint-mark bit in the packed head word.
const ENDPOINT_MARK: u32 = 1 << 30;

/// The ledger's next pair index at length `len`, or `None` at the index
/// ceiling — the packing reserves every index at and above
/// [`NO_PAIR`] (the mark bits), so the last indexable pair is
/// `NO_PAIR - 1` and a longer ledger declines instead of indexing.
fn next_pair_index(len: usize) -> Option<u32> {
    let index = u32::try_from(len).ok()?;
    (index < NO_PAIR).then_some(index)
}

/// The record's candidate-mark bit in the packed head word.
const CANDIDATE_MARK: u32 = 1 << 31;

/// One touched node's record: the run's every per-node fact, in one
/// place — the dedup bitmask, the two marks, and the head of the node's
/// pair chain through the ledger. The record table is the run's one
/// "which nodes did this run touch" structure. Twelve bytes of data,
/// sixteen with the `u64`'s alignment padding, so a map slot is 32
/// (stamp 4, padding 4, key 8, record 16): the head index and the two marks
/// share one word ([`NO_PAIR`] bounds the index below the mark bits) —
/// the slot's size is the BFS's cache footprint per touched node.
#[derive(Debug, Clone, Copy)]
struct NodeRecord {
    /// The ever-visited (node, state) bitmask — the dedup set.
    seen: u64,
    /// The packed head word: the low 30 bits the ledger index of this
    /// node's newest pair ([`NO_PAIR`] until its first) — the chain
    /// [`PairEntry::next`] continues — and the [`ENDPOINT_MARK`] and
    /// [`CANDIDATE_MARK`] bits above it. Read and written through the
    /// accessors below only.
    link: u32,
}

impl NodeRecord {
    /// A node the run reached for the first time: nothing seen, nothing
    /// marked, no pair yet.
    fn untouched() -> Self {
        Self {
            seen: 0,
            link: NO_PAIR,
        }
    }

    /// The ledger index of the node's newest pair, or [`NO_PAIR`].
    fn head(self) -> u32 {
        self.link & NO_PAIR
    }

    /// Make `index` the node's newest pair, answering the previous head.
    fn push_head(&mut self, index: u32) -> u32 {
        let previous = self.head();
        self.link = (self.link & !NO_PAIR) | index;
        previous
    }

    /// Whether the node carries `mark` ([`ENDPOINT_MARK`] or
    /// [`CANDIDATE_MARK`]).
    fn marked(self, mark: u32) -> bool {
        self.link & mark != 0
    }
}

/// One first arrival's answer ([`FrontierTables::arrive`]): whether the
/// pair is new, and whether its node was already an accepted endpoint
/// before this arrival — the BFS runner's two per-arrival reads, in one
/// record lookup.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Arrived {
    /// The pair was unseen: it joined the ledger and is to be expanded.
    pub(crate) new: bool,
    /// The node was already an accepted endpoint (the accept-on-arrival
    /// check's input — read before any mark this arrival leads to).
    pub(crate) endpoint: bool,
}

/// The run's table ceiling, crossed at an arrival: the pair was
/// recorded nowhere (no mark, no cell, no ledger entry), and the caller
/// folds the fact into the run's table-budget decline — the per-arrival
/// backstop that keeps both the byte budget and the packing's index
/// space a decline at every granularity, never a panic and never an
/// unbounded allocation. The level-boundary check usually fires first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TableCeiling;

/// One ledger entry: a first-arrival (node, state) pair, its parent
/// (`None` exactly for the seed pair — every chain's terminator), and
/// the link to the same node's previous pair.
#[derive(Debug, Clone, Copy)]
struct PairEntry {
    /// The pair's node.
    node: NodeId,
    /// The pair's automaton state.
    state: u32,
    /// The ledger index of the same node's previous pair, or
    /// [`NO_PAIR`].
    next: u32,
    /// The first arrival's parent; `None` for the seed pair.
    parent: Option<Parent>,
}

/// The first-arrival parent table of one frontier run, as its readers
/// hold it: per touched (node, state) pair, the predecessor pair and the
/// step kind. A pair has an entry exactly when it is marked seen (the
/// dedup set and the ledger are written together); the one entry
/// without a parent is the run's seed pair — the chain walk's
/// terminator. A borrowed view over the run's owned [`FrontierTables`] —
/// the halt probe, the re-arrival rule's chain proof, and the witness
/// reconstruction read this shape, never a second copy of the ledger.
#[derive(Debug)]
pub(crate) struct Parents<'t> {
    tables: &'t FrontierTables,
}

impl Parents<'_> {
    /// The recorded parent of `(node, state)`'s first arrival: `None`
    /// for the seed pair (the walk's start, every chain's terminator)
    /// or a pair the run never reached. One record lookup, then the
    /// node's pair chain — at most its state count long.
    pub(crate) fn parent_of(&self, node: NodeId, state: u32) -> Option<Parent> {
        let mut index = self.tables.nodes.get(node)?.head();
        while index != NO_PAIR {
            let entry = &self.tables.pairs[index as usize];
            if entry.state == state {
                return entry.parent;
            }
            index = entry.next;
        }
        None
    }

    /// The automaton's state count. A test-only accessor (the runner
    /// tests' table-shape assertions read it).
    #[cfg(test)]
    pub(crate) fn states(&self) -> usize {
        self.tables.states
    }
}

/// The frontier family's per-clause table set (ADR-0478 Decision 6's
/// allocation half, sparse): created once per engaged clause and
/// recycled across the clause's seeds. Nothing is allocated up front —
/// the node records and the ledger grow with the pairs a run touches —
/// and the per-seed reset ([`Self::begin_seed`]) is O(1): the record
/// map's generation bump and the ledger's length reset. A recycled set
/// keeps its capacity (the largest seed's footprint so far), never a
/// span-sized one.
///
/// The write surface is the method set alone: the BFS runner
/// (`super::frontier`) records first arrivals and marks endpoints and
/// candidates through [`Self::begin_seed`], [`Self::arrive`], and the
/// mark methods beside them; every other consumer reads through the
/// views ([`Self::parents`], the run's endpoint list).
///
/// One run at a time holds the set: the runner's BFS
/// ([`run_with_halt`](super::frontier::run_with_halt)) takes it whole
/// (the clause's pool left empty for the run's duration) and carries it
/// inside a served outcome's run; the caller hands it back through the
/// run's own `reclaim` (in `super::frontier`) once the run's consumers
/// are done. A run that errors out or declines drops its
/// taken set — its clause is aborting or falling through, the pool with
/// it, so an emptied pool is never reused as if it carried a clause's
/// tables.
#[derive(Debug, Default)]
pub(crate) struct FrontierTables {
    /// The per-touched-node records — the run's one touched-node set.
    nodes: NodeMap<NodeRecord>,
    /// The current run's first-arrival pairs, in arrival order, level
    /// after level — the level structure itself (level boundaries are
    /// the run's `per_level` counts) and the parent table in one
    /// vector, each node's entries linked from its record.
    pairs: Vec<PairEntry>,
    /// The automaton's state count — the wiring check's fact.
    states: usize,
}

impl FrontierTables {
    /// The clause's empty tables for an automaton of `states` states —
    /// no allocation until the first arrival.
    pub(crate) fn new(states: usize) -> Self {
        Self {
            nodes: NodeMap::default(),
            pairs: Vec::new(),
            states,
        }
    }

    /// The per-seed reset in O(1): the record map's generation bump
    /// retires every record of the finished run at once, and the
    /// ledger's entries are plain data, cleared by length.
    pub(super) fn begin_seed(&mut self) {
        self.nodes.clear();
        self.pairs.clear();
    }

    /// Record an arrival at `(node, state)` — the BFS runner's dedup
    /// filter's check-and-set and the ledger append in one record
    /// lookup: answers whether the pair is new (a pair is expanded
    /// exactly once ever; a new pair joins the ledger with `parent`,
    /// `None` exactly for the seed pair) and whether the node was
    /// already an accepted endpoint. The pair's mark cannot change the
    /// accept-on-arrival answer the runner derives from the returned
    /// endpoint fact: the rule's chain proof walks the predecessor's
    /// chain, which an unseen pair is not on.
    pub(super) fn arrive(
        &mut self,
        node: NodeId,
        state: u32,
        parent: Option<Parent>,
    ) -> Result<Arrived, TableCeiling> {
        // Both ceilings are per arrival, not per level: the index space
        // (the packing's reserved bits) and the byte budget itself —
        // a single high-fan-out level must cross neither before its
        // next boundary, so the run declines here instead of growing
        // an unbounded ledger (the level-boundary check stays as the
        // early, cheaper stop).
        let index = next_pair_index(self.pairs.len()).ok_or(TableCeiling)?;
        if !within_table_budget(self.heap_bytes()) {
            return Err(TableCeiling);
        }
        let (record, _) = self.nodes.touch(node, NodeRecord::untouched);
        let endpoint = record.marked(ENDPOINT_MARK);
        let bit = 1u64 << state;
        if record.seen & bit != 0 {
            return Ok(Arrived {
                new: false,
                endpoint,
            });
        }
        record.seen |= bit;
        let next = record.push_head(index);
        self.pairs.push(PairEntry {
            node,
            state,
            next,
            parent,
        });
        Ok(Arrived {
            new: true,
            endpoint,
        })
    }

    /// Whether `(node, state)`'s pair is marked seen — the BFS runner's
    /// level-0 read (the seed's accepting-state scan over its pair
    /// set).
    pub(super) fn pair_seen(&self, node: NodeId, state: u32) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|record| record.seen & (1u64 << state) != 0)
    }

    /// Whether `node` is already an accepted endpoint — the BFS
    /// runner's accept-on-arrival check (one endpoint per node, the
    /// first arrival's) and candidate pass's guard.
    pub(super) fn endpoint_marked(&self, node: NodeId) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|record| record.marked(ENDPOINT_MARK))
    }

    /// Mark `node` an accepted endpoint — the BFS runner's admission
    /// write.
    pub(super) fn mark_endpoint(&mut self, node: NodeId) {
        self.nodes.touch(node, NodeRecord::untouched).0.link |= ENDPOINT_MARK;
    }

    /// Whether `node` is already a below-min candidate — the BFS
    /// runner's candidate guard (one candidate per node).
    pub(super) fn candidate_marked(&self, node: NodeId) -> bool {
        self.nodes
            .get(node)
            .is_some_and(|record| record.marked(CANDIDATE_MARK))
    }

    /// Mark `node` a below-min candidate — the BFS runner's candidate
    /// write.
    pub(super) fn mark_candidate(&mut self, node: NodeId) {
        self.nodes.touch(node, NodeRecord::untouched).0.link |= CANDIDATE_MARK;
    }

    /// The ledger's pair count — the BFS runner's level-boundary
    /// bookkeeping read.
    pub(super) fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// The ledger's `index`th pair, in arrival order — the BFS runner's
    /// level-loop expansion read.
    pub(super) fn pair_at(&self, index: usize) -> (NodeId, u32) {
        let entry = &self.pairs[index];
        (entry.node, entry.state)
    }

    /// Whether the set is shaped for a run's automaton — the BFS
    /// runner's wiring check (a mismatch is a wiring bug, asserted
    /// there).
    pub(super) fn shaped_for(&self, states: usize) -> bool {
        self.states == states
    }

    /// The exact bytes the set holds — the record map's slot array and
    /// the ledger's allocation, the table ceiling's accounting read
    /// ([`within_table_budget`]).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.nodes.heap_bytes() + self.pairs.capacity() * size_of::<PairEntry>()
    }

    /// The touched-node count of the current run — a test-only accessor
    /// (the sparse-sizing pins read it).
    #[cfg(test)]
    pub(crate) fn touched_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// The parent-table view over this set — the readers' one shape.
    pub(crate) fn parents(&self) -> Parents<'_> {
        Parents { tables: self }
    }
}

/// One accepted endpoint, in arrival order — the reconstruction's input.
/// `state` is the arrival state of the accepting transition (a final
/// state epsilon-reachable behind it adds no hops and no nodes, so the
/// node sequence is settled at the rel arrival), `hops` the observed
/// arrival's hop count (in `[min_hops, max_hops]` for observed
/// in-bounds admissions; below `min_hops` for a below-min candidate the
/// rule's search admitted — the observed arrival, the proven trail's
/// own arrival recorded nowhere), and `arrival` how the accepting
/// arrival happened. The arrival rides here, not in [`Parents`]: an
/// accept-on-arrival re-reach of an already-visited pair finds that
/// slot holding the pair's *earlier* first arrival — the recorded
/// arrival's own step is recoverable only here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The endpoint node.
    pub(crate) node: NodeId,
    /// The accepting arrival's automaton state.
    pub(crate) state: u32,
    /// The accepting arrival's hop count, in `[min_hops, max_hops]`.
    pub(crate) hops: u32,
    /// How the accepting arrival happened — the zero-length walk, or an
    /// accepting transition over a rel from a predecessor pair (the
    /// coupled `pred`/`rel` facts one shape, never two Options that can
    /// disagree).
    pub(crate) arrival: Arrival,
}

/// An endpoint's accepting arrival: the zero-length walk (the seed,
/// `min_hops == 0` — no rel, no predecessor), or one accepting
/// transition over `rel` from the pair `(node, state)` — the pair and
/// the rel are one fact, carried together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// The walk completes at hop 0 in an accepting state.
    Zero,
    /// An accepting transition over `rel` from the predecessor pair
    /// `(node, state)`.
    Rel {
        /// The pair the accepting transition expanded from.
        pred: (NodeId, u32),
        /// The rel the arrival traversed.
        rel: RelId,
    },
}

/// One reconstructed witness chain, seed-first: the node sequence and
/// the rel sequence of a complete accepting run. The chain-replay
/// form ([`frontier_emit`](super::frontier_emit)'s per-endpoint
/// reconstruction) and the re-arrival rule's searches
/// ([`rearival`](super::rearival)'s witness returns) agree on this one
/// shape — the emission binds rows from either source through the same
/// reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chain {
    /// The chain's nodes, seed first, target last.
    pub(crate) nodes: Vec<NodeId>,
    /// The chain's rels, one per hop, in walk order.
    pub(crate) rels: Vec<RelId>,
}

/// One transition's expansion inputs, resolved once per run: the
/// transition's index into the NFA's table and its type filter in the
/// interned domain (LABS-801). The compiler left ids; the names beside
/// them are the write overlay's arm (`RelTypes::matches_name`) and come
/// from the interner's own `Arc<str>` units — a clone is a refcount, never
/// a copy, and the per-pair walk compares ids. One canonical resolver: the
/// re-arrival rule's search ([`super::rearival`]) walks the same table,
/// never a second copy.
#[derive(Debug, Clone)]
pub(crate) struct Arm {
    pub(crate) transition: usize,
    pub(crate) types: RelTypes,
}

/// The per-state expansion table: every transition indexed by its from
/// state, its interned type ids carried as the resolved filter the
/// adjacency walk reads (LABS-801) — built once per run, never per pair.
pub(crate) fn arms(view: &ExecGraph<'_>, nfa: &FrontierNfa) -> Vec<Vec<Arm>> {
    let mut arms = vec![Vec::<Arm>::new(); nfa.states as usize];
    for (i, t) in nfa.transitions.iter().enumerate() {
        let types = RelTypes::from_interned(
            t.types
                .iter()
                .map(|&tid| (tid, view.base().rel_type_name(tid))),
        );
        arms[t.from as usize].push(Arm {
            transition: i,
            types,
        });
    }
    arms
}

/// The epsilon edges indexed by from state — the runner's one view of
/// the compiler's epsilon list.
pub(crate) fn epsilon_index(nfa: &FrontierNfa) -> Vec<Vec<u32>> {
    let mut eps_from = vec![Vec::new(); nfa.states as usize];
    for &(from, to) in &nfa.epsilons {
        eps_from[from as usize].push(to);
    }
    eps_from
}

/// The test seam's table set for one standalone run — fresh tables for
/// the automaton. The production rider creates one set per engaged
/// clause and recycles it across its seeds; a test driving a single run
/// directly needs the one-shot spelling (the reuse itself is pinned by
/// the dedicated recycling tests, never assumed here).
#[cfg(test)]
pub(crate) fn fresh_tables(nfa: &FrontierNfa) -> FrontierTables {
    FrontierTables::new(nfa.states as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index ceiling's exact boundary: every length below the
    /// packing's reserved mark-bit space indexes, the first length at
    /// it does not — the decline the runner folds into the table
    /// budget, never a panic.
    #[test]
    fn the_index_ceiling_is_the_mark_bits_boundary() {
        assert_eq!(next_pair_index(0), Some(0));
        assert_eq!(next_pair_index(NO_PAIR as usize - 1), Some(NO_PAIR - 1));
        assert_eq!(next_pair_index(NO_PAIR as usize), None);
        assert_eq!(next_pair_index(u32::MAX as usize + 1), None);
    }
}

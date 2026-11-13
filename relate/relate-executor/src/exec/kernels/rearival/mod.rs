//! The re-arrival rule (ADR-0478 Decision 3): the frontier BFS's
//! pool-exactness seam, serving both pools. For the default pool
//! (DIFFERENT RELATIONSHIPS ⇒ TRAIL) it is the trail-existence check the
//! runner consults at every accepting arrival it cannot already prove a
//! trail for: the BFS answers WALK reachability; the engine's default
//! pool answers trail existence — the two coincide unless every in-bounds
//! walk to a node reuses a relationship (the measured `knows{1,2}` SF0.1
//! case: engine-trail 855 distinct endpoints vs the frontier's 856, the
//! delta being the seed, re-reached by a rel-reusing 2-walk whose
//! shortest trail falls below `min`;
//! the frontier-NFA design's §1 rule). For the WALK
//! pool (bounded REPEATABLE ELEMENTS) the in-bounds arrivals are the
//! frontier's native semantics — no rule question — and the seam answers
//! exactly one shape for it: the below-min candidates, through the
//! walk-length check (point 4 below).
//!
//! **Remedy (a) — a bounded local trail-existence search — not (b).**
//! The design note §7.1's cheaper alternative (b), "accept a re-arrival
//! only when ≥ 2 distinct rels connect the duplicated rel's pair", is
//! unsound: it is type-blind — a same-pair rel of a type the transition
//! cannot traverse repairs nothing (`0-[:KNOWS]-1-[:KNOWS]-2` plus a
//! `LIKES` parallel on the middle pair: (b) sees the parallel and
//! admits, no in-bounds `KNOWS` trail exists, the engine rejects —
//! pinned in `rearival_tests`), and a parallel of the right type may
//! already be used elsewhere on the chain. Remedy (a) proves trails,
//! never repairs walks.
//!
//! The rule's two proofs, cheapest first:
//!
//! 1. **The representative-chain proof** (no search): the arrival's
//!    representative walk — the predecessor pair's first-arrival parent
//!    chain plus the arriving rel — is a valid accepting NFA run of the
//!    arrival's own hop count, in bounds. If its node sequence is
//!    simple, the walk is a trail: a repeated relationship forces a
//!    repeated node (a rel's second traversal departs from an endpoint
//!    the first traversal already visited), so node-simple ⇒
//!    rel-distinct. This is where the design note's "a shortest walk is
//!    always a trail" claim lives, and it is the whole story for first
//!    arrivals of the flat one-state family. It is NOT the whole story
//!    for every first arrival: a context rel sharing a type with the
//!    round it precedes (`(x)-[:KNOWS]->(m)-[:KNOWS*1..]-(y)`) admits a
//!    rel-reusing walk as a pair's FIRST arrival — measured against the
//!    engine and pinned in `rearival_tests` — so the rule runs for
//!    every accepting arrival whose representative chain is not
//!    node-simple, first or re-.
//! 2. **The bounded trail-existence search**: a distinct-rels DFS from
//!    the seed over the same NFA, seeking any accepting arrival at the
//!    target with hops in `[min_hops, max_hops]` — any length in
//!    bounds, not the arriving hop count, because the endpoint contract
//!    is "a trail exists", never "this walk is one".
//! 3. **The below-min candidates** (found by the differential corpus,
//!    not anticipated by the design note): a node whose every OBSERVED
//!    accepting arrival sits below `min` can still be the endpoint of a
//!    longer trail — one whose final hop departs from a pair the dedup
//!    filter expanded only at its own, earlier level, so the BFS never
//!    observes the in-bounds arrival at all (a self-loop at the seed:
//!    the trail `1-(loop)-1-(rel)-0` of `*2..3`, node 0 observed only
//!    at hop 1). The runner flags every below-min accepting arrival as
//!    a candidate and asks the rule once per candidate after the BFS —
//!    the same memoized, budget-shared search (no chain proof applies:
//!    the observed arrival is out of bounds by definition).
//! 4. **The walk-length check** — the WALK pool's version of the
//!    candidate rule. Walk semantics have a `min` too, and the same
//!    dedup blind spot: a self-loop at the seed hides the 2-walk
//!    `1-(loop)-1-(rel)-0` of `*2..3` from the BFS exactly as it hides
//!    the trail, so the walk pool flagged nothing and answered a wrong
//!    endpoint set (engine REPEATABLE `{0,1}`, walk pool `{1}` — the
//!    measured minimal case, pinned in `rearival_tests`). Rel reuse
//!    being legal, no trail proof applies; a candidate is admitted iff
//!    an in-bounds WALK to the node exists — a
//!    reachability-within-hop-bounds question over the same NFA. With
//!    no distinct-rels set to carry, the search's whole state is the
//!    position `(node, state, hops)`: a failed position is a proven
//!    dead end, memoized, so the search is polynomial in
//!    pairs × hop bound — strictly cheaper than the trail search, whose
//!    rel-set state forbids position memoization. The completeness
//!    argument carries over from the default pool: any in-bounds
//!    endpoint's walk has a final hop departing a pair `(u, s)` the BFS
//!    marked at a first-arrival level `j ≤ hops - 1`, and expanding
//!    that pair at level `j` fires the same accepting transition at hop
//!    `j + 1 ≤ max` — so every in-bounds endpoint has an OBSERVED
//!    accepting arrival, in-bounds (admitted outright, the pool's
//!    native semantics) or below-min (flagged as a candidate); the
//!    walk-length check then re-proves what the unobserved arrival
//!    would have shown, and admits no node without a found in-bounds
//!    walk — exact in both directions. The matcher declines unbounded
//!    REPEATABLE (ADR-0478 §5), so every production walk-pool shape is
//!    bounded and the check exact; an unbounded `run` call falls to
//!    [`REARRIVAL_DEPTH_CEILING`] and the budget corner below.
//!
//! **The bound** (the rule's whole point — never a full unbounded
//! enumeration): the chain proof is O(arrival hop count) table walks,
//! no graph access; the search is capped twice — depth at `max_hops`,
//! or [`REARRIVAL_DEPTH_CEILING`] for an unbounded quantifier (a trail
//! is at most rel-count long; 64 covers every realistic trail while the
//! engine's own unbounded default-pool cap is the rel count), and work
//! at a shared [`REARRIVAL_WORK_BUDGET`] expansion budget across the
//! whole run — a run can never spend more on the rule than that, spread
//! over as many checks as it performs. The verdict per target node is
//! memoized: the search is target-shaped, not arrival-shaped, so one
//! node is searched once however many times it is re-arrived.
//!
//! **Witnesses, not just verdicts** (the `witness-emit` node's
//! extension): every search that proves a path returns it — the found
//! branch as a [`Chain`], memoized with the verdict — because the
//! capped emission's witness for a below-min candidate, or for a
//! default-pool re-arrival the chain proof could not settle, is the
//! *path the search proved*, never the recorded arrival's
//! representative (which is exactly the chain that failed the proof).
//! The capped ACYCLIC/SIMPLE modes add the third search,
//! [`RearivalRule::mode_witness`]: node-simple (the close allowed for
//! SIMPLE) alongside rel-distinct, the per-witness mode check's own
//! bound.
//!
//! **One search, three policies.** The three bounded searches — the
//! trail-existence DFS, the walk-length check, the node-simple
//! ACYCLIC/SIMPLE ask — share their whole skeleton (the budget guard,
//! the branch reset, the start state's epsilon-closure loop, the
//! per-rel expansion, the witness clone), so the search core
//! ([`search`], the child module split from this one by the file-size
//! rule) spells that skeleton ONCE: [`SeekTables`] carries
//! the immutable tables, [`RearivalRule::search`] below drives,
//! [`seek`] is the step core, and [`SearchPolicy`] is the one axis the
//! three differ on — what state a search carries, which rels it may
//! traverse, which arrivals it may descend into, and whether a
//! fully-explored miss is a memoizable dead end. The three named
//! searches are one-line policy selections ([`TrailPolicy`],
//! [`WalkPolicy`], [`NodeSimple`]).
//!
//! **Soundness direction**: an endpoint is admitted only on a proven
//! trail — the chain proof or a search that found one. A shape the
//! bounded search cannot cover is NOT admitted — but the two covers
//! differ, and the difference is load-bearing (ADR-0516): a miss at the
//! depth ceiling is a disproof *within the bounds* (the search answers
//! "within `[min_hops, depth_cap]`", exact for every bounded
//! quantifier; a longer trail stays the documented conservative
//! corner), while a search the shared WORK budget cut is no verdict at
//! all. The verdict is therefore tri-state ([`Verdict`]): a cut search
//! memoizes as [`Verdict::Exhausted`], never as a disproof, and the
//! rule flags the run ([`RearivalRule::budget_exhausted`]) so the rider
//! wiring discards the run's buffered rows and declines the clause to
//! the next rung unchanged — the DFS answers these shapes exactly, and
//! no partial or unproven answer escapes. The silent alternative was
//! the measured bug (LABS-952): under a `*k..k` one-state cycle every
//! endpoint is a below-min candidate, the first hopeless candidate's
//! search drains the shared budget, and caching the cut as a rejection
//! emptied the whole answer.

mod search;

use relate_core::{NodeId, Result};

use crate::ctx::{FrontierCounters, REARRIVAL_WORK_BUDGET};
use crate::overlay::ExecGraph;

use self::search::{
    seek, NodeSimple, Position, SearchPolicy, SeekTables, Stop, TrailPolicy, WalkPolicy,
};
use super::frontier_tables::{epsilon_index, Arm, Chain, Parents, Step};
use super::nfa::{FrontierNfa, Pool};
use super::node_map::NodeMap;

/// The trail-existence search's depth ceiling for unbounded quantifiers
/// (`max_hops = None`): a trail is at most rel-count hops long, but the
/// engine's own unbounded default-pool cap being the rel count does not
/// make rel count a searchable bound — 64 covers every trail the
/// differential corpus and the measured classes produce, and a longer
/// one is the documented conservative rejection.
pub(crate) const REARRIVAL_DEPTH_CEILING: u32 = 64;

/// The re-arrival rule's run state: the shared tables the proofs read
/// ([`SeekTables`]), the recycled sparse buffers ([`RuleBuffers`],
/// borrowed), and the shared expansion budget. Constructed once per run
/// by the BFS runner under the run's pool; asked once per accepting
/// arrival the default pool's chain proof cannot settle, and once per
/// below-min candidate under either pool.
pub(crate) struct RearivalRule<'a, 'g> {
    /// The pool this rule instance answers in — fixed for the run, so
    /// the one verdict table serves whichever search the pool routes to
    /// without ever mixing the two.
    pool: Pool,
    /// The immutable tables every bounded search reads, one value
    /// instead of a dozen positional parameters — held by the rule,
    /// borrowed by each search.
    tables: SeekTables<'a, 'g>,
    /// The seed the run started from — every search is seed-rooted.
    seed: NodeId,
    /// The rule's sparse state, borrowed from the clause's
    /// [`RuleBuffers`] for this run's lifetime — the memoized verdicts
    /// and the chain proof's node-visit set, created once per engaged
    /// clause and recycled across its seeds, sized by the nodes the
    /// rule actually asks about (never a span-sized allocation). The
    /// buffers are per-CLAUSE storage carrying PER-RUN semantics —
    /// [`RuleBuffers::begin_run`] bumps the memo's generation, so every
    /// verdict of a previous run is stale without a sweep.
    bufs: &'a mut RuleBuffers,
    /// The search's current branch, seed-first: the rels are the
    /// distinct-rels state (`contains` on descent — the depth ceiling
    /// bounds the scan) and the whole vector is the found branch's
    /// witness, one branch state, never a set beside a list. The
    /// walk-length check carries no rel-distinctness state, only the
    /// witness side of this same shape.
    branch: Chain,
    /// The remaining shared expansion budget.
    work: u64,
    /// The budget this rule instance started with — the spent-work
    /// fraction's denominator ([`FrontierCounters::budget_fraction`]),
    /// the test seam's own value under `with_budget`.
    work_budget: u64,
    /// The run's PROFILE facts, the counters this rule accumulates: the
    /// memo's hits and misses and the admission asks by verdict — a
    /// [`FrontierCounters`] carrying only the rule's own fields, merged
    /// into the run's counters by the runner (never summed per rule
    /// instance anywhere else).
    tally: FrontierCounters,
    /// Whether any search of this run cut on the shared budget
    /// (ADR-0516) — the rider wiring's decline fact: a run with a cut
    /// search carries an unproven endpoint set, so the wiring discards
    /// its buffered rows and falls through to the next rung.
    exhausted: bool,
}

/// The rule's per-clause recycled buffers: the memoized verdict map and
/// the chain proof's node-visit set, both sparse ([`NodeMap`] — sized by
/// the nodes the rule asks about, never by the store's span), created
/// once per engaged clause and handed to each run's [`RearivalRule`] in
/// turn — the rule's half of the frontier family's allocation bound
/// (ADR-0478 Decision 6's allocation half). The reset between runs and
/// between chain walks is generational, never a sweep: [`NodeMap::clear`]
/// bumps the map's generation, so a fresh memo per run (and a fresh
/// visit set per walk) costs O(1), the per-run verdict semantics exact
/// (a verdict is seed-shaped, so a previous run's verdicts must never
/// leak into the next).
#[derive(Default)]
pub(crate) struct RuleBuffers {
    /// Memoized pool-search verdicts per target node, for the current
    /// run. A verdict is target-shaped, so it is computed once per node
    /// however many re-arrivals or candidates reach it; the per-run
    /// clear is what keeps one run's verdicts from serving the next.
    /// Since the witness reconstruction (this feature's
    /// `frontier_emit`) needs the *path* a found search proved, a found
    /// verdict carries it — a found search never runs twice.
    verdicts: NodeMap<Verdict>,
    /// The chain proof's node-visit set — cleared per walk in O(1), so
    /// a proof costs O(hops).
    visited: NodeMap<()>,
}

/// One bounded search's three-way verdict (ADR-0516): the budget corner
/// made explicit. A budget-cut search is neither a proof nor a
/// disproof — memoizing it as a rejection was the silent-wrong-answer
/// bug (LABS-952): under a `*k..k` one-state cycle every endpoint is a
/// below-min candidate, the first hopeless candidate's search drains
/// the shared budget, and caching the cut as "rejected" emptied every
/// remaining endpoint's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The search proved an in-bounds path — the witness chain it found
    /// rides (the emission's deferred asks bind it).
    Proven(Chain),
    /// The search ran to exhaustion within its bounds: a proven miss —
    /// the only verdict that rejects.
    Disproven,
    /// The shared work budget cut the search: no verdict either way.
    /// Memoized as itself, never as a disproof — a later ask under the
    /// same run reads the same honest answer (the budget is still
    /// spent), and the rule's exhaustion flag declines the run.
    Exhausted,
}

impl RuleBuffers {
    /// The empty clause-local set — no allocation until the rule's
    /// first memoized verdict or chain walk, recycled from there.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Begin one run's rule: every previous run's verdicts go stale
    /// together — a fresh memo in O(1), the map's generation bump.
    fn begin_run(&mut self) {
        self.verdicts.clear();
    }

    /// The exact bytes the buffers' maps hold — the rule's half of the
    /// table ceiling's accounting
    /// ([`super::frontier_tables::within_table_budget`]). The proven
    /// witnesses' own chain vectors are not counted: each is at most
    /// the search depth long, and their number is bounded by the
    /// rule's shared work budget.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.verdicts.heap_bytes() + self.visited.heap_bytes()
    }
}

impl<'a, 'g> RearivalRule<'a, 'g> {
    /// The rule under the production budget, over the clause's recycled
    /// [`RuleBuffers`].
    pub(crate) fn new(
        view: &'a ExecGraph<'g>,
        nfa: &'a FrontierNfa,
        arms: &'a [Vec<Arm>],
        seed: NodeId,
        pool: Pool,
        bufs: &'a mut RuleBuffers,
    ) -> Self {
        Self::with_budget(view, nfa, arms, seed, pool, REARRIVAL_WORK_BUDGET, bufs)
    }

    /// The constructor behind [`Self::new`], and the test seam: an
    /// explicit expansion budget, so a conservatism test fails a small
    /// graph fast instead of building a shape that crosses
    /// [`REARRIVAL_WORK_BUDGET`].
    pub(crate) fn with_budget(
        view: &'a ExecGraph<'g>,
        nfa: &'a FrontierNfa,
        arms: &'a [Vec<Arm>],
        seed: NodeId,
        pool: Pool,
        work: u64,
        bufs: &'a mut RuleBuffers,
    ) -> Self {
        // The per-state epsilon closure: the runner's `epsilon_index`
        // is from-indexed and expands on the fly; the search needs the
        // to-set per arrival, closed once here over that same index.
        let eps_from = epsilon_index(nfa);
        let eps_closure = (0..nfa.states)
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
            .collect::<Vec<_>>();
        // The borrowed buffers' fresh run: one generation bump, every
        // previous run's verdicts stale together.
        bufs.begin_run();
        Self {
            pool,
            tables: SeekTables {
                view,
                nfa,
                arms,
                eps_closure,
                min_hops: nfa.min_hops,
                depth_cap: nfa.max_hops.unwrap_or(REARRIVAL_DEPTH_CEILING),
            },
            seed,
            bufs,
            branch: Chain {
                nodes: vec![seed],
                rels: Vec::new(),
            },
            work,
            work_budget: work,
            tally: FrontierCounters::default(),
            exhausted: false,
        }
    }

    /// Whether any search of this run cut on the shared work budget —
    /// read once by the runner when the run's tables are packaged
    /// (ADR-0516): the endpoint set of an exhausted run is unproven, so
    /// the rider wiring declines the clause instead of shipping it.
    pub(crate) fn budget_exhausted(&self) -> bool {
        self.exhausted
    }

    /// The rule's PROFILE facts for the run that owned it — the runner
    /// merges them into the run's [`FrontierCounters`]: the memo's hits
    /// and misses, the admission asks by verdict, and the work spent
    /// (the budget this rule started with minus what remains — a cut
    /// search drains it to exactly zero).
    pub(crate) fn counters(&self) -> FrontierCounters {
        let mut tally = self.tally;
        tally.rearrival_work = self.work_budget - self.work;
        tally
    }

    /// The exact bytes the rule's borrowed buffers hold — the runner's
    /// level-boundary ceiling check reads it beside the tables' own.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.bufs.heap_bytes()
    }

    /// Whether the accepting arrival of `arrival`, expanded from the
    /// pair `from`, is trail-admissible — the default pool's question;
    /// the walk pool's in-bounds arrivals are its native semantics,
    /// admitted without a rule question, so the runner never asks this
    /// under [`Pool::Walk`]. The caller guarantees the arrival's
    /// transition accepts and its hop count is in bounds — the
    /// representative walk is then a valid accepting run of exactly
    /// that hop count, so a node-simple chain settles the question
    /// without a search; otherwise the memoized bounded search answers.
    /// The answer is admission only (ADR-0516): a disproof is the one
    /// rejection, and a budget cut neither admits nor rejects — the cut
    /// surfaces through the rule's exhaustion flag
    /// ([`Self::budget_exhausted`]), which declines the run, so no
    /// answer ever escapes on an unproven arrival.
    pub(crate) fn admits(
        &mut self,
        parents: &Parents<'_>,
        from: (NodeId, u32),
        arrival: NodeId,
    ) -> Result<bool> {
        if self.representative_is_trail(parents, from, arrival) {
            self.tally.representative_proofs += 1;
            Ok(true)
        } else {
            Ok(matches!(self.pool_witness(arrival)?, Verdict::Proven(_)))
        }
    }

    /// The pool's own witness for `target` — the path its search proved,
    /// memoized per target: the trail-existence search's found trail
    /// under the default pool ([`TrailPolicy`]), the walk-length
    /// check's found walk under the walk pool ([`WalkPolicy`]). The
    /// answer is three-way (ADR-0516): [`Verdict::Proven`] carries the
    /// path, [`Verdict::Disproven`] is the pool's rejection (never an
    /// admission without a path), and [`Verdict::Exhausted`] is the
    /// budget cut — no verdict, and the run declines rather than
    /// answering on it. The witness reconstruction's ask for a
    /// below-min candidate the runner admitted, and for a default-pool
    /// re-arrival the chain proof could not settle: the recorded
    /// arrival's representative is not the proven path, so the search's
    /// own answer is the witness. The runner's below-min candidate pass
    /// asks the same question (a [`Verdict::Proven`] admits).
    ///
    /// One search per node, however many arrivals or candidates name it
    /// — the pool's own search, routed once here, carrying the found
    /// witness. A verdict counts only within this run (the memo's
    /// per-run clear); a previous run's (a different seed's — verdicts
    /// are seed-shaped) is gone and the search runs anew. The memoized
    /// value is the search's own three-way answer (ADR-0516): a
    /// budget-cut search caches as [`Verdict::Exhausted`], so the memo
    /// can never later reject on a search that proved nothing. A guard's
    /// evaluation error is no verdict: it is raised, never memoized —
    /// the run aborts on it, exactly as the walker does.
    pub(crate) fn pool_witness(&mut self, target: NodeId) -> Result<Verdict> {
        // The memoized answer first: a searched target never runs twice.
        if let Some(verdict) = self.bufs.verdicts.get(target) {
            let verdict = verdict.clone();
            self.tally.memo_hits += 1;
            self.count_verdict(&verdict);
            return Ok(verdict);
        }
        self.tally.memo_misses += 1;
        let verdict = match self.pool {
            Pool::Default => self.search::<TrailPolicy>(target)?,
            Pool::Walk => self.search::<WalkPolicy>(target)?,
        };
        self.count_verdict(&verdict);
        self.bufs.verdicts.insert(target, verdict.clone());
        Ok(verdict)
    }

    /// One ask's verdict onto the tally — the admission verdicts the
    /// PROFILE record reports (representative-proof / search-proven /
    /// search-disproven / declined-exhausted), the one counting point
    /// every `pool_witness` ask shares, memo hit or miss alike.
    fn count_verdict(&mut self, verdict: &Verdict) {
        match verdict {
            Verdict::Proven(_) => self.tally.search_proven += 1,
            Verdict::Disproven => self.tally.search_disproven += 1,
            Verdict::Exhausted => self.tally.declined_exhausted += 1,
        }
    }

    /// A per-witness mode witness (the capped ACYCLIC/SIMPLE emission's
    /// deferred ask): a bounded search for an in-bounds path to
    /// `target` whose NODE sequence satisfies the mode — every node
    /// distinct, with `CLOSE = true` admitting the SIMPLE mode's one
    /// exception, a final hop back to the seed (the closed cycle) —
    /// and whose rels stay distinct (the DFS's own walk enforces
    /// rel-uniqueness for every trail-enforcing mode, and the close can
    /// repeat the seed node over a rel the path already used). The
    /// answer is three-way (ADR-0516): [`Verdict::Disproven`] is "no
    /// such path within the bounds" — exact when the search ran to
    /// exhaustion, the documented conservative corner when the depth
    /// ceiling cut it — and [`Verdict::Exhausted`] is the budget cut,
    /// which declines the run rather than dropping the row. Not
    /// memoized: each deferred endpoint is asked once by the emission,
    /// so there is no repeated ask to collapse.
    pub(crate) fn mode_witness<const CLOSE: bool>(&mut self, target: NodeId) -> Result<Verdict> {
        self.search::<NodeSimple<CLOSE>>(target)
    }

    /// The one bounded-search driver every pool shares: the budget
    /// guard, the branch reset (the policy's fresh state beside it),
    /// the start state's epsilon-closure loop, and the found branch's
    /// witness clone. The three named searches are one-line policy
    /// selections over it ([`TrailPolicy`], [`WalkPolicy`], and
    /// [`NodeSimple`] through [`Self::mode_witness`]). The step
    /// core's channel maps onto the verdict here, exactly once
    /// (ADR-0516): a found path (the branch holds the witness), a
    /// fully-explored miss, or the budget's cut — a cut flags the run
    /// ([`Self::budget_exhausted`]) and answers [`Verdict::Exhausted`],
    /// never a miss. A guard's evaluation error ([`Stop::Failed`]) is
    /// raised as the `Err`, never mapped onto a verdict.
    fn search<P: SearchPolicy>(&mut self, target: NodeId) -> Result<Verdict> {
        if self.work == 0 {
            // The budget's end before any step: the search's answer is
            // the cut, never a miss — and the run it happened in is
            // flagged for the rider wiring's decline.
            self.exhausted = true;
            return Ok(Verdict::Exhausted);
        }
        let mut policy = P::init(self.seed);
        self.branch.rels.clear();
        self.branch.nodes.truncate(1);
        for &state in &self.tables.eps_closure[self.tables.nfa.start as usize] {
            if !self.tables.state_admits(state, self.seed)? {
                // A start position whose guard rejects the seed is a
                // position the walk never stands at — the runner's own
                // level-0 closure check, mirrored here.
                continue;
            }
            let start = Position {
                node: self.seed,
                state,
                hops: 0,
            };
            match seek::<P>(
                &self.tables,
                &mut policy,
                &mut self.branch,
                &mut self.work,
                target,
                start,
            ) {
                Err(Stop::Found) => return Ok(Verdict::Proven(self.branch.clone())),
                Ok(()) => {}
                // The budget's end: no verdict, and the run it happened
                // in is flagged for the rider wiring's decline.
                Err(Stop::Exhausted) => {
                    self.exhausted = true;
                    return Ok(Verdict::Exhausted);
                }
                // A guard's evaluation error: the walker's own abort.
                Err(Stop::Failed(error)) => return Err(error),
            }
        }
        Ok(Verdict::Disproven)
    }

    /// The chain proof: the arrival's representative walk (the
    /// predecessor pair's parent chain plus the arriving rel) is
    /// node-simple — hence rel-distinct, hence a trail. A repeated
    /// relationship forces a repeated node: the second traversal of a
    /// rel departs from an endpoint the first traversal already
    /// visited. O(hops) parent-table walks, no graph access.
    fn representative_is_trail(
        &mut self,
        parents: &Parents<'_>,
        from: (NodeId, u32),
        arrival: NodeId,
    ) -> bool {
        // A fresh visit set per walk: O(1), the map's generation bump.
        // `mark` answers whether the visit is new.
        self.bufs.visited.clear();
        let mark = |node: NodeId, visited: &mut NodeMap<()>| visited.touch(node, || ()).1;
        let (mut node, mut state) = from;
        if !mark(node, &mut self.bufs.visited) || !mark(arrival, &mut self.bufs.visited) {
            return false;
        }
        while let Some(parent) = parents.parent_of(node, state) {
            match parent.step {
                // A rel step moves to the predecessor node — a revisit
                // there is the repeat the proof exists to catch. The
                // rel id rides the step but the proof needs only the
                // node (a repeated rel forces a repeated node).
                Step::Rel(_) => {
                    if !mark(parent.node, &mut self.bufs.visited) {
                        return false;
                    }
                }
                // An epsilon skip stays on the node — no visit, no
                // repeat (the same node is already marked).
                Step::Epsilon => {}
            }
            node = parent.node;
            state = parent.state;
        }
        true
    }
}

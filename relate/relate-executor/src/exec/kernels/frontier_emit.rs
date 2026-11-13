//! Witness reconstruction and capped emission (ADR-0478 Decision 1's
//! output-sensitive half, Decision 4's row order): the reader of the
//! BFS's parent+rel tables that reconstructs at most `cap` witness
//! chains and binds, for each, the row shapes the per-row DFS emits —
//! the path variable (`Value::from_path` over the chain's nodes and
//! rels, the `gpm/emit.rs` rider's own construction), the group lists
//! (the rel list of a flat spine, the per-round node and rel lists of
//! a group spine; a subpath variable declines upstream at the NFA
//! compile, so no subpath list ever reaches here), the context rel and
//! node bindings, the boundary bindings, and the used-rel set the capped
//! emission carries.
//!
//! The emission is **per-level** (the design note §5's two-phase
//! option, the one taken): the BFS runner is driven with a halt probe
//! that reconstructs witnesses as levels complete, so a cap fills from
//! shallow witnesses without visiting deeper levels — reconstruction
//! cost is `cap × depth` after the levels the cap needed, never per
//! path (pinned by the output-sensitivity test: a complete graph whose
//! trail count is factorial while the run's edge visits stay the
//! degree sum).
//!
//! **Per-witness mode checks** — the `gpm_path_mode_allows` table
//! itself, reused verbatim, never a second vocabulary — run on every
//! reconstructed witness before it is emitted:
//!
//! - the **WALK pool** checks nothing (rel and node reuse are its
//!   semantics);
//! - the **default pool** answers TRAIL rows — an unwritten mode is the
//!   DIFFERENT RELATIONSHIPS pool, and an explicit `WALK` prefix under
//!   it keeps the match mode's rel-uniqueness (the engine's own
//!   `enforce_unique` fold) — so every default-pool spelling checks
//!   rel-distinctness; the representative chain of a chain-proof
//!   admission is node-simple, hence rel-distinct (the re-arrival
//!   rule's argument), and each chain is verified rather than trusted;
//! - **ACYCLIC** checks node-distinctness and **SIMPLE** checks it too,
//!   with the one exception a final close back to the seed — the
//!   per-witness cycle checks the capped emission owes the matcher
//!   contract (capped ACYCLIC/SIMPLE reach the frontier; declining is
//!   not an available answer).
//!
//! **Deferred endpoints** — a witness that fails the in-bounds or mode
//! check is not dropped silently, it is deferred to the post-BFS pass
//! and re-asked through the re-arrival rule's own searches, which
//! return their found path as the witness: a below-min candidate's
//! witness is the pool search's path (never the recorded arrival's
//! representative, which is exactly the out-of-bounds chain), a
//! search-admitted default-pool arrival's witness is the trail the
//! search proved, and a capped ACYCLIC/SIMPLE deferral takes the
//! node-simple search's path. This is what keeps the row count exact:
//! every endpoint the runner admitted can emit a row, through its own
//! chain or the rule's.
//!
//! **The early-halt rule under candidates (the pinned decision)**: the
//! BFS halts the moment `want` witnesses are *accepted from observed
//! in-bounds arrivals* — checked after the level-0 setup and after
//! every level. Below-min candidates never count toward the cap and
//! never block it: an early halt drops them (and the runner skips the
//! candidate pass whole), which is exactly the MatchCap rider's own
//! semantics — the DFS's cap halts mid-enumeration and drops every
//! path it never enumerated (ADR-0475), and a filled cap is a
//! contract-free prefix regardless of what the candidates would have
//! added (ADR-0478 Decision 4). When the BFS instead ends naturally —
//! an empty level or `max` — the candidates are answered (the runner's
//! post-BFS pass) and their witnesses fill a still-short cap.
//!
//! **Row order and multiplicity (Decision 4, restated where it
//! bites)**: LIMIT without ORDER BY has no order contract — the
//! emission offers witnesses in endpoint-arrival order (deferred
//! endpoints after the direct ones, in their own arrival order), a
//! DFS-order approximation, not a promise. One witness is reconstructed
//! per endpoint: the pair-level answer the frontier serves. An endpoint
//! reached by many distinct paths contributes one row, so the first-cap
//! row *set* can differ from the DFS's under path multiplicity — both
//! valid, both contract-free prefixes, the documented consequence; the
//! differential suite (the feature's later node) pins the divergence
//! classes this leaves.

use crate::UsedSet;
use std::cell::Cell;
use std::collections::BTreeMap;

use relate_core::{Node, NodeId, Path, RelId, Relationship, Result, Value};
use relate_ir::{PathMode, PathPattern, PatternElement, QuantifiedPathPattern};

use crate::ctx::{ExecCtx, FrontierCounters};
use crate::frontier_decline::FrontierRunDecline;
use crate::pattern_walk::{bind_path_names, memoized_path_rung};
use crate::row::{Row, RowRung};

use super::super::pattern_policies::{gpm_group_names, gpm_path_mode_allows, Matched};
use super::frontier::{run_with_halt, FrontierRun, RunOutcome, WalkBudget};
use super::frontier_match::FrontierContract;
use super::frontier_tables::{Arrival, Chain, Endpoint, FrontierTables, Parents, Step};
use super::nfa::{FrontierNfa, Pool};
use super::program::{Admission, AdmissionPrepared, NodePred, NodeTarget};
use super::rearival::{RearivalRule, RuleBuffers, Verdict};

/// The capped emission's paired answer (ADR-0516): the accepted rows
/// and the served run that carried them — one fact, never a tuple
/// whose halves a caller could split and drop one of. The decline owns
/// neither half: no run, no rows, only the counters (the record pass's
/// input) and the typed reason — the emission's accepted buffer drops
/// with the run, so no consumer can emit a row the rule never proved,
/// and no caller can reach a row beside a decline.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // the run rides unboxed, like the outcome's own served arm — the row list's 24 bytes trip the lint's threshold, not a real size problem
pub(crate) enum CappedRun {
    /// The proven rows and the run that carried them (its tables for
    /// the caller's recycle, its facts for the record pass).
    Served {
        /// The accepted witness rows, in offer order.
        matched: Matched,
        /// The served run — the caller's recycle and PROFILE wiring.
        run: FrontierRun,
    },
    /// The run-time decline (either half's budget cut): the counters
    /// alone and the typed reason.
    Declined {
        /// The declined run's counters — the record pass's input.
        facts: FrontierCounters,
        /// The decline's typed reason: one of the two run-time declines
        /// (ADR-0516's re-arrival budget, or ADR-0521's table ceiling).
        reason: FrontierRunDecline,
    },
}

impl CappedRun {
    /// The answer's PROFILE facts, either arm — the record pass's one
    /// input (a declined run still walked; its counters still count),
    /// the outcome reader's own spelling.
    pub(crate) fn facts(&self) -> FrontierCounters {
        match self {
            CappedRun::Served { run, .. } => run.facts(),
            CappedRun::Declined { facts, .. } => *facts,
        }
    }
}

/// The capped consumer's emission: run the frontier BFS from `seed`
/// under the contract's pool, reconstructing witnesses per level and
/// halting at the filling level, and return at most `want` rows — each
/// a complete binding of the row shapes the DFS emits, with its
/// used-rel set (the capped emission's carrier) — with the served run
/// that carried them, as [`CappedRun`]: the rows and the run one
/// paired answer, so a decline (which owns neither half) can never
/// leave a caller holding half of it (the caller's PROFILE/stat wiring
/// and the decline fall-through read the same variant). `want` is the
/// caller's remaining cap — the rider wiring's per-seed spelling of the
/// consumer's shrinking want, so the contract itself is shared by every
/// seed, never copied. `entering` is the clause's entering row (the
/// answer-shape gate binds at most the seed-side variable); its seed
/// binding may be a scan stub, and the emission's own boundary bindings
/// are the full node values the projection would refresh it to.
///
/// `tables` and `bufs` are the clause's recycled table set (the
/// allocation bound, `frontier`'s module docs): a served run carries
/// the tables back out for the caller's recycle, and the deferred
/// pass's rule reuses the buffers under a fresh run stamp. A declined
/// outcome (ADR-0516, a budget cut in either half) carries no run and
/// no rows: the emission's accepted buffer drops with the run, so no
/// consumer can emit a row the rule never proved.
///
/// The other consumers never call here: the EXISTS verdict and the
/// distinct-endpoint answer read the run's endpoints directly (their
/// own rider wiring).
pub(crate) fn capped_rows(
    ctx: ExecCtx<'_>,
    seed: NodeId,
    contract: &FrontierContract<'_>,
    entering: &Row,
    want: usize,
    tables: &mut FrontierTables,
    bufs: &mut RuleBuffers,
) -> Result<CappedRun> {
    let pool = contract.pool;
    let mut emission = Emission {
        ctx,
        seed,
        pattern: contract.pattern,
        nfa: &contract.nfa,
        target: &contract.target,
        pool,
        mode: contract.pattern.mode,
        min_hops: contract.nfa.min_hops,
        max_hops: contract.nfa.max_hops,
        trailing: trailing_hops(contract.pattern),
        want,
        entering: entering.clone(),
        accepted: Vec::new(),
        cursor: 0,
        deferred: Vec::new(),
        // the emission's path-name rung memo (see the field): empty
        // until the first bound witness fills it
        path_rung: Cell::new(None),
    };
    let outcome = run_with_halt(
        ctx,
        seed,
        &contract.nfa,
        WalkBudget::standard(),
        pool,
        tables,
        bufs,
        |parents: &Parents<'_>, endpoints: &[Endpoint]| emission.on_level(parents, endpoints),
    )?;
    // The deferred pass's own rule shares the verdict (ADR-0516): a
    // budget cut in either half declines the run, and a declined run
    // hands back no rows at all — the emission's accepted buffer drops
    // with it, so no consumer can emit a row the rule never proved. A
    // declined run runs no deferred pass either: its endpoint set is
    // unproven, there is nothing left to re-ask through the rule.
    Ok(match outcome {
        RunOutcome::Served(walked) => {
            let mut walked = walked;
            match emission.finish(&walked, bufs)? {
                PassBudget::Held(deferred) => {
                    // The deferred pass's own tally rides the served run:
                    // its asks are work the record pass must count, and
                    // the walk fields it never touches are zero.
                    walked.rule += deferred;
                    CappedRun::Served {
                        matched: emission.accepted,
                        run: walked,
                    }
                }
                PassBudget::Cut(deferred) => {
                    // The rule that cut is the deferred pass's own — its
                    // tally, not the runner's, rides the decline.
                    walked.rule += deferred;
                    CappedRun::Declined {
                        facts: walked.facts(),
                        reason: FrontierRunDecline::RearrivalBudget,
                    }
                }
            }
        }
        RunOutcome::Declined { facts, reason } => CappedRun::Declined { facts, reason },
    })
}

/// The emission's running state: the per-level pass's accumulator. The
/// cursor and the deferred list make the pass idempotent across asks —
/// every endpoint is decided exactly once, at the first level boundary
/// after its arrival (its chain is level-stable: the parents of a
/// first arrival are written before the arrival's own level).
struct Emission<'a> {
    ctx: ExecCtx<'a>,
    seed: NodeId,
    pattern: &'a PathPattern,
    nfa: &'a FrontierNfa,
    target: &'a NodeTarget<'a>,
    pool: Pool,
    mode: Option<PathMode>,
    min_hops: u32,
    max_hops: Option<u32>,
    /// The top-level rel hops after the quantified spine — the reader
    /// the round count needs (`hops - leading - trailing` over the
    /// spine's fixed round length).
    trailing: usize,
    want: usize,
    entering: Row,
    /// The accepted rows, in offer order.
    accepted: Matched,
    /// How many of the run's endpoints the per-level pass has decided.
    cursor: usize,
    /// Endpoints whose own chain is not a witness (below-min
    /// candidates, mode-check failures) — the post-BFS pass re-asks
    /// them through the rule's searches.
    deferred: Vec<Endpoint>,
    /// The witness rows' path-name rung memo — built from the first
    /// bound witness's row (every witness binds the same names against
    /// the one entering row), one per emission, stamped by every later
    /// witness.
    path_rung: Cell<Option<RowRung>>,
}

/// The deferred pass's budget answer (ADR-0516): whether the pass's own
/// rule finished every ask it made or cut on the shared work budget —
/// the run outcome's second decline source beside the runner's own
/// rule, never a silently dropped row. Either arm carries the pass
/// rule's own tally ([`RearivalRule::counters`]): the runner's instance
/// proved the admissions, this one answered the deferrals, and the
/// record pass counts both — the caller merges it into the run's
/// counters, where a bare flag would leave the pass's work invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PassBudget {
    /// Every deferred ask the pass made ran to its verdict.
    Held(FrontierCounters),
    /// A deferred ask cut on the shared work budget: no verdict — the
    /// run declines, the caller builds the declined outcome from this.
    Cut(FrontierCounters),
}

impl<'a> Emission<'a> {
    /// One level boundary: decide every undecided endpoint through the
    /// target's admission and its own chain, in arrival order, while
    /// the cap has room. Whether the cap is full — the runner's halt
    /// verdict.
    fn on_level(&mut self, parents: &Parents<'_>, endpoints: &[Endpoint]) -> Result<bool> {
        self.level_pass(Admission::of(self.target), parents, endpoints)
    }

    /// The per-level pass over the target's admission — the wrapper
    /// predicate the kernels share (Admission's own NodePred impl,
    /// `program.rs`), every check a statically dispatched call.
    fn level_pass(
        &mut self,
        pred: Admission<'_>,
        parents: &Parents<'_>,
        endpoints: &[Endpoint],
    ) -> Result<bool> {
        let mut prepared: Option<AdmissionPrepared<'_>> = None;
        while self.cursor < endpoints.len() && self.accepted.len() < self.want {
            let endpoint = endpoints[self.cursor];
            self.cursor += 1;
            if !pred.admits(&mut prepared, self.ctx, endpoint.node)? {
                continue;
            }
            match self.chain_witness(parents, &endpoint)? {
                Some(bound) => self.accepted.push(bound),
                None => self.deferred.push(endpoint),
            }
        }
        Ok(self.accepted.len() >= self.want)
    }

    /// One endpoint's own chain as a witness: the reconstructed chain,
    /// in bounds, mode-allowed, and bound to its row. `None` defers —
    /// the chain is out of bounds (a below-min candidate the runner
    /// admitted through the rule's search) or fails the mode check (a
    /// search-admitted arrival whose representative is not the proven
    /// path) — the post-BFS pass re-asks it.
    fn chain_witness(
        &self,
        parents: &Parents<'_>,
        endpoint: &Endpoint,
    ) -> Result<Option<(Row, UsedSet)>> {
        let chain = chain_of(parents, self.seed, endpoint);
        if !self.in_bounds(&chain) {
            return Ok(None);
        }
        let records = self.chain_records(&chain);
        if !self.mode_allows(&records) {
            return Ok(None);
        }
        Ok(Some(self.bind_row(&chain, &records)?))
    }

    /// The post-BFS pass: decide any endpoint the per-level asks never
    /// saw (the runner's candidate pass appends them after the last
    /// boundary), then answer the deferred endpoints through the rule's
    /// searches until the cap fills. An early-halted run has its cap
    /// full and both passes are no-ops — its candidates were dropped at
    /// the halt, the cap rider's own semantics. The pass's own budget
    /// answer rides the return (ADR-0516): a cut declines the run (the
    /// caller builds the declined outcome from it), never a silently
    /// dropped row.
    fn finish(&mut self, walked: &FrontierRun, bufs: &mut RuleBuffers) -> Result<PassBudget> {
        self.on_level(&walked.parents(), &walked.endpoints)?;
        if self.accepted.len() >= self.want || self.deferred.is_empty() {
            // No deferred ask ran — a zero tally, the pass rule's own
            // fields at their defaults.
            return Ok(PassBudget::Held(FrontierCounters::default()));
        }
        // The rule instance for the deferred asks: run-scoped like the
        // runner's own (its searches answer these asks through the same
        // bounded, budget-shared machinery, memoized within the
        // emission — the runner's instance proved the admissions, this
        // one returns the paths) — the clause's recycled buffers under
        // a fresh run stamp, a fresh memo and budget exactly as a fresh
        // instance would carry. The arms ride the run itself — the
        // one name resolution per run, never rebuilt here.
        let view = self.ctx.graph;
        #[cfg(test)]
        let budget =
            super::rearival_deferred_budget_override().unwrap_or(crate::ctx::REARRIVAL_WORK_BUDGET);
        #[cfg(not(test))]
        let budget = crate::ctx::REARRIVAL_WORK_BUDGET;
        let mut rule = RearivalRule::with_budget(
            view,
            self.nfa,
            &walked.arms,
            self.seed,
            self.pool,
            budget,
            bufs,
        );
        for endpoint in std::mem::take(&mut self.deferred) {
            if self.accepted.len() >= self.want {
                break;
            }
            let Some(chain) = self.deferred_witness(&mut rule, endpoint.node)? else {
                continue;
            };
            if !self.in_bounds(&chain) {
                continue;
            }
            let records = self.chain_records(&chain);
            if !self.mode_allows(&records) {
                continue;
            }
            self.accepted.push(self.bind_row(&chain, &records)?);
        }
        Ok(if rule.budget_exhausted() {
            PassBudget::Cut(rule.counters())
        } else {
            PassBudget::Held(rule.counters())
        })
    }

    /// A deferred endpoint's witness — the rule's own search for the
    /// check the emission runs (the one table both readers share):
    /// the walk pool's below-min candidates take the walk-length
    /// check's path; the default pool's take the trail search's path;
    /// the capped ACYCLIC/SIMPLE deferrals take the node-simple
    /// search's, closed for SIMPLE. A disproof defers nothing; a budget
    /// cut surfaces through the rule's exhaustion flag (ADR-0516) —
    /// `finish` reports it, the run declines, and no row is silently
    /// dropped here. A guard's evaluation error is raised, never a
    /// disproof.
    fn deferred_witness(
        &mut self,
        rule: &mut RearivalRule<'_, '_>,
        target: NodeId,
    ) -> Result<Option<Chain>> {
        let verdict = match self.witness_mode() {
            WitnessMode::Walk | WitnessMode::Trail => rule.pool_witness(target)?,
            WitnessMode::Acyclic => rule.mode_witness::<false>(target)?,
            WitnessMode::Simple => rule.mode_witness::<true>(target)?,
        };
        Ok(match verdict {
            Verdict::Proven(chain) => Some(chain),
            Verdict::Disproven | Verdict::Exhausted => None,
        })
    }

    /// The per-witness check a reconstructed chain runs, and the
    /// deferred ask's search kind — one table, both readers, derived
    /// from the pool and the written mode: the WALK pool checks
    /// nothing; the default pool answers TRAIL rows (an unwritten mode
    /// is the DIFFERENT RELATIONSHIPS pool, and an explicit `WALK`
    /// prefix under it keeps the match mode's rel-uniqueness — the
    /// engine's own `enforce_unique` fold), tightened by a written
    /// ACYCLIC/SIMPLE.
    fn witness_mode(&self) -> WitnessMode {
        match self.pool {
            Pool::Walk => WitnessMode::Walk,
            Pool::Default => match self.mode {
                Some(PathMode::Acyclic) => WitnessMode::Acyclic,
                Some(PathMode::Simple) => WitnessMode::Simple,
                Some(PathMode::Trail) | None | Some(PathMode::Walk) => WitnessMode::Trail,
            },
        }
    }

    /// Whether the chain's hop count is inside the run's bounds.
    fn in_bounds(&self, chain: &Chain) -> bool {
        let hops = chain.rels.len() as u32;
        hops >= self.min_hops && self.max_hops.is_none_or(|max| hops <= max)
    }

    /// The per-witness mode check — [`gpm_path_mode_allows`] itself,
    /// over the once-materialized chain records: the one table the
    /// DFS's own per-candidate pipeline applies, so an emitted row is a
    /// row the DFS's pipeline would have kept, by the same predicate.
    /// The WALK pool's arm reads no records (rel and node reuse are its
    /// semantics — nothing to check, nothing to materialize for the
    /// check; the binding is the records' only reader). SIMPLE takes
    /// the table twice: its own row is nodes-only (the close may
    /// repeat the seed), and the DFS's walk enforces rel-uniqueness for
    /// every trail-enforcing mode (`enforce_unique`) — the close may
    /// repeat the seed node, never a rel — which is the table's TRAIL
    /// row, the one canonical rel-distinctness predicate.
    fn mode_allows(&self, records: &ChainRecords) -> bool {
        match self.witness_mode() {
            WitnessMode::Walk => true,
            WitnessMode::Trail => {
                gpm_path_mode_allows(Some(PathMode::Trail), records.node_ids(), records.rel_ids())
            }
            WitnessMode::Acyclic => gpm_path_mode_allows(
                Some(PathMode::Acyclic),
                records.node_ids(),
                records.rel_ids(),
            ),
            WitnessMode::Simple => {
                gpm_path_mode_allows(Some(PathMode::Trail), records.node_ids(), records.rel_ids())
                    && gpm_path_mode_allows(
                        Some(PathMode::Simple),
                        records.node_ids(),
                        records.rel_ids(),
                    )
            }
        }
    }

    /// The chain's materialized records — full node and rel values (the
    /// values the projection's stub refresh would produce), read once
    /// per witness by the mode check and the row binding alike, never
    /// twice.
    fn chain_records(&self, chain: &Chain) -> ChainRecords {
        ChainRecords {
            nodes: chain
                .nodes
                .iter()
                .map(|&id| {
                    self.ctx
                        .graph
                        .node(id)
                        .expect("a chain node is live under the view that walked it")
                })
                .collect(),
            rels: chain
                .rels
                .iter()
                .map(|&rid| {
                    self.ctx
                        .graph
                        .rel(rid)
                        .expect("a chain rel is live under the view that walked it")
                })
                .collect(),
        }
    }

    /// Bind one witness chain to its row — every binding the DFS's
    /// complete state carries: the boundary pair, the interior context
    /// nodes, the context rels, the spine's group lists, the pattern's
    /// path names (the outer variable and each named parenthesized
    /// subpath), and the used-rel set. The returned row is the row the
    /// materializing walk would push, minus the order it would push it
    /// in (Decision 4).
    fn bind_row(&self, chain: &Chain, records: &ChainRecords) -> Result<(Row, UsedSet)> {
        let nodes = &records.nodes;
        let rels = &records.rels;
        let mut row = self.entering.clone();
        // The element walk: a rel element consumes one hop, a node
        // element binds the node its position reaches — `pos` hops in,
        // `nodes[pos]` — and the one quantified element consumes its
        // rounds of `L` hops while accumulating the group lists. The
        // spine is a fixed-length plain-rel chain (the matcher's spine
        // gate), so the hop count alone decomposes the chain.
        let hops = chain.rels.len();
        let mut pos = 0usize;
        for element in &self.pattern.elements {
            match element {
                PatternElement::Node(node) => {
                    if let Some(name) = &node.variable {
                        // The seed-side binding may ride the entering
                        // row as a scan stub — the single-seed premise
                        // makes the overwrite the same node's full
                        // value.
                        // Text render: the row schema's names are String.
                        row.insert(name.to_string(), Value::from_node(nodes[pos].clone()));
                    }
                }
                PatternElement::Relationship(rel) => {
                    if let Some(name) = &rel.variable {
                        row.insert(
                            name.to_string(),
                            Value::from_relationship(rels[pos].clone()),
                        );
                    }
                    pos += 1;
                }
                PatternElement::Quantified(qpp) => {
                    let round_len = round_len(qpp);
                    let rounds = (hops - pos - self.trailing) / round_len;
                    bind_groups(&mut row, qpp, nodes, rels, pos, rounds);
                    pos += rounds * round_len;
                }
            }
        }
        // the one path-name binding spelling (pattern_walk) — every
        // name the pattern introduces (the outer variable, a named
        // parenthesized subpath) gets the whole path's value; a pattern
        // that binds no path name pays nothing. The append stamps the
        // emission's memoized rung (pattern_walk's
        // `memoized_path_rung`): every witness binds the same names
        // against the one entering row.
        let rung = memoized_path_rung(&self.path_rung, &row, self.pattern);
        bind_path_names(&mut row, self.pattern, &rung, || {
            Value::from_path(Path {
                nodes: nodes.clone(),
                rels: rels.clone(),
            })
        });
        Ok((row, chain.rels.iter().copied().collect()))
    }
}

/// One witness chain's materialized records: the full node and rel
/// values, materialized once per witness and shared by the per-witness
/// mode check and the row binding — never twice per chain.
struct ChainRecords {
    nodes: Vec<Node>,
    rels: Vec<Relationship>,
}

impl ChainRecords {
    /// The records' node ids, in chain order — the mode check's view.
    fn node_ids(&self) -> impl ExactSizeIterator<Item = NodeId> + Clone + '_ {
        self.nodes.iter().map(|node| node.id)
    }

    /// The records' rel ids, in chain order — the mode check's view.
    fn rel_ids(&self) -> impl Iterator<Item = RelId> + '_ {
        self.rels.iter().map(|rel| rel.id)
    }
}

/// The per-witness check's own vocabulary — [`gpm_path_mode_allows`]'s
/// modes plus the pool's no-check arm, so the check and the deferred
/// ask read one table.
enum WitnessMode {
    /// No check: the WALK pool. A deferred endpoint is a below-min
    /// candidate, answered by the walk-length check's path.
    Walk,
    /// Rel-distinct: every default-pool spelling (the unwritten mode
    /// is the DIFFERENT RELATIONSHIPS pool; a `WALK` prefix under it
    /// keeps the match mode's rel-uniqueness; `TRAIL` writes it).
    Trail,
    /// Node-distinct (the capped ACYCLIC emission).
    Acyclic,
    /// Node-distinct except a final close back to the seed (the capped
    /// SIMPLE emission).
    Simple,
}

/// The quantified spine's hop count per round: its inner chain is
/// node/rel-alternating plain rels (the matcher's spine gate), so
/// `(len - 1) / 2` hops close each round.
fn round_len(qpp: &QuantifiedPathPattern) -> usize {
    (qpp.elements.len() - 1) / 2
}

/// The top-level rel hops after the pattern's quantified spine — the
/// trailing context the round count subtracts. Zero when the spine is
/// the pattern's last hop position.
fn trailing_hops(pattern: &PathPattern) -> usize {
    pattern
        .elements
        .iter()
        .skip_while(|element| !matches!(element, PatternElement::Quantified(_)))
        .skip(1)
        .filter(|element| matches!(element, PatternElement::Relationship(_)))
        .count()
}

/// The spine's group lists, one value per named inner element per
/// round, in round order — the rel list of a flat spine (`L = 1`), the
/// per-round node and rel lists of a group spine. A subpath variable
/// declines upstream at the NFA compile, so no subpath list exists to
/// bind. `rounds` is the caller's decomposition (the hop count's, the
/// one spelling of that fact).
fn bind_groups(
    row: &mut Row,
    qpp: &QuantifiedPathPattern,
    nodes: &[Node],
    rels: &[Relationship],
    pos: usize,
    rounds: usize,
) {
    let mut names: Vec<String> = Vec::new();
    gpm_group_names(&qpp.elements, &mut names);
    let mut lists: BTreeMap<&str, Vec<Value>> = names
        .iter()
        .map(|name| (name.as_str(), Vec::new()))
        .collect();
    let round_len = round_len(qpp);
    for round in 0..rounds {
        for (index, element) in qpp.elements.iter().enumerate() {
            let name = match element {
                PatternElement::Node(node) => node.variable.as_ref(),
                PatternElement::Relationship(rel) => rel.variable.as_ref(),
                // The matcher's spine gate declined the nested spelling.
                PatternElement::Quantified(_) => None,
            };
            let Some(name) = name else {
                continue;
            };
            // An inner node `j` sits at chain node index
            // `pos + round·L + j` (element index `2j`); an inner rel
            // `j` is chain rel index `pos + round·L + j - 1` (element
            // index `2j - 1`).
            let value = if index % 2 == 0 {
                Value::from_node(nodes[pos + round * round_len + index / 2].clone())
            } else {
                Value::from_relationship(rels[pos + round * round_len + (index - 1) / 2].clone())
            };
            // Text render: the group lists are keyed by the collected
            // String vocabulary.
            let name_text = name.to_string();
            lists
                .get_mut(name_text.as_str())
                .expect("gpm_group_names collected every inner element name")
                .push(value);
        }
    }
    for (name, values) in lists {
        row.insert(name.to_string(), Value::List(values.into()));
    }
}

/// One endpoint's witness chain from the run's tables: the recorded
/// accepting arrival's own rel, then the predecessor pair's parent
/// chain — epsilon skips stay on their node, rel steps step back —
/// reversed to seed-first. The recorded arrival's predecessor and rel
/// ride the endpoint record's [`Arrival`] (its pair's parent slot may
/// hold an earlier first arrival); below it, every pair on the chain
/// is a first arrival, so the parent table is exact. The zero-length
/// endpoint is the seed alone.
fn chain_of(parents: &Parents<'_>, seed: NodeId, endpoint: &Endpoint) -> Chain {
    let (mut node, mut state, rel) = match endpoint.arrival {
        // The zero-length endpoint: the walk completes at hop 0, no
        // rel, no predecessor.
        Arrival::Zero => {
            return Chain {
                nodes: vec![endpoint.node],
                rels: Vec::new(),
            }
        }
        Arrival::Rel { pred, rel } => (pred.0, pred.1, rel),
    };
    // The endpoint over the arrival rel reaches the predecessor pair's
    // node; every rel step further back pushes the step's departure
    // node, an epsilon skip stays on its node, and the parentless pair
    // the walk ends at is the seed's start pair (already pushed by the
    // last rel step).
    let mut nodes = vec![endpoint.node, node];
    let mut rels = vec![rel];
    while let Some(parent) = parents.parent_of(node, state) {
        match parent.step {
            Step::Rel(rid) => {
                rels.push(rid);
                nodes.push(parent.node);
            }
            Step::Epsilon => {}
        }
        node = parent.node;
        state = parent.state;
    }
    // Release-checked, like every liveness invariant this reader states:
    // a parent chain terminates at the seed's start pair.
    assert_eq!(
        nodes.last(),
        Some(&seed),
        "a parent chain terminates at the seed's start pair"
    );
    nodes.reverse();
    rels.reverse();
    Chain { nodes, rels }
}

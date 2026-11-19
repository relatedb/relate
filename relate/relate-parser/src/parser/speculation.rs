//! The parser's speculative-probe kit: the always-restore save/restore
//! bracket ([`Parser::probe`]), the memoized per-position verdicts
//! ([`Parser::probe_verdict`]), and the node-pattern tree cache that
//! keeps a probe's parse work when the probe is discarded. All of the
//! kit's state lives on one owner ([`Speculation`]) so the parser
//! core's field list stays grammar-only and the depth, memo, and cache
//! policies each have one home.
//!
//! The *commit-on-success* rewinds — a try-parse that keeps its result
//! and restores only on the other path, such as the schema reader's
//! node-then-relationship discrimination and the type and CALL
//! backtracks — are deliberately not this kit's policy: they
//! conditionally restore, and open-code that pair where they stand.

use std::collections::HashMap;

use super::comprehension_where::BracketProof;
use super::side_effects::PatternSideEffectFlags;
use super::Parser;
use crate::ast::NodePattern;

/// Which always-restore lookahead a memoized verdict belongs to — the
/// gate half of the verdict-memo key, so each gate's verdicts stay
/// distinct while the memo policy itself has one home
/// ([`Parser::probe_verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum ProbeGate {
    /// The `=`-form pattern-comprehension gate
    /// (`pattern_comprehension_follows_equals`).
    EqualsComprehension,
    /// The `(`-form node-pattern gate
    /// (`node_pattern_precedes_relationship`).
    LeftParenNodePattern,
}

/// The verdict-memo key's three dimensions: the probe-start position, the
/// gate, and the comprehension context the gate's parse ran under. The
/// parse is deterministic and a probe restores the position and every
/// side-effect flag before returning, so a verdict depends on exactly
/// this triple — the context is a dimension because the gates' parses
/// read it through their grammar-specific tail classifications, and the
/// same position can legitimately be probed under different contexts: a
/// failed map-comprehension speculation's tokens are re-parsed by the
/// map-literal fallback outside the context, so a narrower key would
/// cross-feed verdicts between the two parses.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct VerdictKey {
    pos: usize,
    gate: ProbeGate,
    comprehension_where: Option<BracketProof>,
}

/// One parked node-pattern parse: the token position a re-parse of the
/// same `(` ends at, and the parse's side-effect footprint (the forms
/// its own tokens noted, replayed by a take). No tree is stored: a
/// probe taking this entry needs only the verdict (the entry's
/// existence proves the position parses), its end position, and the
/// side effects — so `park_node_pattern` moves nothing and clones
/// nothing, and the committing parse (probe depth 0) always re-parses
/// for real. A deep `NodePattern` clone would recurse once per AST
/// spine node, so a wide flat expression in the WHERE (`x0 OR … OR
/// x7999`) would overflow the parse path's stack budget (ADR-0257)
/// where the iterative first parse fits — the stand-in take removes
/// that recursion entirely.
pub(super) struct CachedNodePattern {
    pub(super) end_pos: usize,
    pub(super) footprint: PatternSideEffectFlags,
}

/// The speculative-probe state, one owner: how many probes are on the
/// stack, the [`VerdictKey`]-keyed verdict memo, and the node-pattern
/// tree cache keyed by the `(`'s absolute token index.
///
/// Tree-cache policy, stated once: a tree is **inserted** only while a
/// probe is active (`depth > 0`) — a committed parse's own trees are
/// the parse itself, nothing to reuse — and **taken** at any depth,
/// including a committing parse at depth 0 and a nested probe's parse
/// at depth ≥ 1: without the take, every enclosing probe re-parsed the
/// whole subtree below it, O(size × depth) parse work where the
/// pre-probe token mirror was linear (measured on the fan-out guard in
/// `tests/complexity.rs`: a 2.74 MB depth-450 100-fan-out query at
/// ~51 s release uncached, ~1 s cached — the probe's descent builds
/// each position's tree once and every enclosing/committing parse
/// moves it up).
#[derive(Default)]
pub(super) struct Speculation {
    /// Nesting depth of currently-active speculative probes.
    pub(super) depth: u32,
    /// Memoized verdicts, keyed by [`VerdictKey`]. A `true` verdict's
    /// tokens are re-entered by the caller's committing parse (which
    /// re-applies every side effect for real), so the memo cannot skip
    /// them. LABS-183's shape: a node pattern's inline WHERE or property
    /// map can itself hold a pattern predicate, so an unmemoized probe
    /// re-descends the whole nested term at every level; with the memo
    /// each position's verdict is computed once per context.
    verdicts: HashMap<VerdictKey, bool>,
    /// Parked node-pattern parse verdicts, keyed by the `(`'s absolute
    /// token index — see the cache policy on [`Self`].
    node_patterns: HashMap<usize, CachedNodePattern>,
}

impl Speculation {
    /// The memoized verdict for a key, if probed before.
    pub(super) fn verdict(&self, key: VerdictKey) -> Option<bool> {
        self.verdicts.get(&key).copied()
    }

    /// Record a verdict for a key.
    pub(super) fn record_verdict(&mut self, key: VerdictKey, verdict: bool) {
        self.verdicts.insert(key, verdict);
    }

    /// Take the tree parked at `pos`, if any — a take is a remove: the
    /// tree moves to the taking parse.
    pub(super) fn take_tree(&mut self, pos: usize) -> Option<CachedNodePattern> {
        self.node_patterns.remove(&pos)
    }

    /// Park `tree` at `pos`. The parse is deterministic, so a later
    /// insert at the same position parks the same tree.
    pub(super) fn park_tree(&mut self, pos: usize, tree: CachedNodePattern) {
        self.node_patterns.insert(pos, tree);
    }
}

impl<'a> Parser<'a> {
    /// The one always-restore speculation policy: snapshot the token
    /// position and the clause-level side-effect flags, run `f`,
    /// restore both — a failed or discarded probe leaves no parser
    /// state behind, so clause-end mixing checks see only the
    /// committed parse. The lookahead gates ([`Self::probe_verdict`]'s
    /// callers) go through this.
    pub(super) fn probe<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved_pos = self.save_pos();
        let saved_flags = self.side_effects;
        // Parked footprints must be context-free: a probe can run at a
        // position whose committing re-parse enters a nested accumulator
        // scope (the comprehension body's `mem::take` of `quantifiers`),
        // so a footprint taken as a difference against the enclosing
        // state lets ambient bits mask the parse's own. Starting the
        // whole accumulator group fresh — the same reset a clause start
        // performs — makes every parked footprint the parse's own forms
        // absolutely, and the take's replay exact in any scope. Verdicts
        // are unaffected: parse success never reads these fields.
        self.side_effects = PatternSideEffectFlags::default();
        self.speculation.depth += 1;
        let out = f(&mut *self);
        self.speculation.depth -= 1;
        self.restore_pos(saved_pos);
        self.side_effects = saved_flags;
        out
    }

    /// The one memoized-verdict policy for always-restore lookaheads:
    /// look the [`VerdictKey`] (position, gate, comprehension context)
    /// verdict up, run the probe under [`Self::probe`] on a miss, insert
    /// the verdict. Both lookahead gates ride this — the `=`-form
    /// pattern-comprehension gate and the `(`-form node-pattern gate —
    /// so the memo discipline exists once (LABS-183: without it each
    /// nesting level re-probes the whole nested term, 2^k parse cost on
    /// a sub-KB query).
    pub(super) fn probe_verdict(
        &mut self,
        gate: ProbeGate,
        probe: impl FnOnce(&mut Self) -> bool,
    ) -> bool {
        let key = VerdictKey {
            pos: self.pos,
            gate,
            comprehension_where: self.comprehension_where,
        };
        if let Some(verdict) = self.speculation.verdict(key) {
            return verdict;
        }
        let verdict = self.probe(probe);
        self.speculation.record_verdict(key, verdict);
        verdict
    }

    /// Take the node-pattern parse parked at the current position, if a
    /// probe recorded one — serving **probe contexts only** (depth > 0):
    /// restore the recorded end position, replay the parse's side-effect
    /// footprint, and return a stand-in node. The stand-in is data the
    /// enclosing probe's parse embeds and discards — probes consume
    /// verdicts, never tree content — while the committing parse (depth
    /// 0) never takes, so real trees only ever come from a real parse.
    /// See [`Speculation`] for the cache policy.
    pub(super) fn take_cached_node_pattern(&mut self) -> Option<NodePattern> {
        if self.speculation.depth == 0 {
            return None;
        }
        let cached = self.speculation.take_tree(self.pos)?;
        self.restore_pos(cached.end_pos);
        self.side_effects.absorb(&cached.footprint);
        Some(NodePattern {
            variable: None,
            label_expr: None,
            properties: Vec::new(),
            has_properties: false,
            properties_param: None,
            where_expr: None,
        })
    }

    /// Record the just-parsed node pattern's verdict for `start` when a
    /// probe is active — the end position and the side-effect footprint
    /// (the difference past `before`: the take context holds the same
    /// prefix's forms, so the replay reproduces the real end state
    /// exactly; the scope-reset accumulators carry their absolute value
    /// because probes start them fresh). No tree is parked — see
    /// [`CachedNodePattern`].
    pub(super) fn park_node_pattern(&mut self, start: usize, before: PatternSideEffectFlags) {
        if self.speculation.depth == 0 {
            return;
        }
        let footprint = PatternSideEffectFlags::delta_from(&before, &self.side_effects);
        self.speculation.park_tree(
            start,
            CachedNodePattern {
                end_pos: self.pos,
                footprint,
            },
        );
    }
}

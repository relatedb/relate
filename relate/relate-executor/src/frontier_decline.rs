//! The frontier rung's decline vocabulary — the typed reasons the PROFILE
//! record names when a clause's frontier rung did not serve it. Lives at
//! the crate root, below the matcher that constructs the plan-time half
//! and the rider wiring that records both halves: the plan-time gates' vocabulary
//! ([`FrontierDecline`], ADR-0478), the run-time declines' vocabulary
//! ([`FrontierRunDecline`], ADR-0516/ADR-0521), and the one sum the
//! record carries ([`FrontierDeclineReason`]).

/// Declares one decline vocabulary from a single table: the enum, its
/// declaration-ordered `ALL` (the parity pin's iteration source against
/// the wire table `plan_keys::DECLINE_REASONS`), and its stable wire
/// name `as_str` (the `StartReason::as_str` pattern). Each variant
/// carries its wire name beside its docs, so the three cannot drift.
macro_rules! decline_vocabulary {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$meta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $( $(#[$meta])* $variant, )+
        }

        impl $name {
            /// Every decline of this vocabulary, in declaration order —
            /// the parity pin's iteration source against the wire table
            /// (`plan_keys::DECLINE_REASONS`).
            pub const ALL: [$name; [$($wire),+].len()] = [$($name::$variant),+];

            /// The decline's stable wire name: the vocabulary the plan
            /// args and the renderers spell, single-sourced here against
            /// the wire table the parity pin compares.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( $name::$variant => $wire, )+
                }
            }
        }
    };
}

decline_vocabulary! {
    /// Every plan-time gate's decline, typed so a test or plan dump names the
    /// gate that kept the DFS. The decline is always safe: the per-row DFS
    /// answers identically. Every gate decides before a single walk — the
    /// rung's run-time declines are their own type ([`FrontierRunDecline`]:
    /// the re-arrival budget and the table ceiling). The PROFILE record
    /// carries either one as a [`FrontierDeclineReason`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FrontierDecline {
        /// The clause does not carry exactly one pattern: the frontier is a
        /// single-pattern, single-quantifier family (a UNION's arms are
        /// separate clauses, each decided on its own — the non-quantified arm
        /// needs no kernel at all).
        MultiplePatterns => "multiple-patterns",
        /// The entering row binds anything but the pattern's seed-side
        /// variable: a prebound group-list pin (the legacy exact-trail rule),
        /// a prebound target or path variable, or a mid-statement clause's
        /// outer bindings — per-row semantics the single-seed frontier does
        /// not reproduce.
        CarryingRow => "carrying-row",
        /// The mode is outside the family: an explicit TRAIL/ACYCLIC/SIMPLE
        /// path mode outside the capped consumer (the capped emission's
        /// per-witness cycle checks serve exactly that consumer —
        /// `frontier_emit`; full enumeration and distinct-endpoint answers
        /// stay per-path under these modes), any trail-enforcing mode under
        /// the REPEATABLE pool (rel reuse is that pool's semantics), or an
        /// unbounded quantifier under the WALK
        /// pool — REPEATABLE ELEMENTS keeps its authored
        /// `UnsafeRepeatableElements` rejection (ADR-0478 §5), and the
        /// bounded-WALK answer shape is bounded by definition.
        Mode => "mode",
        /// A nested quantifier inside the quantified spine — a var-length
        /// rel or a quantified group among its inner elements. The spine
        /// then unrolls (variable round length) or accumulates
        /// list-of-list groups, and the emission's fixed-round
        /// reconstruction (`frontier_emit`) cannot bind those rows; the DFS
        /// keeps the shape, answers identical. Parser-unreachable (the
        /// front end's `NestedQuantification` rule rejects both spellings):
        /// the gate is the defensive bound for hand-built IR.
        NestedSpine => "nested-spine",
        /// A MATCH-WHERE allReduce the inliner runs over one of this
        /// quantification's groups — ADR-0478 §2's hard decline: the
        /// inliner's per-branch prefix semantics cannot survive pair dedup
        /// (one (node, state) pair stands for many prefixes with different
        /// accumulator values), so that shape keeps the DFS with its
        /// inliner.
        AllReduceGroup => "all-reduce-group",
        /// A repeated element variable pins a binding the pair-dedup cannot
        /// reconcile: the target (or an interior node) repeating another
        /// node's variable, a rel variable repeated across hops or colliding
        /// with a node name — the walker's pre-bound reconciliation
        /// semantics.
        PinnedBinding => "pinned-binding",
        /// A MATCH-WHERE conjunct outside the closed predicate vocabulary on
        /// the boundary nodes: it does not decode (a function over the row,
        /// `OR`, a nondeterministic bound), it names a group list, the path
        /// variable, a rel, an interior or an outer variable, or it is a
        /// second predicate on one boundary variable. The one exception is
        /// `<>` between the two boundary variables, admitted for the
        /// distinct-endpoint consumer alone (`BoundaryPair`).
        Conjunct => "conjunct",
        /// The NFA compiler declined the shape — its declines are this arm's
        /// declines: a second top-level quantified element, a subpath
        /// variable, a pattern WHERE or a selector, rel-predicate inner
        /// hops, admission-bearing interior nodes (the auto rung; the
        /// forced rung compiles them as guards), the 64-state ceiling, a
        /// nested unbounded quantifier, broken alternation, an uninterned
        /// type name, overflowing bounds.
        NfaShape => "nfa-shape",
        /// A quantified group whose `WHERE` references its own pattern
        /// variables (the fraud f03 shape) — the forced rung compiles it
        /// (the predicate rides the round-closing transition, each
        /// referenced variable read at its last binding), but no consumer
        /// proof covers serving a predicate-guarded run: the pair-dedup
        /// answers these shapes approximately, so every consumer declines
        /// and the DFS answers exactly.
        GuardedGroup => "guarded-group",
        /// The consumer needs every row, not a pair-shaped answer: no cap and
        /// no distinct/aggregate fold — full enumeration (two trails to one
        /// node are two answers), a grouped or filtered suffix, a suffix that
        /// is not a RETURN/WITH, or a `LIMIT 0`/overflowing cap the cap rider
        /// itself declines.
        FullEnumeration => "full-enumeration",
        /// `ORDER BY` with `LIMIT`: ordered truncation needs the full
        /// candidate set to sort (ADR-0478 §2 — ordered caps keep the DFS,
        /// exactly as they keep full enumeration in ADR-0475).
        OrderedCap => "ordered-cap",
        /// The answer counts or folds the path multiset, not the endpoint
        /// set: `count(p)`, `count(*)`, a non-DISTINCT `count`/`collect`, or
        /// a multiplicity-sensitive fold (`sum`, `avg`, …) — path counts are
        /// exponential by nature; the DFS keeps them on this rung (the one
        /// path count the family serves, the WALK pool's plain `count`, is
        /// the walk-count fold's — `frontier_count`, ADR-0523).
        PathCount => "path-count",
        /// A non-capped consumer consumes the path variable or a group list —
        /// only the ≤ `want` reconstructed witnesses of the capped shape
        /// carry those bindings, so a distinct/aggregate answer over them
        /// would read bindings the frontier never produced.
        PathConsumer => "path-consumer",
        /// The cost gate: a bounded recursion whose estimated trails cost the
        /// DFS less than the frontier's pairs and probes (or whose hops the
        /// statistics cannot price) — the DFS is cheaper (no reconstruction
        /// pass, the existing row plumbing) and keeps the clause.
        Cost => "cost",
    }
}

decline_vocabulary! {
    /// The frontier rung's run-time declines (ADR-0516): the reasons a rung
    /// that engaged and ran still falls through, typed so the PROFILE record
    /// names the one that kept the DFS. The plan-time gates' own
    /// vocabulary ([`FrontierDecline`]) is the other half of the record's
    /// [`FrontierDeclineReason`] — every one of those decides before a
    /// single walk. The rider wiring records it and the metric record
    /// carries it. Two today: the re-arrival budget's exhaustion (ADR-0516)
    /// and the sparse tables' allocation ceiling (ADR-0521); a later
    /// run-time decline adds its variant (and its wire name) here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FrontierRunDecline {
        /// The re-arrival rule's shared work budget cut a search — the
        /// endpoint set is unproven, so the run's buffered rows are
        /// discarded and the clause falls through to the next rung
        /// unchanged (the DFS answers these shapes exactly). Constructed on
        /// the production decline path — the run's outcome carries it
        /// (`frontier::RunOutcome::Declined`) and the rider wiring records
        /// it for the PROFILE record.
        RearrivalBudget => "rearrival-budget",
        /// The run's sparse tables — sized by the (node, state) pairs it
        /// touched, never by the store's node-id span — plus the re-arrival
        /// rule's buffers crossed the allocation ceiling
        /// (`FRONTIER_TABLE_BUDGET`, ADR-0521) at a level boundary. A
        /// run-time decline like [`Self::RearrivalBudget`] — the rung ran, so
        /// its counters stay recorded beside the reason — never a compile
        /// gate: the run's buffered rows are discarded and the DFS, with its
        /// own caps, answers the clause.
        TableBudget => "table-budget",
    }
}

/// Why a clause's frontier rung did not serve it, as the PROFILE record
/// carries it (ADR-0516, extended): either a plan-time gate kept the
/// clause off the rung before a single walk ([`FrontierDecline`]), or the
/// rung engaged, ran, and fell through ([`FrontierRunDecline`], its
/// counters recorded beside). One typed sum, so the record's one field
/// names the first reason whichever half decided; the wire spells the
/// same names (`plan_keys::DECLINE_REASONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontierDeclineReason {
    /// A plan-time gate declined: the rung never ran (zero counters).
    Plan(FrontierDecline),
    /// The rung ran and fell through: its counters stay recorded.
    Run(FrontierRunDecline),
}

impl FrontierDeclineReason {
    /// The reason's stable wire name — the inner decline's own spelling,
    /// never a second table.
    pub fn as_str(self) -> &'static str {
        match self {
            FrontierDeclineReason::Plan(decline) => decline.as_str(),
            FrontierDeclineReason::Run(decline) => decline.as_str(),
        }
    }
}

impl From<FrontierDecline> for FrontierDeclineReason {
    fn from(decline: FrontierDecline) -> Self {
        FrontierDeclineReason::Plan(decline)
    }
}

impl From<FrontierRunDecline> for FrontierDeclineReason {
    fn from(decline: FrontierRunDecline) -> Self {
        FrontierDeclineReason::Run(decline)
    }
}

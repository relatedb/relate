//! A cached plan's token footprint (ADR-0687): the store tokens the
//! resolved plan reads, and the row counts its counted tokens were priced
//! at when it was compiled.
//!
//! The footprint is what a later commit is checked against — the tokens a
//! plan outside its footprint never reads cannot invalidate it — so it
//! names every token the plan resolved (ADR-0541: labels, relationship
//! types and property keys carry the store's id on the plan), split by
//! the role the read gives it: a label's count is
//! [`PatternStats::label_rows`]', a type's is
//! [`PatternStats::rel_type_rows`]', and a name read as both appears in
//! both lists.
//!
//! The counts are read through the same stats source the plan was
//! compiled with, under the same publication read guard, so each recorded
//! number is the one the planner was told. Beside the per-token counts
//! the footprint records the equality and range seek cardinalities the
//! plan's endpoint pricing read (ADR-0687 decision 5), through the one
//! derivation the planner prices ([`best_seek`]): each seek is recorded
//! with the consult's identity, so admission re-reads its live count
//! without walking the plan again.
//!
//! A name the store had not interned at plan time has no id to stamp
//! (ADR-0687 decision 3): the footprint records that fact, and admission
//! keeps ADR-0503's per-commit check for such an entry.

use relate_ir::{walk_clauses, walk_pattern_payloads, Clause, Expr, PatternPayload};
use relate_names::{unresolved, NamedToken, SmallName, Token, TokenId};
use relate_planner::{best_seek, walk_plan_parts, where_conjuncts_of, LogicalPlan, PatternStats};

/// The cardinality ratio at which a footprint number's move no longer
/// leaves the plan's ordering plausibly intact (ADR-0687 decision 5):
/// `max(planned/live, live/planned) >= 2.0` marks the entry dirty, below
/// it the entry is re-stamped at the current generation and serves. Two
/// times is the smallest move at which the planner's cardinality ordering
/// flips plausibly.
pub(crate) const DRIFT_FACTOR: u64 = 2;

/// One equality or range seek the plan's endpoint pricing read
/// (ADR-0687 decision 5): the winning `(label, property)` pair's names, the
/// op and the bound, and the cardinality [`best_seek`] answered at plan
/// time. The identity is what admission re-reads live, so the drift check
/// needs no second walk of the plan and no positional pairing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SeekRecord {
    /// The winning pair's label, as the plan knew it.
    pub(crate) label: SmallName,
    /// The indexed property the seek serves.
    pub(crate) prop: SmallName,
    /// [`relate_ir::BinaryOp::Eq`] for the folded point read, the oriented
    /// comparison op for the window.
    pub(crate) op: relate_ir::BinaryOp,
    /// The winning pair's bound expression, as the planner folded or
    /// recognized it.
    pub(crate) bound: Expr,
    /// The count [`best_seek`] answered for this consult at plan time.
    pub(crate) planned: u64,
}

impl SeekRecord {
    /// The seek's live cardinality: the same consult the plan-time pick
    /// answered, re-read through the stats source by name — the one place
    /// the two levels' vocabularies meet ([`unresolved`] resolves the name
    /// through the store's interner exactly as an unresolved plan token
    /// does).
    fn live_rows(&self, stats: &dyn PatternStats) -> Option<u64> {
        let label = unresolved(self.label.clone());
        let prop = unresolved(self.prop.clone());
        if self.op == relate_ir::BinaryOp::Eq {
            stats.equality_rows(&label, &prop, &self.bound)
        } else {
            stats.range_rows(&label, &prop, self.op, &self.bound)
        }
    }
}

/// What one plan reads (ADR-0687 decision 1). The token lists are
/// deduplicated and sorted by id, so the footprint's bytes and its
/// comparisons are order-stable; the seek records keep the clause walk's
/// order and each consult's own identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlanFootprint {
    /// Node labels the plan reads, each with the label row count
    /// [`PatternStats::label_rows`] answered. `None` is the stats
    /// source's decline — the planner priced the label without a count —
    /// never a fabricated zero.
    pub(crate) labels: Vec<(TokenId, Option<u64>)>,
    /// Relationship types the plan reads, each with
    /// [`PatternStats::rel_type_rows`].
    pub(crate) rel_types: Vec<(TokenId, Option<u64>)>,
    /// Property keys the plan reads. No per-token count: the planner
    /// prices a key only through a `(label, key)` pair consult, which
    /// charges no token of its own (ADR-0687 decision 5).
    pub(crate) prop_keys: Vec<TokenId>,
    /// The equality and range seeks the plan's endpoint pricing read
    /// (ADR-0687 decision 5), one per winning consult in the clause walk's
    /// order — each carrying the count [`best_seek`] answered, so the
    /// drift check re-reads that same consult live.
    pub(crate) seeks: Vec<SeekRecord>,
    /// Whether any name the plan reads was absent from the store (or left
    /// unresolved) at plan time (ADR-0687 decision 3): such a name has no
    /// id to stamp, and a later commit can intern it, so an entry carrying
    /// one keeps ADR-0503's any-commit check instead of the footprint's.
    pub(crate) absent: bool,
}

impl PlanFootprint {
    /// The newest generation at which any token the plan reads changed
    /// (ADR-0687 decision 1): admission compares it with the entry's plan
    /// generation — not after it, the entry is clean. The one spelling of
    /// the footprint's stamp read; `last_changed` is the store's
    /// per-token table read under the publication read guard.
    pub(crate) fn newest_change(&self, last_changed: impl Fn(u32) -> u64) -> u64 {
        self.labels
            .iter()
            .map(|(id, _)| id.0)
            .chain(self.rel_types.iter().map(|(id, _)| id.0))
            .chain(self.prop_keys.iter().map(|id| id.0))
            .map(last_changed)
            .max()
            .unwrap_or(0)
    }

    /// Whether any recorded cardinality — a token's row count or a seek's
    /// count — has moved past [`DRIFT_FACTOR`] from the number this plan
    /// was priced at (ADR-0687 decision 5), read live through `stats` —
    /// the same stats source the plan was compiled with. A token whose
    /// recorded or live count is unknown is skipped: a decline is never
    /// read as zero drift-free.
    pub(crate) fn drift_past_threshold(&self, stats: &dyn PatternStats) -> bool {
        let label_moved = |(id, planned): &(TokenId, Option<u64>)| {
            planned.is_some_and(|planned| {
                stats
                    .label_rows(&id_token(*id))
                    .is_some_and(|live| moved_past(planned, live))
            })
        };
        let type_moved = |(id, planned): &(TokenId, Option<u64>)| {
            planned.is_some_and(|planned| {
                stats
                    .rel_type_rows(&id_token(*id))
                    .is_some_and(|live| moved_past(planned, live))
            })
        };
        self.labels.iter().any(label_moved)
            || self.rel_types.iter().any(type_moved)
            // Each recorded seek re-reads its own consult, so a moved count
            // is drift and a declined one is never read as no-drift.
            || self.seeks.iter().any(|seek| {
                seek.live_rows(stats)
                    .is_some_and(|live| moved_past(seek.planned, live))
            })
    }
}

/// A recorded id as the [`NamedToken`] the stats source re-reads it by:
/// [`Token::Id`] answers the id without consulting the name, so the empty
/// name beside it is never read (`GraphStats::id_of`'s dispatch).
fn id_token(id: TokenId) -> NamedToken {
    (SmallName::from_name(""), Token::Id(id))
}

/// Whether `live` moved from `planned` by [`DRIFT_FACTOR`] or more, either
/// direction — the integer form of `max(planned/live, live/planned) >=
/// 2.0`, exact at the threshold and total over a zero count (which is an
/// infinite ratio against any non-zero one).
fn moved_past(planned: u64, live: u64) -> bool {
    let (low, high) = if planned <= live {
        (planned, live)
    } else {
        (live, planned)
    };
    high != low && high >= low.saturating_mul(DRIFT_FACTOR)
}

/// The token footprint of `plan` and the per-token counts it was planned
/// against (ADR-0687), read through `stats` — the caller passes the same
/// [`crate::stats::GraphPatternStats`] construction the plan was compiled
/// with, under the same publication read guard (ADR-0218), so a recorded
/// count is the one the planner was told.
///
/// Only [`relate_names::Token::Id`] is a footprint token: an absent name
/// has no id to stamp, and a name planning left unresolved was never
/// resolved against this store (ADR-0541's coverage), so neither appears.
pub(crate) fn plan_footprint(plan: &LogicalPlan, stats: &dyn PatternStats) -> PlanFootprint {
    let mut labels: Vec<NamedToken> = Vec::new();
    let mut rel_types: Vec<NamedToken> = Vec::new();
    // Two accumulators only because the clause and expression callbacks
    // are two closures; they merge into one list below. The absent flags
    // split the same way — a shared capture would be two mutable borrows.
    let mut pattern_keys: Vec<TokenId> = Vec::new();
    let mut expr_keys: Vec<TokenId> = Vec::new();
    let mut seeks: Vec<SeekRecord> = Vec::new();
    let mut pattern_absent = false;
    let mut expr_absent = false;
    walk_plan_parts(plan, &mut |part| {
        walk_clauses(
            part,
            &mut |clause| {
                // Only a MATCH reads tokens by id: the write clauses keep
                // their name arm (ADR-0541 §3) and resolve nothing.
                let Clause::Match {
                    patterns,
                    where_clause,
                    ..
                } = clause
                else {
                    return;
                };
                for pattern in patterns {
                    walk_pattern_payloads(pattern, &mut |payload| match payload {
                        PatternPayload::Node(node) => {
                            labels.extend(node.labels.iter().cloned());
                            pattern_keys.extend(key_ids(&node.prop_keys));
                            pattern_absent |= unresolved_key(&node.prop_keys);
                        }
                        PatternPayload::Relationship(rel) => {
                            rel_types.extend(rel.types.iter().cloned());
                            pattern_keys.extend(key_ids(&rel.prop_keys));
                            pattern_absent |= unresolved_key(&rel.prop_keys);
                        }
                        PatternPayload::Quantified(_) => {}
                    });
                }
                // The plan's endpoint pricing, in the same clause visit:
                // each MATCH pattern node's winning [`best_seek`] pick, the
                // one derivation the planner prices and the plan tree
                // narrates. The `where` conjuncts are the view the planner
                // prices windows from ([`where_conjuncts_of`]).
                let conjuncts = where_conjuncts_of(where_clause.as_ref());
                for pattern in patterns {
                    for node in &pattern.nodes {
                        let Some(pick) = best_seek(node, conjuncts.iter().copied(), stats) else {
                            continue;
                        };
                        seeks.push(SeekRecord {
                            label: pick.label.clone(),
                            prop: pick.prop,
                            op: pick.op,
                            bound: pick.bound.clone(),
                            planned: pick.rows,
                        });
                    }
                }
            },
            &mut |expr| {
                if let Expr::Property { key_id, .. } = expr {
                    expr_keys.extend(key_id.id().map(TokenId));
                    expr_absent |= key_id.id().is_none();
                }
            },
        );
    });
    let mut prop_keys = pattern_keys;
    prop_keys.extend(expr_keys);
    prop_keys.sort_unstable_by_key(|id| id.0);
    prop_keys.dedup();
    // ADR-0687 decision 3: any name the plan reads without a store id —
    // absent from the interner at plan time or left unresolved — makes
    // the entry keep ADR-0503's any-commit check.
    let absent = pattern_absent
        || expr_absent
        || labels
            .iter()
            .chain(rel_types.iter())
            .any(|(_, token)| !matches!(token, Token::Id(_)));
    PlanFootprint {
        labels: counted(&labels, |token| stats.label_rows(token)),
        rel_types: counted(&rel_types, |token| stats.rel_type_rows(token)),
        prop_keys,
        seeks,
        absent,
    }
}

/// Whether any key in `keys` carries no plan-time store id (ADR-0687
/// decision 3).
fn unresolved_key(keys: &[NamedToken]) -> bool {
    keys.iter().any(|(_, token)| !matches!(token, Token::Id(_)))
}

/// The resolved ids of a pattern's property keys (ADR-0541); an absent
/// or unresolved key has none.
fn key_ids(keys: &[NamedToken]) -> Vec<TokenId> {
    keys.iter()
        .filter_map(|(_, token)| token.id().map(TokenId))
        .collect()
}

/// Each token once, beside the count `read` answers for it, in id order —
/// the footprint reads a name once however many positions spell it. The
/// dedup is the module's one spelling, the same sort-then-dedup the property
/// keys take: sort by id, then drop the repeats.
fn counted(
    tokens: &[NamedToken],
    read: impl Fn(&NamedToken) -> Option<u64>,
) -> Vec<(TokenId, Option<u64>)> {
    let mut entries: Vec<(TokenId, Option<u64>)> = tokens
        .iter()
        .filter_map(|token| token.1.id().map(|id| (TokenId(id), read(token))))
        .collect();
    entries.sort_unstable_by_key(|(id, _)| id.0);
    entries.dedup_by_key(|(id, _)| id.0);
    entries
}

#[cfg(test)]
mod tests {
    use relate_names::{Token, TokenResolver};
    use relate_planner::ContainsCount;

    use super::*;

    /// A store that has interned exactly `Person`, `KNOWS` and `age`.
    struct Interned;

    impl TokenResolver for Interned {
        fn resolve(&self, name: &str) -> Token {
            match ["Person", "KNOWS", "age"]
                .iter()
                .position(|known| *known == name)
            {
                Some(i) => Token::Id(TokenId(i as u32)),
                None => Token::Absent,
            }
        }
    }

    /// The stats source: a fixed count per role, no consult behaviour.
    struct Rows;

    impl PatternStats for Rows {
        fn label_rows(&self, _label: &NamedToken) -> Option<u64> {
            Some(12)
        }

        fn equality_rows(
            &self,
            _label: &NamedToken,
            _property: &NamedToken,
            _value: &Expr,
        ) -> Option<u64> {
            None
        }

        fn contains_rows(
            &self,
            _label: &NamedToken,
            _property: &NamedToken,
            _needle: &Expr,
        ) -> Option<ContainsCount> {
            None
        }

        fn rel_type_rows(&self, _rel_type: &NamedToken) -> Option<u64> {
            Some(4)
        }
    }

    fn footprint_of(cypher: &str) -> PlanFootprint {
        let query = relate_front::parse_and_lower(cypher).unwrap();
        let plan =
            relate_planner::plan_with_tokens(query, &mut |_| false, &Rows, &Interned).unwrap();
        plan_footprint(&plan, &Rows)
    }

    /// A stats source whose every consult answers a number, so the seek
    /// cardinalities are visible in the footprint.
    struct Counts {
        label: u64,
        equality: u64,
        range: u64,
    }

    impl PatternStats for Counts {
        fn label_rows(&self, _label: &NamedToken) -> Option<u64> {
            Some(self.label)
        }

        fn equality_rows(
            &self,
            _label: &NamedToken,
            _property: &NamedToken,
            _value: &Expr,
        ) -> Option<u64> {
            Some(self.equality)
        }

        fn range_rows(
            &self,
            _label: &NamedToken,
            _property: &NamedToken,
            _op: relate_ir::BinaryOp,
            _bound: &Expr,
        ) -> Option<u64> {
            Some(self.range)
        }

        fn contains_rows(
            &self,
            _label: &NamedToken,
            _property: &NamedToken,
            _needle: &Expr,
        ) -> Option<ContainsCount> {
            None
        }

        fn rel_type_rows(&self, _rel_type: &NamedToken) -> Option<u64> {
            Some(0)
        }
    }

    /// The plan `plan_footprint` and `drift_past_threshold` share in the
    /// seek tests: a statement compiled against `stats`.
    fn plan_with(stats: &dyn PatternStats, cypher: &str) -> LogicalPlan {
        let query = relate_front::parse_and_lower(cypher).unwrap();
        relate_planner::plan_with_tokens(query, &mut |_| false, stats, &Interned).unwrap()
    }

    /// The footprint of a statement compiled against `stats`.
    fn footprint_with(stats: &dyn PatternStats, cypher: &str) -> PlanFootprint {
        plan_footprint(&plan_with(stats, cypher), stats)
    }

    /// A one-label MATCH reads that label's id, with the count the stats
    /// source answers for it, and every name resolved: no absent flag.
    #[test]
    fn a_one_label_match_yields_that_label_id() {
        let footprint = footprint_of("MATCH (n:Person) RETURN n");
        assert_eq!(footprint.labels, vec![(TokenId(0), Some(12))]);
        assert!(footprint.rel_types.is_empty());
        assert!(footprint.prop_keys.is_empty());
        assert!(!footprint.absent);
    }

    /// A statement that reads no token has an empty footprint.
    #[test]
    fn a_token_free_statement_yields_an_empty_footprint() {
        let footprint = footprint_of("RETURN 1 AS one");
        assert_eq!(footprint, PlanFootprint::default());
    }

    /// Labels, relationship types and property keys each land in their
    /// own list — pattern keys and property expressions alike.
    #[test]
    fn every_token_role_is_collected() {
        let footprint = footprint_of("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 RETURN m");
        assert_eq!(footprint.labels, vec![(TokenId(0), Some(12))]);
        assert_eq!(footprint.rel_types, vec![(TokenId(1), Some(4))]);
        assert_eq!(footprint.prop_keys, vec![TokenId(2)]);
    }

    /// A pattern property key is a footprint token too, and a name read in
    /// several positions is listed once.
    #[test]
    fn pattern_keys_are_collected_once() {
        let footprint = footprint_of("MATCH (n:Person {age: 1}) RETURN n.age AS age");
        assert_eq!(footprint.labels, vec![(TokenId(0), Some(12))]);
        assert_eq!(footprint.prop_keys, vec![TokenId(2)]);
    }

    /// Absent names resolve to no id, so they contribute no footprint
    /// token, and the footprint flags them (ADR-0687 decision 3: the
    /// entry keeps ADR-0503's any-commit check).
    #[test]
    fn absent_names_contribute_no_token_and_set_the_flag() {
        let footprint = footprint_of("MATCH (n:Nobody)-[:NEVER]->(m) WHERE n.nope > 1 RETURN m");
        assert!(footprint.labels.is_empty());
        assert!(footprint.rel_types.is_empty());
        assert!(footprint.prop_keys.is_empty());
        assert!(footprint.absent);
    }

    /// The newest change over the footprint's ids is the maximum of the
    /// per-token stamps across every role — labels, types and keys alike
    /// (ADR-0687 decision 1).
    #[test]
    fn newest_change_takes_the_latest_stamp_over_every_role() {
        let mut footprint = footprint_of("MATCH (n:Person)-[:KNOWS]->(m) WHERE n.age > 1 RETURN m");
        footprint.prop_keys.push(TokenId(9));
        let stamps = |id: u32| match id {
            0 => 3,  // Person
            1 => 7,  // KNOWS
            2 => 5,  // age
            _ => 11, // the pushed key
        };
        assert_eq!(footprint.newest_change(stamps), 11);
        // No token ever changed: zero, the table's "never" generation.
        assert_eq!(footprint.newest_change(|_| 0), 0);
        assert_eq!(
            PlanFootprint::default().newest_change(|_| 99),
            0,
            "an empty footprint has no change to report"
        );
    }

    /// A recorded count that doubled — either direction — is past the
    /// threshold; a smaller move, and an equal count, are not. `Rows`
    /// answers 12 for a label and 4 for a type.
    #[test]
    fn drift_past_threshold_is_the_two_times_ratio() {
        let labels = |planned: u64| PlanFootprint {
            labels: vec![(TokenId(0), Some(planned))],
            ..PlanFootprint::default()
        };
        assert!(labels(6).drift_past_threshold(&Rows), "12/6 = 2.0");
        assert!(labels(24).drift_past_threshold(&Rows), "24/12 = 2.0");
        assert!(!labels(7).drift_past_threshold(&Rows), "12/7 < 2.0");
        assert!(!labels(12).drift_past_threshold(&Rows), "no move");
        assert!(
            labels(0).drift_past_threshold(&Rows),
            "zero to twelve is an infinite ratio"
        );
        // Zero to zero is no move (the same predicate over an empty store).
        struct Empty;
        impl PatternStats for Empty {
            fn label_rows(&self, _label: &NamedToken) -> Option<u64> {
                Some(0)
            }
            fn equality_rows(
                &self,
                _label: &NamedToken,
                _property: &NamedToken,
                _value: &Expr,
            ) -> Option<u64> {
                None
            }
            fn contains_rows(
                &self,
                _label: &NamedToken,
                _property: &NamedToken,
                _needle: &Expr,
            ) -> Option<ContainsCount> {
                None
            }
            fn rel_type_rows(&self, _rel_type: &NamedToken) -> Option<u64> {
                Some(0)
            }
        }
        assert!(
            !PlanFootprint {
                labels: vec![(TokenId(0), Some(0))],
                ..PlanFootprint::default()
            }
            .drift_past_threshold(&Empty),
            "zero to zero did not move"
        );
        // A type's count (4) drifts on its own role's number.
        let rel_types = |planned: u64| PlanFootprint {
            rel_types: vec![(TokenId(1), Some(planned))],
            ..PlanFootprint::default()
        };
        assert!(rel_types(2).drift_past_threshold(&Rows));
        assert!(!rel_types(3).drift_past_threshold(&Rows));
        // A count the stats source declines on either side is never read
        // as drift.
        assert!(!PlanFootprint {
            labels: vec![(TokenId(0), None)],
            ..PlanFootprint::default()
        }
        .drift_past_threshold(&Rows));
    }

    /// ADR-0687 decision 5: the footprint records the equality and range
    /// seek cardinalities the plan's endpoint pricing read — one record per
    /// winning consult, and none for a pattern with neither.
    #[test]
    fn equality_and_range_seek_counts_are_recorded() {
        let stats = Counts {
            label: 10,
            equality: 3,
            range: 2,
        };
        let planned = |cypher: &str| {
            footprint_with(&stats, cypher)
                .seeks
                .iter()
                .map(|seek| seek.planned)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            planned("MATCH (n:Person {age: 1}) RETURN n"),
            vec![3],
            "the equality point read's count"
        );
        assert_eq!(
            planned("MATCH (n:Person) WHERE n.age > 1 RETURN n"),
            vec![2],
            "the range window's count"
        );
        assert!(
            planned("MATCH (n:Person) RETURN n").is_empty(),
            "no seek predicate, no seek number"
        );
    }

    /// A seek count that moved past the threshold is drift; a smaller move
    /// is not — each recorded seek re-reads its own consult live.
    #[test]
    fn seek_drift_compares_against_the_same_consult() {
        let recorded = Counts {
            label: 10,
            equality: 3,
            range: 2,
        };
        let plan = plan_with(&recorded, "MATCH (n:Person {age: 1}) RETURN n");
        let footprint = plan_footprint(&plan, &recorded);
        assert_eq!(footprint.seeks.len(), 1);
        assert_eq!(footprint.seeks[0].planned, 3);
        let doubled = Counts {
            label: 10,
            equality: 6,
            range: 2,
        };
        assert!(
            footprint.drift_past_threshold(&doubled),
            "3 -> 6 crosses the threshold"
        );
        let small = Counts {
            label: 10,
            equality: 4,
            range: 2,
        };
        assert!(
            !footprint.drift_past_threshold(&small),
            "3 -> 4 stays inside"
        );
    }

    /// A stats source that declines a count records no number rather than
    /// a fabricated zero.
    #[test]
    fn a_declined_count_is_recorded_as_absent() {
        struct Declines;
        impl PatternStats for Declines {
            fn label_rows(&self, _label: &NamedToken) -> Option<u64> {
                None
            }
            fn equality_rows(
                &self,
                _label: &NamedToken,
                _property: &NamedToken,
                _value: &Expr,
            ) -> Option<u64> {
                None
            }
            fn contains_rows(
                &self,
                _label: &NamedToken,
                _property: &NamedToken,
                _needle: &Expr,
            ) -> Option<ContainsCount> {
                None
            }
            fn rel_type_rows(&self, _rel_type: &NamedToken) -> Option<u64> {
                None
            }
        }
        let query = relate_front::parse_and_lower("MATCH (n:Person) RETURN n").unwrap();
        let plan =
            relate_planner::plan_with_tokens(query, &mut |_| false, &Declines, &Interned).unwrap();
        assert_eq!(
            plan_footprint(&plan, &Declines).labels,
            vec![(TokenId(0), None)]
        );
    }
}

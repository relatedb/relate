//! The expression-memo group's wire shape (ADR-0654): the typed view of
//! the `Memo*` args a PROFILE node carries when its memos were probed, the
//! closed vocabulary of memo types the plan root names, and the one
//! read-side validation (all-or-none, non-negative, PROFILE-only) the other
//! metric groups have. `plan_wire` reads the group through
//! [`memo_group`]; the producer is `relate`'s `PlanNode::to_value`.

use super::plan_args::metric_group_ints;
use super::plan_keys;
use crate::CypherValue;

/// The memo types a plan root's `MemoKinds` can name (ADR-0654 §3): a
/// closed set of stable names, the vocabulary's one spelling — the
/// executor's memo type maps onto it, never a free string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoKindWire {
    /// The node-label guard memo of a traversal stage.
    NodeLabel,
    /// The `datetime(<string>)` constructor memo.
    Datetime,
}

impl MemoKindWire {
    /// Every kind, in vocabulary order; the bit of a kind in
    /// [`MemoKindsWire`] is its index here.
    pub const ALL: [MemoKindWire; 2] = [MemoKindWire::NodeLabel, MemoKindWire::Datetime];
    /// The node-label memo's wire name.
    pub const NODE_LABEL: &'static str = "node-label";
    /// The datetime constructor memo's wire name.
    pub const DATETIME: &'static str = "datetime";
    /// Every wire name, in [`Self::ALL`] order.
    pub const NAMES: [&'static str; 2] = [Self::NODE_LABEL, Self::DATETIME];

    /// The kind's wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            MemoKindWire::NodeLabel => Self::NODE_LABEL,
            MemoKindWire::Datetime => Self::DATETIME,
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`MemoKindWire`]s, a bitset so the metric bundles stay `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoKindsWire(u8);

impl MemoKindsWire {
    /// The set with `kind` added.
    pub const fn with(self, kind: MemoKindWire) -> Self {
        Self(self.0 | kind.bit())
    }

    /// The union of two sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether the set holds `kind`.
    pub const fn contains(self, kind: MemoKindWire) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Whether the set holds no kind.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The members' wire names, in vocabulary order.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        MemoKindWire::ALL
            .into_iter()
            .filter(move |kind| self.contains(*kind))
            .map(MemoKindWire::as_str)
    }

    /// The set named by `names`; `None` when a name is outside the
    /// vocabulary.
    pub fn from_names<'n>(names: impl IntoIterator<Item = &'n str>) -> Option<Self> {
        names.into_iter().try_fold(Self::default(), |set, name| {
            let kind = MemoKindWire::ALL
                .into_iter()
                .find(|kind| kind.as_str() == name)?;
            Some(set.with(kind))
        })
    }
}

/// The expression-memo counters a PROFILE node reports (ADR-0654): the
/// per-memo facts of ADR-0640 summed over the node's memos, plus how many
/// of them gave up at their hit-rate review. `bytes` is the tables'
/// allocation when read and is never part of `memory_bytes`. `kinds` is
/// non-empty on the plan root only (constructor memos attach there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanMemoWire {
    pub probes: i64,
    pub hits: i64,
    pub inserts: i64,
    pub refused: i64,
    pub bytes: i64,
    pub disabled_count: i64,
    pub kinds: MemoKindsWire,
}

/// Read one node's memo group (ADR-0654): `None` when the node carries no
/// memo key, the typed counters when it carries the all-or-none integers of
/// a probed memo on a PROFILE node. Anything else — a partial group, a
/// group on an EXPLAIN node, zero probes, a negative value, a kind outside
/// [`MemoKindWire::NAMES`], kinds without the group — is a wire-contract
/// break that fails loudly.
pub(super) fn memo_group(
    args: &crate::IndexMap<String, CypherValue>,
    has_metrics: bool,
) -> Option<PlanMemoWire> {
    let ints = metric_group_ints(args, plan_keys::MEMO_GROUP_KEYS);
    let kinds = match args.get(plan_keys::MEMO_KINDS) {
        None => MemoKindsWire::default(),
        Some(CypherValue::List(names)) => {
            let names = names.iter().map(|name| match name {
                CypherValue::Str(name) => name.as_str(),
                other => panic!(
                    "plan wire {} entries are strings (ADR-0654), got {other:?}",
                    plan_keys::MEMO_KINDS
                ),
            });
            MemoKindsWire::from_names(names).unwrap_or_else(|| {
                panic!(
                    "plan wire {} names come from {:?} (ADR-0654)",
                    plan_keys::MEMO_KINDS,
                    MemoKindWire::NAMES
                )
            })
        }
        Some(other) => panic!(
            "plan wire {} is a list of strings when present (ADR-0654), got {other:?}",
            plan_keys::MEMO_KINDS
        ),
    };
    match (has_metrics, ints) {
        (_, None) if kinds.is_empty() => None,
        (true, Some([probes, hits, inserts, refused, bytes, disabled_count])) if probes > 0 => {
            Some(PlanMemoWire {
                probes,
                hits,
                inserts,
                refused,
                bytes,
                disabled_count,
                kinds,
            })
        }
        (has_metrics, ints) => panic!(
            "plan wire {}..{} are non-negative integers emitted together \
             with a positive {}, only on a PROFILE node, {} only beside \
             them (ADR-0654), got metrics={has_metrics} ints={ints:?} \
             kinds={kinds:?}",
            plan_keys::MEMO_PROBES,
            plan_keys::MEMO_DISABLED_COUNT,
            plan_keys::MEMO_PROBES,
            plan_keys::MEMO_KINDS
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::plan_node_wire;
    use crate::wire::plan_test_fixtures::{args_mut, plan_wire_node};

    fn put_memo_group(node: &mut CypherValue, probes: i64, hits: i64, bytes: i64) {
        let args = args_mut(node);
        for (key, value) in [
            (plan_keys::MEMO_PROBES, probes),
            (plan_keys::MEMO_HITS, hits),
            (plan_keys::MEMO_INSERTS, 3),
            (plan_keys::MEMO_REFUSED, 1),
            (plan_keys::MEMO_BYTES, bytes),
            (plan_keys::MEMO_DISABLED_COUNT, 2),
        ] {
            args.insert(key.to_string(), CypherValue::Int(value));
        }
    }

    /// ADR-0654: the memo group parses to its typed form beside the
    /// root's kinds, and a node without it reads `None` (presence-only).
    #[test]
    fn the_memo_group_round_trips_and_is_absent_when_unused() {
        let mut root = plan_wire_node("ProduceResults", 0, Some(1), vec![]);
        assert_eq!(plan_node_wire(&root).metrics.unwrap().memo, None);
        put_memo_group(&mut root, 100, 90, 4096);
        args_mut(&mut root).insert(
            plan_keys::MEMO_KINDS.to_string(),
            CypherValue::List(vec![
                CypherValue::Str("node-label".into()),
                CypherValue::Str("datetime".into()),
            ]),
        );
        let memo = plan_node_wire(&root).metrics.unwrap().memo.unwrap();
        assert_eq!(
            (memo.probes, memo.hits, memo.inserts, memo.refused),
            (100, 90, 3, 1)
        );
        assert_eq!((memo.bytes, memo.disabled_count), (4096, 2));
        assert_eq!(
            memo.kinds.names().collect::<Vec<_>>(),
            MemoKindWire::NAMES.to_vec()
        );
    }

    #[test]
    #[should_panic(expected = "are an all-or-none group")]
    fn a_partial_memo_group_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, Some(1), vec![]);
        args_mut(&mut node).insert(plan_keys::MEMO_PROBES.to_string(), CypherValue::Int(5));
        let _ = plan_node_wire(&node);
    }

    #[test]
    #[should_panic(expected = "only on a PROFILE node")]
    fn a_memo_group_on_an_explain_node_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, None, vec![]);
        put_memo_group(&mut node, 5, 1, 8);
        let _ = plan_node_wire(&node);
    }

    #[test]
    #[should_panic(expected = "only on a PROFILE node")]
    fn a_zero_probe_memo_group_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, Some(1), vec![]);
        put_memo_group(&mut node, 0, 0, 0);
        let _ = plan_node_wire(&node);
    }

    #[test]
    #[should_panic(expected = "names come from")]
    fn an_unknown_memo_kind_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, Some(1), vec![]);
        put_memo_group(&mut node, 5, 1, 8);
        args_mut(&mut node).insert(
            plan_keys::MEMO_KINDS.to_string(),
            CypherValue::List(vec![CypherValue::Str("bogus".into())]),
        );
        let _ = plan_node_wire(&node);
    }
}

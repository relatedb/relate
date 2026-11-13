//! Point reads of one node or relationship through the write overlay: a
//! scratch deletion hides the element, a scratch copy answers for it, and
//! otherwise the snapshot at `snapshot_ts` does.

use std::sync::Arc;

use relate_core::{Node, NodeId, RelId, Relationship, Result, Value};
use relate_ir::Direction;
use relate_names::Token;

use super::ExecGraph;

/// One candidate node as the scan-predicate check reads it: the write
/// scratch's materialized copy (string-keyed), or the snapshot's live
/// record (interned ids). The two arms carry the same facts — labels and
/// one-slot property reads — each keyed the way its storage is keyed, so
/// the predicate evaluation is spelled once over both.
pub(crate) enum ScanNode<'a> {
    Scratch(&'a Node),
    Live(&'a crate::store::LiveNode),
}

impl ScanNode<'_> {
    /// Whether the node carries `label`, by the interned `lid` on the
    /// live record and by the string on the scratch copy.
    pub(crate) fn has_label(&self, label: &str, lid: Option<u32>) -> bool {
        match self {
            Self::Scratch(n) => n.labels.iter().any(|l| l == label),
            Self::Live(live) => lid.is_some_and(|lid| live.has_label(lid)),
        }
    }

    /// The node's `key` property, null when absent — the same read
    /// `eval::prop` makes of a bound node, resolved through the interned
    /// `kid` on the live record and the string on the scratch copy.
    pub(crate) fn prop(&self, key: &str, kid: Option<u32>) -> Value {
        match self {
            Self::Scratch(n) => n.properties.get(key).cloned().unwrap_or(Value::Null),
            Self::Live(live) => kid
                .and_then(|kid| live.props.get(kid))
                .cloned()
                .unwrap_or(Value::Null),
        }
    }

    /// The node's `key` property by reference, `None` when absent — the
    /// borrowed form of [`ScanNode::prop`] for the reads that only
    /// inspect the value (the temporal construction borrows its argument,
    /// the padded-ISO fast path parses a `&str`), so the per-candidate
    /// gate never clones the stored string. The caller's `check` closure
    /// runs inside the one store read the gate opened, so the borrow
    /// outlives the check.
    pub(crate) fn prop_value(&self, key: &str, kid: Option<u32>) -> Option<&Value> {
        match self {
            Self::Scratch(n) => n.properties.get(key),
            Self::Live(live) => kid.and_then(|kid| live.props.get(kid)),
        }
    }
}

/// What the overlay scratch says about one element (ADR-0016). Every point
/// read below starts from this prologue: a scratch deletion hides the
/// element, a scratch copy answers for it, and scratch silence falls
/// through to the committed snapshot at `snapshot_ts`.
enum ScratchAnswer<T> {
    /// The scratch deleted the element: the read answers "gone".
    Deleted,
    /// The scratch holds the element's copy, which answered the read.
    Answered(T),
    /// The scratch says nothing: read the snapshot.
    Snapshot,
}

impl<T> ScratchAnswer<T> {
    /// Resolve the prologue into the read's answer: `deleted` when the
    /// scratch hid the element, the scratch copy's answer when it spoke,
    /// and `snapshot()` otherwise.
    fn resolve(self, deleted: T, snapshot: impl FnOnce() -> T) -> T {
        match self {
            Self::Deleted => deleted,
            Self::Answered(answer) => answer,
            Self::Snapshot => snapshot(),
        }
    }
}

impl ExecGraph<'_> {
    /// The node side of the visibility prologue: `read` answers from the
    /// scratch's copy of `id` when it holds one.
    fn node_prologue<T>(&self, id: NodeId, read: impl FnOnce(&Node) -> T) -> ScratchAnswer<T> {
        if let Some(cell) = self.scratch {
            let s = cell.borrow();
            if s.batch.deleted_nodes.contains(&id) {
                return ScratchAnswer::Deleted;
            }
            if let Some(n) = s.batch.nodes.get(&id) {
                return ScratchAnswer::Answered(read(n));
            }
        }
        ScratchAnswer::Snapshot
    }

    /// The relationship side of the visibility prologue: `read` answers
    /// from the scratch's copy of `id` when it holds one.
    fn rel_prologue<T>(
        &self,
        id: RelId,
        read: impl FnOnce(&Relationship) -> T,
    ) -> ScratchAnswer<T> {
        if let Some(cell) = self.scratch {
            let s = cell.borrow();
            if s.batch.deleted_rels.contains(&id) {
                return ScratchAnswer::Deleted;
            }
            if let Some(r) = s.batch.rels.get(&id) {
                return ScratchAnswer::Answered(read(r));
            }
        }
        ScratchAnswer::Snapshot
    }

    pub fn node(&self, id: NodeId) -> Option<Node> {
        self.node_prologue(id, |n| Some(n.clone()))
            .resolve(None, || self.base.node_at(id, self.snapshot_ts()))
    }

    /// Existence check without materializing the node. The profiled hot path
    /// (`node` clones the full `Node` — labels + property map) makes this the
    /// common-case read in a write statement; callers that only need to know
    /// a node is present must not pay for its properties.
    pub fn node_exists(&self, id: NodeId) -> bool {
        self.node_prologue(id, |_| true)
            .resolve(false, || self.base.node_visible_at(id, self.snapshot_ts()))
    }

    /// Label ids without materializing properties. Returns owned label
    /// strings; callers that only compare labels pay no property map clone.
    pub fn node_labels(&self, id: NodeId) -> Option<Vec<String>> {
        self.node_prologue(id, |n| Some((*n.labels).clone()))
            .resolve(None, || self.base.node_labels_at(id, self.snapshot_ts()))
    }

    pub fn rel(&self, id: RelId) -> Option<Relationship> {
        self.rel_prologue(id, |r| Some(r.clone()))
            .resolve(None, || self.base.rel_at(id, self.snapshot_ts()))
    }

    pub fn rel_ends(&self, id: RelId) -> Option<(NodeId, NodeId)> {
        self.rel_prologue(id, |r| Some((r.start, r.end)))
            .resolve(None, || self.base.rel_ends_at(id, self.snapshot_ts()))
    }

    pub fn hop(&self, current: NodeId, rid: RelId, dir: Direction) -> Option<NodeId> {
        let (start, end) = self.rel_ends(rid)?;
        crate::store::hop_other(current, start, end, dir)
    }

    /// Plan-time label cardinality estimate (ADR-0218), forwarded for the
    /// exec layer's pattern-start estimates (ADR-0351).
    pub fn label_count(&self, label: &str) -> u64 {
        self.base.label_count(label)
    }

    /// Existence check without materializing the relationship.
    pub fn rel_exists(&self, id: RelId) -> bool {
        self.rel_prologue(id, |_| true)
            .resolve(false, || self.base.rel_visible_at(id, self.snapshot_ts()))
    }

    /// The type name shared (`Arc<str>` — one refcount bump off the
    /// store's interned name; the batch record's copy is the only
    /// allocation, and only when the rel is scratch). The selector's
    /// per-walk type reads ride this, never the `String` clone.
    pub fn rel_type_shared(&self, id: RelId) -> Option<Arc<str>> {
        self.rel_prologue(id, |r| Some(Arc::from(r.rel_type.as_str())))
            .resolve(None, || {
                self.base.rel_type_name_shared(id, self.snapshot_ts())
            })
    }

    /// Type name without materializing the relationship's property map.
    pub fn rel_type(&self, id: RelId) -> Option<String> {
        self.rel_prologue(id, |r| Some(r.rel_type.clone()))
            .resolve(None, || {
                self.base
                    .rel_stub_at(id, self.snapshot_ts())
                    .map(|r| r.rel_type)
            })
    }

    pub fn node_stub(&self, id: NodeId) -> Option<Node> {
        self.node_prologue(id, |n| {
            Some(Node {
                id: n.id,
                labels: n.labels.clone(),
                properties: crate::store::empty_properties(),
            })
        })
        .resolve(None, || self.base.node_stub_at(id, self.snapshot_ts()))
    }

    pub fn rel_stub(&self, id: RelId) -> Option<Relationship> {
        self.rel_prologue(id, |r| {
            Some(Relationship {
                id: r.id,
                start: r.start,
                end: r.end,
                rel_type: r.rel_type.clone(),
                properties: crate::store::empty_properties(),
            })
        })
        .resolve(None, || self.base.rel_stub_at(id, self.snapshot_ts()))
    }

    pub fn node_prop(&self, id: NodeId, key: &str) -> Option<Value> {
        self.node_prologue(id, |n| n.properties.get(key).cloned())
            .resolve(None, || self.base.node_prop_at(id, self.snapshot_ts(), key))
    }

    /// The plan token as this view must read it — the one rebase policy
    /// every token domain reads (ADR-0541 × ADR-0210): an `Absent` token
    /// records a plan-time fact, the store had never interned the name,
    /// and a `CALL { … } IN TRANSACTIONS` statement's own batch commits
    /// intern names mid-statement, after the plan resolved them but
    /// before its later parts read. A scratch rebased past this view's
    /// admission pin — the state only those mid-statement commits
    /// produce — therefore turns `Absent` into `Unresolved`, so the
    /// caller's by-name lookup answers it; every other view keeps the
    /// plan's answer (a view reading at its own pin cannot see data
    /// committed after it). `Id` passes through: the interner is
    /// append-only, so a plan-time id stays the name's id. The label
    /// twin of this re-check is [`Self::node_has_uninterned_label`].
    pub(crate) fn live_token(&self, token: Token) -> Token {
        if token == Token::Absent && self.snapshot_ts() != self.pinned_ts {
            Token::Unresolved
        } else {
            token
        }
    }

    /// The store id a plan-resolved property-key token names (ADR-0541):
    /// the token through [`Self::live_token`]'s rebase policy, then the
    /// documented `Token::id_or_else` dispatch with the base's by-name
    /// lookup.
    pub(crate) fn prop_key_id_resolved(&self, text: &str, token: Token) -> Option<u32> {
        self.live_token(token)
            .id_or_else(|| self.base.prop_key_id(text))
    }

    /// [`Self::node_prop`] reading a plan-resolved key: no interner lookup
    /// per call on the snapshot arm. An `Unresolved` key falls back to the
    /// string lookup and an `Absent` one answers `None` without a store
    /// read — unless the scratch was rebased past this view's pin
    /// ([`Self::prop_key_id_resolved`]); the scratch arm is string-keyed
    /// either way.
    pub(crate) fn node_prop_resolved(
        &self,
        id: NodeId,
        (name, token): &relate_names::NamedToken,
    ) -> Option<Value> {
        name.with_str(|text| {
            if *token == Token::Unresolved {
                return self.node_prop(id, text);
            }
            self.node_prologue(id, |n| n.properties.get(text).cloned())
                .resolve(None, || {
                    self.prop_key_id_resolved(text, *token)
                        .and_then(|kid| self.base.node_prop_id_at(id, self.snapshot_ts(), kid))
                })
        })
    }

    /// [`Self::node_prop_resolved`] for a relationship.
    pub(crate) fn rel_prop_resolved(
        &self,
        id: RelId,
        (name, token): &relate_names::NamedToken,
    ) -> Option<Value> {
        name.with_str(|text| {
            if *token == Token::Unresolved {
                return self.rel_prop(id, text);
            }
            self.rel_prologue(id, |r| r.properties.get(text).cloned())
                .resolve(None, || {
                    self.prop_key_id_resolved(text, *token)
                        .and_then(|kid| self.base.rel_prop_id_at(id, self.snapshot_ts(), text, kid))
                })
        })
    }

    /// The label check for a name the store never interned (`Absent`, or an
    /// interned id missing): no committed node carries it, so only a write
    /// overlay can, by string — `None` for an invisible node.
    pub(crate) fn node_has_uninterned_label(&self, id: NodeId, label: &str) -> Option<bool> {
        if self.has_write_overlay() {
            self.node_has_label(id, label)
        } else {
            Some(false)
        }
    }

    /// The scan-predicate gate's one candidate read: `check` sees the
    /// candidate as a [`ScanNode`] — the scratch copy when the overlay
    /// holds one, else the snapshot's visible live record — and its
    /// verdict settles the candidate. An invisible or scratch-deleted
    /// node rejects without `check` running, the same verdict
    /// `node_exists`-gated paths give.
    pub(crate) fn node_scan_check(
        &self,
        id: NodeId,
        check: impl Fn(ScanNode<'_>) -> Result<bool>,
    ) -> Result<bool> {
        self.node_prologue(id, |n| check(ScanNode::Scratch(n)))
            .resolve(Ok(false), || {
                self.base
                    .with_visible_node(id, self.snapshot_ts(), |live| check(ScanNode::Live(live)))
                    .unwrap_or(Ok(false))
            })
    }

    pub fn rel_prop(&self, id: RelId, key: &str) -> Option<Value> {
        self.rel_prologue(id, |r| r.properties.get(key).cloned())
            .resolve(None, || self.base.rel_prop_at(id, self.snapshot_ts(), key))
    }

    pub fn node_prop_keys(&self, id: NodeId) -> Vec<String> {
        self.node_prologue(id, |n| n.properties.keys().cloned().collect())
            .resolve(Vec::new(), || {
                self.base.node_prop_keys_at(id, self.snapshot_ts())
            })
    }

    pub fn rel_prop_keys(&self, id: RelId) -> Vec<String> {
        self.rel_prologue(id, |r| r.properties.keys().cloned().collect())
            .resolve(Vec::new(), || {
                self.base.rel_prop_keys_at(id, self.snapshot_ts())
            })
    }
}

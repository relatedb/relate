//! The shared plumbing of the retained-history object families
//! (ADR-0610): the `transactions/` and `chains/` directories of
//! lsn-windowed JSON parts a checkpoint publishes and an open loads,
//! codec-agnostic. Each family keeps its own part body, publish policy
//! and load fold in its own module; this module owns the plumbing both
//! folds consult — the family's identity ([`PartKind`]), the part-name
//! grammar, the directory listing and its covered tip, the collapse's
//! deletion, and the bounded row batch both publishes stream through
//! ([`RowBatch`]) — parameterized by the family, so each family module
//! names one [`PartFamily`] and the grammar cannot drift between them.

use relate_core::{Error, Result};

use crate::StoreTarget;

/// The retained-history object families (ADR-0610): the discriminant
/// carrying a family's identity — the directory its parts live under
/// and the family the store's one object-name classification
/// ([`crate::snapshot::object_name`]) resolves them to — so a consumer
/// enumerates [`Self::ALL`] and asks the kind, never one family by
/// hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PartKind {
    /// The `transactions/` retained transaction-record family.
    Transactions,
    /// The `chains/` retained version-chain family.
    Chains,
}

impl PartKind {
    /// Both retained-history families — the one list the store-level
    /// enumerators (the object-name classification) iterate, so a new
    /// family lands in the list and every enumerator derives.
    pub(crate) const ALL: [PartKind; 2] = [PartKind::Transactions, PartKind::Chains];

    /// The family's directory — the one spelling of its name every
    /// path consults (never re-derived beside it).
    pub(crate) fn dir(self) -> &'static str {
        match self {
            PartKind::Transactions => "transactions",
            PartKind::Chains => "chains",
        }
    }

    /// The noun the families' shared size class names this family's
    /// parts by in its over-bound refusal
    /// ([`crate::snapshot::bounded_read_for`]).
    pub(crate) fn part_noun(self) -> &'static str {
        match self {
            PartKind::Transactions => "retained-transaction",
            PartKind::Chains => "retained-chain",
        }
    }

    /// The family itself — the [`PartFamily`] the directory-parameterized
    /// helpers bind to this kind.
    pub(crate) const fn family(self) -> PartFamily {
        PartFamily::new(self)
    }
}

/// One part of a family's listing: the object's name with its parsed
/// lsn window — the listing's one row, so a consumer reads the window
/// the listing already proved ([`PartFamily::covered_hi`], the loads'
/// `hi` filter) instead of re-parsing the name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Part {
    /// The part's object name, `<dir>/<lo>-<hi>.json`.
    pub(crate) name: String,
    /// The part's covered lsn window, inclusive on both ends.
    pub(crate) lo: u64,
    pub(crate) hi: u64,
}

/// One lsn-windowed JSON part family (ADR-0610): the family's identity
/// plus the directory-parameterized helpers every such family consults.
/// Constructed once per family, in the family's own module — the one
/// place its [`PartKind`] is spelled.
pub(crate) struct PartFamily {
    /// The family's identity — its directory and object-family
    /// classification in one discriminant.
    kind: PartKind,
}

impl PartFamily {
    /// The family whose parts live under `kind`'s directory.
    pub(crate) const fn new(kind: PartKind) -> Self {
        Self { kind }
    }

    /// A part's object name, `<dir>/<lo>-<hi>.json` with both bounds
    /// zero-padded to the u64 decimal width so the directory listing
    /// sorts by window. The covered span is inclusive on both ends —
    /// the `transactions/` family's records' own first and last lsn,
    /// the `chains/` family's checkpoint window.
    pub(crate) fn part_name(&self, lo: u64, hi: u64) -> String {
        format!("{}/{lo:020}-{hi:020}.json", self.kind.dir())
    }

    /// Parse a `<dir>/<lo>-<hi>.json` object name; `None` for anything
    /// else — the family's one name grammar, so the bounded-read class,
    /// the load and the family's own deletes all share it. A name is a
    /// candidate only with both bounds present, non-padded, and
    /// non-decreasing.
    pub(crate) fn parse_part_name(&self, path: &str) -> Option<(u64, u64)> {
        let file = path.strip_prefix(&format!("{}/", self.kind.dir()))?;
        let stem = file.strip_suffix(".json")?;
        let (lo, hi) = stem.split_once('-')?;
        if lo.len() != 20 || hi.len() != 20 {
            return None;
        }
        if !lo.bytes().chain(hi.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let lo: u64 = lo.parse().ok()?;
        let hi: u64 = hi.parse().ok()?;
        (hi >= lo).then_some((lo, hi))
    }

    /// Every part under the family directory with its parsed lsn
    /// window, sorted by window — the listing proves the grammar once,
    /// so its consumers read the window off the row instead of
    /// re-parsing the name.
    pub(crate) fn parts(&self, target: &StoreTarget) -> Result<Vec<Part>> {
        if !target
            .vfs
            .exists(self.kind.dir())
            .map_err(|e| Error::storage(e.to_string()))?
        {
            return Ok(Vec::new());
        }
        let mut parts = Vec::new();
        for entry in target
            .vfs
            .list_dir(self.kind.dir())
            .map_err(|e| Error::storage(e.to_string()))?
        {
            let path = format!("{}/{}", self.kind.dir(), entry);
            if let Some((lo, hi)) = self.parse_part_name(&path) {
                parts.push(Part { name: path, lo, hi });
            }
        }
        parts.sort_by_key(|part| (part.lo, part.hi));
        Ok(parts)
    }

    /// The family's covered tip: the highest existing part's `hi`, zero
    /// when the family holds no part — parts are append-only and
    /// lsn-ascending, so the last window in the sorted listing bounds
    /// everything the family already holds (a crashed checkpoint's part
    /// included: its window is the true delta of its interval, so the
    /// next checkpoint builds on it). Both families' publishes consult
    /// this one helper, so the covered-window policy lands once.
    pub(crate) fn covered_hi(&self, target: &StoreTarget) -> Result<u64> {
        Ok(self.parts(target)?.last().map(|part| part.hi).unwrap_or(0))
    }

    /// Drop every part in the family — the durable half of the collapse
    /// (ADR-0610). The parts come from the family's own listing
    /// ([`Self::parts`]); the drop consults the names alone, the
    /// windows stay the listing's. A native directory never deletes
    /// through an alias: a candidate that does not metadata as a plain
    /// file is skipped, never followed ([`plain_candidate`]).
    pub(crate) fn drop_family(&self, target: &StoreTarget, parts: &[Part]) -> Result<()> {
        for part in parts {
            if !plain_candidate(target, &part.name) {
                continue;
            }
            target
                .vfs
                .remove_file(&part.name)
                .map_err(|e| Error::storage(e.to_string()))?;
        }
        Ok(())
    }
}

/// The part envelopes' closing bytes — both families' parts end their
/// row array and the object the same way, so the one spelling lives
/// here beside the row appender whose every part closes with it.
pub(crate) const PART_CLOSE: &[u8] = b"]}";

/// Whether the `root`-relative `name` metadata as a plain file, never
/// an alias — the one deletion-safety predicate the retained-history
/// families' collapse drop and the orphan sweeps share: a candidate
/// that is not a plain file is skipped, never followed.
pub(crate) fn plain_file(root: &std::path::Path, name: &str) -> bool {
    std::fs::symlink_metadata(root.join(name))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Whether `name` is a deletion candidate on `target` — the
/// native-root Option spelled once here instead of at every caller: a
/// store with no native root deletes through the VFS as always, a
/// native directory deletes only a candidate that metadata as a plain
/// file ([`plain_file`]). The retained-history collapse drop and the
/// orphan sweeps' entry guard both read this one predicate.
pub(crate) fn plain_candidate(target: &StoreTarget, name: &str) -> bool {
    match &target.native_root {
        Some(root) => plain_file(root, name),
        None => true,
    }
}

/// What one flushed batch left behind — the family's half of the
/// flush, and the one bit of family policy the shared appender
/// carries: a `transactions/` batch is a whole part (its rows end with
/// it, so the next part's first row appends bare), a `chains/` batch
/// is one chunk of the interval's single part (the row list spans
/// flushes, so a flushed row's successor still appends with its comma).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Flushed {
    /// The batch was a whole part: the comma state resets with the
    /// buffer, so the next part's rows start their own list.
    Part,
    /// The batch was a chunk of one continuing part: the comma state
    /// spans the flush, so the rows after it continue the same list.
    Chunk,
}

/// The family half of a [`RowBatch`]: what the appender hands its
/// family when the batch fills — the family appends its own close
/// fragment (if its parts close per batch), writes the bytes wherever
/// its part lives (a `put_object` part for `transactions/`, the next
/// chunk of the open part for `chains/`), and answers [`Flushed`] so
/// the comma discipline is stated once.
pub(crate) trait FlushSink {
    fn flush_batch(&mut self, bytes: &mut Vec<u8>) -> Result<Flushed>;
}

/// The family publish's bounded row batch (ADR-0555): the one row
/// appender both retained-history families stream through. The
/// appender owns the buffer, the comma state and the budget check —
/// serialize the row, flush the batch through the family's sink before
/// the row would cross [`crate::FAMILY_PUBLISH_BATCH_BYTES`], keep the
/// comma between rows — so a publish's peak memory is one batch
/// whatever the interval's dirty delta. Each family supplies its
/// envelope fragments around the rows and its [`FlushSink`].
pub(crate) struct RowBatch {
    /// The batch's serialized bytes: the family's envelope fragments
    /// and its comma-separated rows — byte-identical to the whole-body
    /// `serde_json::to_vec` of the former one-shot encodes, so the
    /// loads' decodes are unchanged.
    bytes: Vec<u8>,
    /// Whether the batch holds a row already — the comma between rows.
    row_open: bool,
}

impl RowBatch {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            row_open: false,
        }
    }

    /// Append one envelope fragment (an array opening, a section
    /// transition, the part's close) and reset the comma state — a
    /// fragment starts a fresh row list, so the row after it appends
    /// without a comma.
    pub(crate) fn fragment(&mut self, fragment: &[u8]) {
        self.bytes.extend_from_slice(fragment);
        self.row_open = false;
    }

    /// Append one row's serialized form behind the comma discipline,
    /// flushing the batch through `sink` first when the row would cross
    /// the batch budget. `open` is the family's part-opening fragment,
    /// written when this append starts a fresh buffer (after any
    /// flush) — a family whose envelope opens once per part (`chains/`,
    /// across the batch flushes) passes an empty fragment and opens
    /// through its own once-flags instead. Returns whether this append
    /// opened a fresh buffer — the family's cue to reset its part
    /// naming.
    pub(crate) fn row<T: serde::Serialize>(
        &mut self,
        open: &[u8],
        value: &T,
        sink: &mut dyn FlushSink,
    ) -> Result<bool> {
        let row = serde_json::to_vec(value).map_err(|e| {
            Error::storage(format!("retained-history row serialization failed: {e}"))
        })?;
        if !self.bytes.is_empty()
            && self.bytes.len() + row.len() > crate::FAMILY_PUBLISH_BATCH_BYTES
        {
            self.flush(sink)?;
        }
        let opened = self.bytes.is_empty();
        if opened {
            self.bytes.extend_from_slice(open);
        }
        if self.row_open {
            self.bytes.push(b',');
        }
        self.bytes.extend_from_slice(&row);
        self.row_open = true;
        Ok(opened)
    }

    /// Flush the batch through `sink` — a no-op on an empty batch, so
    /// a fully covered interval publishes nothing and a final
    /// fragment-only batch still lands.
    pub(crate) fn flush(&mut self, sink: &mut dyn FlushSink) -> Result<()> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let flushed = sink.flush_batch(&mut self.bytes)?;
        self.bytes.clear();
        match flushed {
            Flushed::Part => self.row_open = false,
            Flushed::Chunk => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Part;
    use crate::{checkpoint_at, parse_uri_with_options, SnapshotCodecKind};
    use relate_executor::Graph;

    /// The collapse's deletion through both families (ADR-0610): an
    /// off-preview checkpoint over an empty retained table drops the
    /// `transactions/` family (empty table) and the `chains/` family
    /// (retention off) in one [`checkpoint_at`] — and a candidate that
    /// is not a plain file (a part-named alias to a victim outside the
    /// store) is skipped, never followed: the plain parts go, the alias
    /// and its target survive.
    #[cfg(unix)]
    #[test]
    fn the_collapse_drop_skips_non_plain_candidates_in_both_families() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let store = parse_uri_with_options(
            dir.path().join("db").to_str().unwrap(),
            None,
            Some(SnapshotCodecKind::Json),
        )
        .unwrap();
        // The victim outside the store both families' aliases point at.
        let victim = dir.path().join("victim.json");
        std::fs::write(&victim, b"{\"outside\":true}").unwrap();
        let root = store.filesystem_dir().unwrap().to_path_buf();
        for family in [&crate::transactions::FAMILY, &crate::chains::FAMILY] {
            // One plain part the drop deletes, one grammar-named alias
            // it must refuse to follow.
            crate::snapshot::put_object(&store, &family.part_name(1, 4), b"{}").unwrap();
            symlink(&victim, root.join(family.part_name(5, 9))).unwrap();
        }
        let graph = Graph::default();
        checkpoint_at(&store, &graph, 4, graph.commit_ts(), None).unwrap();
        for family in [&crate::transactions::FAMILY, &crate::chains::FAMILY] {
            let parts = family.parts(&store).unwrap();
            assert_eq!(
                parts,
                vec![Part {
                    name: family.part_name(5, 9),
                    lo: 5,
                    hi: 9
                }],
                "the plain parts are dropped, the alias is skipped"
            );
        }
        // Never followed: both families' drops leave the victim's bytes
        // alone.
        assert_eq!(std::fs::read(&victim).unwrap(), b"{\"outside\":true}");
    }
}

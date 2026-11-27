//! The ordered window — the bounded top-k container an `ORDER BY ...
//! LIMIT` clause folds its producer into (ADR-0555 §3). Split out of
//! `order.rs` when the family grew past a third of it: the sort's own
//! vocabulary (keys, parking, comparison, paging) stays there, the
//! container and its charge protocol live here.

use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;

use relate_core::{Result, Value};

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::order::{cmp_sort_keys, fill_parked_keys, OrderKeys};

/// One entry of a [`TopKWindow`]: the parked keys beside the row they sort,
/// with the arrival sequence that keeps the selection stable.
///
/// `Ord` is the sort's own comparator ([`cmp_sort_keys`] over the parked
/// keys, the sequence as the tiebreak): better sorts *smaller*, so a
/// `BinaryHeap`'s root is the entry the next better row evicts.
struct TopKEntry<'k, 'a> {
    keys: &'k OrderKeys<'a>,
    parked: Vec<Value>,
    seq: usize,
    row: Row,
    /// The entry's landing figure, taken once at construction (the row
    /// solely owned its payloads then) and refunded verbatim on eviction —
    /// re-measuring at eviction would read the shared key payloads as zero.
    bytes: u64,
}

impl PartialEq for TopKEntry<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

impl Eq for TopKEntry<'_, '_> {}

impl PartialOrd for TopKEntry<'_, '_> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopKEntry<'_, '_> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        cmp_sort_keys(self.keys, &self.parked, self.seq, &other.parked, other.seq)
    }
}

/// The bounded form of an ordered window (ADR-0555 §3: "a top-k heap for
/// ordered limits") — the container an `ORDER BY ... LIMIT` (plus its
/// `SKIP`) folds its producer into, row by row, instead of collecting the
/// projection's every output row first. At most `keep` entries are ever
/// held: past capacity the newcomer replaces the worst entry only when it
/// sorts before it, and is dropped otherwise.
///
/// The window is the retaining container and charges what it holds
/// (ADR-0463): each entry's row and key columns are added on insert and
/// subtracted on eviction, so the running charge is the live window, never
/// the input. The projection's per-row charge (`plain_rows`'s
/// `retain_bytes`) does not exist on this path — the window is that
/// charge's bounded replacement.
pub(crate) struct TopKWindow<'k, 'a> {
    keys: &'k OrderKeys<'a>,
    keep: std::num::NonZeroUsize,
    seq: usize,
    /// Worst entry at the root — the one the next better row evicts.
    heap: BinaryHeap<TopKEntry<'k, 'a>>,
    /// The comparison scratch: one key vector refilled per offered row, so
    /// a row the window *rejects* — the overwhelming majority on a large
    /// input — costs no allocation. While the scratch holds the vector it
    /// is uncharged: its buffer is one key row's worth of slots, a fixed
    /// cost of the container like the heap's own storage. On admission the
    /// filled vector is *moved* into the entry as that entry's parked keys
    /// and is charged with it through [`entry_bytes`]; the next comparison
    /// reallocates a fresh, uncharged scratch.
    scratch: Vec<Value>,
    /// The live window's bytes — the one source of its size. The charge
    /// tracks them whether or not a workspace ledger is installed, so the
    /// default-on per-allocation cap reads the same figure the ledger
    /// admission does ([`relate_core::tokens::WorkspaceCharge::bytes`]).
    charge: relate_core::tokens::WorkspaceCharge,
}

impl<'k, 'a> TopKWindow<'k, 'a> {
    /// `keep` entries at most. The capacity is a [`std::num::NonZeroUsize`], so the
    /// "at least one entry" invariant is the type's rather than a promise
    /// the caller keeps: a zero-capacity page is the clause's own decision
    /// ([`super::classification::ProjectionClass::window_keep`]) and never
    /// reaches a window. The
    /// charge starts empty and tracks the live window — no reservation to
    /// make yet, so the construction cannot fail.
    pub(crate) fn new(keys: &'k OrderKeys<'a>, keep: std::num::NonZeroUsize) -> Self {
        Self {
            keys,
            keep,
            seq: 0,
            heap: BinaryHeap::new(),
            scratch: Vec::with_capacity(keys.len()),
            charge: relate_core::tokens::WorkspaceCharge::empty(),
        }
    }

    /// Offer one projected row (its keys already parked). Both charges are
    /// settled **before** the window mutates: the prospective total is
    /// computed by peeking the entry the push would evict, so a refusal —
    /// either the per-allocation cap or the workspace ledger — leaves the
    /// heap and the running total exactly as they were.
    pub(crate) fn push(&mut self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        // The landing figure is taken while the row still solely owns its
        // payloads: the key refill below shares each parked payload with the
        // row's own `__ord*` column, and the ownership-aware walk counts a
        // payload only where one holder owns it — measured afterwards, the
        // key payloads would count zero times and the workspace cap would
        // not cover them.
        let bytes = entry_bytes(&row, self.keys.len());
        fill_parked_keys(self.keys, &row, &mut self.scratch);
        // The prospective state: a window with room keeps the entry; a full
        // one keeps it only when it sorts before the worst held, and then
        // evicts that worst. A newcomer that loses to the worst is dropped
        // uncharged, with the window untouched — and, since it is compared
        // from the scratch, without allocating.
        // One reading of the occupancy: the window is full or it is not, and
        // both the eviction figure and the pop below read that one answer.
        let full = self.heap.len() == self.keep.get();
        let evicted_bytes = if full {
            let worst = self.heap.peek().expect("a full window");
            if !cmp_sort_keys(self.keys, &self.scratch, self.seq, &worst.parked, worst.seq).is_lt()
            {
                // The offered row departs uncharged, so its key clones must
                // depart with it: `clear` keeps the buffer, drops the values.
                self.scratch.clear();
                return Ok(());
            }
            worst.bytes
        } else {
            0
        };
        // Every charge reads the same figure: the window's steady state once
        // this entry is in and the evicted one is out. Checking the
        // pre-eviction total here would refuse a row whose steady state fits.
        let net = self.charge.bytes() + bytes - evicted_bytes;
        // Both admissions settle before the window mutates. A refusal is one
        // exit, and it takes the offered row's clones with it like every
        // other — the row departs uncharged, so nothing of it may outlive it.
        if let Err(error) = graph
            .check_expression_allocation(net)
            .and_then(|()| self.charge.resize(net))
        {
            self.scratch.clear();
            return Err(error);
        }
        // Admitted — only now does the window mutate, and the eviction
        // happens first so the heap never holds `keep + 1` entries. The
        // filled scratch moves into the entry; the evicted entry's own key
        // vector drops with it, its bytes already refunded.
        if full {
            self.heap.pop().expect("the window is over capacity");
        }
        let parked = std::mem::take(&mut self.scratch);
        let entry = TopKEntry {
            keys: self.keys,
            parked,
            seq: self.seq,
            row,
            bytes,
        };
        self.seq += 1;
        self.heap.push(entry);
        Ok(())
    }

    /// Offer a candidate by its keys alone: `fill` writes the parked values,
    /// in key order, into the window's own comparison scratch, and `build`
    /// makes the full row (items, then these keys parked) and runs only when
    /// the keys would enter the window. A candidate the window rejects costs
    /// its keys and nothing else: no row, no projection, and no allocation
    /// (the scratch is reused). The admission and the charge are
    /// [`Self::push`]'s own, run on the built row, so the window ends in the
    /// state `push` reaches.
    pub(crate) fn push_keyed(
        &mut self,
        graph: &ExecGraph<'_>,
        fill: impl FnOnce(&mut Vec<Value>) -> Result<()>,
        build: impl FnOnce(Vec<Value>) -> Result<Row>,
    ) -> Result<()> {
        let mut keys = std::mem::take(&mut self.scratch);
        keys.clear();
        let filled = fill(&mut keys);
        if filled.is_ok() && self.is_full() {
            let worst = self.heap.peek().expect("a full window");
            if !cmp_sort_keys(self.keys, &keys, self.seq, &worst.parked, worst.seq).is_lt() {
                keys.clear();
                self.scratch = keys;
                return Ok(());
            }
        }
        if let Err(error) = filled {
            keys.clear();
            self.scratch = keys;
            return Err(error);
        }
        self.push(graph, build(keys)?)
    }

    /// Whether the window holds its `keep` entries.
    pub(crate) fn is_full(&self) -> bool {
        self.heap.len() == self.keep.get()
    }

    /// The live window's charged bytes — the entry figure the window's own
    /// charge tracks (PROFILE's terminal-retention fact for a window sink).
    pub(crate) fn retained_bytes(&self) -> u64 {
        self.charge.bytes()
    }

    /// The held rows, best first — at most `keep`, already in the sort's
    /// order, so the finish tail sorts nothing on this shape. The window's
    /// charge is released with it; the rows are the caller's.
    pub(crate) fn finish(self) -> Vec<Row> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| entry.row)
            .collect()
    }
}

/// One entry's landing figure: the row's retained payloads and its buffer
/// slots — [`crate::overlay::RetainedTotal::add_row`]'s spelling for the
/// payloads, so both
/// containers price a row's heap identically — plus everything the window
/// holds beside it: the parked key columns the row does not carry, the
/// value buffer they and the row's slots share, and the heap element the
/// entry itself occupies. A window admitted under a large `LIMIT` must not
/// hold materially more than the total it passed.
fn entry_bytes(row: &Row, key_columns: usize) -> u64 {
    let values = row.values();
    // Each held allocation priced once: the row's landing figure (its
    // payload heap and its value slots — the same figure a retained `Vec`
    // charges per row), the parked key vector's slots, and the heap element
    // the entry occupies — which already carries the row's and the vectors'
    // inline headers, so no `size_of::<Row>()` is added beside it.
    crate::workspace::row_landing_bytes(values)
        + crate::workspace::value_buffer_bytes(key_columns)
        + std::mem::size_of::<TopKEntry<'_, '_>>() as u64
}

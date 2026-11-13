//! The group table: the index from a group's key to its slot, the groups'
//! seeds, and the groups' accumulators — all flat, all indexed by group.
//!
//! A group is one entry in the index (its fixed-size [`KeyTuple`]), one
//! slot of the seed store (only the first-row columns the finish reads,
//! [`SeedStore`]) and one slot in each accumulator column
//! ([`AccTable`]). Groups are appended in landing order and never removed,
//! so the index order is the observable emission order.
//!
//! # What a group costs
//!
//! [`GroupBill`] charges each landing once, from the real layout, and that
//! one figure feeds both the expression budget's running total and the
//! statement's workspace ledger. Per group:
//!
//! * the index entry: the key tuple (24 bytes), its 8-byte hash and the
//!   8-byte table slot, under the reservation model's doubling;
//! * the key's heap: nothing for a word key (null, integer, node id, …),
//!   one 64-byte tree plus its payload for a string/list/map/NaN part, and
//!   16 bytes a part for a multi-part key's slice;
//! * the seed: the first-seen key values the items read and the kept
//!   first-row columns, 24 bytes each plus any payload the group solely
//!   owns — or, in the whole-row mode, the row;
//! * the accumulators: eight bytes for `count(*)`, a streaming fold and
//!   the bytes it holds for `sum`/`avg`/`min`/`max`/`count(x)`, and the
//!   retained operands of `collect`/percentiles as they settle.
//!
//! So `WITH v, count(*) AS n` over node keys costs 88 bytes a group before
//! the reservation model's doubling (the key's 16, the kept node's 24, the
//! counter's 8, the entry's 40), however many other columns the input rows
//! carry.

use indexmap::map::Entry;
use indexmap::IndexMap;
use relate_core::tokens::{peak_hold, WorkspaceCharge};
use relate_core::Result;

use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;
use crate::workspace::{buffer_growth, RetainedRows};

use super::columns::AccTable;
use super::key::{KeyTuple, KeyValues};
use super::seed::SeedStore;

/// Bytes of one index entry: the key tuple, its cached hash, and the
/// hash table's index slot.
const INDEX_ENTRY: u64 = (size_of::<KeyTuple>() + 2 * size_of::<usize>()) as u64;

/// The one bill for the aggregation's retained bytes: the group table
/// (index, keys, seeds, and accumulator slots) and, as rows settle, what
/// retaining accumulators keep (`collect` operands, percentile samples,
/// the payload a `min`/`max` holds, parked errors). The expression
/// budget's running total and the workspace ledger are fed the same figure.
pub(super) struct GroupBill {
    /// The aggregation's one retaining container: the group table and
    /// what its retaining accumulators keep grow it together.
    pub(super) retained: RetainedTotal,
    /// The workspace-ledger twin of the table bytes billed so far (its own
    /// [`WorkspaceCharge::bytes`] is the running total); `None` when
    /// workspace accounting is off.
    workspace: Option<WorkspaceCharge>,
}

/// The one bill for the input vector the fold holds: its measured bytes
/// and the workspace-ledger charge that follows them down as each row's
/// payload moves into the group table.
pub(super) struct InputBill {
    retained: RetainedRows,
    charge: WorkspaceCharge,
}

impl InputBill {
    /// Bill the whole input vector up front.
    pub(super) fn open(rows: &[Row]) -> Result<Self> {
        let retained = RetainedRows::measure(rows);
        let charge = WorkspaceCharge::retain(retained.bytes())?;
        peak_hold("aggregation");
        Ok(Self { retained, charge })
    }

    /// Release `row`'s share: its payload moves on to the group table.
    pub(super) fn release(&mut self, row: &Row) -> Result<()> {
        self.retained.release_row(row);
        self.charge.resize(self.retained.bytes())
    }
}

impl GroupBill {
    pub(super) fn open(graph: &ExecGraph<'_>, account: bool) -> Result<Self> {
        Ok(Self {
            retained: graph.retain_bytes(0)?,
            workspace: account.then(WorkspaceCharge::empty),
        })
    }

    /// Bill `bytes` of retained aggregation state, ahead of the landing or
    /// the append so a refusal never leaves that state retained. Group
    /// landings and accumulator settles share this path, so a `collect`
    /// payload reserves the same ledger bytes the expression cap already
    /// counts.
    pub(super) fn add(&mut self, graph: &ExecGraph<'_>, bytes: u64) -> Result<()> {
        self.retained.add_bytes(graph, bytes)?;
        if let Some(charge) = &mut self.workspace {
            charge.resize(charge.bytes().saturating_add(bytes))?;
            peak_hold("aggregation");
        }
        Ok(())
    }

    /// Test settle drives a bill that already owns the expression total
    /// and does not touch the workspace ledger. The placeholder left in
    /// `total` is replaced when the settle returns the real total.
    #[cfg(test)]
    pub(super) fn take_retained(total: &mut RetainedTotal, graph: &ExecGraph<'_>) -> Result<Self> {
        Ok(Self {
            retained: std::mem::replace(total, graph.retain_bytes(0)?),
            workspace: None,
        })
    }

    /// Put the expression total back where the test settle found it.
    #[cfg(test)]
    pub(super) fn return_retained(self, total: &mut RetainedTotal) {
        *total = self.retained;
    }
}

/// Every group of one aggregation.
pub(super) struct GroupTable<'e> {
    pub(super) index: IndexMap<KeyTuple, ()>,
    pub(super) seeds: SeedStore,
    pub(super) accs: AccTable<'e>,
    /// The keyless group seeded before any row arrives (the ADR-0453
    /// phantom group of an empty input) has no seed until its first row
    /// lands as one. Only that group is ever seedless.
    pub(super) phantom: bool,
}

/// How an offered row's key met the table.
pub(super) enum Landing {
    /// An existing group (its index) and whether this row becomes its
    /// seed (only the keyless group's first row does).
    Existing { group: usize, as_seed: bool },
    /// A new group's index.
    New { group: usize },
}

impl<'e> GroupTable<'e> {
    pub(super) fn new(seeds: SeedStore, accs: AccTable<'e>) -> Self {
        Self {
            index: IndexMap::new(),
            seeds,
            accs,
            phantom: false,
        }
    }

    /// Bytes of the index entry the next group adds.
    fn entry_bytes(&self) -> u64 {
        let n = self.index.len();
        buffer_growth(n, n + 1, INDEX_ENTRY)
    }

    /// Seed the keyless group before any row arrives: one empty-keyed
    /// entry and its accumulator slots, billed through the same sequence
    /// a row landing pays.
    pub(super) fn open_keyless(
        &mut self,
        graph: &ExecGraph<'_>,
        bill: &mut GroupBill,
    ) -> Result<()> {
        bill.add(graph, self.entry_bytes() + self.accs.landing_bytes())?;
        self.accs.open_group();
        self.index.insert(KeyTuple::Many(Box::new([])), ());
        self.phantom = true;
        Ok(())
    }

    /// Look `key` up and bill what `row` adds: a new group's entry, key,
    /// seed and accumulator slots, or — for the keyless group's first row —
    /// its seed. A row joining an occupied group adds nothing: the group
    /// keeps constant-size accumulators, never its rows.
    pub(super) fn land(
        &mut self,
        graph: &ExecGraph<'_>,
        bill: &mut GroupBill,
        key: KeyTuple,
        keys: &KeyValues,
        row: &Row,
    ) -> Result<Landing> {
        let entry_bytes = self.entry_bytes();
        match self.index.entry(key) {
            Entry::Occupied(entry) => {
                let group = entry.index();
                let as_seed = self.phantom;
                if as_seed {
                    bill.add(graph, self.seeds.landing_bytes(0, keys, row)?)?;
                }
                Ok(Landing::Existing { group, as_seed })
            }
            Entry::Vacant(entry) => {
                let group = entry.index();
                let bytes = entry_bytes
                    .saturating_add(entry.key().heap_bytes())
                    .saturating_add(self.seeds.landing_bytes(group, keys, row)?)
                    .saturating_add(self.accs.landing_bytes());
                bill.add(graph, bytes)?;
                entry.insert(());
                self.accs.open_group();
                Ok(Landing::New { group })
            }
        }
    }
}

//! The store's build chain, one home (ADR-0665): the row-space
//! admission, the one graph walk that gathers the columns, the columnar
//! table it seals into, and the single-column array builders the table
//! composes — everything that turns a graph into the sealed file the
//! store reads.

use relate_core::{Error, NodeId, Result};
use rustc_hash::FxHashMap;

use vortex::array::arrays::primitive::PrimitiveArray;
use vortex::array::arrays::struct_::StructArray;
use vortex::array::arrays::varbin::VarBinArray;
use vortex::array::validity::Validity;
use vortex::array::ArrayRef;
use vortex::array::IntoArray;
use vortex::buffer::{Buffer, ByteBuffer};
use vortex::file::{OpenOptionsSessionExt, VortexFile, WriteOptionsSessionExt};
use vortex::io::runtime::current::CurrentThreadRuntime;
use vortex::io::runtime::BlockingRuntime;
use vortex::io::session::RuntimeSessionExt;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

use super::{column_name, vortex_error, BitSet, Column, ColumnState, Graph};

/// The struct's one guaranteed child, so a property-less graph still
/// seals a valid one-field struct — nothing reads it back (the surviving
/// rows live in `gather`'s in-memory visibility bitset; the decode
/// projects the key columns by name).
const ROW_COLUMN: &str = "__row";

/// The store's dense-structure budget: every carried key allocates one
/// column sized by the ROW COUNT (and every label one bitset), so a
/// crafted graph whose distinct-key count meets its node count — each node
/// carrying one unique key — would reserve O(rows × keys) cells at open,
/// an amplification the row-space bound cannot see (the compact records
/// hold only the present cells; the columns are dense). The budget bounds
/// the product; a real graph's columns sit orders of magnitude below it.
const MAX_DENSE_CELLS: usize = 1 << 24;

/// What one graph walk collects: the visible rows, the per-label bitsets and
/// the gathered columns — all from the same `all_node_ids_at` sweep.
pub(super) struct Gathered {
    pub(super) visible: BitSet,
    pub(super) label_bits: FxHashMap<u32, BitSet>,
    pub(super) columns: FxHashMap<u32, Column>,
}

/// The store's one admission check, owning its own derivation: the
/// row-indexed layout (id = row) allocates by the id SPACE, not the live
/// count — every column and bitset is sized by the max id, so a
/// client-inflated sparse space (ids far past the live count) would size
/// the tables by the attacker's number. Refuse the attach — the opt-in
/// asked for this store and the graph's shape cannot serve it — rather
/// than allocate the inflated tables. The bound tolerates deletion gaps
/// (4× the live count) plus a fixed 64k headroom for small graphs; a
/// dense row remap that decouples rows from ids is the recorded
/// follow-up (the typed-column rung). Returns the admitted row count,
/// so the caller cannot hand the guard a mismatched pair.
pub(super) fn row_space(ids: &[NodeId]) -> Result<usize> {
    let row_count = ids.iter().copied().max().map_or(0, |id| id + 1) as usize;
    let live = ids.len();
    if row_count > live.saturating_mul(4).max(1 << 16) {
        return Err(Error::storage(format!(
            "the graph's node-id space ({row_count} rows over {live} live nodes) \
             is too sparse for the store's row-indexed layout"
        )));
    }
    Ok(row_count)
}

/// Walk the graph once: visible rows, per-label bitsets, gathered columns.
/// Fallible: every new dense structure (a key's column, a label's bitset)
/// is checked against [`MAX_DENSE_CELLS`] as it appears, so the crafted
/// sparse-keys amplification bails at the first over-budget allocation
/// instead of reserving it.
pub(super) fn gather(
    graph: &Graph,
    ts: u64,
    ids: Vec<NodeId>,
    row_count: usize,
) -> Result<Gathered> {
    let mut visible = BitSet::new(row_count);
    let mut label_bits: FxHashMap<u32, BitSet> = FxHashMap::default();
    let mut columns: FxHashMap<u32, Column> = FxHashMap::default();
    for id in ids {
        let row = id as usize;
        visible.set(row);
        // `all_node_ids_at` yields exactly the ids `node_at` resolves at
        // the same `ts` (the same visibility predicate) — the miss cannot
        // fire, stated rather than silently skipped.
        let node = graph
            .node_at(id, ts)
            .expect("all_node_ids_at yields visible ids");
        for label in node.labels.iter() {
            if let Some(lid) = graph.interned_id(label) {
                if !label_bits.contains_key(&lid) {
                    admit_dense(row_count, label_bits.len() + columns.len() + 1)?;
                }
                label_bits
                    .entry(lid)
                    .or_insert_with(|| BitSet::new(row_count))
                    .set(row);
            }
        }
        for (name, value) in node.properties.iter() {
            let Some(kid) = graph.interned_id(name) else {
                continue;
            };
            if !columns.contains_key(&kid) {
                admit_dense(row_count, label_bits.len() + columns.len() + 1)?;
            }
            columns
                .entry(kid)
                .or_default()
                .state
                .push(row, value, row_count);
        }
    }
    Ok(Gathered {
        visible,
        label_bits,
        columns,
    })
}

/// One dense structure's admission — the product of the row count and the
/// structure count against [`MAX_DENSE_CELLS`].
pub(super) fn admit_dense(row_count: usize, structures: usize) -> Result<()> {
    let cells = row_count.saturating_mul(structures);
    if cells > MAX_DENSE_CELLS {
        return Err(Error::storage(format!(
            "the graph's {structures} property keys and labels over {row_count} rows \
             exceed the store's dense-column budget"
        )));
    }
    Ok(())
}

/// Build the row column and one child per surviving key, in kid order. The
/// child builder is chosen by the column's own state, so a value of the
/// wrong type has no path into it.
pub(super) fn columnar_table(
    columns: &FxHashMap<u32, Column>,
    row_count: usize,
) -> Result<ArrayRef> {
    let mut kids: Vec<u32> = columns.keys().copied().collect();
    kids.sort_unstable();
    let mut names: Vec<String> = vec![ROW_COLUMN.to_string()];
    let mut children: Vec<ArrayRef> = vec![PrimitiveArray::new(
        Buffer::from((0..row_count as i64).collect::<Vec<_>>()),
        Validity::NonNullable,
    )
    .into_array()];
    for kid in &kids {
        names.push(column_name(*kid));
        children.push(match &columns[kid].state {
            ColumnState::Int(values) => int_column(values),
            ColumnState::Str(values) => str_column(values),
            ColumnState::Empty | ColumnState::Poisoned => {
                return Err(vortex_error(format!(
                    "key {kid} reached the table without a faithful column state"
                )))
            }
        });
    }
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let array = StructArray::try_new(name_refs.into(), children, row_count, Validity::NonNullable)
        .map_err(vortex_error)?;
    Ok(array.into_array())
}

/// Write the table to an in-memory file and open it.
pub(super) fn seal_table(
    array: ArrayRef,
) -> Result<(VortexSession, CurrentThreadRuntime, VortexFile)> {
    let rt = CurrentThreadRuntime::new();
    let session = VortexSession::default().with_handle(rt.handle());
    let mut bytes = Vec::new();
    session
        .write_options()
        .blocking(&rt)
        .write(&mut bytes, array.to_array_iterator())
        .map_err(vortex_error)?;
    let vfile = session
        .open_options()
        .open_buffer(ByteBuffer::copy_from(&bytes))
        .map_err(vortex_error)?;
    Ok((session, rt, vfile))
}

fn int_column(values: &[Option<i64>]) -> ArrayRef {
    let data: Vec<i64> = values.iter().map(|value| value.unwrap_or(0)).collect();
    let valid: Vec<bool> = values.iter().map(Option::is_some).collect();
    PrimitiveArray::new(Buffer::from(data), Validity::from_iter(valid)).into_array()
}

fn str_column(values: &[Option<String>]) -> ArrayRef {
    let data: Vec<Option<&str>> = values.iter().map(|value| value.as_deref()).collect();
    VarBinArray::from_nullable_strs(data).into_array()
}

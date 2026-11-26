//! Rewrite units (ADR-0183). A checkpoint rewrites id ranges as units, not
//! per previous part. It plans over the [`Slot`]s of the kind's layout: a
//! window, or a kept part that overlaps a dirty id, seeds a unit, and the
//! unit absorbs neighboring slots while its id range spans at most
//! `MAX_WINDOW` ids, its kept parts' files hold no more rows and bytes
//! together than one file can, and its live decoded bytes fit the target.
//! Planning cuts each unit into files in the same walk that sizes it: one
//! file for a unit that fits, byte-budgeted files for a lone seed over the
//! target. Each file copies clean rows out of the kept parts' previous
//! shards and encodes the rest. Kept parts outside every unit are kept as
//! is, and an incremental rewrite absorbs no neighbour at all.

mod unit;

use super::layout::{lay_out, Slot};
use super::*;

use std::ops::Range;

use crate::codec::parquet::{file_ceiling, DecodedBudget};
use unit::UnitWriter;

/// Rewrite one kind's previous `parts`, in any order, around its sorted
/// `dirty` ids: lay them out over the sorted live `all_ids` with new
/// windows `w` ids wide, plan the rewrite units and write them. Returns the
/// parts to publish and the replaced parts' paths to retire.
pub(super) fn rewrite(
    ctx: &mut ShardWrite<'_>,
    kind: Kind,
    parts: Vec<ShardPart>,
    all_ids: &[i64],
    dirty: &[i64],
    w: i64,
) -> Result<(Vec<ShardPart>, Vec<String>)> {
    // Decision 1: from here on the slots are disjoint.
    let (slots, mut retired) = lay_out(parts, all_ids, w);
    let mut out = Vec::with_capacity(slots.len());
    for step in plan_units(ctx, kind, &slots, all_ids, dirty)? {
        match step {
            // Decision 4: a part outside every unit is reused verbatim.
            Step::Keep(part) => out.push(part.clone()),
            Step::Rewrite(unit) => {
                let kept: Vec<&ShardPart> =
                    slots[unit.slots].iter().filter_map(Slot::kept).collect();
                out.extend(UnitWriter::new(ctx, kind, &kept, dirty)?.write(&unit.files, all_ids)?);
                retired.extend(kept.into_iter().map(|part| part.path.clone()));
            }
        }
    }
    Ok((out, retired))
}

/// What planning does with each slot, in id order.
pub(super) enum Step<'s> {
    /// Publish a clean kept part as is.
    Keep(&'s ShardPart),
    Rewrite(PlannedUnit),
}

/// A unit's member slots, as indexes into the sorted slots, and the files
/// planning cut it into: id ranges that tile the members' union range, or
/// none when the unit holds no live id.
pub(super) struct PlannedUnit {
    pub(super) slots: Range<usize>,
    pub(super) files: Vec<Range<i64>>,
}

impl Rewrite {
    /// Whether a unit may absorb its free neighbours (ADR-0183 Decision 2):
    /// compaction does, an incremental rewrite never reads a clean shard.
    fn absorbs(self) -> bool {
        match self {
            Self::Coalesce => true,
            Self::Incremental => false,
        }
    }
}

impl ShardWrite<'_> {
    /// The cap a file of more than one row is written into: the target, or
    /// the file ceiling were it lower.
    fn file_cap(&self) -> u64 {
        self.target_bytes.min(file_ceiling())
    }
}

/// Manifest rows and bytes of a unit's kept parts: what their files hold,
/// known without a graph walk.
#[derive(Clone, Copy)]
struct Published {
    rows: u64,
    bytes: u64,
}

impl Published {
    fn of(slot: &Slot) -> Self {
        let part = slot.kept();
        Self {
            rows: part.map_or(0, |part| part.rows),
            bytes: part.map_or(0, |part| part.bytes),
        }
    }

    /// These and `slot`'s together, when they stay within `max`.
    fn join(self, slot: &Slot, max: Self) -> Option<Self> {
        let other = Self::of(slot);
        let rows = self.rows.checked_add(other.rows)?;
        let bytes = self.bytes.checked_add(other.bytes)?;
        (rows <= max.rows && bytes <= max.bytes).then_some(Self { rows, bytes })
    }
}

/// Decisions 2-4: plan `slots`, sorted and disjoint with every live id in
/// one, in id order. A clean kept slot is kept unless a unit absorbs it;
/// every other slot is a seed, and each seed not yet in a unit starts one.
/// One walk measures the seed's live rows and cuts them into files. A seed
/// within the target absorbs free neighbors, left then right, while the
/// unit fits, and is one file; a seed over the target absorbs nothing and
/// keeps its cuts. Seeds are taken left to right, so no slot joins two
/// units. An incremental rewrite never absorbs, so its units are exactly
/// the seeding slots.
pub(super) fn plan_units<'s>(
    ctx: &ShardWrite<'_>,
    kind: Kind,
    slots: &'s [Slot],
    all_ids: &[i64],
    dirty: &[i64],
) -> Result<Vec<Step<'s>>> {
    let live = |range: &Range<i64>| ids_in(all_ids, range);
    // The id width of the union of `slots[lo..hi]`.
    let width = |lo: usize, hi: usize| slots[hi - 1].span().end - slots[lo].span().start;
    // One file's page envelope rows and byte cap.
    let max = Published {
        rows: kind.page_envelope_rows() as u64,
        bytes: ctx.file_cap(),
    };
    let mut steps = Vec::new();
    let mut seed = 0;
    while seed < slots.len() {
        let slot = &slots[seed];
        if let Some(part) = clean(slot, dirty) {
            steps.push(Step::Keep(part));
            seed += 1;
            continue;
        }
        let ids = live(&slot.span());
        let (cuts, mut budget) = cut_seed(ctx, kind, ids)?;
        let (mut lo, mut hi) = (seed, seed + 1);
        if ctx.rewrite.absorbs() && cuts.is_empty() && budget.within() {
            // Decision 2: the merged range spans at most `MAX_WINDOW` ids,
            // the ADR-0022 window bound; its kept parts' files hold no more
            // rows than one file's page envelope and no more bytes than its
            // cap between them, so files that miss together, such as the
            // halves of a file halved on an envelope miss, stay apart; and
            // its live rows fit the budget. The width and the manifest are
            // checked first, so a slot that cannot join is never measured.
            let mut published = Published::of(slot);
            let mut absorbs = |slot: &Slot, width: i64| -> Result<bool> {
                let joined = if width <= MAX_WINDOW {
                    published.join(slot, max)
                } else {
                    None
                };
                match joined {
                    Some(joined) if budget.fill_all(live(&slot.span()))? => {
                        published = joined;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            };
            // A kept step not yet in a unit is the free slot left of `lo`.
            while matches!(steps.last(), Some(Step::Keep(_)))
                && absorbs(&slots[lo - 1], width(lo - 1, hi))?
            {
                steps.pop();
                lo -= 1;
            }
            while hi < slots.len() && absorbs(&slots[hi], width(lo, hi + 1))? {
                hi += 1;
            }
        }
        let range = slots[lo].span().start..slots[hi - 1].span().end;
        let files = if live(&range).is_empty() {
            Vec::new()
        } else {
            tile(range, cuts)
        };
        steps.push(Step::Rewrite(PlannedUnit {
            slots: lo..hi,
            files,
        }));
        seed = hi;
    }
    Ok(steps)
}

/// Cut `ids`, a seed's live ids, into files where the next row would pass
/// the target, the decoded envelope, or the kind's page-envelope row cap
/// (ADR-0534: a wide part's leaf columns make rows the binding bound),
/// measuring each row once. Returns the id each file after the first
/// starts at, and the budget holding the last file.
fn cut_seed<'g>(
    ctx: &ShardWrite<'g>,
    kind: Kind,
    ids: &[i64],
) -> Result<(Vec<i64>, DecodedBudget<'g>)> {
    let mut budget = DecodedBudget::new(ctx.graph, ctx.ts, kind, ctx.target_bytes);
    let cuts = budget.cut(ids)?;
    Ok((cuts, budget))
}

fn span(part: &ShardPart) -> Range<i64> {
    part.id_lo..part.id_hi
}

/// The part of a kept `slot` no dirty id falls in.
fn clean<'s>(slot: &'s Slot, dirty: &[i64]) -> Option<&'s ShardPart> {
    slot.kept()
        .filter(|part| ids_in(dirty, &span(part)).is_empty())
}

/// `range` cut before each of `starts`, ascending ids strictly inside it:
/// the first file starts at `range.start`, each later one at its first id.
fn tile(range: Range<i64>, starts: Vec<i64>) -> Vec<Range<i64>> {
    let mut files = Vec::with_capacity(starts.len() + 1);
    let mut lo = range.start;
    for start in starts {
        files.push(lo..start);
        lo = start;
    }
    files.push(lo..range.end);
    files
}

/// The ids of sorted `ids` in `range`.
fn ids_in<'i>(ids: &'i [i64], range: &Range<i64>) -> &'i [i64] {
    let start = ids.partition_point(|id| *id < range.start);
    let end = start + ids[start..].partition_point(|id| *id < range.end);
    &ids[start..end]
}

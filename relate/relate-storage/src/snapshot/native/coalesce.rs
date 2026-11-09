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
//! is.

use super::layout::{lay_out, Slot};
use super::*;

use std::ops::Range;
use std::sync::Arc;

use crate::codec::parquet::{
    encode_wide_rows, file_ceiling, part_columns, preflight, previous_shard_columns,
    with_part_shard_writer_to, ColumnType, DecodedBudget, ShardCursor, ShardError, ShardWriter,
    VfsChunkReader, MAX_PROMOTED_COLUMNS,
};

/// Rewrite one kind's previous `parts`, in any order, around its sorted
/// `dirty` ids: lay them out over the sorted live `all_ids` with new
/// windows `w` ids wide, plan the rewrite units and write them. Returns the
/// parts to publish and the replaced parts' paths to retire.
pub(super) fn rewrite(
    ctx: &mut ShardWrite<'_>,
    parts: Vec<ShardPart>,
    all_ids: &[i64],
    dirty: &[i64],
    w: i64,
) -> Result<(Vec<ShardPart>, Vec<String>)> {
    // Decision 1: from here on the slots are disjoint.
    let (slots, mut retired) = lay_out(parts, all_ids, w);
    let mut out = Vec::with_capacity(slots.len());
    for step in plan_units(ctx, &slots, all_ids, dirty)? {
        match step {
            // Decision 4: a part outside every unit is reused verbatim.
            Step::Keep(part) => out.push(part.clone()),
            Step::Rewrite(unit) => {
                let kept: Vec<&ShardPart> =
                    slots[unit.slots].iter().filter_map(Slot::kept).collect();
                out.extend(UnitWriter::new(ctx, &kept, dirty)?.write(&unit.files, all_ids)?);
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
/// units.
pub(super) fn plan_units<'s>(
    ctx: &ShardWrite<'_>,
    slots: &'s [Slot],
    all_ids: &[i64],
    dirty: &[i64],
) -> Result<Vec<Step<'s>>> {
    let live = |range: &Range<i64>| ids_in(all_ids, range);
    // The id width of the union of `slots[lo..hi]`.
    let width = |lo: usize, hi: usize| slots[hi - 1].span().end - slots[lo].span().start;
    // One file's page envelope rows and byte cap.
    let max = Published {
        rows: ctx.kind.page_envelope_rows(ctx.encoding.wide()) as u64,
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
        let (cuts, mut budget) = cut_seed(ctx, ids)?;
        let (mut lo, mut hi) = (seed, seed + 1);
        if cuts.is_empty() && budget.within() {
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
fn cut_seed<'g>(ctx: &ShardWrite<'g>, ids: &[i64]) -> Result<(Vec<i64>, DecodedBudget<'g>)> {
    let mut budget = DecodedBudget::new(
        ctx.graph,
        ctx.ts,
        ctx.kind,
        ctx.target_bytes,
        ctx.encoding.wide(),
    );
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

/// The index in `kept` of the part whose previous shard holds clean `id`
/// and the unit copies, or `None` to encode it: `id` is dirty, lies in a
/// window (no previous shard), or belongs to a member the unit
/// re-encodes (ADR-0534). `kept` are sorted and disjoint, so at most one
/// covers `id` — the one predicate every run decision consults.
fn source(kept: &[Member<'_>], dirty: &[i64], id: i64) -> Option<usize> {
    if dirty.binary_search(&id).is_ok() {
        return None;
    }
    let member = kept
        .partition_point(|member| member.part.id_lo <= id)
        .checked_sub(1)?;
    (id < kept[member].part.id_hi && !kept[member].reencode).then_some(member)
}

/// Writes one rewrite unit's planned files (Decision 3), each copying clean
/// rows out of the kept parts' previous shards and encoding the rest,
/// through one cursor. Every file is a wide part (ADR-0534): its columns
/// carried from the parts it copies and promoted from the rows it encodes.
struct UnitWriter<'u, 'a> {
    ctx: &'u mut ShardWrite<'a>,
    /// The parts of the unit's kept slots, whose previous shards hold the
    /// unit's clean rows, each with whether the unit re-encodes it
    /// instead of copying (ADR-0534).
    kept: Vec<Member<'u>>,
    dirty: &'u [i64],
    /// The columns the kept parts' previous shards carry (ADR-0534): a
    /// copied row's values live only through them.
    carried: Vec<(String, ColumnType)>,
    /// The one previous shard held open.
    open: Option<OpenShard>,
}

/// What a unit copies and re-encodes (ADR-0534): the columns its kept
/// parts' previous shards carry — a copied row's values live only through
/// them — and the kept parts, each marked for copy or re-encode.
struct Carried<'p> {
    columns: Vec<(String, ColumnType)>,
    members: Vec<Member<'p>>,
}

/// One kept part of a unit, with the unit's decision about it (ADR-0534).
#[derive(Clone, Copy)]
struct Member<'u> {
    part: &'u ShardPart,
    /// Whether the unit re-encodes this part's rows instead of copying
    /// them: a part whose column disagrees with the unit's carries values
    /// the copy cannot place, and a union wider than the column cap cannot
    /// copy at all. Its rows come from the live graph, like dirty rows.
    reencode: bool,
}

/// The previous shard the cursor reads: that of `kept[member]`.
struct OpenShard {
    member: usize,
    cursor: ShardCursor,
}

impl<'u, 'a> UnitWriter<'u, 'a> {
    fn new(ctx: &'u mut ShardWrite<'a>, kept: &[&'u ShardPart], dirty: &'u [i64]) -> Result<Self> {
        let carried = Self::carried_columns(ctx, kept)?;
        Ok(Self {
            ctx,
            kept: carried.members,
            dirty,
            carried: carried.columns,
            open: None,
        })
    }

    /// The unit's carried columns and its kept parts, each marked for
    /// copy or re-encode (ADR-0534): a wide checkpoint reads every kept
    /// part's schema from its file — its own columns, none for a
    /// legacy-shaped one — and merges them; a part whose column disagrees
    /// with the unit's is re-encoded, its rows reground from the live
    /// graph, and a union wider than the column cap re-encodes every
    /// part, the columns then coming from the encoded rows alone. A
    /// legacy checkpoint reads no footer: it carries none and copies
    /// every part, its parts the kind's fixed schema.
    fn carried_columns<'p>(ctx: &ShardWrite<'_>, kept: &[&'p ShardPart]) -> Result<Carried<'p>> {
        let mut carried = std::collections::BTreeMap::new();
        let mut members = Vec::with_capacity(kept.len());
        for part in kept {
            // Only a wide checkpoint reads the part's schema: a legacy
            // one copies and encodes exactly as before, no footer reads
            // it does not use.
            let columns = if ctx.encoding.wide() {
                previous_shard_columns(
                    Arc::clone(&ctx.target.vfs),
                    &part.path,
                    ctx.kind,
                    span(part),
                )?
            } else {
                Vec::new()
            };
            let mut reencode = false;
            for (key, ty) in &columns {
                if carried.get(key).is_some_and(|held| *held != *ty) {
                    reencode = true;
                    break;
                }
            }
            if !reencode {
                carried.extend(columns);
            }
            members.push(Member { part, reencode });
        }
        if carried.len() > MAX_PROMOTED_COLUMNS {
            carried.clear();
            for member in &mut members {
                member.reencode = true;
            }
        }
        Ok(Carried {
            columns: carried.into_iter().collect(),
            members,
        })
    }

    /// Write the planned `files`, which tile the unit's range, in id order.
    fn write(&mut self, files: &[Range<i64>], all_ids: &[i64]) -> Result<Vec<ShardPart>> {
        let mut out = Vec::with_capacity(files.len());
        for file in files {
            out.extend(self.write_file(file.clone(), ids_in(all_ids, file))?);
        }
        Ok(out)
    }

    /// Write `ids`, the live ids in `file`, as one part. A file of more than
    /// one id goes into a sink capped at the target, so a file over the
    /// target misses its envelope like one over the file ceiling or the
    /// reader's preflight, and is halved. A single row is capped by the file
    /// ceiling alone, so it can be written over the target. Every other
    /// failure fails the checkpoint, the refusal of a file with no rows
    /// included, which `build_file` raises before any halving.
    fn write_file(&mut self, file: Range<i64>, ids: &[i64]) -> Result<Vec<ShardPart>> {
        let one_row = ids.len() == 1;
        let cap = if one_row {
            file_ceiling()
        } else {
            self.ctx.file_cap()
        };
        match self.build_file(&file, ids, cap) {
            Ok(part) => Ok(vec![part]),
            Err(ShardError::Envelope(_)) if !one_row => {
                // Both halves start behind the cursor the missed file left.
                self.open = None;
                self.split_ids(file, ids)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Halve `ids` at its middle id and write each half, halved again on an
    /// envelope miss, like a planned file. `ids` must be at least two,
    /// strictly ascending and inside `file`, as the ids of a file
    /// [`write_file`](Self::write_file) halves are: then the middle id lies
    /// strictly inside `file`, and the two non-empty halves tile it.
    fn split_ids(&mut self, file: Range<i64>, ids: &[i64]) -> Result<Vec<ShardPart>> {
        debug_assert!(ids.len() >= 2 && ids.is_sorted_by(|a, b| a < b));
        debug_assert!(file.start <= ids[0] && ids[ids.len() - 1] < file.end);
        let (left, right) = ids.split_at(ids.len() / 2);
        let mid = right[0];
        let mut out = self.write_file(file.start..mid, left)?;
        out.extend(self.write_file(mid..file.end, right)?);
        Ok(out)
    }

    /// Write `ids` as one file of at most `cap` bytes: runs of copied
    /// clean rows and encoded rows through one writer, never a file per
    /// run. A wide checkpoint's file carries its columns — carried from
    /// the parts it copies plus the promoted keys of the rows it encodes
    /// — decided before the writer opens, the schema the writer and the
    /// batch builders share (ADR-0534); a legacy one keeps the kind's
    /// fixed schema. A file with no rows is refused before the writer
    /// closes, so before any envelope miss could halve it: publishing it
    /// would leave a manifest `read_manifest` rejects.
    fn build_file(
        &mut self,
        file: &Range<i64>,
        ids: &[i64],
        cap: u64,
    ) -> std::result::Result<ShardPart, ShardError> {
        let (graph, ts, kind, wide) = (
            self.ctx.graph,
            self.ctx.ts,
            self.ctx.kind,
            self.ctx.encoding.wide(),
        );
        // The members are cloned out so the run-splitting closures do not
        // borrow the writer they run inside.
        let (kept, dirty) = (self.kept.clone(), self.dirty);
        // ADR-0534: the rows the file encodes are its dirty and new ids
        // plus the rows of a kept part the unit re-encodes; `source` is
        // the one predicate — a copyable member or `None` to encode.
        let source_of = |id: &i64| source(kept.as_slice(), dirty, *id);
        // A wide part carries the unit's carried columns plus its encoded
        // rows' promoted keys; a legacy part the kind's fixed schema — the
        // wide schema with no promoted columns (ADR-0534).
        let columns = if wide {
            let encoded: Vec<i64> = ids
                .iter()
                .copied()
                .filter(|id| source_of(id).is_none())
                .collect();
            part_columns(kind, graph, ts, &encoded, &self.carried)
        } else {
            Vec::new()
        };
        self.publish_candidate(file, &columns, cap, |me, writer| {
            let mut written = 0;
            for run in ids.chunk_by(|a, b| source_of(a) == source_of(b)) {
                written += match source_of(&run[0]) {
                    Some(member) => me.copy(member, run, writer)?,
                    None => encode_wide_rows(writer, graph, ts, kind, run, &columns)?,
                };
            }
            if written == 0 {
                return Err(ShardError::Fatal(Error::storage(format!(
                    "Parquet shard [{}, {}) would hold no rows",
                    file.start, file.end
                ))));
            }
            Ok(written)
        })
    }

    /// The one candidate-publication protocol every shard file runs
    /// (ADR-0183): reserve a candidate under `file`, open its writer
    /// under `columns` at `cap`, run `write` — which takes the writer and
    /// the unit it copies from — then fsync the file and the kind's
    /// directory, preflight what a reader runs, and stat the published
    /// file. The candidate is discarded, its failure passed through, when
    /// any step refuses.
    fn publish_candidate(
        &mut self,
        file: &Range<i64>,
        columns: &[(String, ColumnType)],
        cap: u64,
        write: impl FnOnce(
            &mut Self,
            &mut ShardWriter<&mut crate::vfs::CappedVfsWriter<'_>>,
        ) -> std::result::Result<usize, ShardError>,
    ) -> std::result::Result<ShardPart, ShardError> {
        let (path, mut output) = self.reserve_candidate(file)?;
        let kind = self.ctx.kind;
        let result = with_part_shard_writer_to(&mut *output, kind, columns, cap, |writer| {
            write(self, writer)
        })
        .and_then(|rows| {
            output.sync_all().map_err(ShardError::Fatal)?;
            self.ctx
                .target
                .vfs
                .sync_dir(self.ctx.kind.dir())
                .map_err(ShardError::Fatal)?;
            Ok(rows)
        });
        drop(output);
        let result = result.and_then(|rows| {
            let source =
                VfsChunkReader::open(Arc::clone(&self.ctx.target.vfs), &path, file_ceiling())
                    .map_err(ShardError::Fatal)?;
            preflight(source).map_err(ShardError::Envelope)?;
            Ok(rows)
        });
        match result {
            Ok(rows) => {
                let bytes = self
                    .ctx
                    .target
                    .vfs
                    .metadata(&path)
                    .map_err(ShardError::Fatal)?
                    .len;
                Ok(ShardPart {
                    path,
                    id_lo: file.start,
                    id_hi: file.end,
                    rows: rows as u64,
                    bytes,
                })
            }
            Err(error) => Err(self.discard_candidate(&path, error)),
        }
    }

    fn reserve_candidate(
        &mut self,
        file: &Range<i64>,
    ) -> std::result::Result<(String, Box<dyn crate::vfs::VfsFile>), ShardError> {
        let target = self.ctx.target;
        let dir = self.ctx.kind.dir();
        prepare_object_dir(target, dir, "shard directory").map_err(ShardError::Fatal)?;
        let (path, output) = reserve_exclusive(
            &*target.vfs,
            || ShardName::random(self.ctx.kind, file.start, file.end).map(|name| name.path()),
            self.ctx.owned,
            "Parquet shard part",
        )
        .map_err(ShardError::Fatal)?;
        Ok((path, output))
    }

    fn discard_candidate(&mut self, path: &str, error: ShardError) -> ShardError {
        match self.ctx.target.vfs.remove_file(path) {
            Ok(()) => {
                self.ctx.owned.retain(|owned| owned != path);
                error
            }
            Err(cleanup) => ShardError::Fatal(Error::storage(format!(
                "{error:?}; could not remove failed Parquet candidate {path}: {cleanup}"
            ))),
        }
    }

    /// Copy `run`, clean ids of `kept[member]`, out of its previous shard.
    /// The held cursor serves the same member again: a unit's runs ascend,
    /// and a halved file drops it before its halves, which start behind it.
    /// Any other held shard is dropped before the next is read, so at most
    /// one is held, and a failed copy consumes the cursor and holds none.
    fn copy<W: std::io::Write + Send + crate::codec::parquet::OutputLimit>(
        &mut self,
        member: usize,
        run: &[i64],
        writer: &mut ShardWriter<W>,
    ) -> std::result::Result<usize, ShardError> {
        let held = self.open.take().filter(|open| open.member == member);
        let cursor = match held {
            Some(open) => open.cursor,
            None => {
                let part = self.kept[member].part;
                ShardCursor::open(
                    Arc::clone(&self.ctx.target.vfs),
                    &part.path,
                    self.ctx.kind,
                    span(part),
                )?
            }
        };
        let cursor = cursor.take(run, writer)?;
        self.open = Some(OpenShard { member, cursor });
        Ok(run.len())
    }
}

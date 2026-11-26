//! The rewrite unit's writer (ADR-0183 Decision 3, ADR-0534): the files
//! planning cut a unit into, each copying clean rows out of the kept
//! parts' previous shards and encoding the rest.

use super::{ids_in, span};
use crate::snapshot::native::*;

use std::ops::Range;
use std::sync::Arc;

use crate::codec::parquet::{
    encode_wide_rows, file_ceiling, part_columns, preflight, previous_shard_columns,
    with_part_shard_writer_to, ColumnType, ShardCursor, ShardError, ShardWriter, VfsChunkReader,
    MAX_PROMOTED_COLUMNS,
};

/// The member of `kept`, the part whose previous shard holds clean `id`
/// and the unit copies, or `None` to encode it: `id` is dirty, lies in a
/// window (no previous shard), or belongs to a member the unit
/// re-encodes (ADR-0534). `kept` are sorted and disjoint, so at most one
/// covers `id` — the one predicate every run decision consults.
fn source<'u>(kept: &[Member<'u>], dirty: &[i64], id: i64) -> Option<Member<'u>> {
    if dirty.binary_search(&id).is_ok() {
        return None;
    }
    let member = kept[..kept.partition_point(|member| member.part.id_lo <= id)]
        .last()
        .copied()?;
    (id < member.part.id_hi && !member.reencode).then_some(member)
}

/// Writes one rewrite unit's planned files (Decision 3), each copying clean
/// rows out of the kept parts' previous shards and encoding the rest,
/// through one cursor. Every file is a wide part (ADR-0534): its columns
/// carried from the parts it copies and promoted from the rows it encodes.
pub(super) struct UnitWriter<'u, 'a> {
    ctx: &'u mut ShardWrite<'a>,
    /// The kind this unit rewrites, fixed by its pass.
    kind: Kind,
    /// The parts of the unit's kept slots, whose previous shards hold the
    /// unit's clean rows, each with whether the unit re-encodes it
    /// instead of copying (ADR-0534).
    kept: Vec<Member<'u>>,
    dirty: &'u [i64],
    /// The columns the kept parts' previous shards carry (ADR-0534): a
    /// copied row's values live only through them.
    carried: Vec<(String, ColumnType)>,
    /// The one previous shard held open.
    open: Option<OpenShard<'u>>,
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
pub(super) struct Member<'u> {
    part: &'u ShardPart,
    /// Whether the unit re-encodes this part's rows instead of copying
    /// them: a part whose column disagrees with the unit's carries values
    /// the copy cannot place, and a union wider than the column cap cannot
    /// copy at all. Its rows come from the live graph, like dirty rows.
    reencode: bool,
}

impl PartialEq for Member<'_> {
    /// Two members are the same when they are the same kept part.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.part, other.part)
    }
}

/// The previous shard the cursor reads: that of `member`.
struct OpenShard<'u> {
    member: Member<'u>,
    cursor: ShardCursor,
}

impl<'u, 'a> UnitWriter<'u, 'a> {
    pub(super) fn new(
        ctx: &'u mut ShardWrite<'a>,
        kind: Kind,
        kept: &[&'u ShardPart],
        dirty: &'u [i64],
    ) -> Result<Self> {
        let carried = Self::carried_columns(ctx, kind, kept)?;
        Ok(Self {
            ctx,
            kind,
            kept: carried.members,
            dirty,
            carried: carried.columns,
            open: None,
        })
    }

    /// The unit's carried columns and its kept parts, each marked for
    /// copy or re-encode (ADR-0534): the checkpoint reads every kept
    /// part's schema from its file and merges them; a part whose column
    /// disagrees with the unit's is re-encoded, its rows reground from
    /// the live graph, and a union wider than the column cap re-encodes
    /// every part, the columns then coming from the encoded rows alone.
    /// Every part is wide since ADR-0553, so every kept part carries a
    /// footer to read.
    fn carried_columns<'p>(
        ctx: &ShardWrite<'_>,
        kind: Kind,
        kept: &[&'p ShardPart],
    ) -> Result<Carried<'p>> {
        let mut carried = std::collections::BTreeMap::new();
        let mut members = Vec::with_capacity(kept.len());
        for part in kept {
            // A kept part's rows are re-read, not re-derived (ADR-0534),
            // so its columns carry forward unchanged.
            let columns =
                previous_shard_columns(Arc::clone(&ctx.target.vfs), &part.path, kind, span(part))?;
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
    pub(super) fn write(
        &mut self,
        files: &[Range<i64>],
        all_ids: &[i64],
    ) -> Result<Vec<ShardPart>> {
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
    /// run. The file carries its columns — carried from the parts it
    /// copies plus the promoted keys of the rows it encodes — decided
    /// before the writer opens, the schema the writer and the batch
    /// builders share (ADR-0534). A file with no rows is refused before
    /// the writer closes, so before any envelope miss could halve it:
    /// publishing it would leave a manifest `read_manifest` rejects.
    fn build_file(
        &mut self,
        file: &Range<i64>,
        ids: &[i64],
        cap: u64,
    ) -> std::result::Result<ShardPart, ShardError> {
        let (graph, ts, kind) = (self.ctx.graph, self.ctx.ts, self.kind);
        // ADR-0534: the rows the file encodes are its dirty and new ids
        // plus the rows of a kept part the unit re-encodes; `source` is
        // the one predicate — a copyable member or `None` to encode. The
        // run plan is decided before the writer opens, while the unit's
        // own fields are still borrowable: the writer closure takes
        // `&mut Self`, so it cannot borrow `self.kept` — it consumes the
        // plan instead, never a clone of the members.
        let runs: Vec<(&[i64], Option<Member<'u>>)> = ids
            .chunk_by(|a, b| {
                source(&self.kept, self.dirty, *a) == source(&self.kept, self.dirty, *b)
            })
            .map(|run| (run, source(&self.kept, self.dirty, run[0])))
            .collect();
        // The part carries the unit's carried columns plus its encoded
        // rows' promoted keys — the wide schema with no promoted columns
        // when none promote (ADR-0534).
        let encoded: Vec<i64> = runs
            .iter()
            .filter(|(_, member)| member.is_none())
            .flat_map(|(run, _)| run.iter().copied())
            .collect();
        let columns = part_columns(kind, graph, ts, &encoded, &self.carried);
        self.publish_candidate(file, &columns, cap, |me, writer| {
            let mut written = 0;
            for (run, member) in runs {
                written += match member {
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
        let kind = self.kind;
        let result = with_part_shard_writer_to(&mut *output, kind, columns, cap, |writer| {
            write(self, writer)
        })
        .and_then(|rows| {
            output.sync_all().map_err(ShardError::Fatal)?;
            self.ctx
                .target
                .vfs
                .sync_dir(self.kind.dir())
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
        let dir = self.kind.dir();
        prepare_object_dir(target, dir, "shard directory").map_err(ShardError::Fatal)?;
        let (path, output) = reserve_exclusive(
            &*target.vfs,
            || ShardName::random(self.kind, file.start, file.end).map(|name| name.path()),
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

    /// Copy `run`, clean ids of `member`, out of its previous shard.
    /// The held cursor serves the same member again: a unit's runs ascend,
    /// and a halved file drops it before its halves, which start behind it.
    /// Any other held shard is dropped before the next is read, so at most
    /// one is held, and a failed copy consumes the cursor and holds none.
    fn copy<W: std::io::Write + Send + crate::codec::parquet::OutputLimit>(
        &mut self,
        member: Member<'u>,
        run: &[i64],
        writer: &mut ShardWriter<W>,
    ) -> std::result::Result<usize, ShardError> {
        let held = self.open.take().filter(|open| open.member == member);
        let cursor = match held {
            Some(open) => open.cursor,
            None => {
                let part = member.part;
                ShardCursor::open(
                    Arc::clone(&self.ctx.target.vfs),
                    &part.path,
                    self.kind,
                    span(part),
                )?
            }
        };
        let cursor = cursor.take(run, writer)?;
        self.open = Some(OpenShard { member, cursor });
        Ok(run.len())
    }
}

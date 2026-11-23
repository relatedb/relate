//! Segment framing shared by writable recovery and read-only follow.
use std::io::SeekFrom;

use relate_core::{Error, Result};
use relate_engine_api::RedoOp;

use crate::vfs::{atomic_write_stream, virt_join, Vfs, VfsFile, VfsOpen};

use super::{FileWal, WalCursor, WalRecord, MAGIC, ONTOLOGY_MAGIC};

/// The id of a brand-new WAL's first segment — the one name for the fact
/// `write_empty_segment` writes, `Wal::open`'s no-segment branch pushes,
/// and a source release's fresh inventory starts at.
pub(super) const FIRST_SEGMENT_ID: u32 = 1;

pub(super) fn segment_path(prefix: &str, id: u32) -> String {
    virt_join(prefix, &format!("{id:08}.log"))
}

pub(super) fn list_segment_ids(vfs: &dyn Vfs, prefix: &str) -> Result<Vec<u32>> {
    let mut ids = Vec::new();
    for name in vfs.list_dir(prefix)? {
        if let Some(id) = parse_seg_name(&name) {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

pub(crate) fn parse_seg_name(name: &str) -> Option<u32> {
    let s = name.strip_suffix(".log")?;
    if s.len() != 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

pub(super) fn write_magic(vfs: &dyn Vfs, path: &str, magic: &[u8; 8], sync: bool) -> Result<()> {
    let mut file = vfs.open(path, VfsOpen::create_truncate())?;
    file.write_all(magic)?;
    // The magic is the segment's identity header — a metadata-only write to a
    // fresh file, not the commit barrier. `rotate` barriers the *old* data
    // segment via the `synced` check in `close_active_segment`; this syncs
    // the new segment's header so a crash after rotate can open it. Relaxed
    // (snapshot) rotation deliberately skips it: its stable-storage boundary
    // is the completed checkpoint, and `repair_short_new_magic` recovers a
    // short or empty header.
    if sync {
        file.sync_all()?;
    }
    Ok(())
}

/// Write a brand-new empty first segment under `prefix` — the directory,
/// and the no-ontology magic (`MDBWAL01`, ADR-0120 §4) — the one
/// construction both a fresh store's open (`Wal::open`'s no-segment
/// branch) and a source release (`Wal::release_source`) build. Returns
/// the segment path it wrote, so a caller's append handle opens the
/// written value instead of re-deriving the path and id from the prefix.
/// The release path needs the construction as a WRITE, never as a scan:
/// `Wal::open` adopts whatever segment inventory already sits under
/// `prefix`, so a release onto a vfs that already holds that prefix must
/// not go through it.
pub(super) fn write_empty_segment(vfs: &dyn Vfs, prefix: &str) -> Result<String> {
    vfs.create_dir_all(prefix)?;
    let path = segment_path(prefix, FIRST_SEGMENT_ID);
    write_magic(vfs, &path, MAGIC, true)?;
    Ok(path)
}

/// The one classification of a highest segment shorter than the magic,
/// shared by writable repair, the read-only skip and the live follower:
/// `true` when the segment's entire contents are empty or a proper prefix
/// of the magic — a torn rotate, which cannot include even a frame header —
/// and `false` for any other short header, which is corruption everywhere.
/// Complete headers are `false` too: the normal scanner owns them.
pub(super) fn is_torn_short_magic(vfs: &dyn Vfs, path: &str) -> Result<bool> {
    if vfs.metadata(path)?.len >= MAGIC.len() as u64 {
        return Ok(false);
    }
    let body = crate::vfs::read_to_vec(vfs, path)?;
    Ok(MAGIC.starts_with(&body))
}

/// Called only on the highest segment after every predecessor scanned cleanly.
/// A torn rotate — the segment created but stopped before its header was
/// written, or holding a proper magic prefix — is completed to a whole magic
/// through an atomic replacement. Mismatching and complete headers are left
/// to the normal scanner, which handles complete headers and all frame
/// corruption.
pub(super) fn repair_short_new_magic(vfs: &dyn Vfs, path: &str) -> Result<()> {
    // ADR-0120 §4: a repaired short header always becomes `MDBWAL01` — the
    // segment holds no frames, so no ontology op can ever be appended to it.
    if is_torn_short_magic(vfs, path)? {
        crate::vfs::atomic_write(vfs, path, MAGIC)?;
    }
    Ok(())
}

pub(super) fn open_append(vfs: &dyn Vfs, path: &str) -> Result<Box<dyn VfsFile>> {
    vfs.open(path, VfsOpen::append_create())
}

/// Copy the intact prefix into an atomic replacement, returning the number of
/// bytes it discarded. Truncating the original in place could lose committed
/// records if reading or sync fails.
///
/// The discarded size is read from the handle that copies the prefix, so the
/// caller never re-derives it: a file shorter than the scanned prefix (a
/// truncation between the scan and the trim) is a typed error, not a wrapped
/// subtraction.
pub(super) fn replace_torn_tail(vfs: &dyn Vfs, path: &str, end: u64) -> Result<u64> {
    atomic_write_stream(vfs, path, |destination| {
        // Scoped inside the producer: the source closes before the helper's
        // rename, including on Windows where open-file replacement fails.
        let mut source = vfs.open(path, VfsOpen::read_only())?;
        let total = source.len()?;
        let discarded = total.checked_sub(end).ok_or_else(|| {
            Error::storage(format!(
                "torn-tail trim of {path}: physical length {total} is below the scanned prefix {end}"
            ))
        })?;
        let mut left = end;
        let mut buf = [0u8; 64 * 1024];
        while left > 0 {
            let size = left.min(buf.len() as u64) as usize;
            source.read_exact(&mut buf[..size])?;
            destination.write_all(&buf[..size])?;
            left -= size as u64;
        }
        Ok(discarded)
    })
}

pub(super) fn replay_dir(vfs: &dyn Vfs, prefix: &str, from_lsn: u64) -> Result<Vec<WalRecord>> {
    let mut out = Vec::new();
    scan_dir(vfs, prefix, from_lsn, &mut |rec| {
        out.push(rec);
        Ok(())
    })?;
    Ok(out)
}

pub(super) fn scan_dir(
    vfs: &dyn Vfs,
    prefix: &str,
    from_lsn: u64,
    on_record: &mut impl FnMut(WalRecord) -> Result<()>,
) -> Result<()> {
    let ids = list_segment_ids(vfs, prefix)?;
    let mut last_ts = 0;
    for id in &ids {
        scan_seg(
            vfs,
            &segment_path(prefix, *id),
            0,
            ScanWindow {
                from_lsn,
                limit: usize::MAX,
                last_ts: &mut last_ts,
            },
            SegSource {
                id: *id,
                active: Some(id) == ids.last(),
            },
            &mut *on_record,
        )?;
    }
    Ok(())
}

pub(super) fn records_after_file(
    f: &mut FileWal,
    from_lsn: u64,
    limit: usize,
) -> Result<Vec<WalRecord>> {
    let hint = f.page_hint.filter(|h| h.lsn == from_lsn);

    let mut segs = Vec::new();
    for (id, max_lsn, _) in &f.closed {
        if let Some(h) = hint {
            if *id < h.seg_id {
                continue;
            }
        } else if *max_lsn <= from_lsn {
            continue;
        }
        segs.push(*id);
    }
    if hint.map(|h| f.seg_id >= h.seg_id).unwrap_or(true) && !segs.contains(&f.seg_id) {
        segs.push(f.seg_id);
    }
    segs.sort_unstable();
    segs.dedup();

    let mut out = Vec::new();
    let mut last_hint = None;
    // Segments skipped by the cursor were checked on open; their final timestamp
    // still bounds the first record in a later segment.
    let mut last_ts = segs
        .first()
        .and_then(|first| {
            f.closed
                .iter()
                .rev()
                .find(|(id, _, _)| id < first)
                .map(|(_, _, ts)| *ts)
        })
        .unwrap_or(0);
    for seg in segs {
        if out.len() >= limit {
            break;
        }
        let path = segment_path(&f.prefix, seg);
        let remaining = limit - out.len();
        // Only an offset past the physical file end is a stale hint. Never
        // retry an actual read/seek failure as if it were a stale offset.
        let start = match hint.filter(|h| h.seg_id == seg) {
            Some(h) => {
                let len = f.vfs.metadata(&path)?.len;
                if h.offset <= len {
                    // Resume this segment at the hinted offset; its commit
                    // timestamp still bounds the first record read.
                    last_ts = h.commit_ts;
                    h.offset
                } else {
                    // Past the file end: the hint is stale. Drop it and
                    // rescan this segment from the start.
                    f.page_hint = None;
                    0
                }
            }
            // No hint for this segment: scan from the start.
            None => 0,
        };
        let (recs, end) = read_seg_limited(
            &*f.vfs,
            &path,
            start,
            ScanWindow {
                from_lsn,
                limit: remaining,
                last_ts: &mut last_ts,
            },
            SegSource {
                id: seg,
                active: seg == f.seg_id,
            },
        )?;
        if let Some(h) = end {
            last_hint = Some(h);
        }
        out.extend(recs);
    }
    if let Some(h) = last_hint {
        f.page_hint = Some(h);
    }
    Ok(out)
}

/// Records and the read cursor after the last delivered frame.
pub(super) type SegRead = (Vec<WalRecord>, Option<WalCursor>);

pub(super) struct SegScan {
    pub(super) hint: Option<WalCursor>,
    /// Offset where a tolerated tail starts: an incomplete frame at EOF on
    /// any segment (a relaxed rotate skips the barrier, so a torn suffix can
    /// land on a just-rotated closed segment too), or a complete final frame
    /// whose checksum failed on the active segment (a relaxed append). The
    /// same checksum failure in a closed segment is bitrot and stays a hard
    /// error; a checksum-valid frame whose payload does not decode is never
    /// tolerated anywhere (ADR-0114).
    pub(super) tail_at: Option<u64>,
    /// The segment's identity magic, read from its header on every scan
    /// (ADR-0120 §4 needs it per segment for the ontology-op corruption
    /// check, and `Wal` tracks the active segment's magic from it).
    pub(super) magic: [u8; 8],
}

pub(super) struct SegSource {
    pub(super) id: u32,
    pub(super) active: bool,
}

pub(super) struct ScanWindow<'a> {
    pub(super) from_lsn: u64,
    pub(super) limit: usize,
    pub(super) last_ts: &'a mut u64,
}

pub(super) fn read_seg_limited(
    vfs: &dyn Vfs,
    path: &str,
    start_off: u64,
    window: ScanWindow<'_>,
    segment: SegSource,
) -> Result<SegRead> {
    if window.limit == 0 {
        return Ok((Vec::new(), None));
    }
    let mut out = Vec::new();
    let scan = scan_seg(vfs, path, start_off, window, segment, |rec| {
        out.push(rec);
        Ok(())
    })?;
    Ok((out, scan.hint))
}

// Shared framing/CRC/JSON decoder for summaries, startup replay, explicit
// materializing replay and bounded record reads. No Vec of records is needed
// until a caller explicitly requests one.
//
// A tolerated tail is reported via `tail_at`. An incomplete frame at EOF is
// tolerated on *any* segment: a relaxed (snapshot) rotation deliberately
// skips the barrier, so a crash can leave a torn suffix on a just-rotated
// closed segment as well as the active one. A complete last frame that fails
// its checksum is tolerated only on the active segment (a relaxed append);
// the same failure in a closed segment is bitrot, not a torn write. In both
// cases `open` trims the acknowledged prefix with `replace_torn_tail`. A
// complete frame whose checksum matches but whose payload does not decode
// was written completely by something this binary does not understand — in
// practice a newer Relate — so it is a hard error in every segment and
// position (ADR-0114); trimming it would silently drop a committed
// transaction.
pub(super) fn scan_seg(
    vfs: &dyn Vfs,
    path: &str,
    start_off: u64,
    window: ScanWindow<'_>,
    segment: SegSource,
    mut on_record: impl FnMut(WalRecord) -> Result<()>,
) -> Result<SegScan> {
    let mut f = vfs.open(path, VfsOpen::read_only())?;
    let file_len = f.len()?;
    if file_len < MAGIC.len() as u64 {
        return Err(Error::storage(format!("truncated WAL magic: {path}")));
    }
    // The magic is read on every scan, also when the caller seeks past the
    // header: the ADR-0120 §4 corruption check (an ontology op in a
    // non-`MDBWAL02` segment) needs it per segment.
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if !crate::version::wal::is_readable(&magic) {
        return Err(Error::storage(format!("invalid WAL magic: {path}")));
    }
    if start_off > 0 {
        if start_off < MAGIC.len() as u64 || start_off > file_len {
            return Err(Error::storage(format!("invalid WAL offset: {path}")));
        }
        f.seek(SeekFrom::Start(start_off))?;
    }
    let mut pos = start_off.max(MAGIC.len() as u64);
    let mut delivered = 0;
    let mut end = None;
    let mut tail_at = None;
    loop {
        if delivered >= window.limit {
            break;
        }
        let remaining = file_len - pos;
        if remaining == 0 {
            break;
        }
        let frame_start = pos;
        if remaining < 8 {
            // Incomplete frame header at EOF.
            tail_at = Some(frame_start);
            break;
        }
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr)?;
        pos += 8;
        let crc = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        // A frame's validity bound is what the format can encode — the
        // 32-bit length field — never the store's per-open admission cap
        // (ADR-0650): a record written under a larger cap than a later
        // open's is still a valid frame, and refusing it here would brick
        // the store. An absurd length is caught by the file-length check
        // below as an incomplete payload (a torn tail), so only the
        // structurally invalid zero length is refused here.
        if len == 0 {
            return Err(Error::storage(format!(
                "invalid WAL payload length 0 in {path}"
            )));
        }
        let payload_end = pos
            .checked_add(len as u64)
            .ok_or_else(|| Error::storage(format!("WAL length overflow: {path}")))?;
        if payload_end > file_len {
            // Incomplete payload at EOF.
            tail_at = Some(frame_start);
            break;
        }
        let mut payload = vec![0u8; len];
        f.read_exact(&mut payload)?;
        pos = payload_end;
        if crc32fast::hash(&payload) != crc {
            if segment.active && pos == file_len {
                tail_at = Some(frame_start);
                break;
            }
            return Err(Error::storage("WAL checksum mismatch"));
        }
        let rec: WalRecord = serde_json::from_slice(&payload).map_err(|e| {
            Error::storage(format!(
                "undecodable WAL record at offset {frame_start} in {path}: \
                 may have been written by a newer Relate ({e})"
            ))
        })?;
        // ADR-0120 §4: an ontology op is only ever written to an `MDBWAL02`
        // segment; decoded from any other (readable) segment it is
        // corruption, never replayed.
        if magic != *ONTOLOGY_MAGIC && rec.ops.iter().any(RedoOp::is_ontology_op) {
            return Err(Error::storage(format!(
                "ontology op in non-MDBWAL02 WAL segment {path} at offset {frame_start}"
            )));
        }
        if !super::commit_ts_advances(*window.last_ts, rec.commit_ts) {
            return Err(Error::storage(format!(
                "non-increasing or unsupported WAL commit timestamp {} after {} in {path}",
                rec.commit_ts, *window.last_ts
            )));
        }
        *window.last_ts = rec.commit_ts;
        super::validate_redo_ids(&rec.ops)?;
        if rec.lsn > window.from_lsn {
            end = Some(WalCursor {
                lsn: rec.lsn,
                seg_id: segment.id,
                offset: pos,
                commit_ts: rec.commit_ts,
            });
            on_record(rec)?;
            delivered += 1;
        }
    }
    Ok(SegScan {
        hint: end,
        tail_at,
        magic,
    })
}

/// The verdict of a read-only follower's segment walk: the records it may
/// replay in order and the cursor the next poll resumes from, the resting
/// position when nothing was deliverable, or the gap verdict that asks the
/// caller to reload from the snapshot.
pub(super) enum FollowOutcome {
    /// Consecutive records after the follower's position; `next` is only a
    /// proposal: the caller commits it through `commit_follow_cursor` once
    /// the records are applied.
    Records {
        records: Vec<WalRecord>,
        next: WalCursor,
    },
    /// Nothing to deliver. `at` is the follower's resting position — the
    /// cursor the walk started from when that cursor was live, the active
    /// segment's end otherwise (never under the magic) — for the handle to
    /// store through its one cursor-store path.
    Idle { at: WalCursor },
    /// A segment the cursor still needs is gone, or the active segment
    /// shrank under it: the caller reloads from the snapshot.
    Gap,
}

/// The read-only follower's one segment walk — in the module that owns every
/// other segment policy (the header's "shared by writable recovery and
/// read-only follow"): which segments a follower reads, the torn-header
/// tolerance it shares with the read-only open, the shrink exemption for a
/// cursor the open placed at the magic over a not-yet-written header, and
/// the consecutive-LSN rule that turns a hole into a `Gap`. The cursor is
/// the follower's whole position — the walk seeds its expectations from it —
/// and an empty segment inventory is a `Gap` verdict here, not a
/// precondition on the caller.
pub(super) fn follow_walk(
    vfs: &dyn Vfs,
    prefix: &str,
    ids: &[u32],
    cursor: WalCursor,
) -> Result<FollowOutcome> {
    let Some(&first_id) = ids.first() else {
        return Ok(FollowOutcome::Gap);
    };
    let active_id = *ids.last().unwrap_or(&first_id);
    let cursor_live = ids.contains(&cursor.seg_id);
    if cursor_live {
        let path = segment_path(prefix, cursor.seg_id);
        let len = vfs.metadata(&path)?.len;
        // A highest segment shorter than its magic is a rotate in
        // progress or one that crashed. The read-only open skipped it
        // with a cursor at `MAGIC.len()`; that is not a shrink.
        let short_active = cursor.seg_id == active_id && len < MAGIC.len() as u64;
        if len < cursor.offset && !(short_active && cursor.offset == MAGIC.len() as u64) {
            return Ok(FollowOutcome::Gap);
        }
    }
    let start_seg = if cursor_live { cursor.seg_id } else { first_id };
    let mut out = Vec::new();
    let mut last_hint = None;
    let mut last_ts = cursor.commit_ts;
    let mut expect = cursor.lsn.saturating_add(1);
    for seg in ids.iter().copied().filter(|id| *id >= start_seg) {
        let path = segment_path(prefix, seg);
        let active = active_id == seg;
        // Mirror the read-only open: a highest segment whose whole
        // contents are a torn magic prefix has no frames — the writer
        // completes it, the follower reads it on a later poll. Any other
        // short header is corruption and fails the scan below.
        if active && is_torn_short_magic(vfs, &path)? {
            continue;
        }
        // A segment equal to the cursor's is by construction one of `ids`,
        // so this arm implies the cursor was live and its offset stands.
        let start = if seg == cursor.seg_id {
            cursor.offset
        } else {
            0
        };
        let (recs, end) = read_seg_limited(
            vfs,
            &path,
            start,
            ScanWindow {
                from_lsn: cursor.lsn,
                limit: usize::MAX,
                last_ts: &mut last_ts,
            },
            SegSource { id: seg, active },
        )?;
        for rec in recs {
            if rec.lsn != expect {
                return Ok(FollowOutcome::Gap);
            }
            expect = expect.saturating_add(1);
            out.push(rec);
        }
        if let Some(hint) = end {
            last_hint = Some(hint);
        }
    }
    if out.is_empty() {
        let at = if cursor_live {
            cursor
        } else {
            let path = segment_path(prefix, active_id);
            let len = vfs.metadata(&path)?.len;
            WalCursor {
                lsn: cursor.lsn,
                seg_id: active_id,
                offset: len.max(MAGIC.len() as u64),
                commit_ts: cursor.commit_ts,
            }
        };
        return Ok(FollowOutcome::Idle { at });
    }
    let next = last_hint
        .ok_or_else(|| Error::storage("WAL follow cursor missing after a non-empty catch-up"))?;
    Ok(FollowOutcome::Records { records: out, next })
}

pub(super) struct SegmentScan {
    pub(super) closed: Vec<(u32, u64, u64)>,
    pub(super) lsn: u64,
    pub(super) last_commit_ts: u64,
    pub(super) current_bytes: u64,
    pub(super) current_commits: u64,
    pub(super) cursor: Option<WalCursor>,
    pub(super) trims: Vec<(String, u64)>,
    pub(super) trimmed_active: bool,
    /// Identity magic of the active (highest) segment. Defaults to
    /// `MDBWAL01` when the active segment was too short to scan (a
    /// read-only open skips it); such a WAL never appends.
    pub(super) current_magic: [u8; 8],
}

/// The running recovery state every scanned segment folds into: the highest
/// LSN and the latest read cursor (both from each segment's scan cursor),
/// and the tolerated tails a writable recovery trims after the scan. One
/// home for the fold, so the closed-segment loop and the active segment
/// absorb identically. The running commit timestamp stays a `scan_segments`
/// local: `scan_seg` folds it through the scan window's `last_ts` out-param.
struct Recovery {
    lsn: u64,
    cursor: Option<WalCursor>,
    trims: Vec<(String, u64)>,
}

impl Recovery {
    /// Absorb one scanned segment: advance the LSN from the segment's read
    /// cursor and adopt that cursor — the cursor carries the last delivered
    /// record's LSN, the one channel `scan_seg` reports it through — and, on
    /// writable recovery only, record a tolerated tail for the later trim.
    /// The trim-vs-never-trim mode arrives as a parameter, not struct state:
    /// `scan_segments` owns every mode distinction, and the active segment's
    /// caller derives `trimmed_active` from the `SegScan` it owns. The fold
    /// itself returns nothing.
    fn absorb(&mut self, scan: &SegScan, path: &str, writable: bool) {
        if let Some(hint) = scan.hint {
            self.lsn = self.lsn.max(hint.lsn);
            self.cursor = Some(hint);
        }
        if writable {
            if let Some(at) = scan.tail_at {
                self.trims.push((path.to_string(), at));
            }
        }
    }
}

/// Scan every segment once. Writable recovery repairs a torn new magic and
/// records tolerated tails for a later trim. Read-only follow skips a torn
/// highest segment as frameless and never trims.
pub(super) fn scan_segments(
    vfs: &dyn Vfs,
    prefix: &str,
    ids: &[u32],
    checkpoint_lsn: u64,
    writable: bool,
) -> Result<SegmentScan> {
    let (&current_id, closed_ids) = ids
        .split_last()
        .ok_or_else(|| Error::storage("WAL segment list is empty"))?;
    let mut closed = Vec::new();
    let mut last_commit_ts = 0u64;
    let mut recovery = Recovery {
        lsn: checkpoint_lsn,
        cursor: None,
        trims: Vec::new(),
    };

    // Closed segments scan identically in both modes: no short-header
    // handling, no commit counting, no active-tail bookkeeping.
    for &id in closed_ids {
        let path = segment_path(prefix, id);
        let scan = scan_seg(
            vfs,
            &path,
            0,
            ScanWindow {
                from_lsn: 0,
                limit: usize::MAX,
                last_ts: &mut last_commit_ts,
            },
            SegSource { id, active: false },
            |_| Ok(()),
        )?;
        recovery.absorb(&scan, &path, writable);
        let (max_lsn, max_ts) = scan.hint.map_or((0, 0), |hint| (hint.lsn, hint.commit_ts));
        closed.push((id, max_lsn, max_ts));
    }

    // The active segment owns every mode-dependent distinction: writable
    // recovery repairs a torn new magic (only with a verified predecessor);
    // read-only follow skips a torn highest segment on a multi-segment WAL
    // as frameless, while any other short header — and a *sole* segment
    // shorter than the magic, torn or not, without a predecessor to prove
    // the rotate — falls through to `scan_seg`'s truncated-magic error.
    let path = segment_path(prefix, current_id);
    // A short highest header is interpretable only with a verified
    // predecessor — the one load-bearing precondition both modes share.
    let predecessor_verified = !closed_ids.is_empty();
    let skip_active = !writable && predecessor_verified && is_torn_short_magic(vfs, &path)?;
    let mut current_commits = 0u64;
    let mut current_magic = *MAGIC;
    let mut trimmed_active = false;
    let active_scan = if skip_active {
        None
    } else {
        if writable && predecessor_verified {
            repair_short_new_magic(vfs, &path)?;
        }
        Some(scan_seg(
            vfs,
            &path,
            0,
            ScanWindow {
                from_lsn: 0,
                limit: usize::MAX,
                last_ts: &mut last_commit_ts,
            },
            SegSource {
                id: current_id,
                active: true,
            },
            |rec| {
                if rec.lsn > checkpoint_lsn {
                    current_commits += 1;
                }
                Ok(())
            },
        )?)
    };
    if let Some(scan) = &active_scan {
        recovery.absorb(scan, &path, writable);
        current_magic = scan.magic;
        trimmed_active = writable && scan.tail_at.is_some();
    }
    // A skipped short header is frameless — its physical length is where the
    // active segment resumes — exactly as a fully scanned segment resumes at
    // its tolerated tail, if any, or its physical end.
    let current_bytes = match active_scan.as_ref().and_then(|scan| scan.tail_at) {
        Some(at) => at,
        None => vfs.metadata(&path)?.len,
    };
    Ok(SegmentScan {
        closed,
        lsn: recovery.lsn,
        last_commit_ts,
        current_bytes,
        current_commits,
        cursor: recovery.cursor,
        trims: recovery.trims,
        trimmed_active,
        current_magic,
    })
}

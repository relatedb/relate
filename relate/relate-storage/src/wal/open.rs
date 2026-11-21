//! The `Wal` constructor family: the writable and read-only opens — every
//! store-open form takes the resolved per-record payload cap (ADR-0650),
//! with [`Wal::open`] the ad-hoc default-cap form — and the in-memory
//! handle. Split from the root module so the log's open-policy cluster
//! (segment inventory, torn-tail trim, the payload cap) has its own home
//! beside `encoded`/`rotation`/`scan`.

use std::sync::Arc;

use super::*;

impl Wal {
    /// Open for append at `payload_cap` (the store's resolved value,
    /// ADR-0650; an ad-hoc open passes
    /// [`Error::WAL_PAYLOAD_CAP_BYTES`]), returning the handle plus the
    /// open-time report: the torn tails this open trimmed (ADR-0114's
    /// tolerated cases). The trims are open-time output, not handle state —
    /// the opener that wants to report the durability event takes them from
    /// the return; every other caller uses [`open`](Self::open), which
    /// drops them.
    pub fn open_with_report(
        vfs: Arc<dyn Vfs>,
        prefix: &str,
        checkpoint_lsn: u64,
        payload_cap: u64,
    ) -> Result<(Self, Vec<TrimmedTail>)> {
        Self::open_notifying(vfs, prefix, checkpoint_lsn, payload_cap, &mut |_| {})
    }

    /// [`open_with_report`](Self::open_with_report) that also hands each
    /// trim to `on_trim` the moment it is durable — before any later step of
    /// this open can fail. A trim rewrites the torn segment for good, so an
    /// open that fails afterwards (here, or in the caller's replay and
    /// migration steps) would otherwise lose the durability event: a retry
    /// finds no torn tail left to report. The returned list is the same
    /// trims the callback saw.
    pub fn open_notifying(
        vfs: Arc<dyn Vfs>,
        prefix: &str,
        checkpoint_lsn: u64,
        payload_cap: u64,
        on_trim: &mut dyn FnMut(&TrimmedTail),
    ) -> Result<(Self, Vec<TrimmedTail>)> {
        check_payload_cap(payload_cap, "WAL payload cap")?;
        vfs.create_dir_all(prefix)?;
        let mut ids = list_segment_ids(&*vfs, prefix)?;
        if ids.is_empty() {
            ids.push(FIRST_SEGMENT_ID);
            // A brand-new store holds no ontology: `MDBWAL01` (ADR-0120 §4)
            // — the empty-segment construction
            // [`write_empty_segment`] owns.
            write_empty_segment(&*vfs, prefix)?;
        }
        let current_id = *ids.last().unwrap();
        // Tolerated-tail trims are applied only after every segment has been
        // scanned without error: a hard failure must leave all WAL bytes
        // untouched (ADR-0114).
        let scan = scan_segments(&*vfs, prefix, &ids, checkpoint_lsn, true)?;
        let mut trimmed_tails = Vec::new();
        for (path, end) in &scan.trims {
            let discarded_bytes = replace_torn_tail(&*vfs, path, *end)?;
            let tail = TrimmedTail {
                path: path.clone(),
                discarded_bytes,
            };
            on_trim(&tail);
            trimmed_tails.push(tail);
        }
        let active_path = segment_path(prefix, current_id);
        let file = open_append(&*vfs, &active_path)?;
        // A trimmed active segment must not grow again under the same id.
        // A follower cursor in the discarded tail then sees `len < cursor`
        // and reloads, instead of applying frames that reused those offsets.
        let trimmed_active = scan.trimmed_active;
        let mut wal = Self::from_scan(
            scan,
            FileWal {
                vfs,
                prefix: prefix.to_string(),
                seg_id: current_id,
                file,
                closed: Vec::new(),
                page_hint: None,
                follow_cursor: None,
            },
            false,
            payload_cap,
        );
        if trimmed_active {
            wal.rotate_away_from_trimmed_active()?;
        }
        Ok((wal, trimmed_tails))
    }

    /// [`open_with_report`](Self::open_with_report) for callers that do not
    /// report the trimmed tails: the report is dropped, the handle is the
    /// same. The ad-hoc default-cap form (ADR-0650) — a store-configured
    /// open passes its resolved cap to
    /// [`open_with_report`](Self::open_with_report) instead.
    pub fn open(vfs: Arc<dyn Vfs>, prefix: &str, checkpoint_lsn: u64) -> Result<Self> {
        Ok(Self::open_with_report(vfs, prefix, checkpoint_lsn, Error::WAL_PAYLOAD_CAP_BYTES)?.0)
    }

    /// Scan an existing WAL without creating segments or trimming a torn tail
    /// (ADR-0136) at `payload_cap` (the store's resolved value, ADR-0650; an
    /// ad-hoc open passes [`Error::WAL_PAYLOAD_CAP_BYTES`]). Incomplete
    /// frames at the end of the active segment are ignored. A missing WAL
    /// is an error.
    pub fn open_readonly(
        vfs: Arc<dyn Vfs>,
        prefix: &str,
        checkpoint_lsn: u64,
        payload_cap: u64,
    ) -> Result<Self> {
        check_payload_cap(payload_cap, "WAL payload cap")?;
        let ids = list_segment_ids(&*vfs, prefix)?;
        if ids.is_empty() {
            return Err(Error::storage("read-only open requires an existing WAL"));
        }
        let current_id = *ids.last().unwrap();
        let mut scan = scan_segments(&*vfs, prefix, &ids, checkpoint_lsn, false)?;
        let active_path = segment_path(prefix, current_id);
        let file = vfs.open(&active_path, VfsOpen::read_only())?;
        // A skipped short highest segment leaves `current_bytes` under the
        // magic. A cursor there would fail the offset check on every poll.
        let cursor = scan.cursor.take().unwrap_or(WalCursor {
            lsn: scan.lsn,
            seg_id: current_id,
            offset: scan.current_bytes.max(MAGIC.len() as u64),
            commit_ts: scan.last_commit_ts,
        });
        Ok(Self::from_scan(
            scan,
            FileWal {
                vfs,
                prefix: prefix.to_string(),
                seg_id: current_id,
                file,
                closed: Vec::new(),
                page_hint: None,
                follow_cursor: Some(cursor),
            },
            true,
            payload_cap,
        ))
    }

    pub fn memory() -> Self {
        Self::open(Arc::new(MemoryVfs::new()), "wal", 0).expect("memory WAL")
    }

    /// The one handle construction from a completed segment scan: both
    /// opens build only their `FileWal` — the append handle, or the
    /// read-only follower with its cursor — and delegate here, so every
    /// scan-derived field is read once and the two opens cannot drift in
    /// what they carry out of the scan. `read_only` is the one flag the
    /// two handles differ by beyond their `FileWal`.
    fn from_scan(
        mut scan: SegmentScan,
        mut inner: FileWal,
        read_only: bool,
        payload_cap: u64,
    ) -> Self {
        inner.closed = std::mem::take(&mut scan.closed);
        Self {
            inner,
            lsn: scan.lsn,
            bytes: scan.current_bytes,
            commits: scan.current_commits,
            last_commit_ts: scan.last_commit_ts,
            synced: true,
            poisoned: false,
            read_only,
            payload_cap,
            // Conservative default until the store layer reports the real
            // ontology state: an active `MDBWAL02` segment means the WAL
            // holds (or held) an ontology op, so keep writing `MDBWAL02`.
            ontology_held: scan.current_magic == *ONTOLOGY_MAGIC,
            active_magic: scan.current_magic,
            source_released: false,
            #[cfg(test)]
            barrier_calls: 0,
        }
    }
}

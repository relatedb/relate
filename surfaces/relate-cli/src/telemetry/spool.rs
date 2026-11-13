//! The telemetry spool (ADR-0545): exit-time local persistence for
//! pending event envelopes, claimed and delivered by the next enabled
//! run's drain thread.
//!
//! One file per run (`telemetry-spool-<uuid>.jsonl`, one JSON envelope
//! per line, mode 0600 on unix at creation like the other telemetry
//! state): a run's exit writes its backlog to its own fresh file, so
//! concurrent runs never share a target and no run ever writes into a
//! file that existed before it. A drain claims files by renaming them
//! to unique `telemetry-claim-<uuid>.jsonl` names — the rename is what
//! makes delete-on-confirm safe: a later run's file lands under a fresh
//! name, never in the file being drained. Leftover claims (a crashed or
//! killed drain) are claimed again by the next sweep, so delivery is
//! at-least-once; the sink dedups on the per-event `$insert_id`
//! (ADR-0079 §2), making it effectively-once.
//!
//! The sweep and the clear take regular files only: a symbolic link in
//! the config directory is never claimed, never read, never renamed —
//! only this module's own regular files are.
//!
//! Parsing is attempt-once: lines of a claimed file that do not parse
//! are dropped when the claim is removed after a confirmed delivery — a
//! corrupt line can never be delivered, and keeping its file alive would
//! wedge every future sweep.
//!
//! All I/O errors are swallowed (ADR-0079 §5): the spool is best-effort
//! scratch, and no telemetry failure may print or fail a run. The
//! bounds are the backlog cap (each file carries at most that many
//! events), the sweep cap (a drain claims at most
//! [`MAX_CLAIMS_PER_SWEEP`] files, the rest waiting for the next run),
//! and the retention cap (past [`MAX_PENDING_FILES`] pending files a
//! sweep drops the excess). Undeliverable content is attempt-once — a
//! corrupt line is dropped with its file after the delivery attempt,
//! and a claim that cannot be read at all is removed on the spot:
//! retained, it would sort to the head of every later sweep and starve
//! delivery.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::envelope::{spool_line, uuid_v4};

/// The spool file prefix: `telemetry-spool-<uuid>.jsonl`, one file per
/// run, written only by that run's exit. `pub(crate)` so the src-side
/// tests consult the fact instead of re-spelling it.
pub(crate) const SPOOL_PREFIX: &str = "telemetry-spool-";

/// The claim file prefix: `telemetry-claim-<uuid>.jsonl`, the rename
/// target a drain takes a spool or leftover-claim file under before
/// delivering it.
const CLAIM_PREFIX: &str = "telemetry-claim-";

/// The spool and claim file extension.
const EXTENSION: &str = ".jsonl";

/// Claim files one drain sweep takes; the rest wait for the next enabled
/// run, so a pathological spool directory cannot keep a drain thread
/// alive indefinitely.
const MAX_CLAIMS_PER_SWEEP: usize = 8;

/// The spool's total pending-file bound: past it a sweep drops the
/// excess, mirroring the in-memory backlog's drop-on-overflow, so a
/// machine that stays offline keeps a bounded backlog of its newest
/// pending runs instead of an unbounded one.
const MAX_PENDING_FILES: usize = 64;

/// This run's spool file path under `dir` (ADR-0545 §1): one per process,
/// so concurrent runs never share a target.
pub(crate) fn spool_file(dir: &Path) -> PathBuf {
    dir.join(format!("{SPOOL_PREFIX}{}{EXTENSION}", uuid_v4()))
}

/// Write `events` to the run's fresh `file` as one JSON line each. The
/// bytes are staged under a non-sweepable name in the same directory
/// (the shared staging-name recipe) and renamed into place, so the file
/// becomes sweep-visible only with its full content — a concurrent
/// drain can never claim a half-written file and discard this run's
/// exit write. Created private from birth (0600 on unix, the shared
/// exclusive-create step); any I/O failure drops the events, and every
/// error is swallowed (ADR-0079 §5, ADR-0545 §1).
pub(crate) fn write(file: &Path, events: &[Value]) {
    if events.is_empty() {
        return;
    }
    // Serialize before creating so a failure can never leave a partial
    // line; an envelope that cannot serialize is dropped — the
    // pipeline's one swallowed handling of that step (`spool_line`).
    let mut body = String::new();
    for event in events {
        if let Some(line) = spool_line(event) {
            body.push_str(&line);
            body.push('\n');
        }
    }
    let Some(dir) = file.parent() else {
        return;
    };
    // The staged name's leading dot keeps it out of the sweep's
    // predicate; the rename lands it atomically under the run's name.
    let staged = dir.join(crate::atomic_file::staging_name(SPOOL_PREFIX, EXTENSION));
    let write_staged = || -> std::io::Result<()> {
        let mut staged_file = crate::atomic_file::create_exclusive(&staged, 0o600)?;
        staged_file.write_all(body.as_bytes())?;
        drop(staged_file);
        fs::rename(&staged, file)
    };
    if write_staged().is_err() {
        let _ = fs::remove_file(&staged);
    }
}

/// One claimed file and its parsed envelopes.
pub(crate) struct Claim {
    /// The claim path (the renamed file): removed after confirmed
    /// delivery.
    pub(crate) path: PathBuf,
    /// The envelopes that parsed; corrupt lines are absent (see the
    /// module doc's attempt-once policy).
    pub(crate) events: Vec<Value>,
}

/// Claim up to [`MAX_CLAIMS_PER_SWEEP`] spool and leftover-claim files
/// under `dir`, renaming each to a fresh claim name and parsing its
/// lines. Regular files only — a symbolic link is never claimed, read,
/// or renamed. Files beyond the cap stay for the next sweep; so do the
/// files of a concurrent drain this one raced (its rename fails). Every
/// I/O error is swallowed (ADR-0079 §5).
pub(crate) fn claim_all(dir: &Path) -> Vec<Claim> {
    let mut paths: Vec<PathBuf> = spool_paths(dir).collect();
    // Sorted for a deterministic sweep order, stable for tests and fair
    // across runs.
    paths.sort();
    let mut claims = Vec::new();
    // Every rename counts against the sweep cap — an unreadable claim is
    // still work the sweep did, and a directory full of them must not
    // rename past the cap unbounded.
    let mut taken = 0;
    for path in &paths {
        if taken == MAX_CLAIMS_PER_SWEEP {
            break;
        }
        let claim_path = dir.join(format!("{CLAIM_PREFIX}{}{EXTENSION}", uuid_v4()));
        if fs::rename(path, &claim_path).is_err() {
            continue;
        }
        taken += 1;
        // Post-rename regular-file check: the sweep filtered by the
        // directory entry's own type (never followed), but a hostile
        // co-tenant with directory write access could swap the entry for
        // a symbolic link in the window before the rename. The renamed
        // claim is re-checked by its own metadata — still never followed
        // — and a non-regular claim is removed (the link itself, never
        // its target) and skipped.
        if !fs::symlink_metadata(&claim_path).is_ok_and(|meta| meta.file_type().is_file()) {
            remove(&claim_path);
            continue;
        }
        // An unreadable claim is attempt-once, like a corrupt line: the
        // content is undeliverable, and retaining it would sort it to
        // the head of every later sweep (claim names sort before spool
        // names) — eight such files would wall off delivery for good.
        // A transient failure loses one best-effort batch, never a
        // slot forever.
        let Some(events) = read_lines(&claim_path) else {
            remove(&claim_path);
            continue;
        };
        claims.push(Claim {
            events,
            path: claim_path,
        });
    }
    // The retention bound, applied to the survivors: the sweep renamed
    // up to `MAX_CLAIMS_PER_SWEEP` entries into fresh claim names, so
    // the pre-scan list is stale — re-read the directory and drop the
    // excess past `MAX_PENDING_FILES`. Unreadable claims count like any
    // other pending file, so the bound holds on the failure path too.
    // (Uuid order — the names carry no timestamps; the bound is what
    // matters, not which — mirroring the in-memory backlog's
    // drop-on-overflow so a machine that stays offline keeps a bounded
    // backlog, not an unbounded one.)
    let mut survivors: Vec<PathBuf> = spool_paths(dir).collect();
    survivors.sort();
    for excess in survivors.iter().skip(MAX_PENDING_FILES) {
        remove(excess);
    }
    claims
}

/// Remove a claim file after confirmed delivery (or an empty one).
/// Swallowed: an undeletable claim is re-claimed and re-delivered by a
/// later run, which the sink dedups.
pub(crate) fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Remove every spool and claim file under `dir` — `telemetry off`
/// (ADR-0545 §5): turning telemetry off means no further transmissions,
/// the pending ones included. Returns how many files were removed.
pub(crate) fn clear(dir: &Path) -> usize {
    spool_paths(dir)
        .filter(|path| fs::remove_file(path).is_ok())
        .count()
}

/// This module's regular files under `dir`, by the one predicate the
/// sweep, the clear, and the src-side tests share — so the three can
/// never disagree about what belongs to the spool. A symbolic link is
/// not one of this module's files: `is_file` on a directory entry's own
/// type is false for links (never followed), and the sweep re-checks
/// the renamed claim besides.
pub(crate) fn spool_paths(dir: &Path) -> impl Iterator<Item = PathBuf> {
    // read_dir → Option-tolerant chain: the Result (unreadable dir) yields
    // nothing, each ReadDir's io::Results yield only their Ok entries.
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    entries
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_telemetry_name)
        })
}

/// Whether `name` is one of this module's file names.
fn is_telemetry_name(name: &str) -> bool {
    (name.starts_with(SPOOL_PREFIX) || name.starts_with(CLAIM_PREFIX)) && name.ends_with(EXTENSION)
}

/// The parsed envelopes of one file — one JSON value per non-empty line,
/// a line that does not parse absent (attempt-once, module doc), and
/// `None` when the file cannot be read at all: the caller's one signal
/// for the attempt-once removal — undeliverable content, never an
/// empty claim.
fn read_lines(path: &Path) -> Option<Vec<Value>> {
    let text = fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::home_dir::permission_bits_enforced;
    use serde_json::json;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Every `telemetry-spool-*` name currently under `dir`, sorted —
    /// the residue the next sweep would take, seen through the
    /// production scan (`spool_paths`) rather than a second predicate.
    fn spool_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = spool_paths(dir)
            .filter_map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| name.starts_with(SPOOL_PREFIX))
                    .map(str::to_string)
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_creates_one_line_per_event_and_is_readable_back() {
        let dir = tmp();
        let file = spool_file(dir.path());
        write(&file, &[json!({"event": "a"}), json!({"event": "b"})]);
        let text = fs::read_to_string(&file).expect("spool file");
        assert_eq!(text, "{\"event\":\"a\"}\n{\"event\":\"b\"}\n");
    }

    #[cfg(unix)]
    #[test]
    fn write_creates_the_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        let file = spool_file(dir.path());
        write(&file, &[json!({"event": "a"})]);
        let mode = fs::metadata(&file).expect("file").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the spool must be owner-only like state.json");
    }

    #[test]
    fn write_lands_atomically_with_full_content() {
        // The staged-then-renamed write means the sweep-visible file
        // never exists half-written, and a file already at the
        // unguessable uuid path (stale or manufactured) is replaced
        // whole by the rename — the write_atomic precedent.
        let dir = tmp();
        let file = spool_file(dir.path());
        fs::write(&file, "stale\n").expect("preexisting");
        write(&file, &[json!({"event": "a"})]);
        assert_eq!(
            fs::read_to_string(&file).expect("spool file"),
            "{\"event\":\"a\"}\n",
            "the final file carries exactly this run's events"
        );
        // No staging residue: the sweep sees exactly one spool file.
        assert_eq!(spool_paths(dir.path()).count(), 1);
    }

    #[test]
    fn claim_all_renames_and_parses_spool_and_leftover_claim_files() {
        let dir = tmp();
        for _ in 0..2 {
            fs::write(
                spool_file(dir.path()),
                "{\"event\":\"e1\"}\n{\"event\":\"e2\"}\n",
            )
            .expect("seed spool");
        }
        // A leftover claim from a killed drain is swept like a spool file.
        fs::write(
            dir.path().join(format!("{CLAIM_PREFIX}old{EXTENSION}")),
            "{\"event\":\"e3\"}\n",
        )
        .expect("seed claim");
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 3, "both spools and the leftover claim");
        let mut events: Vec<&str> = claims
            .iter()
            .flat_map(|c| c.events.iter().map(|e| e["event"].as_str().expect("event")))
            .collect();
        events.sort();
        assert_eq!(events, &["e1", "e1", "e2", "e2", "e3"]);
        // Every claim is renamed out of the swept names.
        assert!(spool_files(dir.path()).is_empty());
        for claim in &claims {
            assert!(claim.path.to_str().expect("path").contains(CLAIM_PREFIX));
        }
    }

    #[test]
    fn claim_all_leaves_the_rest_for_the_next_sweep_past_the_cap() {
        let dir = tmp();
        for _ in 0..(MAX_CLAIMS_PER_SWEEP + 2) {
            fs::write(spool_file(dir.path()), "{\"event\":\"e\"}\n").expect("seed");
        }
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), MAX_CLAIMS_PER_SWEEP);
        assert_eq!(spool_files(dir.path()).len(), 2, "the overflow waits");
    }

    #[test]
    fn claim_all_ignores_unrelated_files() {
        let dir = tmp();
        fs::write(dir.path().join("state.json"), "{}").expect("state");
        fs::write(dir.path().join("config.json"), "{}").expect("config");
        fs::write(dir.path().join("telemetry-notes.jsonl"), "{}\n").expect("near-miss");
        fs::write(spool_file(dir.path()), "{\"event\":\"e\"}\n").expect("spool");
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 1);
        for survivor in ["state.json", "config.json", "telemetry-notes.jsonl"] {
            assert!(
                dir.path().join(survivor).exists(),
                "{survivor} must not be claimed"
            );
        }
    }

    #[test]
    fn a_corrupt_line_is_dropped_and_its_siblings_deliver() {
        // Attempt-once (module doc): the good lines parse, the corrupt
        // one is absent; the file is removed after the confirmed delivery
        // the caller performs.
        let dir = tmp();
        let file = spool_file(dir.path());
        fs::write(&file, "{\"event\":\"good\"}\nnot json at all\n").expect("seed");
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].events.len(), 1);
        assert_eq!(claims[0].events[0]["event"], "good");
        remove(&claims[0].path);
        assert!(!file.exists());
    }

    #[test]
    fn a_fully_corrupt_file_delivers_nothing_and_is_removed() {
        // Zero parsed events make the caller's all-confirmed check
        // vacuously true, so the claim is removed: a corrupt file can
        // never wedge the sweep.
        let dir = tmp();
        let file = spool_file(dir.path());
        fs::write(&file, "not json\nalso not json\n").expect("seed");
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 1);
        assert!(claims[0].events.is_empty());
        remove(&claims[0].path);
        assert!(!file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_never_claimed_read_or_cleared() {
        use std::os::unix::fs::symlink;
        let dir = tmp();
        let outside = tmp();
        // A planted link named like a spool file, pointing at a file the
        // user can read: the sweep must not read through it, and clear
        // must not remove it or its target.
        let secret = outside.path().join("secret.jsonl");
        fs::write(&secret, "{\"event\":\"EXFIL\"}\n").expect("secret");
        let link = spool_file(dir.path());
        symlink(&secret, &link).expect("symlink");
        fs::write(spool_file(dir.path()), "{\"event\":\"real\"}\n").expect("spool");

        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 1, "only the regular file is claimed");
        let body = fs::read_to_string(&claims[0].path).expect("claim body");
        assert_eq!(body, "{\"event\":\"real\"}\n");
        assert!(
            !body.contains("EXFIL"),
            "the link target must never be read"
        );

        // The link itself is untouched — still a link, target still there.
        let still_a_link =
            |path: &Path| fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
        assert!(still_a_link(&link), "the planted link survives every sweep");
        assert!(secret.exists());

        // clear() removes this module's files only — the claim from
        // above, never the link or its target.
        assert_eq!(clear(dir.path()), 1);
        assert!(still_a_link(&link));
        assert!(secret.exists());
    }

    #[test]
    fn clear_removes_only_telemetry_files() {
        let dir = tmp();
        fs::write(spool_file(dir.path()), "{\"event\":\"e\"}\n").expect("spool");
        fs::write(
            dir.path().join(format!("{CLAIM_PREFIX}stale{EXTENSION}")),
            "{\"event\":\"e\"}\n",
        )
        .expect("claim");
        fs::write(dir.path().join("state.json"), "{}").expect("state");
        assert_eq!(clear(dir.path()), 2);
        assert!(dir.path().join("state.json").exists());
        assert!(spool_files(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_claim_is_removed_attempt_once() {
        // Attempt-once (module doc): undeliverable content never wedges
        // a sweep — a retained unreadable claim would sort to the head
        // of every later sweep (claim names sort before spool names)
        // and starve delivery, so it is removed on the spot, like a
        // corrupt line.
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        if !permission_bits_enforced(dir.path()) {
            eprintln!(
                "note: this runner bypasses permission bits (root?); \
                 the unreadable-claim enforcement cannot be tested here"
            );
            return;
        }
        let file = spool_file(dir.path());
        fs::write(&file, "{\"event\":\"e\"}\n").expect("seed");
        let mut perms = fs::metadata(&file).expect("metadata").permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&file, perms).expect("unreadable");
        let claims = claim_all(dir.path());
        assert!(claims.is_empty(), "an unreadable claim yields nothing");
        assert_eq!(
            spool_paths(dir.path()).count(),
            0,
            "the unreadable claim is removed, not retained"
        );
    }

    #[test]
    fn the_retention_bound_drops_the_excess_pending_files() {
        // Past MAX_PENDING_FILES the sweep drops the excess (uuid order,
        // the bound is what matters): a machine that stays offline keeps
        // a bounded backlog, mirroring the in-memory drop-on-overflow.
        let dir = tmp();
        for _ in 0..(MAX_PENDING_FILES + 2) {
            fs::write(spool_file(dir.path()), "{\"event\":\"e\"}\n").expect("seed");
        }
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), MAX_CLAIMS_PER_SWEEP);
        // The claims (renamed) plus the unclaimed remainder together
        // stay at the bound: 8 claims + 56 spool files = 64 pending.
        let pending = spool_paths(dir.path()).count();
        assert_eq!(pending, MAX_PENDING_FILES, "the spool is bounded");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_claims_cannot_escape_the_retention_bound() {
        // The failure path the rust review flagged: unreadable files
        // must neither grow past the bound nor starve the sweep. With
        // attempt-once removal each sweep consumes up to the cap of
        // them, and the retention bound still drops the excess beyond
        // MAX_PENDING_FILES among the survivors.
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        if !permission_bits_enforced(dir.path()) {
            eprintln!(
                "note: this runner bypasses permission bits (root?); \
                 the unreadable-claim enforcement cannot be tested here"
            );
            return;
        }
        let seeds = MAX_PENDING_FILES + 6;
        for _ in 0..seeds {
            let file = spool_file(dir.path());
            fs::write(&file, "{\"event\":\"e\"}\n").expect("seed");
            let mut perms = fs::metadata(&file).expect("metadata").permissions();
            perms.set_mode(0o000);
            fs::set_permissions(&file, perms).expect("unreadable");
        }
        let claims = claim_all(dir.path());
        assert!(claims.is_empty(), "unreadable claims yield nothing");
        // One sweep: the cap's worth removed, the rest still bounded.
        assert_eq!(
            spool_paths(dir.path()).count(),
            seeds - MAX_CLAIMS_PER_SWEEP,
            "the sweep consumes the cap's worth and the rest stay bounded"
        );
    }
}

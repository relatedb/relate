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
//! Ordering is by file age — content mtime, which survives the claim
//! rename, so a file's age is the age of the events it holds, not the
//! moment a sweep took it. Claims and spool files share the one order,
//! so a batch of undeliverable claims cannot sort ahead of newer spool
//! files and wall them off; the oldest pending events are always the
//! first delivered and the last dropped.
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
//! sweep drops the NEWEST overflow — the pile becomes the archive of a
//! machine's oldest undelivered events, which cannot be re-recorded,
//! while the newest overflow is statistically redundant with the runs
//! that follow it). Undeliverable content is attempt-once — a
//! corrupt line is dropped with its file after the delivery attempt,
//! and a claim that cannot be read at all is removed on the spot:
//! retained, it would sort to the head of every later sweep and starve
//! delivery.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use relate_feedback::ids::uuid_v4;

use super::envelope::spool_line;

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

/// The bound on one spool file's bytes (ADR-0615's hardening of the
/// hostile-co-tenant shape): a real file carries at most the backlog
/// cap's worth of envelopes, each a few hundred bytes — 1 MiB is two
/// orders above that. A file past the bound is not this module's output
/// and is refused by the capped read (at most the cap plus one byte is
/// ever read — the overflow byte is what refuses it); it is then removed
/// attempt-once, like unreadable content, so it cannot starve a sweep.
pub(crate) const MAX_SPOOL_FILE_BYTES: u64 = 1024 * 1024;

/// Claim files one drain sweep takes; the rest wait for the next enabled
/// run, so a pathological spool directory cannot keep a drain thread
/// alive indefinitely. `pub(crate)` for the drain-side tests that pin
/// the steady one-sweep shape.
pub(crate) const MAX_CLAIMS_PER_SWEEP: usize = 8;

/// The spool's total pending-file bound: past it a sweep drops the newest
/// overflow, keeping the oldest pending files — the machine's earliest
/// undelivered events, which nothing can re-record, while the newest
/// overflow is redundant with the runs that follow it. Mirrors the
/// in-memory backlog's bounded-drop posture so a machine that stays
/// offline keeps a bounded backlog of its oldest pending runs.
const MAX_PENDING_FILES: usize = 64;

/// One file's age: its content mtime, the moment the run that wrote it
/// flushed. The claim rename preserves mtime, so a claimed file keeps the
/// age of the events it holds. A file whose mtime cannot be read sorts as
/// the epoch — the oldest by default: the sweep still claims it, and the
/// rename and read carry their own failure handling.
fn modified_at(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(UNIX_EPOCH)
}

/// The pending files under `dir`, oldest first — the one ordering the
/// sweep claims in and the retention bound cuts from: `(age, name)`, so
/// same-age files (one second's burst of runs) still order deterministically.
fn pending_oldest_first(dir: &Path) -> Vec<PathBuf> {
    let mut keyed: Vec<(SystemTime, PathBuf)> = spool_paths(dir)
        .map(|path| (modified_at(&path), path))
        .collect();
    keyed.sort();
    keyed.into_iter().map(|(_, path)| path).collect()
}

/// This run's spool file path under `dir` (ADR-0545 §1): one per process,
/// so concurrent runs never share a target.
pub(crate) fn spool_file(dir: &Path) -> PathBuf {
    dir.join(file_name(SPOOL_PREFIX, &uuid_v4()))
}

/// The name grammar both file kinds share: `<prefix><id><extension>` —
/// the one producer, so a spool name and a claim name cannot drift apart
/// from what [`is_telemetry_name`] sweeps.
fn file_name(prefix: &str, id: &str) -> String {
    format!("{prefix}{id}{EXTENSION}")
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
/// under `dir`, oldest first, renaming each to a fresh claim name and
/// parsing its lines. Regular files only — a symbolic link is never
/// claimed, read, or renamed. Files beyond the cap stay for the next
/// sweep; so do the files of a concurrent drain this one raced (its
/// rename fails). Every I/O error is swallowed (ADR-0079 §5).
pub(crate) fn claim_all(dir: &Path) -> Vec<Claim> {
    // Oldest first: the sweep's ordering and the retention cut share the
    // one helper, so what claims first and what survives the bound can
    // never disagree about age.
    let paths = pending_oldest_first(dir);
    let mut claims = Vec::new();
    // Every rename counts against the sweep cap — an unreadable claim is
    // still work the sweep did, and a directory full of them must not
    // rename past the cap unbounded.
    let mut taken = 0;
    for path in &paths {
        if taken == MAX_CLAIMS_PER_SWEEP {
            break;
        }
        let claim_path = dir.join(file_name(CLAIM_PREFIX, &uuid_v4()));
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
            let _ = remove(&claim_path);
            continue;
        }
        // An unreadable claim is attempt-once, like a corrupt line: the
        // content is undeliverable, and retaining it would put it at the
        // head of every later sweep (the oldest, by its preserved mtime)
        // — eight such files would wall off delivery for good. A
        // transient failure loses one best-effort batch, never a slot
        // forever.
        let Some(events) = read_lines(&claim_path) else {
            let _ = remove(&claim_path);
            continue;
        };
        claims.push(Claim {
            events,
            path: claim_path,
        });
    }
    // The retention bound, with the claim survivors it applies to
    // (its own policy and doc: [`enforce_retention_bound`]).
    enforce_retention_bound(dir);
    claims
}

/// The retention bound applied to the survivors of a sweep (ADR-0615):
/// re-read the directory — the sweep renamed up to its cap into fresh
/// claim names, so any pre-scan list is stale — oldest first, and drop
/// the excess past [`MAX_PENDING_FILES`] from the NEWEST end. Unreadable
/// claims count like any other pending file, so the bound holds on the
/// failure path too.
fn enforce_retention_bound(dir: &Path) {
    let survivors = pending_oldest_first(dir);
    for excess in survivors.iter().skip(MAX_PENDING_FILES) {
        let _ = remove(excess);
    }
}

/// Remove a claim file after its disposition. The result reaches the
/// sweep, which counts a failed removal as retryable: the claim stays
/// on disk for the next run (which the sink dedups), and the pass stops
/// instead of re-POSTing it. The retention bound and the unreadable and
/// symlink removals ignore it — their files leave with the sweep either
/// way or starve it.
pub(crate) fn remove(path: &Path) -> std::io::Result<()> {
    fs::remove_file(path)
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
/// `None` when the file cannot be read at all or sits past
/// [`MAX_SPOOL_FILE_BYTES`]: the caller's one signal for the
/// attempt-once removal — undeliverable content, never an empty claim,
/// and never an unbounded read.
fn read_lines(path: &Path) -> Option<Vec<Value>> {
    // The one bounded read the workspace owns (relate-wire's read_cap):
    // at most the cap plus one byte is read — the overflow byte is what
    // refuses an oversized file — so a file that grows past the cap
    // between the claim and this read is still refused, and an
    // unopenable or non-UTF-8 file is undeliverable, like the unreadable
    // claim. An empty line needs no case of its own: strict JSON rejects
    // it, exactly as it rejects any corrupt line.
    let file = fs::File::open(path).ok()?;
    let bytes = match relate_wire::read_cap::read_capped(file, MAX_SPOOL_FILE_BYTES) {
        Ok(relate_wire::read_cap::CappedRead::Complete(bytes)) => bytes,
        // The cap refused the file, or the read failed.
        _ => return None,
    };
    let text = String::from_utf8(bytes).ok()?;
    Some(
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    )
}

/// The one pending telemetry file's contents — exactly one file, or
/// the helper names what it found instead. The scan is the production
/// sweep's own ([`spool_paths`]).
#[cfg(test)]
pub(super) fn spool_contents(dir: &Path) -> String {
    let mut found: Vec<PathBuf> = spool_paths(dir).collect();
    assert_eq!(found.len(), 1, "expected one spool file, found {found:?}");
    std::fs::read_to_string(found.remove(0)).expect("read spool file")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::home_dir::permission_bits_enforced;
    use crate::telemetry::event_names;
    use serde_json::json;
    use std::time::Duration;

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
            dir.path().join(file_name(CLAIM_PREFIX, "old")),
            "{\"event\":\"e3\"}\n",
        )
        .expect("seed claim");
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), 3, "both spools and the leftover claim");
        let mut events = event_names(claims.iter().flat_map(|c| c.events.iter()));
        events.sort();
        assert_eq!(events, ["e1", "e1", "e2", "e2", "e3"]);
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
        let _ = remove(&claims[0].path);
        assert!(!file.exists());
    }

    /// A file past the byte bound is not this module's output — the
    /// capped read refuses it after at most cap-plus-one bytes, and a
    /// planted oversized file is removed attempt-once (the
    /// hostile-co-tenant shape the bound exists for).
    #[test]
    fn an_oversized_file_is_capped_and_removed() {
        let dir = tmp();
        let file = spool_file(dir.path());
        fs::write(&file, "x".repeat(MAX_SPOOL_FILE_BYTES as usize + 1)).expect("plant");
        let claims = claim_all(dir.path());
        assert!(claims.is_empty(), "the oversized file yields no claim");
        assert_eq!(
            spool_paths(dir.path()).count(),
            0,
            "the capped claim is removed attempt-once — counted through the module's
             own residue view, so the rename alone cannot satisfy this"
        );
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
        let _ = remove(&claims[0].path);
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
            dir.path().join(file_name(CLAIM_PREFIX, "stale")),
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
        // Past MAX_PENDING_FILES the sweep drops the excess (from the
        // newest end, oldest first): a machine that stays offline keeps a
        // bounded backlog of its oldest pending runs.
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

    /// The sweep claims oldest first: the file written earliest is the
    /// first claim, so the events closest to being shed by the retention
    /// bound are the first delivered. The gaps between the seeds keep
    /// their mtimes distinct on the coarsest filesystem a suite runs on.
    #[test]
    fn the_sweep_claims_oldest_files_first() {
        let dir = tmp();
        for i in 0..3 {
            fs::write(spool_file(dir.path()), format!("{{\"event\":\"e{i}\"}}\n")).expect("seed");
            if i < 2 {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        let claims = claim_all(dir.path());
        let events = event_names(claims.iter().flat_map(|c| c.events.iter()));
        assert_eq!(events, ["e0", "e1", "e2"]);
    }

    /// Past the bound the drop is from the NEWEST end: the pile keeps the
    /// machine's oldest undelivered events (the irreplaceable history —
    /// an install's first events included) and sheds the newest overflow
    /// (statistically redundant with the runs that follow it).
    #[test]
    fn the_retention_bound_drops_the_newest_files() {
        let dir = tmp();
        let seeds = MAX_PENDING_FILES + 2;
        for i in 0..seeds {
            fs::write(spool_file(dir.path()), format!("{{\"event\":\"e{i}\"}}\n")).expect("seed");
            std::thread::sleep(Duration::from_millis(25));
        }
        let claims = claim_all(dir.path());
        assert_eq!(claims.len(), MAX_CLAIMS_PER_SWEEP);
        assert_eq!(spool_paths(dir.path()).count(), MAX_PENDING_FILES);
        // Every marker that still exists anywhere — claimed or pending —
        // through the one extraction, with the pending side read by the
        // module's own read/parse policy so the test's view of "still
        // present" cannot drift from the drain's.
        let mut present = event_names(claims.iter().flat_map(|c| c.events.iter()));
        let pending_events: Vec<serde_json::Value> = spool_paths(dir.path())
            .filter_map(|path| read_lines(&path))
            .flatten()
            .collect();
        present.extend(event_names(pending_events.iter()));
        assert!(
            event_names(claims.first().expect("a claim").events.iter()).contains(&"e0".to_string()),
            "the sweep's first claims are the oldest files"
        );
        let all_markers: std::collections::BTreeSet<String> = present.into_iter().collect();
        let shed_markers = (1..=2).map(|back| format!("e{}", seeds - back));
        for shed in shed_markers {
            assert!(
                !all_markers.contains(&shed),
                "the newest overflow is shed, {shed} must be gone; still present: {all_markers:?}"
            );
        }
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

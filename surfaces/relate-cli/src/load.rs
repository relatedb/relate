//! `--load SOURCE` (LABS-903, ADR-0500): the source half of the two-axis
//! store grammar — `--database` names the store **target** (a local path or
//! `:memory:`), `--load` names where its content comes **from** (a `.rl`
//! path, a native directory, or an `http(s)` URL to a `.rl` bundle). The
//! two dispositions:
//!
//! - **The memory turn-into** — a `--load` whose target is a memory URI
//!   (the `--database` default): the source opens as
//!   [`relate::OpenMode::Ephemeral`], loads as usual, and then releases
//!   the source — nothing is held open or locked afterwards, and a fetched
//!   copy is removed right after the load. Read-only by construction.
//! - **The durable-target load** — a `--load` whose target is a path: the
//!   target must not exist yet (the load creates it), the source — fetched
//!   to scratch when remote — is cloned into it, the scratch is removed
//!   after the clone, and the run opens the target normally (writable by
//!   default). This is the one way to materialize a remote store at a path
//!   the caller chose.
//!
//! One module owns the grammar for both surfaces that carry the flag — the
//! query session and `serve` — so their `--load` behavior cannot drift:
//! [`memory_turn_into`] is the one derivation of the ephemeral fact,
//! [`access_mode`] the one open-mode resolution ([`Flags::open_mode`] and
//! serve's access build both delegate), [`parse_conflict`] the one
//! parse-time conflict policy, [`resolve`] the one target/source
//! resolution, and [`release_source_after_open`] the one post-open release
//! rule. Each surface reports the errors through its own exit contract
//! ([`LoadError::code`]).

use std::fmt;

use relate::{CloneOptions, Database, OpenMode};

/// Whether a `--load` with this target is the memory turn-into — the one
/// derivation of the ephemeral fact, consulted by [`access_mode`],
/// [`parse_conflict`], and [`resolve`]. Defined over the canonical memory
/// classification ([`relate_storage::is_memory_uri`], the predicate the
/// engine's open, `clone`, and telemetry's storage classification all
/// share), so every memory spelling — `:memory:` and the empty URI —
/// agrees instead of a second, narrower `":memory:" == …` literal. A
/// `--load` with any other target is the durable-target load.
pub(crate) fn memory_turn_into(database: &str, load_given: bool) -> bool {
    load_given && relate_storage::is_memory_uri(database)
}

/// The one open-mode resolution for a `--load` run (ADR-0480's access slot
/// with the one LABS-903 implication layered on it): a memory-target load
/// opens as [`OpenMode::Ephemeral`] — the memory turn-into — whatever the
/// access slot said; every other combination is the access slot's own
/// resolution. [`Flags::open_mode`] (the query path) and serve's access
/// build both delegate here, so the implication has exactly one statement
/// and the two surfaces cannot drift on it. Takes the flag's supplied-ness,
/// like [`memory_turn_into`] — only [`resolve`] needs the source string.
pub(crate) fn access_mode(database: &str, load_given: bool, access: Option<OpenMode>) -> OpenMode {
    if memory_turn_into(database, load_given) {
        OpenMode::Ephemeral
    } else {
        crate::parse_error::resolve_access(access)
    }
}

/// The `--load` static flag conflicts, refused at parse time — before any
/// post-parse resolution, so a `--fetch-params` source is never fetched
/// before the usage error. Takes the flag's supplied-ness, like
/// [`memory_turn_into`] and [`access_mode`] — only [`resolve`] needs the
/// source string. `None` when no `--load` was given or the combination
/// is sound (both messages spell the access flag through
/// [`crate::parse_error::ACCESS_FLAG`], the one vocabulary fact):
///
/// - `--read-only=live` with a `--load` (either target): a loaded source is
///   loaded once and released — there is no WAL left to follow, whatever
///   the target.
/// - any `--read-only` sighting with a **durable** `--load` target: the
///   load creates the store there and then opens it writable; a read-only
///   sighting cannot govern a load. (With a memory target a plain
///   `--read-only` is a harmless restatement — the turn-into is read-only
///   by construction — so only `=live` is refused there, by the rule
///   above.)
pub(crate) fn parse_conflict(
    database: &str,
    load_given: bool,
    access: Option<OpenMode>,
) -> Option<String> {
    use crate::parse_error::ACCESS_FLAG;
    if !load_given {
        return None;
    }
    if access.is_some_and(|m| m.follows()) {
        return Some(format!(
            "--load cannot follow: the source is loaded once and released — {ACCESS_FLAG}=live \
             needs a --database store"
        ));
    }
    if !memory_turn_into(database, true) && access.is_some() {
        return Some(format!(
            "{ACCESS_FLAG} cannot govern a --load that creates {database}: --load into a path \
             creates the store there, then opens it writable"
        ));
    }
    None
}

/// One `--load` resolution's error: a usage error (exit 2, the query path's
/// usage-error code — a refused value shape, safe mode, or a target that
/// already exists) or a runtime failure (exit 1, like a `--fetch-params`
/// source or a failed open — the fetch itself, or a failed clone). The
/// usage(2)/runtime(1) split itself is
/// [`crate::exit_class::ExitClass`]'s, stated once for the crate; this
/// type carries only the class and the message.
#[derive(Debug)]
pub(crate) enum LoadError {
    Usage(String),
    Runtime(String),
}

impl LoadError {
    /// The exit code the error class carries.
    pub(crate) fn code(&self) -> i32 {
        self.class().code()
    }

    /// The exit class — the projection onto [`crate::exit_class::ExitClass`].
    fn class(&self) -> crate::exit_class::ExitClass {
        match self {
            LoadError::Usage(_) => crate::exit_class::ExitClass::Usage,
            LoadError::Runtime(_) => crate::exit_class::ExitClass::Runtime,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Usage(m) | LoadError::Runtime(m) => f.write_str(m),
        }
    }
}

/// The local path or fetched scratch copy a `--load` source resolves to,
/// with the disposition it arrived by — the classification
/// [`remote_db::plan`] already produced, carried to the caller instead of
/// collapsed into a bare path and re-derived there.
enum Materialized {
    /// A local source, passed through unchanged.
    Local(String),
    /// A remote source's fetched scratch copy.
    Staged(String),
}

/// The codec knobs a durable-target load creates the store with (round 4,
/// R-M2): the run's resolved `--snapshot-format` and
/// `--markdown-body-properties`, so the store the clone writes and the
/// open that follows it agree — a differing codec must not create a store
/// the open then refuses, leaving a broken target behind. `serve` carries
/// neither flag and passes the defaults (its open never states a codec).
#[derive(Default)]
pub(crate) struct CreateCodecs {
    /// The destination snapshot codec (`CloneOptions::to_snapshot_format`).
    pub(crate) to_snapshot_format: Option<relate::SnapshotCodecKind>,
    /// The markdown vault's body-property configuration.
    pub(crate) markdown_body_properties: Option<relate::MarkdownBodyProperties>,
}

/// Resolve the store path to open from the two-axis grammar: `database` is
/// the `--database` target, `load` the `--load` source, `create` the codec
/// knobs a durable-target load creates the store with. The target axis is
/// refused first, whatever `--load` says — a URL is a source, never a
/// store to open, so `-d https://…` alone and `-d https://… --load x.rl`
/// are the same exit-2 refusal, before any fetch. With no `--load`, the
/// target is the store; with one, the memory turn-into or the
/// durable-target load applies. Announcements print from here through
/// [`crate::print_notice`] — the load's announcements are success lines,
/// not diagnostics, so they use the notice sink — so both surfaces report
/// the load identically.
pub(crate) fn resolve(
    database: &str,
    load: Option<&str>,
    safe_mode: bool,
    create: CreateCodecs,
) -> Result<String, LoadError> {
    if crate::remote_db::is_remote_uri(database) {
        return Err(LoadError::Usage(format!(
            "--database names the store to open (a local path or :memory:); a remote store \
             is a --load source: --load {database}"
        )));
    }
    let Some(source) = load else {
        return Ok(database.to_string());
    };
    if memory_turn_into(database, true) {
        // The turn-into: the source (or its fetched scratch copy) opens as
        // the ephemeral store; the caller releases the scratch right after
        // the open ([`release_source_after_open`]). A local source is its
        // own announcement-free path; a fetched one names its scratch copy
        // — the machine shape the e2e announcement parse cuts at.
        return match materialize(source, safe_mode)? {
            Materialized::Staged(path) => {
                crate::print_notice(format!(
                    "--load {source}: downloaded to {path}, turning into the in-memory store \
                     (the copy is released after the load)"
                ));
                Ok(path)
            }
            Materialized::Local(path) => Ok(path),
        };
    }
    // The durable-target load: the target must not exist yet (fail before
    // any fetch — the check is a local stat), the source — fetched to
    // scratch when remote — is cloned into it with the run's codec knobs,
    // the scratch is removed after the clone, and the target opens
    // normally. The disposition the source arrived by decides the
    // announcement at this one match — fetched-vs-local has no second
    // representation.
    if std::path::Path::new(database).exists() {
        return Err(LoadError::Usage(format!(
            "--load creates a store at a path that does not exist yet; {database} exists — open \
             it with --database alone"
        )));
    }
    match materialize(source, safe_mode)? {
        // A local source: the clone is the whole story, and the
        // announcement names what it loaded into.
        Materialized::Local(source_path) => {
            clone_into(&source_path, database, source, create)?;
            crate::print_notice(format!("--load {source}: loaded into {database}"));
        }
        // A fetched source: the scratch is unreferenced the moment the
        // clone returns — remove it now, never at exit — and the
        // announcement names the copy it removed, the same machine shape
        // the turn-into's does.
        Materialized::Staged(source_path) => {
            clone_into(&source_path, database, source, create)?;
            crate::remote_db::cleanup_staged();
            crate::print_notice(format!(
                "--load {source}: downloaded to {source_path}, loaded into {database} (the copy \
                 is removed after the clone)"
            ));
        }
    }
    Ok(database.to_string())
}

/// The durable-target load's one clone step: the created store takes the
/// run's codec knobs ([`CreateCodecs`]), so it and the open that follows
/// agree. The source's WAL directory is the source's own (`--wal-dir` on
/// the query path names the OPENED store's, applied by the open).
fn clone_into(
    source_path: &str,
    database: &str,
    source: &str,
    create: CreateCodecs,
) -> Result<(), LoadError> {
    Database::clone(
        source_path,
        database,
        CloneOptions {
            wal_dir: None,
            to_snapshot_format: create.to_snapshot_format,
            markdown_body_properties: create.markdown_body_properties,
        },
    )
    .map_err(|e| LoadError::Runtime(format!("--load {source}: {e}")))
}

/// The post-open release rule, one statement (LABS-903, ADR-0500): an
/// ephemeral open — the `--load` memory turn-into — released the source at
/// the open (the engine closed the host handle), so a fetched scratch copy
/// is unreferenced the moment the open returns and is removed then, never
/// at exit. Keyed on the open mode — the one fact both surfaces already
/// carry to the open ([`Flags::open_mode`] here, `ServeCmd::access` there),
/// the one representation of the turn-into there is — and called by both
/// post-open sites. The durable-target load's own release (the moment the
/// clone returns) lives in [`resolve`].
pub(crate) fn release_source_after_open(mode: OpenMode) {
    if mode == OpenMode::Ephemeral {
        crate::remote_db::cleanup_staged();
    }
}

/// The local path or fetched scratch copy a `--load` source resolves to:
/// [`crate::remote_db::plan`] classifies (and refuses — value shape, safe
/// mode — as usage errors, before any network I/O), the gated fetch stages
/// the remote bytes as scratch (a runtime error), and a local source passes
/// through unchanged.
fn materialize(source: &str, safe_mode: bool) -> Result<Materialized, LoadError> {
    match crate::remote_db::plan(source, safe_mode).map_err(LoadError::Usage)? {
        Some(remote) => crate::remote_db::fetch_and_stage(&remote)
            .map(|staged| Materialized::Staged(staged.display().to_string()))
            .map_err(LoadError::Runtime),
        None => Ok(Materialized::Local(source.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared durable-load fixture: a seeded store directory cloned
    /// into a `.rl` bundle (the source both durable-target tests load).
    fn seeded_bundle(home: &std::path::Path) -> std::path::PathBuf {
        let db = home.join("seed");
        {
            let db = Database::open(db.to_str().unwrap()).unwrap();
            let mut conn = db.connect().unwrap();
            conn.execute("CREATE (p:Person {name: 'Alice'})").unwrap();
            db.checkpoint().unwrap();
        }
        let source = home.join("source.rl");
        Database::clone(
            db.to_str().unwrap(),
            source.to_str().unwrap(),
            CloneOptions::default(),
        )
        .unwrap();
        source
    }

    #[test]
    fn the_memory_turn_into_is_a_load_with_a_memory_target() {
        // The target decides the disposition, and only a load can turn
        // into: a bare memory --database is the plain memory open. Every
        // memory spelling counts — the empty URI is memory to the engine
        // and to telemetry's classification alike.
        assert!(memory_turn_into(":memory:", true));
        assert!(memory_turn_into("", true));
        assert!(!memory_turn_into("out.rl", true));
        assert!(!memory_turn_into(":memory:", false));
        assert!(!memory_turn_into("out.rl", false));
    }

    #[test]
    fn the_one_access_mode_resolution_applies_the_turn_into() {
        // The memory-target load is the ephemeral mode whatever the access
        // slot said; every other combination is the slot's own resolution
        // (ADR-0480) — including the no-flag writable default.
        for access in [None, Some(OpenMode::ReadOnly)] {
            assert_eq!(access_mode(":memory:", true, access), OpenMode::Ephemeral);
        }
        assert_eq!(access_mode("out.rl", true, None), OpenMode::Writable);
        assert_eq!(
            access_mode("out.rl", true, Some(OpenMode::ReadOnly)),
            OpenMode::ReadOnly
        );
        assert_eq!(access_mode("db.rl", false, None), OpenMode::Writable);
    }

    #[test]
    fn a_load_cannot_follow_whatever_the_target() {
        for database in [":memory:", "out.rl"] {
            let conflict = parse_conflict(database, true, Some(OpenMode::Follow)).expect(database);
            assert!(
                conflict.contains("--load cannot follow"),
                "{database}: {conflict}"
            );
        }
    }

    #[test]
    fn a_read_only_sighting_governs_only_a_durable_load_target() {
        // Plain --read-only with the memory target is a harmless
        // restatement (the turn-into is read-only by construction).
        assert_eq!(
            parse_conflict(":memory:", true, Some(OpenMode::ReadOnly)),
            None
        );
        // With a durable target it cannot govern the load that creates it.
        let conflict = parse_conflict("out.rl", true, Some(OpenMode::ReadOnly))
            .expect("the durable-target conflict");
        assert!(
            conflict.contains("--read-only cannot govern a --load that creates out.rl"),
            "{conflict}"
        );
    }

    #[test]
    fn no_load_has_no_conflict_whatever_the_access() {
        for access in [None, Some(OpenMode::ReadOnly), Some(OpenMode::Follow)] {
            assert_eq!(parse_conflict("db.rl", false, access), None);
        }
    }

    #[test]
    fn a_remote_database_value_is_refused_whether_or_not_a_load_is_given() {
        // The target-axis refusal is one rule for both dispositions — the
        // same exit-2 refusal before any fetch, never a fetch followed by
        // the engine's clone error.
        for load in [None, Some("x.rl")] {
            for uri in ["https://example.com/s.rl", "HTTP://127.0.0.1:9/s.rl"] {
                let err = resolve(uri, load, false, CreateCodecs::default()).unwrap_err();
                assert_eq!(err.code(), 2, "{uri} / {load:?}");
                assert!(
                    err.to_string()
                        .contains("a remote store is a --load source"),
                    "{uri} / {load:?}: {err}"
                );
            }
        }
    }

    #[test]
    fn a_local_database_value_passes_through_unchanged() {
        for database in [":memory:", "", "db.rl", "./dir"] {
            let resolved = resolve(database, None, false, CreateCodecs::default()).unwrap();
            assert_eq!(resolved, database);
        }
    }

    #[test]
    fn a_durable_target_load_refuses_an_existing_target_before_any_fetch() {
        // The target exists (a real temp file), so the refusal is the local
        // stat's — safe mode is on and a remote source would be refused by
        // it first only if the stat passed; here the exists-refusal wins
        // and no plan runs.
        let existing = tempfile::tempdir().unwrap();
        let target = existing.path().join("there.rl");
        std::fs::write(&target, b"x").unwrap();
        let err = resolve(
            target.to_str().unwrap(),
            Some("https://example.com/s.rl"),
            true,
            CreateCodecs::default(),
        )
        .unwrap_err();
        assert_eq!(err.code(), 2);
        assert!(err.to_string().contains("does not exist yet"), "{err}");
    }

    #[test]
    fn a_local_source_with_a_durable_target_loads_by_clone() {
        let home = tempfile::tempdir().unwrap();
        // The shared fixture: a seeded store cloned into a bundle.
        let source = seeded_bundle(home.path());

        let target = home.path().join("target.rl");
        let resolved = resolve(
            target.to_str().unwrap(),
            Some(source.to_str().unwrap()),
            false,
            CreateCodecs::default(),
        )
        .expect("the clone loads");
        assert_eq!(resolved, target.to_str().unwrap());
        assert!(target.is_file(), "the clone landed");

        // The target is a real store: it opens and answers.
        let loaded = Database::open(target.to_str().unwrap()).unwrap();
        let mut conn = loaded.connect().unwrap();
        let rows = conn
            .execute("MATCH (p:Person) RETURN count(p) AS c")
            .unwrap();
        assert_eq!(rows.rows[0][0], relate::Value::Int(1));
    }

    #[test]
    fn a_durable_target_load_creates_the_store_with_the_runs_codec() {
        // R-M2: the clone takes the run's codec knobs, so the created store
        // and the open that follows it agree — observable as the JSON
        // snapshot a Json-requesting load leaves behind (the default codec
        // would have written a Parquet manifest instead).
        let home = tempfile::tempdir().unwrap();
        let source = seeded_bundle(home.path());
        let target = home.path().join("target-dir");
        resolve(
            target.to_str().unwrap(),
            Some(source.to_str().unwrap()),
            false,
            CreateCodecs {
                to_snapshot_format: Some(relate::SnapshotCodecKind::Json),
                markdown_body_properties: None,
            },
        )
        .expect("the clone loads");
        assert!(
            target.join("graph.json").is_file(),
            "the created store carries the requested codec's snapshot"
        );
    }
}

//! Telemetry state under the caller's config directory (ADR-0079 §3, §6;
//! ADR-0588 §1): `$XDG_CONFIG_HOME/relate/` when set, otherwise
//! `~/.config/relate/` — the directory is the caller's argument, the CLI
//! keeps the one config-directory rule.
//!
//! - `state.json` holds `install_id` (a random UUID v4, generated once and
//!   reused thereafter), `notice_shown` (the one-time first-run-notice
//!   flag), `feedback_ask_last` (when the feedback grant ask last ran —
//!   one record both ask policies derive from, ADR-0588 §3 as revised),
//!   `cli_version_last_run` (the CLI version this config directory last
//!   ran under — the once-per-version delivery drain's trigger and
//!   claim) and `delivery_backoff_until` (until when exits skip the
//!   delivery drain's wait after a failed attempt — the delivery
//!   cooldown).
//! - `config.json` holds the persistent opt-out key `telemetry` (bool) and
//!   the feedback consent record.
//!
//! Writes are atomic-ish (a non-predictable staging file in the same
//! directory, created exclusively with the mode at birth, then renamed over
//! the target). Each file's read-edit-write is serialized by its own
//! exclusive OS lock — `config.lock` and `state.lock` — so concurrent
//! in-product CLI writers cannot drop each other's keys or revert each
//! other's once-per-install flag; a writer outside the CLI (a hand edit) is
//! not serialized with them, and is guarded instead by the staged write,
//! which never leaves a torn file, and by the refuse-to-rewrite-unparsable
//! rule, which never replaces keys it could not read. `state.json` reads
//! tolerate a missing or corrupt file by regenerating; `config.json` reads
//! distinguish the two — a missing file is an empty map, an unreadable or
//! corrupt one a typed error — because an update from an empty map would
//! drop the keys it could not parse.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use fs4::FileExt;

use crate::ids::uuid_v4;

/// State filename holding the install ID and notice flag.
const STATE_FILE: &str = "state.json";

/// Config filename holding the persistent `telemetry` opt-out key.
const CONFIG_FILE: &str = "config.json";

/// Lock filename serializing the `config.json` read-edit-write.
const CONFIG_LOCK_FILE: &str = "config.lock";

/// Lock filename serializing the `state.json` claim read-edit-write.
const STATE_LOCK_FILE: &str = "state.lock";

/// Contents of `state.json` (ADR-0079 §3, §6). The serde derive is the one
/// schema statement — `save_state` and `read_state` serialize and
/// deserialize it (plus the `install_id` shape check), never re-spelling
/// the field list. Unknown keys (a legacy `feedback_ask_shown`) are
/// ignored; every field but `install_id` defaults for a state file written
/// before it existed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// Random UUID v4, generated once and reused thereafter.
    pub install_id: String,
    /// Whether the one-time first-run notice has been printed.
    #[serde(default)]
    pub notice_shown: bool,
    /// When the feedback grant ask last ran — the one ask record both ask
    /// policies derive from (ADR-0588 §3 as revised): the first-run ask
    /// reads "ever answered" from its presence, the windowed
    /// install/upgrade ask compares it to the grant window. A stamp is
    /// written for every answered ask, grant or decline, so a decline
    /// suppresses re-asks for a window, not forever.
    #[serde(default)]
    pub feedback_ask_last: Option<chrono::DateTime<chrono::Utc>>,
    /// The CLI version this config directory last ran under: a run whose
    /// version differs from the recorded one is the once-per-version
    /// delivery drain (the spool's accumulated backlog drains at that
    /// run's exit), and the run records its version whether or not the
    /// delivery succeeded — attempt-once per version, so a machine that
    /// cannot deliver yet does not pay the elevated drain budget on every
    /// subsequent run.
    #[serde(default)]
    pub cli_version_last_run: Option<String>,
    /// Until when CLI exits skip the delivery drain's wait: stamped after
    /// a drain attempt that failed or ran out of budget, so a machine
    /// with a pending backlog and an unreachable sink does not pay the
    /// wait on every exit. `None` or a past stamp means exits wait again;
    /// the drain thread itself still attempts delivery inside every run.
    #[serde(default)]
    pub delivery_backoff_until: Option<chrono::DateTime<chrono::Utc>>,
}

impl State {
    /// One-time first-run-notice policy (ADR-0079 §6): `true` exactly once
    /// per install — on the first run with telemetry enabled, interactive
    /// or not — setting `notice_shown` as it does.
    pub fn claim_notice(&mut self, dir: &Path) -> io::Result<bool> {
        self.update_locked(dir, |current| {
            if current.notice_shown {
                false
            } else {
                current.notice_shown = true;
                true
            }
        })
    }

    /// Stamp the ask record (`feedback_ask_last` = `now`): both ask surfaces
    /// write the one record, on the same locked read-modify-write as the
    /// notice claim.
    pub fn stamp_feedback_ask(
        &mut self,
        dir: &Path,
        now: chrono::DateTime<chrono::Utc>,
    ) -> io::Result<()> {
        self.update_locked(dir, |current| {
            current.feedback_ask_last = Some(now);
            true
        })
        .map(|_| ())
    }

    /// Record the CLI version this config directory now runs under (the
    /// once-per-version delivery drain's claim): writes only when the
    /// recorded version differs, so every run after the claim compares
    /// and writes nothing. Same locked shape as the notice claim, so
    /// concurrent first runs of the same version claim it once.
    pub fn stamp_cli_version(&mut self, dir: &Path, version: &str) -> io::Result<()> {
        self.update_locked(dir, |current| {
            if current.cli_version_last_run.as_deref() == Some(version) {
                false
            } else {
                current.cli_version_last_run = Some(version.to_string());
                true
            }
        })
        .map(|_| ())
    }

    /// Stamp the delivery backoff window's end (the drain's failed-attempt
    /// cooldown): exits inside the window skip the drain wait; the next
    /// enabled run's drain thread still attempts delivery regardless.
    /// Unconditional like the ask stamp — the window is the latest
    /// failure's, and a fresh timestamp never repeats.
    pub fn stamp_delivery_backoff(
        &mut self,
        dir: &Path,
        until: chrono::DateTime<chrono::Utc>,
    ) -> io::Result<()> {
        self.update_locked(dir, |current| {
            current.delivery_backoff_until = Some(until);
            true
        })
        .map(|_| ())
    }

    /// The one locked read-modify-write every state mutation rides: the
    /// re-read under the state lock instead of writing back the snapshot
    /// this value was loaded from — two concurrent CLI runs each hold their
    /// own snapshot, and the second writer would otherwise put its own flag
    /// over the first's record and a once-per-install claim would fire
    /// twice. `edit` returns whether anything changed; `false` writes
    /// nothing. `self` is left holding what the file now holds, and the
    /// write failure is returned so a caller can tell a recorded claim from
    /// one that will repeat rather than vanishing unasked.
    fn update_locked(
        &mut self,
        dir: &Path,
        edit: impl FnOnce(&mut State) -> bool,
    ) -> io::Result<bool> {
        let _lock = lock_state(dir)?;
        let mut current = read_state(dir).unwrap_or_else(fresh_state);
        if !edit(&mut current) {
            *self = current;
            return Ok(false);
        }
        save_state(dir, &current)?;
        *self = current;
        Ok(true)
    }
}

/// Path of `state.json` inside a config directory.
pub fn state_path(dir: &Path) -> PathBuf {
    dir.join(STATE_FILE)
}

/// Path of `config.json` inside a config directory.
pub fn config_path(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}

/// Whether [`load_or_create_state`] could record the state it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    /// The state was read from, or written to, `state.json`: the install ID
    /// is stable across runs.
    Durable,
    /// The fresh state could not be recorded: it is returned anyway, and the
    /// install ID will change next run.
    Ephemeral,
}

/// Load `state.json`, or generate a fresh install ID and persist it when the
/// file is missing or corrupt. The install ID is therefore stable across
/// runs of the same config directory. A write failure is not fatal: the
/// fresh state is still returned with [`Persistence::Ephemeral`], so the
/// caller decides what a state it cannot record is worth.
///
/// The read-or-create runs under the state lock, re-reading inside it: two
/// concurrent first runs would otherwise each mint an install ID, each
/// report [`Persistence::Durable`], and the second write would discard the
/// ID the first caller is still using.
pub fn load_or_create_state(dir: &Path) -> (State, Persistence) {
    let Ok(_lock) = lock_state(dir) else {
        return (fresh_state(), Persistence::Ephemeral);
    };
    if let Some(state) = read_state(dir) {
        return (state, Persistence::Durable);
    }
    let state = fresh_state();
    let persistence = match save_state(dir, &state) {
        Ok(()) => Persistence::Durable,
        Err(_) => Persistence::Ephemeral,
    };
    (state, persistence)
}

/// A state with no record behind it yet: a fresh install ID and both
/// once-per-install flags unset. The one construction site of a state that
/// is not read from the file.
fn fresh_state() -> State {
    State {
        install_id: uuid_v4(),
        notice_shown: false,
        feedback_ask_last: None,
        cli_version_last_run: None,
        delivery_backoff_until: None,
    }
}

/// Persist `state.json` (atomic-ish: staging file + rename). The derive is
/// the schema: no field list is spelled here.
pub fn save_state(dir: &Path, state: &State) -> io::Result<()> {
    let body =
        serde_json::to_string(state).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic(&state_path(dir), body.as_bytes())
}

/// The parsed `config.json` object. A missing file is an empty map; an
/// unreadable or corrupt one is an [`io::ErrorKind::InvalidData`] error, so a
/// reader can tell "nothing recorded yet" from "this file cannot be trusted".
/// `config.json` holds more than one key (the telemetry opt-out and the
/// feedback consent record), so callers read and rewrite the whole object
/// instead of a single key's value.
pub fn load_config(dir: &Path) -> io::Result<serde_json::Map<String, serde_json::Value>> {
    let path = config_path(dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(serde_json::Map::new());
        }
        Err(error) => return Err(error),
    };
    serde_json::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", path.display()),
        )
    })
}

/// Rewrite `config.json` (atomic-ish) after `edit` mutates the loaded
/// object: every other key survives the write. A file that cannot be parsed
/// is refused rather than overwritten from an empty map, because the write
/// would silently drop every key it could not read.
pub fn update_config(
    dir: &Path,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> io::Result<()> {
    // Concurrent CLI runs both update this file; the exclusive lock makes
    // the read-edit-write atomic across processes (and threads), so one
    // writer cannot drop the other's keys. The OS releases it when the
    // holder exits, so a crash leaves no stale lock behind.
    let _lock = lock_config(dir)?;
    let mut config = load_config(dir)?;
    edit(&mut config);
    write_atomic(
        &config_path(dir),
        serde_json::Value::Object(config).to_string().as_bytes(),
    )
}

/// Take the exclusive update lock on `config.lock`, creating the config
/// directory when missing.
fn lock_config(dir: &Path) -> io::Result<File> {
    lock_file(dir, CONFIG_LOCK_FILE)
}

/// Take the exclusive update lock on `state.lock`, creating the config
/// directory when missing.
fn lock_state(dir: &Path) -> io::Result<File> {
    lock_file(dir, STATE_LOCK_FILE)
}

/// The one lock: an exclusive OS lock on `name` inside `dir`, created when
/// missing, blocking until the current holder releases it. The OS releases
/// it when the holder exits, so a crash leaves no stale lock behind.
fn lock_file(dir: &Path, name: &str) -> io::Result<File> {
    fs::create_dir_all(dir)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))?;
    file.lock_exclusive()?;
    Ok(file)
}

/// The `telemetry` key from `config.json`, keeping the one distinction the
/// opt-out ladder needs: nothing recorded, a value, or a file that cannot be
/// trusted. A corrupt file — or a `telemetry` key that is present but not a
/// boolean — must not read as "nothing recorded", which would fall through to
/// the enabled default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTelemetry {
    /// No file, or no `telemetry` key.
    Missing,
    /// The recorded opt-out value.
    Value(bool),
    /// The file cannot be read or parsed, or the key holds a non-boolean.
    Unreadable,
}

/// The typed [`ConfigTelemetry`] read of `config.json`.
pub fn load_config_telemetry(dir: &Path) -> ConfigTelemetry {
    let Ok(config) = load_config(dir) else {
        return ConfigTelemetry::Unreadable;
    };
    match config.get("telemetry") {
        None => ConfigTelemetry::Missing,
        Some(serde_json::Value::Bool(enabled)) => ConfigTelemetry::Value(*enabled),
        Some(_) => ConfigTelemetry::Unreadable,
    }
}

/// Persist the `telemetry` key in `config.json` (atomic-ish), keeping the
/// other keys (the feedback consent record) intact.
pub fn save_config_telemetry(dir: &Path, enabled: bool) -> io::Result<()> {
    update_config(dir, |config| {
        config.insert("telemetry".to_string(), enabled.into());
    })
}

fn read_state(dir: &Path) -> Option<State> {
    let text = fs::read_to_string(state_path(dir)).ok()?;
    let state: State = serde_json::from_str(&text).ok()?;
    if !is_uuid_shape(&state.install_id) {
        return None;
    }
    Some(state)
}

/// Loose UUID shape check: 8-4-4-4-12 hex groups with dashes. Corrupt or
/// hand-mangled IDs are regenerated rather than trusted.
fn is_uuid_shape(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(lengths)
            .all(|(p, len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Persist `bytes` to `path` through the crate's staged-write kit, created
/// 0600 at birth: both state files hold private data — the install UUID and
/// the opt-out choice.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::atomic_file::write_atomic(path, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_id_is_generated_once_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("relate");
        let (first, persistence) = load_or_create_state(&dir);
        assert_eq!(persistence, Persistence::Durable);
        assert!(is_uuid_shape(&first.install_id));
        assert!(!first.notice_shown);
        assert!(first.feedback_ask_last.is_none());
        assert!(state_path(&dir).is_file());

        let (second, _) = load_or_create_state(&dir);
        assert_eq!(first, second, "install_id must be stable across loads");
    }

    /// Two CLI runs that each loaded the state before either claimed must
    /// both keep their flag: the claim re-reads the record under the lock
    /// rather than writing its own stale snapshot back over the other's.
    /// Writing the loaded snapshot instead loses the first claim.
    #[test]
    fn concurrent_claims_keep_each_others_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("relate");
        let (mut first, _) = load_or_create_state(&dir);
        let (mut second, _) = load_or_create_state(&dir);

        assert!(first.claim_notice(&dir).unwrap());
        // The ask record stamps the same locked shape the notice claims.
        assert!(second.stamp_feedback_ask(&dir, chrono::Utc::now()).is_ok());

        let (recorded, _) = load_or_create_state(&dir);
        assert!(recorded.notice_shown, "{recorded:?}");
        assert!(recorded.feedback_ask_last.is_some(), "{recorded:?}");
        assert_eq!(recorded.install_id, first.install_id);

        // A second claim of the same flag is still not a fresh claim.
        assert!(!second.claim_notice(&dir).unwrap());
        assert!(
            !first.claim_notice(&dir).unwrap(),
            "a second claim of any flag is not fresh"
        );
    }

    /// A state that cannot be written is returned with the fact that it is
    /// ephemeral, not silently reported as recorded.
    #[test]
    fn an_unrecordable_state_is_ephemeral() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // A directory where state.json should be makes the write fail.
        fs::create_dir_all(state_path(&dir)).unwrap();
        let (state, persistence) = load_or_create_state(&dir);
        assert!(is_uuid_shape(&state.install_id));
        assert_eq!(persistence, Persistence::Ephemeral);
    }

    #[test]
    fn state_roundtrip_preserves_notice_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let (mut state, _) = load_or_create_state(dir.as_path());
        state.notice_shown = true;
        save_state(dir.as_path(), &state).unwrap();
        let (reloaded, _) = load_or_create_state(dir.as_path());
        assert_eq!(reloaded, state);
    }

    #[test]
    fn claim_notice_is_true_once_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let (mut state, _) = load_or_create_state(dir);
        assert!(
            state.claim_notice(dir).unwrap(),
            "first check shows the notice"
        );
        assert!(
            !state.claim_notice(dir).unwrap(),
            "second check is suppressed by the notice_shown flag"
        );
        // A later process (fresh load, same dir) sees the persisted flag.
        assert!(load_or_create_state(dir).0.notice_shown);
    }

    #[test]
    fn the_ask_stamp_persists_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let (mut state, _) = load_or_create_state(dir);
        let first = chrono::Utc::now();
        state.stamp_feedback_ask(dir, first).unwrap();
        // A later process (fresh load, same dir) sees the persisted stamp,
        // and a later ask overwrites it — one record, latest ask.
        assert_eq!(load_or_create_state(dir).0.feedback_ask_last, Some(first));
        let second = first + chrono::Duration::hours(1);
        state.stamp_feedback_ask(dir, second).unwrap();
        assert_eq!(load_or_create_state(dir).0.feedback_ask_last, Some(second));
    }

    /// A claim whose record could not be written is an error, not a claim:
    /// the notice repeats on the next run instead of vanishing for good.
    #[test]
    fn an_unrecordable_claim_is_an_error_and_leaves_the_flag_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // A directory where state.json should be makes the write fail.
        fs::create_dir_all(state_path(&dir)).unwrap();
        let mut state = State {
            install_id: "123e4567-e89b-42d3-a456-426614174000".to_string(),
            notice_shown: false,
            feedback_ask_last: None,
            cli_version_last_run: None,
            delivery_backoff_until: None,
        };
        assert!(state.claim_notice(&dir).is_err());
        assert!(!state.notice_shown, "an unrecorded claim is not claimed");
    }

    #[test]
    fn cli_version_stamp_writes_only_on_change_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let (mut state, _) = load_or_create_state(dir);
        // A fresh state has no recorded version; the first stamp writes,
        // the second (same version) does not, and a later different
        // version writes again — the once-per-version claim.
        assert_eq!(state.cli_version_last_run, None);
        state.stamp_cli_version(dir, "1.0.3").unwrap();
        state.stamp_cli_version(dir, "1.0.3").unwrap();
        assert_eq!(
            load_or_create_state(dir).0.cli_version_last_run,
            Some("1.0.3".to_string())
        );
        state.stamp_cli_version(dir, "1.0.4").unwrap();
        assert_eq!(
            load_or_create_state(dir).0.cli_version_last_run,
            Some("1.0.4".to_string())
        );
    }

    #[test]
    fn delivery_backoff_stamp_persists_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let (mut state, _) = load_or_create_state(dir);
        let first = chrono::Utc::now() + chrono::Duration::minutes(15);
        state.stamp_delivery_backoff(dir, first).unwrap();
        assert_eq!(
            load_or_create_state(dir).0.delivery_backoff_until,
            Some(first)
        );
        // A later stamp overwrites — the window is the latest failure's.
        let second = first + chrono::Duration::minutes(15);
        state.stamp_delivery_backoff(dir, second).unwrap();
        assert_eq!(
            load_or_create_state(dir).0.delivery_backoff_until,
            Some(second)
        );
    }

    /// A state file written before the two delivery fields existed loads
    /// with both absent — the schema addition is additive, and an older
    /// binary reading a newer file ignores the keys it does not know.
    #[test]
    fn pre_delivery_state_files_load_with_both_fields_default() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            state_path(&dir),
            r#"{"install_id": "123e4567-e89b-42d3-a456-426614174000", "notice_shown": true}"#,
        )
        .unwrap();
        let (state, _) = load_or_create_state(&dir);
        assert_eq!(state.cli_version_last_run, None);
        assert_eq!(state.delivery_backoff_until, None);
        assert!(state.notice_shown);
    }

    #[test]
    fn corrupt_state_file_is_regenerated_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(state_path(&dir), "not json at all").unwrap();
        let (state, _) = load_or_create_state(&dir);
        assert!(is_uuid_shape(&state.install_id));
        // The regenerated state replaces the corrupt file.
        let (reloaded, _) = load_or_create_state(&dir);
        assert_eq!(state, reloaded);
    }

    #[test]
    fn state_with_invalid_install_id_is_regenerated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            state_path(&dir),
            r#"{"install_id": "not-a-uuid", "notice_shown": true}"#,
        )
        .unwrap();
        let (state, _) = load_or_create_state(&dir);
        assert!(is_uuid_shape(&state.install_id));
        assert!(!state.notice_shown);
    }

    /// Nothing recorded, a recorded value, and an untrusted value read three
    /// ways — and an untrusted `telemetry` value fails closed in the ladder
    /// exactly like a corrupt file: it never reads as "nothing recorded",
    /// which would re-enable telemetry.
    #[test]
    fn config_telemetry_read_distinguishes_missing_value_and_unreadable() {
        use crate::opt_out;
        use crate::test_support::env_of;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Missing);
        fs::create_dir_all(&dir).unwrap();
        fs::write(config_path(&dir), "garbage").unwrap();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Unreadable);
        fs::write(config_path(&dir), r#"{"other": true}"#).unwrap();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Missing);
        for untrusted in [r#"{"telemetry": "yes"}"#, r#"{"telemetry": 0}"#] {
            fs::write(config_path(&dir), untrusted).unwrap();
            let recorded = load_config_telemetry(&dir);
            assert_eq!(recorded, ConfigTelemetry::Unreadable, "{untrusted}");
            assert!(
                !opt_out::resolve(&env_of(&[]), recorded).enabled(),
                "an untrusted telemetry value must not read as enabled: {untrusted}"
            );
        }
    }

    #[test]
    fn config_telemetry_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        save_config_telemetry(&dir, false).unwrap();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Value(false));
        save_config_telemetry(&dir, true).unwrap();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Value(true));
    }

    #[test]
    fn config_writes_preserve_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(config_path(&dir), r#"{"feedback": {"endpoint": "x"}}"#).unwrap();
        save_config_telemetry(&dir, true).unwrap();
        let config = load_config(&dir).unwrap();
        assert_eq!(config["telemetry"], serde_json::json!(true));
        assert_eq!(config["feedback"]["endpoint"], serde_json::json!("x"));
    }

    /// A readable file and an absent one read differently; a corrupt one is
    /// an error rather than an empty map.
    #[test]
    fn config_reads_distinguish_missing_from_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        assert!(load_config(&dir).unwrap().is_empty(), "missing is empty");
        fs::create_dir_all(&dir).unwrap();
        fs::write(config_path(&dir), "not json").unwrap();
        assert_eq!(
            load_config(&dir).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// The consent record lives in the same file: an update must not rewrite
    /// a `config.json` it could not parse from an empty map.
    #[test]
    fn updates_refuse_to_overwrite_an_unparsable_config() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(config_path(&dir), "not json").unwrap();
        assert!(save_config_telemetry(&dir, true).is_err());
        assert_eq!(
            fs::read_to_string(config_path(&dir)).unwrap(),
            "not json",
            "the unparsable file is left untouched"
        );
    }

    #[test]
    fn write_atomic_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        save_config_telemetry(&dir, true).unwrap();
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["config.json", "config.lock"],
            "no staging file remains; the lock file is the only extra entry"
        );
    }

    /// Two writers must not lose each other's keys: a second update waits
    /// for the lock the first one holds.
    #[test]
    fn a_contended_config_update_waits_for_the_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let held = lock_config(&dir).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let writer_dir = dir.clone();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            update_config(&writer_dir, |config| {
                config.insert("telemetry".to_string(), true.into());
            })
            .unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the second writer waits while the lock is held"
        );
        FileExt::unlock(&held).unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the waiting update runs once the lock is free");
        writer.join().unwrap();
        assert_eq!(load_config_telemetry(&dir), ConfigTelemetry::Value(true));
    }

    #[cfg(unix)]
    #[test]
    fn written_state_and_config_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let (state, _) = load_or_create_state(&dir);
        save_state(&dir, &state).unwrap();
        save_config_telemetry(&dir, true).unwrap();
        for path in [state_path(&dir), config_path(&dir)] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} must be 0600, got {mode:o}", path.display());
        }
    }

    #[test]
    fn uuid_shape_validation() {
        assert!(is_uuid_shape("123e4567-e89b-42d3-a456-426614174000"));
        assert!(is_uuid_shape("FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF"));
        assert!(!is_uuid_shape(""));
        assert!(!is_uuid_shape("not-a-uuid"));
        assert!(!is_uuid_shape("123e4567-e89b-42d3-a456-42661417400")); // short
        assert!(!is_uuid_shape("123e4567-e89b-42d3-a456-42661417400g")); // non-hex
        assert!(!is_uuid_shape("123e4567e89b42d3a456426614174000")); // no dashes
    }
}

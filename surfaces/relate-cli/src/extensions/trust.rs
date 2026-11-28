//! The per-user extension trust store (ADR-0449): the recorded GitHub
//! pins a `--github` install is compared against, one entry per extension
//! name, at `config_dir()/extensions-trust.json` (the config-dir rule
//! ADR-0429 §6 shares, so `$XDG_CONFIG_HOME` is respected and no path is
//! hardcoded).
//!
//! The store is what makes `remove` + reinstall unable to bypass a pin:
//! the package directory can be deleted, the trust entry cannot — it
//! outlives the install and is consulted *before* any asset fetch. The
//! first install records the resolved pin (the invocation is the
//! approval); a later install refuses when the same tag resolves to a
//! different commit or the asset bytes change, unless the invocation
//! carries an explicit re-approval.
//!
//! Compiled with `extensions` (dependency-free): `extension list --trust`
//! reads it and `extension remove --forget-pin` clears one entry, so both
//! stay available in a build without `extension-install`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::extensions::provenance::Pin;

/// The trust store's filename under the config dir (ADR-0449). The config
/// dir is `$XDG_CONFIG_HOME/relate/` or `~/.config/relate/` — the
/// same resolution the registry's extensions dir uses.
pub(crate) const TRUST_FILE: &str = "extensions-trust.json";

/// The store's path under a resolved config dir — the one join, so the
/// install verbs, `list --trust` and `remove --forget-pin` cannot
/// disagree about the file.
pub(crate) fn trust_path(config_dir: &Path) -> PathBuf {
    config_dir.join(TRUST_FILE)
}

/// A refused trust-store read: corruption is a refusal naming the file,
/// never a silent empty store (an empty store would reopen the pin
/// bypass the file exists to prevent).
#[derive(Debug)]
pub(crate) enum TrustError {
    /// The store exists but could not be read.
    Unreadable {
        /// The store's path, as the refusal names it.
        path: PathBuf,
        /// The read failure.
        source: std::io::Error,
    },
    /// The store could not be written — the write side's one failure,
    /// naming the file the store already knows.
    Unwritable {
        /// The store's path, as the refusal names it.
        path: PathBuf,
        /// The write failure.
        source: std::io::Error,
    },
    /// The store's content does not decode.
    Invalid {
        /// The store's path, as the refusal names it.
        path: PathBuf,
        /// The decode failure, naming the offending entry.
        source: serde_json::Error,
    },
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(
                    f,
                    "cannot read the extension trust store {}: {source}",
                    path.display()
                )
            }
            Self::Invalid { path, source } => {
                write!(
                    f,
                    "invalid extension trust store {}: {source}",
                    path.display()
                )
            }
            Self::Unwritable { path, source } => {
                write!(
                    f,
                    "cannot write the extension trust store {}: {source}",
                    path.display()
                )
            }
        }
    }
}

/// The recorded pins, keyed by extension name (ADR-0449). The map is
/// ordered so the on-disk rendering and `list --trust` are deterministic;
/// lookups are by exact name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustStore {
    path: PathBuf,
    pins: BTreeMap<String, Pin>,
}

impl TrustStore {
    /// Read the store at `path`. A missing file is the empty store — the
    /// state before the first `--github` install; anything else that
    /// fails to read or decode is a refusal naming the file.
    pub(crate) fn load(path: &Path) -> Result<Self, TrustError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    pins: BTreeMap::new(),
                })
            }
            Err(source) => {
                return Err(TrustError::Unreadable {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let pins: BTreeMap<String, Pin> =
            serde_json::from_str(&text).map_err(|source| TrustError::Invalid {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            pins,
        })
    }

    /// The recorded pin for `name`, when one has been approved.
    pub(crate) fn get(&self, name: &str) -> Option<&Pin> {
        self.pins.get(name)
    }

    /// Record an approved pin for `name` (the first install's approval, or
    /// a re-approval), persisting the store, and hand back the entry the
    /// insert replaced (`None` when the name had no pin) so a caller that
    /// must undo the commit can hand it to [`rollback`]. A write failure
    /// leaves the in-memory store exactly as it was — the object and the
    /// file stay in agreement, so a caller that gives up never holds a pin
    /// the file does not.
    pub(crate) fn record(&mut self, name: String, pin: Pin) -> Result<Option<Pin>, TrustError> {
        let prior = self.pins.insert(name.clone(), pin);
        if let Err(e) = self.write() {
            self.restore(&name, prior);
            return Err(self.unwritable(e));
        }
        Ok(prior)
    }

    /// Undo a [`record`]: put `prior` back (or drop the entry when the
    /// record created it) and persist — the one compensation step the
    /// install runs when the package's landing fails (ADR-0449: the
    /// recorded pin and the package land together or not at all).
    pub(crate) fn rollback(&mut self, name: &str, prior: Option<Pin>) -> Result<(), TrustError> {
        self.restore(name, prior);
        self.write().map_err(|e| self.unwritable(e))
    }

    /// The write side's one failure, named with the store's own path —
    /// callers never re-supply the file they handed the store.
    fn unwritable(&self, source: std::io::Error) -> TrustError {
        TrustError::Unwritable {
            path: self.path.clone(),
            source,
        }
    }

    /// The one compensation step both [`record`]'s write-failure path and
    /// [`rollback`] run: put `prior` back, or drop the entry the record
    /// created. In memory only — the caller decides whether to persist.
    fn restore(&mut self, name: &str, prior: Option<Pin>) {
        match prior {
            Some(entry) => {
                self.pins.insert(name.to_string(), entry);
            }
            None => {
                self.pins.remove(name);
            }
        }
    }

    /// Forget `name`'s pin, returning what was there. The one operation
    /// that reopens the first-install path — `extension remove
    /// --forget-pin` is its only invoker (ADR-0449: the acceptance the
    /// flag exists for).
    pub(crate) fn remove(&mut self, name: &str) -> Option<Pin> {
        self.pins.remove(name)
    }

    /// Every recorded pin, name-ordered, for `extension list --trust`.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (&str, &Pin)> {
        self.pins.iter().map(|(name, pin)| (name.as_str(), pin))
    }

    /// Write the store to its own path, creating the config dir when absent.
    /// The write is staged beside the target and renamed into place, so a
    /// crash never leaves a half-written store (the same shape the
    /// package landing uses).
    pub(crate) fn write(&self) -> std::io::Result<()> {
        let mut entries = String::new();
        for (index, (name, pin)) in self.pins.iter().enumerate() {
            if index > 0 {
                entries.push(',');
            }
            entries.push_str(&format!("{}:{}", serde_json::json!(name), pin.write_json()));
        }
        let text = format!("{{{entries}}}\n");
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let staged = self.path.with_extension("json.tmp");
        std::fs::write(&staged, text)?;
        std::fs::rename(&staged, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(tag: &str, commit: &str) -> Pin {
        Pin {
            repo: "owner/repo".to_string(),
            tag: tag.to_string(),
            commit: commit.to_string(),
            asset_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn a_missing_store_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TrustStore::load(&trust_path(tmp.path())).unwrap();
        assert_eq!(store.rows().count(), 0);
    }

    #[test]
    fn the_store_roundtrips_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = trust_path(tmp.path());
        let mut store = TrustStore::load(&path).unwrap();
        store
            .record("greet".to_string(), pin("v1", &"1".repeat(40)))
            .unwrap();
        let mut loaded = TrustStore::load(&path).unwrap();
        assert_eq!(loaded.get("greet"), Some(&pin("v1", &"1".repeat(40))));
        assert!(loaded.remove("greet").is_some());
        assert!(loaded.get("greet").is_none());
        loaded.write().unwrap();
        assert_eq!(TrustStore::load(&path).unwrap().rows().count(), 0);
        // The store's file outlives the entry: the removal writes an
        // empty object, never a deleted file (a later install still finds
        // the store rather than a first-install state).
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    }

    #[test]
    fn the_store_renders_one_flat_object() {
        let tmp = tempfile::tempdir().unwrap();
        let path = trust_path(tmp.path());
        let mut store = TrustStore::load(&path).unwrap();
        store
            .record("greet".to_string(), pin("v1", &"1".repeat(40)))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "{{\"greet\":{{\"repo\":\"owner/repo\",\"tag\":\"v1\",\"commit\":\"{}\",\
                 \"assetSha256\":\"{}\"}}}}\n",
                "1".repeat(40),
                "a".repeat(64)
            )
        );
    }

    #[test]
    fn a_malformed_store_is_a_refusal_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = trust_path(tmp.path());
        std::fs::write(&path, "{ nope").unwrap();
        let err = TrustStore::load(&path).unwrap_err().to_string();
        assert!(err.starts_with("invalid extension trust store "), "{err}");
        assert!(err.contains(TRUST_FILE), "{err}");
    }

    #[test]
    fn an_unknown_pin_field_fails_the_decode() {
        let tmp = tempfile::tempdir().unwrap();
        let path = trust_path(tmp.path());
        std::fs::write(
            &path,
            r#"{"greet":{"repo":"o/r","tag":"v1","commit":"c","assetSha256":"s","extra":1}}"#,
        )
        .unwrap();
        let err = TrustStore::load(&path).unwrap_err().to_string();
        assert!(
            err.contains("unknown field") && err.contains("extra"),
            "{err}"
        );
    }
}

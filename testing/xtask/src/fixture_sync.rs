//! Guard the facade crate's fixture copy (LABS-1414): the crate-local
//! tree at `relate/relate/tests/fixtures/pre_adr0084_default_wal/` must
//! stay byte-identical to the one `relate-storage` owns at
//! `relate/relate-storage/tests/fixtures/pre_adr0084_default_wal/`.
//! The copy ships in the published crate (`cargo package` cannot include
//! files outside the crate root); this check makes the pair's sync
//! mechanical, so regenerating the source without recopying fails CI
//! instead of riding README prose. The comparison is tree-shaped, not a
//! filename list: a file added, dropped, or drifted on either side is a
//! violation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{slash_path, walk_files};

/// The published crate's copy, relative to the workspace root.
const FACADE: &str = "relate/relate/tests/fixtures/pre_adr0084_default_wal";
/// The tree that owns the bytes, relative to the workspace root.
const SOURCE: &str = "relate/relate-storage/tests/fixtures/pre_adr0084_default_wal";

/// Every file under `root`, keyed by its exact path relative to `root` —
/// `PathBuf`, not a lossy string, so distinct non-UTF-8 names stay
/// distinct keys and a comparison can never collapse them. The walk is
/// [`walk_files`], xtask's one directory walker.
fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    walk_files(root, &mut |path, rel| {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        files.insert(rel.to_path_buf(), bytes);
        Ok(())
    })?;
    Ok(files)
}

/// One violation per file that is missing, extra, or drifted on either
/// side of the pair. Paths render through [`slash_path`], the crate's one
/// spelling.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let facade = tree(&root.join(FACADE))?;
    let source = tree(&root.join(SOURCE))?;
    let mut violations = Vec::new();
    for (rel, source_bytes) in &source {
        match facade.get(rel) {
            Some(facade_bytes) if facade_bytes == source_bytes => {}
            Some(_) => violations.push(format!(
                "{FACADE}/{}: drifted from {SOURCE} — recopy it",
                slash_path(rel)
            )),
            None => violations.push(format!(
                "{FACADE}/{}: missing from the copy — recopy it",
                slash_path(rel)
            )),
        }
    }
    for rel in facade.keys() {
        if !source.contains_key(rel) {
            violations.push(format!(
                "{FACADE}/{}: not in {SOURCE} — stale file in the copy",
                slash_path(rel)
            ));
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch root holding the guard's two hardcoded subtrees, from
    /// the crate's one scratch-dir policy (a `TempDir`, removed on every
    /// exit path) — no per-test naming and no manual removal.
    fn temp_root() -> tempfile::TempDir {
        crate::scratch_dir().expect("scratch dir")
    }

    /// The clean pair: both subtrees built and ready for one mutation —
    /// the arrange every violation test shares.
    fn both_trees() -> tempfile::TempDir {
        let root = temp_root();
        build(&root.path().join(SOURCE));
        build(&root.path().join(FACADE));
        root
    }

    /// Write `bytes` at `rel` under `root`, creating parent directories.
    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("rel always has a parent")).expect("create dirs");
        fs::write(&path, bytes).expect("write file");
    }

    /// A minimal three-file fixture tree (the real one's shape).
    fn build(root: &Path) {
        put(root, "manifest.json", b"manifest");
        put(root, "graph.json", b"graph");
        put(root, "wal/00000001.log", b"frames");
    }

    #[test]
    fn identical_trees_are_clean() {
        let root = both_trees();
        assert_eq!(check(root.path()), Ok(Vec::new()));
    }

    #[test]
    fn drifted_byte_is_a_violation() {
        let root = both_trees();
        put(&root.path().join(FACADE), "manifest.json", b"manifest!");
        let violations = check(root.path()).expect("both trees readable");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("manifest.json: drifted from"),
            "{violations:?}"
        );
    }

    #[test]
    fn file_missing_from_the_copy_is_a_violation() {
        let root = both_trees();
        fs::remove_file(root.path().join(FACADE).join("wal/00000001.log"))
            .expect("remove copy file");
        let violations = check(root.path()).expect("both trees readable");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("wal/00000001.log: missing from the copy"),
            "{violations:?}"
        );
    }

    #[test]
    fn stale_extra_file_in_the_copy_is_a_violation() {
        let root = both_trees();
        put(&root.path().join(FACADE), "wal/00000002.log", b"stale");
        let violations = check(root.path()).expect("both trees readable");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("wal/00000002.log: not in"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_source_tree_is_an_error_not_clean() {
        let root = temp_root();
        build(&root.path().join(FACADE));
        assert!(check(root.path()).is_err());
    }
}

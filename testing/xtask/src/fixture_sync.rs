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

/// The published crate's copy, relative to the workspace root.
const FACADE: &str = "relate/relate/tests/fixtures/pre_adr0084_default_wal";
/// The tree that owns the bytes, relative to the workspace root.
const SOURCE: &str = "relate/relate-storage/tests/fixtures/pre_adr0084_default_wal";

/// Every file under `root`, keyed by its exact path relative to `root` —
/// `PathBuf`, not a lossy string, so distinct non-UTF-8 names stay
/// distinct keys and a comparison can never collapse them.
fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).expect("walked path is under root");
                let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                files.insert(rel.to_path_buf(), bytes);
            }
        }
    }
    Ok(files)
}

/// The message rendering of a fixture-relative path: forward slashes on
/// every platform, matching the check's other path spellings.
fn show(rel: &Path) -> String {
    rel.display().to_string().replace('\\', "/")
}

/// One violation per file that is missing, extra, or drifted on either
/// side of the pair.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let facade = tree(&root.join(FACADE))?;
    let source = tree(&root.join(SOURCE))?;
    let mut violations = Vec::new();
    for (rel, source_bytes) in &source {
        match facade.get(rel) {
            Some(facade_bytes) if facade_bytes == source_bytes => {}
            Some(_) => violations.push(format!(
                "{FACADE}/{}: drifted from {SOURCE} — recopy it",
                show(rel)
            )),
            None => violations.push(format!(
                "{FACADE}/{}: missing from the copy — recopy it",
                show(rel)
            )),
        }
    }
    for rel in facade.keys() {
        if !source.contains_key(rel) {
            violations.push(format!(
                "{FACADE}/{}: not in {SOURCE} — stale file in the copy",
                show(rel)
            ));
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch root holding the guard's two hardcoded subtrees, unique
    /// per test (and per process) so parallel runs never share it.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let root = dir.join(format!("xtask-fixture-sync-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
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
        let root = temp_root("clean");
        build(&root.join(SOURCE));
        build(&root.join(FACADE));
        assert_eq!(check(&root), Ok(Vec::new()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn drifted_byte_is_a_violation() {
        let root = temp_root("drift");
        build(&root.join(SOURCE));
        build(&root.join(FACADE));
        put(&root.join(FACADE), "manifest.json", b"manifest!");
        let violations = check(&root).expect("both trees readable");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("manifest.json: drifted from"),
            "{violations:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn file_missing_from_the_copy_is_a_violation() {
        let root = temp_root("missing");
        build(&root.join(SOURCE));
        build(&root.join(FACADE));
        fs::remove_file(root.join(FACADE).join("wal/00000001.log")).expect("remove copy file");
        let violations = check(&root).expect("both trees readable");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("wal/00000001.log: missing from the copy"),
            "{violations:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_extra_file_in_the_copy_is_a_violation() {
        let root = temp_root("stale");
        build(&root.join(SOURCE));
        build(&root.join(FACADE));
        put(&root.join(FACADE), "wal/00000002.log", b"stale");
        let violations = check(&root).expect("both trees readable");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("wal/00000002.log: not in"),
            "{violations:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_source_tree_is_an_error_not_clean() {
        let root = temp_root("nosource");
        build(&root.join(FACADE));
        assert!(check(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}

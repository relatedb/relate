//! The merged integration-test binary's declared-module guard.
//!
//! The crates that keep their shareable test files in one binary
//! (`tests/integration/`, built from `tests/integration/main.rs` — see
//! `docs/testing/test-binaries.md`) compile only the modules their `main.rs`
//! declares, so a file dropped into the directory without a `mod` line would
//! silently never compile or run. Both crates' `main.rs` carry the same
//! three-line `#[test]` that calls the guard here;
//! `scripts/merge-test-binaries.py` emits that test, and
//! `scripts/tests/test_merge_test_binaries.py` keeps each committed
//! `main.rs` equal to what the generator renders.

use std::collections::BTreeSet;

/// Assert every `.rs` file directly under `<manifest_dir>/tests/<binary>/`
/// is declared by a `mod` line in that directory's `main.rs`.
///
/// `manifest_dir` is typed as a path beside the `binary` name, so the two
/// same-shaped arguments cannot be interchanged.
///
/// The declared set is derived as tokens — each `main.rs` line shaped like
/// `mod <name>;`, trimmed of whitespace, `\r`, and the trailing `;` — so the
/// check depends on the declared-module *set*, not on the generator's exact
/// line layout or line endings.
///
/// # Panics
///
/// Panics naming the offending file when a `.rs` file in the directory has
/// no `mod` line, and on any I/O error reading the directory or `main.rs`.
pub fn assert_every_file_is_declared(manifest_dir: &std::path::Path, binary: &str) {
    let dir = manifest_dir.join("tests").join(binary);
    let main = std::fs::read_to_string(dir.join("main.rs"))
        .unwrap_or_else(|e| panic!("cannot read {}/main.rs: {e}", dir.display()));
    let declared: BTreeSet<String> = declared_modules(&main)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|path| {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_else(|| panic!("non-UTF-8 test file name: {}", path.display()));
            (stem != "main").then(|| stem.to_owned())
        })
        .collect();
    let undeclared: Vec<&String> = on_disk.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "{} under {}/ is not declared in main.rs",
        undeclared
            .iter()
            .map(|stem| format!("{stem}.rs"))
            .collect::<Vec<_>>()
            .join(", "),
        dir.display()
    );
}

/// One entry per `mod <name>;` line in `main.rs`, whatever sits above or
/// beside it.
fn declared_modules(main: &str) -> Vec<&str> {
    main.lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("mod "))
        .filter_map(|rest| rest.strip_suffix(';'))
        .map(str::trim)
        .collect()
}

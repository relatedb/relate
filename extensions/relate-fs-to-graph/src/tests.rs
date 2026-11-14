//! The fs-to-graph importer's behavior tests, in the dir-module pattern
//! the CLI tree's `lab` modules used (LABS-421): the walk, the content
//! capture, the ignore patterns. The write-phase and index failure paths
//! the CLI's importer tests pinned are host-side now (ADR-0430 §7: the
//! host drives the rows this crate emits through its own framing and
//! index execution) — the e2e suites drive them end-to-end through the
//! real `run` mount.

use super::*;
use relate_test_support::write_file;
use serde_json::Value;

/// The `key` property of a row, or `None` when the row does not carry it:
/// the one parameterized row-reading conditional this module's readers
/// share. A row that is not an object is a broken fixture and panics —
/// the one failure policy, owned here (LABS-421).
fn prop<'a>(row: &'a Value, key: &str) -> Option<&'a Value> {
    match row {
        Value::Object(props) => props.get(key),
        other => panic!("a write row is a map: {other:?}"),
    }
}

/// The `name` property of a file row, as a string (every file row has one).
fn row_name(row: &Value) -> &str {
    match prop(row, "name") {
        Some(Value::String(s)) => s,
        other => panic!("file row has no string name: {other:?}"),
    }
}

/// The captured `content` property of a file row, if the row carries one.
fn file_content(row: &Value) -> Option<&str> {
    match prop(row, "content") {
        Some(Value::String(s)) => Some(s),
        None => None,
        other => panic!("file row's content is not a string: {other:?}"),
    }
}

/// The `depth` property of a folder row, as the integer the walk wrote.
fn folder_depth(row: &Value) -> i64 {
    match prop(row, "depth") {
        Some(Value::Number(d)) => d.as_i64().expect("depth is an integer"),
        other => panic!("folder row has no integer depth: {other:?}"),
    }
}

/// The captured content of the named file row, if any.
fn captured_content(rows: &WalkRows, name: &str) -> Option<String> {
    rows.files
        .iter()
        .find(|row| row_name(row) == name)
        .and_then(file_content)
        .map(str::to_string)
}

#[test]
fn capture_content_accepts_text() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write_file(&file, b"hello");
    assert_eq!(
        capture_content(&file, 5, 65536).unwrap().as_deref(),
        Some("hello")
    );
}

#[test]
fn capture_content_rejects_nul_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bin");
    write_file(&file, &[b'a', 0, b'b']);
    assert_eq!(capture_content(&file, 3, 65536).unwrap(), None);
}

#[test]
fn capture_content_rejects_invalid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bin");
    write_file(&file, &[0xff, 0xfe, 0xfd]);
    assert_eq!(capture_content(&file, 3, 65536).unwrap(), None);
}

#[test]
fn capture_content_keeps_valid_prefix_when_tail_is_binary() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mixed.txt");
    let mut bytes = vec![b'a'; PROBE_BYTES];
    bytes.push(0xff);
    write_file(&file, &bytes);

    let content = capture_content(&file, bytes.len() as u64, 65536)
        .unwrap()
        .expect("a file with a text probe keeps its valid prefix");
    assert_eq!(content, "a".repeat(PROBE_BYTES));
}

#[test]
fn capture_content_rejects_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write_file(&file, b"hello world");
    assert_eq!(capture_content(&file, 11, 5).unwrap(), None);
}

#[test]
fn capture_content_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    write_file(&file, b"hi");
    assert_eq!(capture_content(&file, 2, 0).unwrap(), None);
}

#[test]
fn capture_content_propagates_io_errors() {
    // A directory passed where a file is expected cannot be read as a
    // file (open fails on Windows, the read fails with EISDIR on Unix):
    // the error must propagate, not collapse into `None`
    // (binary/oversized).
    let dir = tempfile::tempdir().unwrap();
    let err = capture_content(dir.path(), 0, 65536).unwrap_err();
    assert!(err.contains("reading"), "{err}");
    assert!(err.contains(&dir.path().display().to_string()), "{err}");
}

#[test]
fn walk_skips_dotfiles_and_counts_edges() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("alpha.txt"), b"alpha");
    write_file(&dir.path().join(".hidden.txt"), b"hidden");
    write_file(&dir.path().join("sub/beta.txt"), b"beta");

    let rows = walk(dir.path(), &Opts::default()).unwrap();
    assert_eq!(rows.folders.len(), 2, "root + sub");
    assert_eq!(rows.files.len(), 2, "alpha + beta");
    assert_eq!(rows.edges.len(), 3, "root->sub, root->alpha, sub->beta");
    assert_eq!(
        captured_content(&rows, "alpha.txt").as_deref(),
        Some("alpha")
    );
    assert_eq!(captured_content(&rows, "beta.txt").as_deref(), Some("beta"));
}

#[test]
fn walk_folds_content_into_file_rows_only_when_captured() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("text.txt"), b"text");
    write_file(&dir.path().join("bin.dat"), &[0, 1, 2]);

    let rows = walk(dir.path(), &Opts::default()).unwrap();
    assert_eq!(rows.files.len(), 2);
    assert_eq!(captured_content(&rows, "text.txt").as_deref(), Some("text"));
    assert_eq!(captured_content(&rows, "bin.dat"), None);
}

#[test]
fn walk_includes_dotfiles_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("alpha.txt"), b"alpha");
    write_file(&dir.path().join(".hidden.txt"), b"hidden");

    let opts = Opts {
        include_hidden: true,
        ..Opts::default()
    };
    let rows = walk(dir.path(), &opts).unwrap();
    assert_eq!(rows.folders.len(), 1);
    assert_eq!(rows.files.len(), 2);
    assert_eq!(rows.edges.len(), 2);
}

#[test]
fn ignore_pattern_parse_classifies_a_trailing_slash_as_directory_only() {
    assert_eq!(
        IgnorePattern::parse("node_modules/").unwrap(),
        IgnorePattern {
            name: "node_modules".to_string(),
            dir_only: true,
        }
    );
    assert_eq!(
        IgnorePattern::parse("debug.log").unwrap(),
        IgnorePattern {
            name: "debug.log".to_string(),
            dir_only: false,
        }
    );
}

#[test]
fn ignore_pattern_parse_rejects_empty_values() {
    for value in ["", "/"] {
        let err = IgnorePattern::parse(value).unwrap_err();
        assert!(err.starts_with("invalid --ignore-patterns value:"), "{err}");
    }
}

#[test]
fn ignore_pattern_matches_by_exact_name_only() {
    let bare = IgnorePattern::parse("debug.log").unwrap();
    assert!(bare.matches("debug.log", false), "bare matches files");
    assert!(bare.matches("debug.log", true), "bare matches directories");
    assert!(!bare.matches("xdebug.log", false), "no substring match");
    assert!(!bare.matches("debug.logx", false), "no prefix match");

    let dir_only = IgnorePattern::parse("target/").unwrap();
    assert!(dir_only.matches("target", true));
    assert!(
        !dir_only.matches("target", false),
        "a trailing / never matches a file"
    );
}

#[test]
fn walk_prunes_a_matched_directory_entirely() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("alpha.txt"), b"alpha");
    write_file(&dir.path().join("node_modules/left-pad/index.js"), b"pad");
    write_file(&dir.path().join("sub/target/build.o"), b"obj");

    let opts = Opts {
        ignore_patterns: vec![
            IgnorePattern::parse("node_modules/").unwrap(),
            IgnorePattern::parse("target").unwrap(),
        ],
        ..Opts::default()
    };
    let rows = walk(dir.path(), &opts).unwrap();
    assert_eq!(
        rows.folders.len(),
        2,
        "root + sub; pruned dirs contribute no rows"
    );
    assert_eq!(
        rows.files.len(),
        1,
        "alpha only; pruned subtrees contribute nothing"
    );
    assert_eq!(
        rows.edges.len(),
        2,
        "root->sub, root->alpha; no edges into pruned dirs"
    );
    assert!(captured_content(&rows, "index.js").is_none());
    assert!(captured_content(&rows, "build.o").is_none());
}

#[test]
fn walk_skips_a_bare_matched_file() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("alpha.txt"), b"alpha");
    write_file(&dir.path().join("debug.log"), b"log");

    let opts = Opts {
        ignore_patterns: vec![IgnorePattern::parse("debug.log").unwrap()],
        ..Opts::default()
    };
    let rows = walk(dir.path(), &opts).unwrap();
    assert_eq!(rows.files.len(), 1);
    assert_eq!(rows.edges.len(), 1);
    assert_eq!(
        captured_content(&rows, "alpha.txt").as_deref(),
        Some("alpha")
    );
    assert_eq!(captured_content(&rows, "debug.log"), None);
}

#[test]
fn walk_ignore_patterns_compose_with_the_dotfile_rule() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("alpha.txt"), b"alpha");
    write_file(&dir.path().join(".hidden.txt"), b"hidden");
    write_file(&dir.path().join(".git/config"), b"git");

    // Both filters apply: the dotfile rule drops hidden entries even when
    // no pattern names them, and a bare pattern drops a hidden directory
    // even with --include-hidden set.
    let opts = Opts {
        include_hidden: true,
        ignore_patterns: vec![IgnorePattern::parse(".git").unwrap()],
        ..Opts::default()
    };
    let rows = walk(dir.path(), &opts).unwrap();
    assert_eq!(rows.folders.len(), 1, "the .git directory is pruned");
    assert_eq!(rows.files.len(), 2, "alpha + .hidden.txt");
    assert_eq!(rows.edges.len(), 2);
}

#[cfg(unix)]
#[test]
fn walk_never_follows_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    write_file(&dir.path().join("real.txt"), b"real");
    let outside = tempfile::tempdir().unwrap();
    write_file(&outside.path().join("outside.txt"), b"outside");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("outside.txt"),
        dir.path().join("file-link.txt"),
    )
    .unwrap();

    let rows = walk(dir.path(), &Opts::default()).unwrap();
    assert_eq!(rows.folders.len(), 1, "symlinked dir must not be walked");
    assert_eq!(rows.files.len(), 1, "symlinked file must be skipped");
    assert_eq!(rows.edges.len(), 1);
}

#[cfg(unix)]
#[test]
fn walk_handles_a_deeply_nested_tree_without_recursion() {
    // 400 levels of nesting. The explicit walk stack makes depth a heap
    // concern, so the walk cannot overflow the call stack here; the test
    // cannot go deeper portably because the filesystem's own path-length
    // limit (PATH_MAX 1024 on macOS) rejects longer paths first — a tree
    // that deep now fails with an ordinary I/O error instead of a stack
    // overflow.
    const DEPTH: usize = 400;
    let dir = tempfile::tempdir().unwrap();
    let mut deep = dir.path().to_path_buf();
    for level in 0..DEPTH {
        deep.push("d");
        std::fs::create_dir(&deep).unwrap_or_else(|e| panic!("mkdir level {level}: {e}"));
    }
    write_file(&deep.join("leaf.txt"), b"leaf");

    let rows = walk(dir.path(), &Opts::default()).unwrap();
    assert_eq!(rows.folders.len(), DEPTH + 1, "root + {DEPTH} nested dirs");
    assert_eq!(rows.files.len(), 1);
    assert_eq!(rows.edges.len(), DEPTH + 1, "one edge per dir + the leaf");
    assert_eq!(captured_content(&rows, "leaf.txt").as_deref(), Some("leaf"));

    // The deepest folder row records its distance from the walked root.
    let max_depth = rows
        .folders
        .iter()
        .map(folder_depth)
        .max()
        .expect("the walk visited at least the root");
    assert_eq!(max_depth, DEPTH as i64);
}

/// The committed manifest beside this crate's source is the package the
/// release channel ships (the one manifest the e2e installs): it must
/// decode and declare the contract this child speaks — the mount name, the
/// MCP revision, the flag table the bin's envelope reads, and the commit
/// policy the session is framed by.
#[test]
fn the_committed_manifest_declares_the_contract_this_child_speaks() {
    let manifest: Value = serde_json::from_str(include_str!("../package/extension.json")).unwrap();
    assert_eq!(manifest["kind"], "command");
    assert_eq!(manifest["name"], "fs-to-graph");
    // The protocol names the MCP revision the mount serves, and the
    // retired dual-wire selector is gone: a package declares the one wire
    // ADR-0447 leaves.
    assert_eq!(manifest["protocol"], relate_wire::mcp::MCP_PROTOCOL_VERSION);
    assert!(
        manifest.get("wire").is_none(),
        "the manifest carries no wire field: the mount speaks MCP stdio only"
    );
    assert_eq!(manifest["commit"], relate_wire::PER_BATCH);
    assert_eq!(manifest["positional"]["name"], "DIR");
    assert!(!manifest["requires-fresh-store"].as_bool().unwrap());
    let flags: Vec<(String, &str)> = manifest["flags"]
        .as_array()
        .expect("the flag table is an array")
        .iter()
        .map(|flag| {
            (
                flag["name"].as_str().unwrap().to_string(),
                flag["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        [
            ("max-content-bytes".to_string(), "integer"),
            ("include-hidden".to_string(), "boolean"),
            ("ignore-patterns".to_string(), "repeatable"),
        ]
    );
    // The defaults the child validates against: the same numbers this
    // lib's `Opts::default` resolves.
    assert_eq!(
        manifest["flags"][0]["default"],
        serde_json::json!(DEFAULT_MAX_CONTENT_BYTES)
    );
}

/// The bin's envelope reads the flag names the manifest declares — the
/// two spell one contract, pinned through the kit's one shared
/// assertion (the same helper the git child's test calls) so a manifest
/// edit that breaks the child's envelope fails in this crate, not
/// mid-import.
#[test]
fn the_envelope_keys_match_the_manifest_flag_names() {
    let example = serde_json::to_value(Envelope {
        dir: Some(".".to_string()),
        max_content_bytes: DEFAULT_MAX_CONTENT_BYTES as i64,
        include_hidden: false,
        ignore_patterns: vec![],
    })
    .unwrap();
    let manifest: Value = serde_json::from_str(include_str!("../package/extension.json")).unwrap();
    relate_test_support::envelope::assert_envelope_keys(&example, &manifest);
}

/// The envelope's value validation, with the importer's own wording: a
/// negative content cap and an empty ignore pattern are child-side
/// refusals (the host's kind-checked parse cannot see value ranges,
/// ADR-0429 §5).
#[test]
fn the_envelope_refuses_values_the_flag_kinds_cannot() {
    let base = Envelope {
        dir: None,
        max_content_bytes: DEFAULT_MAX_CONTENT_BYTES as i64,
        include_hidden: false,
        ignore_patterns: vec![],
    };
    assert_eq!(base.opts().unwrap(), Opts::default());
    let negative = Envelope {
        max_content_bytes: -1,
        ..base.clone()
    };
    assert_eq!(
        negative.opts().unwrap_err(),
        "invalid --max-content-bytes: '-1' is not a number"
    );
    let empty_pattern = Envelope {
        ignore_patterns: vec!["/".to_string()],
        ..base
    };
    assert_eq!(
        empty_pattern.opts().unwrap_err(),
        "invalid --ignore-patterns value: '/' is empty"
    );
}

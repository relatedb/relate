//! `fs-to-graph` — import a directory tree into a graph (ADR-0279,
//! ADR-0157), as a first-party extension crate (ADR-0431 §11): the
//! importer logic moved out of the CLI's `lab` tree, a pure source walker
//! whose rows the child writes through the MCP mount's `write_cypher` calls
//! ([`relate_wire::mcp`], ADR-0442) into the store the `relate run`
//! host owns. The crate carries no `Database` and never touches a store —
//! the child's only power is the calls it makes (ADR-0447 §4).
//!
//! The walk is an iterative `std::fs::read_dir` traversal over an explicit
//! stack of open directories — no recursion, so a deeply nested tree cannot
//! overflow the call stack — that never follows symlinks. The three write
//! phases ([`WRITE_FOLDERS`], [`WRITE_FILES`], [`WRITE_EDGES`]) are the
//! statements the child sends as `write_cypher` calls, each framed by the
//! manifest's `per-batch` commit policy (ADR-0602, ADR-0392's framing):
//! every writing call commits as its own transaction, so a mid-import
//! failure leaves the batches that already committed and a re-run of the
//! import completes the graph. Nodes are `MERGE`d on `path`, so
//! re-importing the same tree leaves the node and edge counts unchanged.
//! The `file_content` full-text index ([`INDEX_FILE_CONTENT`]) is the
//! session's one `update_schema` call, executed at the import's
//! index-creation point after the write phases.

use std::path::Path;

use serde_json::Value;

use relate_wire::read_cap::{read_capped, CappedRead};
use relate_wire::{display, file_name};

/// Byte prefix inspected to decide whether a file is text (ADR-0157).
const PROBE_BYTES: usize = 8 * 1024;

/// The folder write phase's Cypher.
pub const WRITE_FOLDERS: &str = "UNWIND $rows AS row \
     MERGE (f:Folder {path: row.path}) \
     SET f.name = row.name, f.depth = row.depth";

/// The file write phase's Cypher.
pub const WRITE_FILES: &str = "UNWIND $rows AS row \
     MERGE (f:File {path: row.path}) \
     SET f.name = row.name, f.extension = row.extension, \
         f.size_bytes = row.size_bytes, f.content = row.content";

/// The `CONTAINS` edge write phase's Cypher.
pub const WRITE_EDGES: &str = "UNWIND $rows AS row \
     MATCH (p:Folder) WHERE p.path = row.parent \
     MATCH (c) WHERE c.path = row.child \
     MERGE (p)-[:CONTAINS]->(c)";

/// The import's index-creation call (ADR-0157): the `file_content`
/// full-text index over `:File(name, content)`, created after the write
/// phases — every batch already committed under the per-batch policy
/// (ADR-0602). `IF NOT EXISTS` keeps a re-import idempotent; the host
/// executes it at the point in the session this child places it — after
/// the last write call.
pub const INDEX_FILE_CONTENT: &str = "CREATE FULLTEXT INDEX file_content IF NOT EXISTS \
     FOR (f:File) ON EACH [f.name, f.content]";

/// One `--ignore-patterns` value, classified by its trailing slash
/// (ADR-0157).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnorePattern {
    /// The path component to match, exactly (no globs).
    name: String,
    /// A trailing `/` restricts the match to directories.
    dir_only: bool,
}

impl IgnorePattern {
    /// Classify a `--ignore-patterns` value: a trailing `/` marks the pattern
    /// directory-only. An empty value, or one that is empty after trimming a
    /// trailing slash (`/`), is a refusal.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (name, dir_only) = match value.strip_suffix('/') {
            Some(stripped) => (stripped, true),
            None => (value, false),
        };
        if name.is_empty() {
            return Err(format!(
                "invalid --ignore-patterns value: '{value}' is empty"
            ));
        }
        Ok(Self {
            name: name.to_string(),
            dir_only,
        })
    }

    /// Whether this pattern ignores the file or directory named `name`.
    fn matches(&self, name: &str, is_dir: bool) -> bool {
        self.name == name && (is_dir || !self.dir_only)
    }
}

/// Default `--max-content-bytes` cap, in bytes (ADR-0157): the importer's
/// own flag default, beside its [`Opts`], so the child's validation and
/// the committed manifest state the number the import runs with.
pub const DEFAULT_MAX_CONTENT_BYTES: u64 = 65536;

/// The queries the import's success summary recommends (ADR-0157 §UX): the
/// paste-ready full-text search over the imported content, the queries the
/// child's summary prints as starters and the e2e suites pin.
pub const STARTER_QUERIES: [&str; 1] =
    ["MATCH (f:File) SEARCH f IN (FULLTEXT INDEX file_content FOR 'term' LIMIT 10) RETURN f.path, f.name;"];

/// Options for one import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    /// Capture file content only when the whole file is at most this many
    /// bytes (and the first 8 KiB are text). `0` disables content capture.
    pub max_content_bytes: u64,
    /// Include entries whose name starts with `.`.
    pub include_hidden: bool,
    /// Path components to prune, in `--ignore-patterns` order (ADR-0157).
    pub ignore_patterns: Vec<IgnorePattern>,
}

impl Default for Opts {
    /// The manifest's flag defaults (ADR-0157), the same field set the
    /// committed `package/extension.json` declares.
    fn default() -> Self {
        Self {
            max_content_bytes: DEFAULT_MAX_CONTENT_BYTES,
            include_hidden: false,
            ignore_patterns: vec![],
        }
    }
}

/// The typed request envelope (ADR-0430 §6): the child's whole input, one
/// JSON object the host resolved from the manifest's flag table — every
/// flag's default or given value, keyed by the names the committed
/// manifest declares, plus the positional. The wire spelling of
/// [`Opts`]: [`Self::opts`] validates and lowers it, refusing a value the
/// manifest's kinds could not (a negative content cap) or a malformed
/// pattern loudly — the importer-side validation the host's kind-checked
/// parse cannot do (ADR-0429 §5).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Envelope {
    /// The `DIR` positional; absent means the current directory.
    #[serde(rename = "DIR", default)]
    pub dir: Option<String>,
    /// `--max-content-bytes`, the manifest's default when absent.
    pub max_content_bytes: i64,
    /// `--include-hidden`, the manifest's default when absent.
    pub include_hidden: bool,
    /// `--ignore-patterns`, one value per occurrence, default-empty.
    pub ignore_patterns: Vec<String>,
}

impl Envelope {
    /// Validate the envelope's values and lower it into the walk's
    /// [`Opts`]: a negative content cap (a value the integer flag kind
    /// admits but the cap never did) and an empty ignore pattern (after
    /// its trailing-slash trim) are refusals with the importer's own
    /// wording — the child renders them as its `error` record (exit 1
    /// through the host, ADR-0430 §5).
    pub fn opts(&self) -> Result<Opts, String> {
        if self.max_content_bytes < 0 {
            return Err(format!(
                "invalid --max-content-bytes: '{}' is not a number",
                self.max_content_bytes
            ));
        }
        let mut ignore_patterns = Vec::with_capacity(self.ignore_patterns.len());
        for pattern in &self.ignore_patterns {
            ignore_patterns.push(IgnorePattern::parse(pattern)?);
        }
        Ok(Opts {
            max_content_bytes: self.max_content_bytes as u64,
            include_hidden: self.include_hidden,
            ignore_patterns,
        })
    }
}

/// What an import walked, as this run's row counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Directories imported, including the walked root.
    pub folders: usize,
    /// Files imported.
    pub files: usize,
    /// `:CONTAINS` edges imported.
    pub edges: usize,
}

/// Rows collected by one walk, one vector per write phase they feed — the
/// child's batch record each phase emits carries its vector's rows
/// (ADR-0430 §1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WalkRows {
    /// The `:Folder` rows, depth-first in `read_dir` order.
    pub folders: Vec<Value>,
    /// The `:File` rows, the same order.
    pub files: Vec<Value>,
    /// The `:CONTAINS` edge rows, the same order.
    pub edges: Vec<Value>,
}

impl WalkRows {
    /// The [`Stats`] for these rows.
    pub fn stats(&self) -> Stats {
        Stats {
            folders: self.folders.len(),
            files: self.files.len(),
            edges: self.edges.len(),
        }
    }
}

/// Walk the tree rooted at `root`, collecting its rows depth-first in
/// `read_dir` order — the same order a recursive pre-order walk would
/// produce — but over an explicit stack of open directories, so an
/// arbitrarily deep tree cannot overflow the call stack (ADR-0157).
/// Symlinks are never followed; a matched ignore pattern prunes a
/// directory entirely; dotfiles are skipped unless
/// [`Opts::include_hidden`] is set. An unreadable directory or stat
/// failure is an `Err` naming the path, never a skipped subtree.
pub fn walk(root: &Path, opts: &Opts) -> Result<WalkRows, String> {
    let mut rows = WalkRows::default();
    walk_dir(root, 0, opts, &mut rows)?;
    Ok(rows)
}

/// The recursive half of [`walk`]: one directory's rows, then its
/// subdirectories'.
fn walk_dir(dir: &Path, depth: usize, opts: &Opts, rows: &mut WalkRows) -> Result<(), String> {
    /// One open directory on the walk stack: its path (for error messages and
    /// `CONTAINS` edge rows), its entries, and its own depth.
    struct Frame {
        dir: std::path::PathBuf,
        entries: std::fs::ReadDir,
        depth: usize,
    }

    rows.folders.push(folder_row(dir, depth));
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    let mut stack = vec![Frame {
        dir: dir.to_path_buf(),
        entries,
        depth,
    }];

    while !stack.is_empty() {
        // Advance the top frame by one entry. The frame borrow ends with this
        // block, so an exhausted frame can be popped and a matched
        // subdirectory can push a new frame below.
        let next = {
            let frame = stack.last_mut().expect("stack is non-empty");
            frame
                .entries
                .next()
                .map(|entry| (frame.dir.clone(), frame.depth, entry))
        };
        let Some((parent, depth, entry)) = next else {
            stack.pop();
            continue;
        };
        let entry = entry.map_err(|e| format!("reading {}: {e}", parent.display()))?;
        let name = entry.file_name();
        if !opts.include_hidden && name.to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", entry.path().display()))?;
        // Never follow symlinks (ADR-0157).
        if file_type.is_symlink() {
            continue;
        }
        let name = name.to_string_lossy();
        // A matched directory is pruned entirely: no row, no edge, no
        // descendants (ADR-0157).
        if opts
            .ignore_patterns
            .iter()
            .any(|p| p.matches(&name, file_type.is_dir()))
        {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            rows.edges.push(edge_row(&parent, &path));
            rows.folders.push(folder_row(&path, depth + 1));
            let entries =
                std::fs::read_dir(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            stack.push(Frame {
                dir: path,
                entries,
                depth: depth + 1,
            });
        } else if file_type.is_file() {
            let meta = entry
                .metadata()
                .map_err(|e| format!("stat {}: {e}", path.display()))?;
            rows.edges.push(edge_row(&parent, &path));
            let content = capture_content(&path, meta.len(), opts.max_content_bytes)?;
            rows.files.push(file_row(&path, meta.len(), content));
        }
        // Sockets, FIFOs and device files are not importable content.
    }
    Ok(())
}

/// Capture a file's content, or `Ok(None)` for binary or oversized files.
///
/// A file qualifies only when the whole file fits within `max_content_bytes`
/// and its first [`PROBE_BYTES`] contain neither a NUL byte nor invalid UTF-8.
/// If the tail past the probe is binary, only the valid UTF-8 prefix is kept.
/// I/O errors opening or reading the file propagate as `Err` and fail the
/// import, exactly like the walk's `read_dir`/`stat` errors (ADR-0157);
/// `None` means binary or oversized, never "unreadable". The read itself is
/// the wire kit's shared [`read_capped`] guard.
fn capture_content(
    path: &Path,
    size: u64,
    max_content_bytes: u64,
) -> Result<Option<String>, String> {
    if max_content_bytes == 0 || size > max_content_bytes {
        return Ok(None);
    }
    let file = std::fs::File::open(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    // A file that grew past the cap between stat and read is oversized too.
    let buf = match read_capped(file, max_content_bytes)
        .map_err(|e| format!("reading {}: {e}", path.display()))?
    {
        CappedRead::Complete(buf) => buf,
        CappedRead::Exceeded => return Ok(None),
    };
    let probe = &buf[..buf.len().min(PROBE_BYTES)];
    if probe.contains(&0) || std::str::from_utf8(probe).is_err() {
        return Ok(None);
    }
    let content = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        // `valid_up_to` guarantees the prefix is valid UTF-8, so the lossy
        // conversion cannot actually replace anything.
        Err(e) => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
    };
    Ok(Some(content))
}

/// One `:Folder` row: the stored path, its final component, and the walked
/// depth.
fn folder_row(path: &Path, depth: usize) -> Value {
    serde_json::json!({
        "path": display(path),
        "name": file_name(path),
        "depth": depth,
    })
}

/// One `:File` row: the stored path, name, extension, size — and the
/// captured content, present only for qualifying files (the property is
/// absent, never null, for the rest).
fn file_row(path: &Path, size: u64, content: Option<String>) -> Value {
    let mut row = serde_json::json!({
        "path": display(path),
        "name": file_name(path),
        "extension": extension(path),
        "size_bytes": size,
    });
    if let Some(content) = content {
        row.as_object_mut()
            .expect("the row is an object")
            .insert("content".to_string(), Value::String(content));
    }
    row
}

/// One `:CONTAINS` edge row: the parent and child by stored path.
fn edge_row(parent: &Path, child: &Path) -> Value {
    serde_json::json!({
        "parent": display(parent),
        "child": display(child),
    })
}

/// A file's extension, empty when it has none.
fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// The importer's tests live in `tests.rs` (the dir-module pattern the CLI
// tree's importers used, LABS-421) so this file stays under the repo's
// 1k-line budget.
#[cfg(test)]
mod tests;

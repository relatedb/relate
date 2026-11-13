//! `git-to-graph` — import a checkout's history into a graph (ADR-0279,
//! ADR-0376), as a first-party extension crate (ADR-0431 §11): the
//! importer logic moved out of the CLI's `lab` tree, a pure source walker
//! whose rows the child writes through the MCP mount's `write_cypher` calls
//! ([`relate_wire::mcp`], ADR-0442) into the store the `relate run`
//! host owns. The crate carries no `Database` and never touches a store —
//! the child's only power is the calls it makes (ADR-0447 §4).
//!
//! The import joins `fs-to-graph`'s `:File.path` keys: every git path is
//! re-rooted at DIR one `/`-separated component at a time, so an import
//! of the same DIR in either order leaves one `:File` node per path. All
//! writes are parameterized `UNWIND $rows AS row` batches, each batch its
//! own explicit transaction (ADR-0392, the manifest's `per-batch` commit
//! policy): one whole-import transaction would serialise its entire redo
//! as a single WAL record past the store's 64 MiB payload cap at the
//! history scale the pair cap admits. The range indexes are created
//! before the first write and the full-text index after the last, each a
//! `update_schema` call at the point in the session this child places it,
//! and each phase's rows are generated lazily from the bounded pre-write
//! aggregates the history is folded into, so only one chunk is ever
//! materialised at a time (ADR-0392).
//!
//! The edge phases always `MERGE` (ADR-0392's completion semantics: a
//! re-run over a partially imported store completes it). The CLI
//! importer's fresh-store `CREATE` fast path (ADR-0394) is gone with the
//! store: a child cannot read the target's node count, and the manifest
//! knob that replaces the read — `requires-fresh-store` — is for actions
//! whose rows only make sense on a fresh store, not this completer.

pub mod git_log;
pub mod plan;

use std::fmt;

/// Default `CO_CHANGED` changeset cap (ADR-0376).
pub const DEFAULT_MAX_CHANGESET: u64 = 50;

/// The `git log` stdout cap in bytes (ADR-0377): reading past it fails the
/// import, and nothing past the cap is parsed.
pub const MAX_LOG_BYTES: u64 = 134_217_728;

/// The distinct `CO_CHANGED` pair cap per import (ADR-0377; raised from
/// 500,000 to 5,000,000 by ADR-0392, whose per-batch transactions and
/// lazily generated rows hold the transient memory the raise admits): the
/// pair past it fails the import before the range indexes and the first
/// batch.
pub const MAX_CO_CHANGED_PAIRS: usize = 5_000_000;

/// The largest accepted `--max-changeset` (ADR-0377): a larger value is a
/// refusal, never clamped.
pub const MAX_CHANGESET: u64 = 1_000;

/// Bytes in one mebibyte, the unit the ADR-0377 log-cap message reports
/// beside the exact byte count.
const MIB: u64 = 1024 * 1024;

/// A refused `git-to-graph` import (ADR-0157, ADR-0376, ADR-0377) — the
/// child's own failures, printed on its stderr (ADR-0447) so the host
/// renders the importer's wording under its `Error:` line.
///
/// The resource checks are typed variants, not formatted strings assembled at
/// a call site, so tests assert that wording in one place (ADR-0377 §4).
/// [`Self::Failed`] carries every other import failure unchanged: git's
/// stderr, a malformed log record, an I/O error. The CLI importer's
/// `Engine` and `IndexFailed` variants are host-side now (ADR-0430 §7: a
/// store error during a batch or an index build fails in the host, not
/// the child).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// `git log` wrote the byte that would exceed its stdout cap.
    LogTooLarge {
        /// The cap the read ran under, in bytes.
        limit: u64,
    },
    /// Counting would insert more distinct `CO_CHANGED` pairs than the cap.
    TooManyPairs {
        /// The cap the count ran under, in distinct pairs.
        limit: usize,
    },
    /// A git path is not an ordinary relative path (ADR-0377 §5).
    UnsafePath {
        /// The commit record that carried the path.
        sha: String,
        /// The path exactly as git rendered it.
        path: String,
        /// Which part of the path rule refused it.
        refusal: PathRefusal,
    },
    /// The `git` program the history reader shells out to was not found on
    /// PATH (ADR-0376): an authored refusal typed like its siblings, never
    /// a formatted string assembled at the spawn site.
    GitNotInstalled,
    /// Any other import failure, already formatted for the failure report.
    Failed(String),
}

/// Why [`ImportError::UnsafePath`] refused a git path (ADR-0377 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRefusal {
    /// The whole path is empty.
    Empty,
    /// One `/`-separated component is not an ordinary relative component.
    Component(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LogTooLarge { limit } => write!(
                f,
                "git log output exceeds the {limit}-byte import limit ({} MiB); \
                 pass --max-commits, --since or --until to read a smaller window",
                limit / MIB
            ),
            Self::TooManyPairs { limit } => write!(
                f,
                "import would write more than {limit} CO_CHANGED pairs; \
                 lower --max-changeset or pass --max-commits to read fewer commits"
            ),
            Self::UnsafePath { sha, path, refusal } => match refusal {
                PathRefusal::Empty => write!(
                    f,
                    "malformed git path {path:?} in commit {sha}: path is empty"
                ),
                PathRefusal::Component(component) => write!(
                    f,
                    "malformed git path {path:?} in commit {sha}: \
                     component {component:?} is not allowed"
                ),
            },
            Self::GitNotInstalled => {
                f.write_str("git was not found on PATH; install git and retry")
            }
            Self::Failed(message) => f.write_str(message),
        }
    }
}

/// The ADR-0377 resource caps an import runs under: the compile-time
/// constants at runtime, a lowered pair in unit tests.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// The `git log` stdout byte cap.
    pub log_bytes: u64,
    /// The distinct `CO_CHANGED` pair cap.
    pub pairs: usize,
}

impl Caps {
    /// The shipped caps (ADR-0377; the pair cap as ADR-0392 raises it).
    pub const ADR: Self = Self {
        log_bytes: MAX_LOG_BYTES,
        pairs: MAX_CO_CHANGED_PAIRS,
    };
}

/// Options for one import (ADR-0376; the window's upper bound is
/// ADR-0597).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    /// `--max-commits`: read at most this many commits; `None` is all history.
    pub max_commits: Option<u64>,
    /// `--max-changeset`: commits touching more files contribute no
    /// `CO_CHANGED` pair (default 50).
    pub max_changeset: u64,
    /// `--since`: git's committer-date lower bound; `None` is no bound.
    pub since: Option<String>,
    /// `--until`: git's committer-date upper bound; `None` is no bound.
    /// With `since` it closes the window, so `--since A --until B` reads a
    /// reproducible range that need not end at HEAD (ADR-0597).
    pub until: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            max_commits: None,
            max_changeset: DEFAULT_MAX_CHANGESET,
            since: None,
            until: None,
        }
    }
}

/// The manifest's empty-string-means-no-bound rule (ADR-0430 §6): the
/// envelope always carries a resolved value, so an empty string is no
/// bound and any other value is the bound itself — the one lowering both
/// date bounds ride.
fn bound(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// What an import planned, as this run's row counts (ADR-0376's `C`, `P`,
/// `F`, `T`, `X`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Commits read from git.
    pub commits: usize,
    /// `:Person` nodes merged.
    pub people: usize,
    /// Distinct `:File` paths merged.
    pub files: usize,
    /// `:TOUCHED` edges written.
    pub touched: usize,
    /// `:CO_CHANGED` edges written.
    pub co_changed: usize,
}

/// The typed request envelope (ADR-0430 §6): the child's whole input, one
/// JSON object the host resolved from the manifest's flag table — every
/// flag's default or given value, keyed by the names the committed
/// manifest declares, plus the positional. The envelope spelling of
/// [`Opts`]: [`Self::opts`] validates and lowers it, refusing a value the
/// manifest's kinds could not — a changeset outside `1..=1000`, a negative
/// commit window — loudly, with the importer's own wording (the host's
/// kind-checked parse cannot see value ranges, ADR-0429 §5).
///
/// The optional-with-no-bound flags ride the wire as their defaults'
/// values: `max-commits` `0` means all history and an empty `since` or
/// `until` means no bound — the envelope always carries a resolved value,
/// so the absent and the explicitly-empty spelling merge (the child cannot
/// tell them apart; ADR-0430 §6's envelope contract).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Envelope {
    /// The `DIR` positional; absent means the current directory.
    #[serde(rename = "DIR", default)]
    pub dir: Option<String>,
    /// `--max-commits`, the manifest's default (`0`) when absent: `0` is
    /// all history, a negative value a refusal.
    pub max_commits: i64,
    /// `--max-changeset`, the manifest's default when absent: `1..=1000`.
    pub max_changeset: i64,
    /// `--since`, the manifest's default (empty) when absent: empty is no
    /// bound.
    pub since: String,
    /// `--until`, the manifest's default (empty) when absent: empty is no
    /// bound (ADR-0597).
    pub until: String,
}

impl Envelope {
    /// Validate the envelope's values and lower it into the import's
    /// [`Opts`]: a negative commit window and a changeset outside
    /// `1..=1000` (values the integer flag kind admits but the caps never
    /// did, ADR-0377 §3) are refusals with the importer's own wording —
    /// the child prints them on stderr and exits 1 (ADR-0447).
    pub fn opts(&self) -> Result<Opts, String> {
        if self.max_commits < 0 {
            return Err(format!(
                "invalid --max-commits: '{}' is not a positive integer",
                self.max_commits
            ));
        }
        if self.max_changeset <= 0 {
            return Err(format!(
                "invalid --max-changeset: '{}' is not a positive integer",
                self.max_changeset
            ));
        }
        if self.max_changeset as u64 > MAX_CHANGESET {
            return Err(format!(
                "invalid --max-changeset: '{}' is above the maximum {MAX_CHANGESET}",
                self.max_changeset
            ));
        }
        Ok(Opts {
            max_commits: (self.max_commits > 0).then_some(self.max_commits as u64),
            max_changeset: self.max_changeset as u64,
            since: bound(&self.since),
            until: bound(&self.until),
        })
    }
}

/// The queries the import's success summary recommends (ADR-0376 §UX), in
/// output order: each runs against a fresh import and finds a row, and
/// the child's summary prints them as starters.
pub const STARTER_QUERIES: [&str; 3] = [
    "MATCH (a:File)-[r:CO_CHANGED]->(b:File) RETURN a.path AS file, b.path AS co_changed, r.count AS times ORDER BY times DESC LIMIT 10;",
    "MATCH (p:Person)-[:AUTHORED]->(c:Commit) RETURN p.name AS person, count(c) AS commits ORDER BY commits DESC LIMIT 10;",
    "MATCH (c:Commit) SEARCH c IN (FULLTEXT INDEX commit_subject FOR 'fix' LIMIT 10) RETURN c.sha AS sha, c.subject AS subject, c.authored_at AS authored_at;",
];

/// The summary's closing line — the pointer to the question catalog in
/// the CLI skill bundle, named by its shipped path: the last starter
/// the child's summary carries, after the queries.
pub const REFERENCE_LINE: &str = "See references/run-git-to-graph.md for more questions; \
     run `relate skill install` to add it to your agent.";

/// The summary's starter list: the recommended queries, then the catalog
/// pointer — the composition the child's summary record carries (the
/// queries and the pointer the CLI importer printed after its counts, in
/// the same order).
pub fn starters() -> Vec<String> {
    let mut starters: Vec<String> = STARTER_QUERIES.iter().map(|q| q.to_string()).collect();
    starters.push(REFERENCE_LINE.to_string());
    starters
}

// The importer's tests live in `tests.rs` (the dir-module pattern the CLI
// tree's importers used, LABS-421) so this file stays under the repo's
// 1k-line budget; the reader's own tests stay beside the reader in
// `git_log.rs`, the plan's in `plan.rs`.
#[cfg(test)]
mod tests;

//! The bounded pre-write plan (ADR-0392): the aggregates one import keeps
//! from the history read to the last emitted row, and the lazily
//! generated phases the child hands the host — each a `batch` record's
//! statement plus its `rows` (ADR-0430 §1), in ADR-0376's order.
//!
//! Every field is bounded by the import's input — distinct people, distinct
//! interned paths, distinct pairs under [`crate::MAX_CO_CHANGED_PAIRS`],
//! the commits read — and each phase's rows are generated lazily from
//! these structures, so the transient memory above the graph is the plan
//! plus one chunk. Every ADR-0377 refusal happens here, before the first
//! `index` or `batch` record: an unsafe path fails while building the
//! plan, a pair cap fails while counting.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use relate_wire::{display, file_name, is_single_path_component};

use crate::git_log::GitCommit;
use crate::{ImportError, PathRefusal, Stats};

/// The person write phase's Cypher.
pub const WRITE_PERSONS: &str = "UNWIND $rows AS row \
     MERGE (p:Person {email: row.email}) \
     SET p.name = row.name";

/// The commit write phase's Cypher.
pub const WRITE_COMMITS: &str = "UNWIND $rows AS row \
     MERGE (c:Commit {sha: row.sha}) \
     SET c.subject = row.subject, \
         c.authored_at = datetime(row.authored_at), \
         c.files_changed = row.files_changed";

/// The file write phase's Cypher.
pub const WRITE_FILES: &str = "UNWIND $rows AS row \
     MERGE (f:File {path: row.path}) \
     ON CREATE SET f.name = row.name";

/// The `AUTHORED` edge write phase's Cypher.
pub const WRITE_AUTHORED: &str = "UNWIND $rows AS row \
     MATCH (p:Person {email: row.email}) \
     MATCH (c:Commit {sha: row.sha}) \
     MERGE (p)-[:AUTHORED]->(c)";

/// The `TOUCHED` edge write phase's Cypher.
pub const WRITE_TOUCHED: &str = "UNWIND $rows AS row \
     MATCH (c:Commit {sha: row.sha}) \
     MATCH (f:File {path: row.path}) \
     MERGE (c)-[:TOUCHED]->(f)";

/// The `CO_CHANGED` edge write phase's Cypher.
pub const WRITE_CO_CHANGED: &str = "UNWIND $rows AS row \
     MATCH (a:File {path: row.a}) \
     MATCH (b:File {path: row.b}) \
     MERGE (a)-[r:CO_CHANGED]->(b) \
     SET r.count = row.count";

/// The range indexes created before the first write batch (ADR-0376):
/// `IF NOT EXISTS` keeps a re-import and a retry safe.
pub const RANGE_INDEXES: [&str; 3] = [
    "CREATE INDEX file_path IF NOT EXISTS FOR (f:File) ON (f.path)",
    "CREATE INDEX commit_sha IF NOT EXISTS FOR (c:Commit) ON (c.sha)",
    "CREATE INDEX person_email IF NOT EXISTS FOR (p:Person) ON (p.email)",
];

/// The full-text index created after the last write batch commits
/// (ADR-0376): no import statement consults it, and deferring it avoids
/// maintaining it per row. The host executes the `index` record at the
/// point in the stream this child places it — after the last batch.
pub const INDEX_COMMIT_SUBJECT: &str = "CREATE FULLTEXT INDEX commit_subject IF NOT EXISTS \
     FOR (c:Commit) ON EACH [c.subject]";

/// One person's newest imported commit (ADR-0376): the greatest parsed
/// `%aI` instant and that commit's `%aN`, the name the `:Person` row
/// writes.
struct PersonSummary {
    /// The greatest `authored_at` instant seen for the person.
    authored_at: i64,
    /// The `%aN` of the commit that carried that instant.
    name: String,
}

/// The bounded pre-write aggregates and the compact per-commit plan one
/// import keeps from the history read to the last emitted row
/// (ADR-0392).
///
/// Every field is bounded by the import's input — distinct people, distinct
/// interned paths, distinct pairs under [`crate::MAX_CO_CHANGED_PAIRS`],
/// the commits read — and each phase's rows are generated lazily from
/// these structures, so the transient memory above the graph is the plan
/// plus one chunk. `stats()` derives the touched count from `commits`
/// rather than the plan carrying a second, hand-synced copy of it.
pub struct Plan {
    /// Interned email → that person's newest imported commit
    /// (ADR-0376). The key handle is shared with every `AUTHORED` row.
    people: BTreeMap<Arc<str>, PersonSummary>,
    /// The distinct stored paths, one interned handle each.
    files: BTreeSet<Arc<str>>,
    /// The distinct `CO_CHANGED` pairs → their qualifying-commit counts.
    pair_counts: BTreeMap<(Arc<str>, Arc<str>), i64>,
    /// The compact per-commit plan the commit, authored and touched phases
    /// generate their rows from.
    commits: Vec<CommitPlan>,
}

/// One commit's compact plan: interned handles only — no parsed git
/// record, no row — so the per-commit term of the plan stays a few
/// handles and one path-handle vector (ADR-0392). The commit's
/// `files_changed` count is `paths.len()` and is written from it, never
/// carried as a second field.
struct CommitPlan {
    sha: Arc<str>,
    email: Arc<str>,
    subject: Arc<str>,
    authored_at: Arc<str>,
    /// The commit's interned stored paths, in git's order.
    paths: Vec<Arc<str>>,
}

impl Plan {
    /// This import's row counts (ADR-0376's `C`, `P`, `F`, `T`, `X`). The
    /// touched count is derived — Σ each commit's path count — not stored.
    pub fn stats(&self) -> Stats {
        Stats {
            commits: self.commits.len(),
            people: self.people.len(),
            files: self.files.len(),
            touched: self.commits.iter().map(|commit| commit.paths.len()).sum(),
            co_changed: self.pair_counts.len(),
        }
    }

    /// The six write phases in ADR-0376's order, their rows generated
    /// lazily from the plan's aggregates so the wire chunker materialises
    /// one record at a time (ADR-0392). The phases are re-derivable: a
    /// re-run over a partially imported store asks for them again. The
    /// edge phases always `MERGE` — the child cannot read the target's
    /// node count, so ADR-0392's completion semantics are the one path
    /// (the host's `requires-fresh-store` manifest knob covers the
    /// fresh-only actions; this importer completes partial stores).
    pub fn phases(&self) -> [(&'static str, Box<dyn Iterator<Item = Value> + '_>); 6] {
        [
            (WRITE_PERSONS, Box::new(self.people.iter().map(person_row))),
            (WRITE_COMMITS, Box::new(self.commits.iter().map(commit_row))),
            (WRITE_FILES, Box::new(self.files.iter().map(file_row))),
            (
                WRITE_AUTHORED,
                Box::new(self.commits.iter().map(authored_row)),
            ),
            (
                WRITE_TOUCHED,
                Box::new(self.commits.iter().flat_map(touched_rows)),
            ),
            (
                WRITE_CO_CHANGED,
                Box::new(self.pair_counts.iter().map(co_changed_row)),
            ),
        ]
    }
}

/// One `:Person` row: the map's interned email handle, and the name of the
/// person's newest imported commit (ADR-0376).
fn person_row(entry: (&Arc<str>, &PersonSummary)) -> Value {
    let (email, person) = entry;
    serde_json::json!({
        "email": &**email,
        "name": person.name,
    })
}

/// One `:Commit` row (ADR-0376). `files_changed` is the commit's path
/// count, written from `paths` — never carried as a second field.
fn commit_row(commit: &CommitPlan) -> Value {
    serde_json::json!({
        "sha": &*commit.sha,
        "subject": &*commit.subject,
        "authored_at": &*commit.authored_at,
        "files_changed": commit.paths.len(),
    })
}

/// One `:File` row; its name is derived from the stored path once
/// (ADR-0376).
fn file_row(path: &Arc<str>) -> Value {
    serde_json::json!({
        "path": &**path,
        "name": file_name(Path::new(&**path)),
    })
}

/// One `:AUTHORED` row (ADR-0376), sharing the plan's handles.
fn authored_row(commit: &CommitPlan) -> Value {
    serde_json::json!({
        "email": &*commit.email,
        "sha": &*commit.sha,
    })
}

/// One commit's `:TOUCHED` rows, in git's path order (ADR-0376).
fn touched_rows(commit: &CommitPlan) -> impl Iterator<Item = Value> + '_ {
    commit.paths.iter().map(|path| {
        serde_json::json!({
            "sha": &*commit.sha,
            "path": &**path,
        })
    })
}

/// One `:CO_CHANGED` row (ADR-0376), sharing the pair map's handles.
fn co_changed_row(entry: (&(Arc<str>, Arc<str>), &i64)) -> Value {
    let ((a, b), count) = entry;
    serde_json::json!({
        "a": &**a,
        "b": &**b,
        "count": count,
    })
}

/// Fold the parsed history into the bounded pre-write aggregates and the
/// compact per-commit plan (ADR-0392).
///
/// This is where every per-commit decision happens — the people map's
/// greatest-instant name, the ADR-0377 §5 path rule, the §2 pair count and
/// its cap — so every refusal fires here, before the first record the child
/// emits. The history is consumed: `GitCommit` records and their strings
/// are dropped as the fold returns, never carried to the rows.
pub fn build(
    dir: &Path,
    history: Vec<GitCommit>,
    max_changeset: u64,
    max_pairs: usize,
) -> Result<Plan, ImportError> {
    let mut plan = Plan {
        people: BTreeMap::new(),
        files: BTreeSet::new(),
        pair_counts: BTreeMap::new(),
        commits: Vec::with_capacity(history.len()),
    };
    // The interner lives for the fold only: every handle it hands out is
    // shared into the plan's aggregates, so nothing reads the set once
    // the plan is returned (ADR-0377, ADR-0392).
    let mut interner = Interner::default();
    for commit in history {
        let email = interner.intern(&normalize_email(&commit.author_email));
        let sha = interner.intern(&commit.sha);
        let instant = commit.authored_instant().map_err(ImportError::Failed)?;
        // ADR-0376: a person's name is the `%aN` of their imported commit
        // with the greatest `authored_at`. git log is committer-date order,
        // which a rebase or cherry-pick can make disagree with author-date
        // order, so the name is chosen by comparing the parsed `%aI`
        // instants.
        plan.people
            .entry(Arc::clone(&email))
            .and_modify(|person| {
                if instant > person.authored_at {
                    person.authored_at = instant;
                    person.name = commit.author_name.clone();
                }
            })
            .or_insert_with(|| PersonSummary {
                authored_at: instant,
                name: commit.author_name.clone(),
            });
        // The changeset dedupes within the commit and orders the paths by
        // Rust `str` ordering, so the pair direction is the stored-path
        // rule. The set lives for this commit only: its pairs are counted
        // in the same iteration instead of retaining every changeset
        // (ADR-0377).
        let mut changeset: BTreeSet<Arc<str>> = BTreeSet::new();
        // First-seen order, deduped: `git log --name-only` can list one
        // path twice for a commit, and the plan's touched rows must stay
        // distinct for the `MERGE` statements to collapse a duplicate
        // anyway. The set's insert is the one membership authority: its
        // return is the first-seen test, so `paths` carries no second
        // tracker.
        let mut paths: Vec<Arc<str>> = Vec::with_capacity(commit.paths.len());
        for git_path in &commit.paths {
            let path = mapped_path(dir, &commit.sha, git_path)?;
            let stored = interner.intern(&display(&path));
            plan.files.insert(Arc::clone(&stored));
            if changeset.insert(Arc::clone(&stored)) {
                paths.push(stored);
            }
        }
        count_co_changes(&changeset, max_changeset, max_pairs, &mut plan.pair_counts)?;
        plan.commits.push(CommitPlan {
            sha,
            email,
            subject: Arc::from(commit.subject.as_str()),
            authored_at: Arc::from(commit.authored_at.as_str()),
            paths,
        });
    }
    Ok(plan)
}

/// Lower `email` so the two spellings of one address merge into one person.
fn normalize_email(email: &str) -> String {
    email.to_lowercase()
}

/// Map one `--relative` git path onto DIR: push each `/`-separated component
/// in order, never concatenate strings (ADR-0376). The stored path renders
/// with the wire kit's one shared rendering, the same one fs-to-graph uses;
/// its final component is derived through [`file_name`] when the `:File`
/// row is built.
///
/// Every component is validated before it is pushed (ADR-0377 §5), and a
/// refusal names the commit record: an accepted path is DIR plus ordinary
/// components only, so it cannot leave DIR.
fn mapped_path(dir: &Path, sha: &str, git_path: &str) -> Result<PathBuf, ImportError> {
    let unsafe_path = |refusal| ImportError::UnsafePath {
        sha: sha.to_string(),
        path: git_path.to_string(),
        refusal,
    };
    if git_path.is_empty() {
        return Err(unsafe_path(PathRefusal::Empty));
    }
    let mut path = dir.to_path_buf();
    for component in git_path.split('/') {
        if !allowed_component(component) {
            return Err(unsafe_path(PathRefusal::Component(component.to_string())));
        }
        path.push(component);
    }
    Ok(path)
}

/// Whether one `/`-separated git path component may be pushed onto DIR
/// (ADR-0377 §5): free of the Windows separator and drive prefix, and
/// exactly one platform `Normal` component — [`is_single_path_component`]
/// owns the rest of the rule (non-empty, neither `.` nor `..`).
fn allowed_component(component: &str) -> bool {
    if component.contains('\\') || has_windows_drive_prefix(component) {
        return false;
    }
    is_single_path_component(component)
}

/// Whether `component` starts with `X:`, a Windows drive prefix on every
/// host (ADR-0377 §5).
fn has_windows_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// One `Arc<str>` per distinct value: every row that carries a path or a
/// commit sha clones the handle instead of allocating the string again
/// (ADR-0377).
#[derive(Default)]
struct Interner {
    values: BTreeSet<Arc<str>>,
}

impl Interner {
    /// The interned handle for `value`, allocated on first use only.
    fn intern(&mut self, value: &str) -> Arc<str> {
        if let Some(existing) = self.values.get(value) {
            return Arc::clone(existing);
        }
        let interned: Arc<str> = Arc::from(value);
        self.values.insert(Arc::clone(&interned));
        interned
    }
}

/// Count one commit's unordered file pairs into the import-wide `counts`,
/// keyed from the smaller stored path to the larger (ADR-0376) by the
/// interned path handles (ADR-0377).
///
/// `changeset` is the commit's interned paths, deduped and ordered by stored
/// path through its `BTreeSet`, and its own `len()` is the commit's
/// `files_changed` count — the one authority the plan documents and writes.
/// A commit contributes only when it touches at most `max_changeset` files;
/// a larger commit still gets its `Commit` and `TOUCHED` rows, just no pair.
/// The result is a pure function of the commit path sets, so a re-import
/// recomputes the same counts and never increments them, and the `Arc<str>`
/// keys are the handles the rows reuse.
///
/// The map length is checked before a new key is inserted (ADR-0377 §2): the
/// pair that would become `max_pairs + 1` returns
/// [`ImportError::TooManyPairs`] and no `CO_CHANGED` row is ever built. An
/// existing key still increments.
fn count_co_changes(
    changeset: &BTreeSet<Arc<str>>,
    max_changeset: u64,
    max_pairs: usize,
    counts: &mut BTreeMap<(Arc<str>, Arc<str>), i64>,
) -> Result<(), ImportError> {
    if changeset.len() as u64 > max_changeset {
        return Ok(());
    }
    let paths: Vec<&Arc<str>> = changeset.iter().collect();
    for (index, a) in paths.iter().enumerate() {
        for b in &paths[index + 1..] {
            let key = (Arc::clone(a), Arc::clone(b));
            if let Some(count) = counts.get_mut(&key) {
                *count += 1;
                continue;
            }
            if counts.len() >= max_pairs {
                return Err(ImportError::TooManyPairs { limit: max_pairs });
            }
            counts.insert(key, 1);
        }
    }
    Ok(())
}

// The plan's tests live in `tests.rs` (the dir-module pattern the reader's
// own tests use, LABS-421) so this file stays under the repo's 1k-line
// budget.
#[cfg(test)]
mod tests;

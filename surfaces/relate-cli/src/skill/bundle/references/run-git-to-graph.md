# Questions the git graph answers

`relate run git-to-graph` imports a checkout's history into the database
`relate run fs-to-graph` already built, so one query can mix what is on
disk with who wrote it, when, and alongside what. This is a catalog of
questions to start from; every entry is one paste-ready query.

The graph has **no import or symbol graph**. `fs-to-graph` stores a file's
text, so "these files change together but don't import each other" can only be
approximated by checking whether either file's text names the other. The
catalog says so where that matters.

## Run it

```
relate run git-to-graph [DIR] [--database PATH] [--max-commits N] [--max-changeset N] [--since DATE] [--until DATE]
```

Both importers are first-party extension packages (ADR-0431 §11): they mount
as `relate run <name>` and speak the extension wire (ADR-0430) to a store
the host owns. `run` is compiled into the `full` bundle (the default) and the
`agent` bundle; a `minimal` build has no `run` command. On a fresh install
the names are not installed yet — `relate extension install <name>` adds
one from the first-party channel once it carries them (ADR-0431); an
uninstalled name fails closed with the install hint.

Minimal use — import the tree and the history into one store, then ask:

```bash
relate run fs-to-graph . --database ./code-graph --ignore-patterns target/
relate run git-to-graph . --database ./code-graph
relate query --database ./code-graph "MATCH (c:Commit) RETURN count(c) AS commits"
```

Both imports are idempotent (`MERGE` on keys, `count` overwritten, never
incremented), so running the two commands again is safe. An explicit `--database`
prints a summary and exits; omit it to import into `:memory:` and, on a TTY,
open the shell. The complete flag table and storage rules are in
`docs/api/extensions/spec.md`.

An import is bounded (ADR-0377; the pair cap as ADR-0392 raises it):
`git log` output is capped at 134217728
bytes (128 MiB), one import writes at most 5000000 distinct `CO_CHANGED`
pairs, and `--max-changeset` accepts `1..=1000` (default 50). Exceeding a
cap fails the import (exit 1) before anything is written, as does a
`--max-changeset` above 1000. Pass `--max-commits`, `--since` or `--until`
to read a smaller window; `--since A --until B` is a closed range, so an
old era imports reproducibly instead of reading everything since A
(ADR-0597). Each write batch commits as its own transaction, so
a failed import can leave the batches before the failure committed; the
idempotent `MERGE`s above make re-running the same command complete it.
Rows are generated lazily one batch at a time over the wire, so the
import's transient memory stays the plan plus one batch (ADR-0392).

## Graph schema

```text
(:Person {email, name})-[:AUTHORED]->(:Commit {sha, subject, authored_at, files_changed})
(:Commit)-[:TOUCHED]->(:File {path, name})
(:File)-[:CO_CHANGED {count}]->(:File)
(:Folder {path, name, depth})-[:CONTAINS]->(:Folder|:File)     # fs-to-graph
(:File {path, name, extension, size_bytes, content})           # fs-to-graph facts
```

`authored_at` is the engine's datetime value (not a string). `CO_CHANGED`
runs from the lexicographically smaller stored path to the larger, so use an
undirected pattern when direction does not matter. `count` is the number of
commits this import read that touched both files, and only commits with at
most `--max-changeset` paths (default 50) contribute a pair.

## File paths

`File.path` starts at **DIR exactly as given** — the current directory when
DIR is omitted, not canonicalized and not repo-rooted — with each DIR-relative
path git prints appended. With `.` the values look like `./src/main.rs`; with
an absolute DIR they are absolute. `fs-to-graph` writes the same shape for the
same DIR, which is the join key between the two imports. Copy a value from the
graph rather than guessing it: a path that does not match the stored string
exactly matches nothing.

## Parameters

Every query uses at most these six parameters; bind each with
`--param name=value` (JSON for arrays and quoted strings). A statement that
references a parameter the CLI was not given fails instead of binding `null`.

| Parameter | Holds | How to find a value |
|-----------|-------|---------------------|
| `$path` | One exact `File.path` | `relate query --database ./code-graph "MATCH (f:File) RETURN f.path ORDER BY f.path LIMIT 5"` |
| `$paths` | JSON array of exact `File.path` values | The same listing; pass `--param 'paths=["./src/a.rs","./src/b.rs"]'` |
| `$prefix` | A path prefix ending at a separator, e.g. `./src/` | Take the directory part of a `File.path` |
| `$email` | One `Person.email` (stored lowercased) | `MATCH (p:Person) RETURN p.email, p.name` |
| `$term` | A full-text term or subject substring | A word from a commit subject or a file's text |
| `$days` | Integer day window, e.g. `30` | How far back the question should look |

## Gotchas

- **Merge commits are not imported.** `--no-merges` and `HEAD` only: merged
  work appears through the commits that carry it, and branches, tags, and the
  DAG are not modeled.
- **`--since` and `--until` filter committer date**, while `authored_at` is the author
  date; a rebase or cherry-pick can make them disagree.
- **Oversized commits are half-visible.** A commit over `--max-changeset`
  keeps its `Commit` and `TOUCHED` rows but contributes no `CO_CHANGED` pair,
  so stored coupling can miss a large change. Re-import with a larger
  `--max-changeset` when that matters.
- **A narrower re-import leaves old edges.** `--since`, `--until` and `--max-commits`
  overwrite only the pairs they recompute; use a fresh `--database` when the window
  changes.
- **`content` is not universal.** `fs-to-graph` stores it only for text files
  at most `--max-content-bytes` (default 65536); binary and oversized files
  have metadata only, and files deleted from the working tree have no
  content at all.
- **Two full-text indexes exist.** `commit_subject` covers `Commit.subject`
  and is created by `git-to-graph`; `file_content` covers `File(name,
  content)` and is created by `fs-to-graph`. Content search is only as fresh
  as the last `fs-to-graph` run.

## Coupling: files that change together

### Which files change together most often?

The pairs with the highest stored co-change count are the ones to inspect
first when a change in one usually means a change in the other.

```cypher
MATCH (a:File)-[r:CO_CHANGED]->(b:File) RETURN a.path AS file, b.path AS co_changed, r.count AS times ORDER BY times DESC LIMIT 10;
```

### Which files change together when every commit counts?

Computing the pair count from `TOUCHED` instead of the stored `CO_CHANGED`
edge also counts changesets over `--max-changeset`, so it can reveal coupling
the stored edge skipped — at the cost of redoing the pair product on every
query, which is why it is the slow entry on a large history.

```cypher
MATCH (a:File)<-[:TOUCHED]-(c:Commit)-[:TOUCHED]->(b:File) WHERE a.path < b.path RETURN a.path, b.path, count(c) AS together ORDER BY together DESC LIMIT 10
```

### What changes alongside the file I am editing?

Ranking the files that share commits with one file shows the other places a
change to it will probably need to touch.

```cypher
MATCH (a:File {path: $path})-[r:CO_CHANGED]-(b:File)
RETURN b.path AS coupled, r.count AS times
ORDER BY times DESC LIMIT 10
```

### Which co-changing pairs have no textual link between them?

Filtering out pairs whose text names the other file leaves files that move
together without an obvious reference — the hidden-coupling signal from
OSK-35, approximate because the graph has no import edges.

```cypher
MATCH (a:File)-[r:CO_CHANGED]->(b:File)
WHERE r.count >= 2
  AND a.content IS NOT NULL AND b.content IS NOT NULL
  AND NOT (a.content CONTAINS b.name)
  AND NOT (b.content CONTAINS a.name)
RETURN a.path AS file, b.path AS partner, r.count AS together
ORDER BY together DESC LIMIT 10
```

## Hotspots and churn

### Which files change most often?

Ordering files by the number of commits that touched them finds the churn
hotspots worth extra review and test coverage.

```cypher
MATCH (c:Commit)-[:TOUCHED]->(f:File)
RETURN f.path AS file, count(c) AS commits
ORDER BY commits DESC, file LIMIT 10
```

### Which commits were the biggest?

Large changesets are where a review is most likely to miss something; this
lists them by the number of files git reported.

```cypher
MATCH (c:Commit)
RETURN c.sha AS sha, c.subject AS subject, c.files_changed AS files_changed
ORDER BY files_changed DESC LIMIT 10
```

### Which files under a directory change most?

Scoping churn to a path prefix turns the whole-repository ranking into a
per-area one.

```cypher
MATCH (c:Commit)-[:TOUCHED]->(f:File)
WHERE f.path STARTS WITH $prefix
RETURN f.path AS file, count(c) AS commits
ORDER BY commits DESC LIMIT 10
```

## Ownership and bus factor

### Who has changed this file the most?

The author with the most commits on a file is its de facto owner and the
first person to ask about it.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File {path: $path})
RETURN p.name AS person, count(*) AS commits
ORDER BY commits DESC LIMIT 10
```

### Which files have only one author?

A file with a single author is a bus-factor-one risk: losing that person
loses the context.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
WITH f, collect(DISTINCT p.name) AS owners, count(DISTINCT p) AS people
WHERE people = 1
RETURN f.path AS file, owners[0] AS owner
ORDER BY file LIMIT 10
```

### Which person is the sole owner of the most files?

Counting one-author files per person shows whose departure would leave the
largest unexplained surface.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
WITH f, collect(DISTINCT p) AS people
WHERE size(people) = 1
UNWIND people AS only
RETURN only.name AS person, count(*) AS sole_owner_files
ORDER BY sole_owner_files DESC LIMIT 10
```

### Which files in a directory are one person's?

The per-directory version of the single-author check finds an area with no
second reader. It counts distinct `Person` nodes, so two people who share a
display name are still two owners.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
WHERE f.path STARTS WITH $prefix
WITH f, collect(DISTINCT p) AS owners, count(DISTINCT p) AS people
WHERE people = 1
UNWIND owners AS only
RETURN only.name AS person, count(*) AS sole_owner_files
ORDER BY sole_owner_files DESC LIMIT 10
```

## Reviewers and experts

### Who should review a change to these files?

The people with the most commits on the touched files are the best first
reviewers; pass the changed paths as a JSON array.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
WHERE f.path IN $paths
RETURN p.name AS reviewer, count(*) AS commits_on_these_files
ORDER BY commits_on_these_files DESC LIMIT 10
```

### Who commits the most overall?

An overall commit ranking is the coarse starting point before narrowing to
the files a change touches.

```cypher
MATCH (p:Person)-[:AUTHORED]->(c:Commit) RETURN p.name AS person, count(c) AS commits ORDER BY commits DESC LIMIT 10;
```

### Who touched this file last?

The most recent author knows the current state of the file and often the
reason it changed.

```cypher
MATCH (p:Person)-[:AUTHORED]->(c:Commit)-[:TOUCHED]->(:File {path: $path})
RETURN p.name AS person, c.authored_at AS last_change
ORDER BY last_change DESC LIMIT 1
```

### Who knows this directory best?

Ranking people by commits in a subtree finds the reviewer for a change that
spans more files than one owner covers.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
WHERE f.path STARTS WITH $prefix
RETURN p.name AS person, count(DISTINCT f) AS files, count(*) AS commits
ORDER BY commits DESC LIMIT 10
```

## Time

### How many commits landed in the last N days?

A windowed count says whether the area is under active change or has gone
quiet.

```cypher
MATCH (c:Commit)
WHERE c.authored_at >= datetime() - duration({days: $days})
RETURN count(c) AS commits, max(c.authored_at) AS newest
```

### Which files changed in the last N days?

The files with the most commits inside the window are where recent work is
concentrated and where regressions are freshest.

```cypher
MATCH (c:Commit)-[:TOUCHED]->(f:File)
WHERE c.authored_at >= datetime() - duration({days: $days})
RETURN f.path AS file, count(c) AS commits
ORDER BY commits DESC LIMIT 10
```

### Which months were the busiest?

Grouping commits by calendar month shows the project's rhythm and puts a
quiet period in context.

```cypher
MATCH (c:Commit)
RETURN c.authored_at.year AS year, c.authored_at.month AS month, count(c) AS commits
ORDER BY year DESC, month DESC LIMIT 12
```

## Commit messages

### Which commits mention a term?

Full-text search over subjects finds the commits for a feature, bug, or
subsystem by the words in its message.

```cypher
MATCH (c:Commit)
SEARCH c IN (FULLTEXT INDEX commit_subject FOR $term LIMIT 25)
RETURN c.sha AS sha, c.subject AS subject
```

### Which commits are fixes?

Searching for the conventional `fix` token separates corrective work from
features and refactors.

```cypher
MATCH (c:Commit) SEARCH c IN (FULLTEXT INDEX commit_subject FOR 'fix' LIMIT 10) RETURN c.sha AS sha, c.subject AS subject, c.authored_at AS authored_at;
```

### Which files do commits mentioning a term touch most often?

Combining the subject filter with file touches shows where the work described
by `$term` actually lands.

```cypher
MATCH (c:Commit)-[:TOUCHED]->(f:File)
WHERE c.subject CONTAINS $term
RETURN f.path AS file, count(*) AS commits
ORDER BY commits DESC LIMIT 10
```

## People and collaboration

### Which pairs of people share files?

Two people with commits on the same files are the natural review pair and the
handover path when one is away.

```cypher
MATCH (f:File)<-[:TOUCHED]-(:Commit)<-[:AUTHORED]-(p:Person)
WITH f, collect(DISTINCT p) AS people
WHERE size(people) >= 2
UNWIND people AS a
UNWIND people AS b
WITH a, b, count(DISTINCT f) AS shared_files
WHERE a.email < b.email
RETURN a.name AS person_a, b.name AS person_b, shared_files
ORDER BY shared_files DESC LIMIT 10
```

### Who has worked on the most files?

Breadth of file ownership distinguishes a generalist from a specialist and
shows who can answer cross-cutting questions.

```cypher
MATCH (p:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
RETURN p.name AS person, count(DISTINCT f) AS files, count(*) AS commits
ORDER BY files DESC LIMIT 10
```

### What has a person worked on recently?

A person's latest commits are the quickest way to see what they are currently
responsible for.

```cypher
MATCH (p:Person {email: $email})-[:AUTHORED]->(c:Commit)
RETURN c.authored_at AS when, c.subject AS subject, c.sha AS sha
ORDER BY when DESC LIMIT 10
```

### Who shares files with this person?

Overlapping files identify a person's closest collaborators and the people to
ask when they are unavailable.

```cypher
MATCH (me:Person {email: $email})-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f:File)
MATCH (other:Person)-[:AUTHORED]->(:Commit)-[:TOUCHED]->(f)
WHERE other.email <> $email
RETURN other.name AS collaborator, count(DISTINCT f) AS shared_files
ORDER BY shared_files DESC LIMIT 10
```

## Composing with fs-to-graph

### Which files are on disk but never committed?

A `File` with no incoming `TOUCHED` edge was never committed, so this is the
untracked surface the history does not cover yet — an empty result means the
working tree is fully committed.

```cypher
MATCH (f:File)
WHERE NOT EXISTS { MATCH (:Commit)-[:TOUCHED]->(f) }
RETURN f.path AS file, f.size_bytes AS bytes
ORDER BY bytes DESC LIMIT 10
```

### Which paths exist only in git?

Paths with a `TOUCHED` edge but no `CONTAINS` edge were deleted, renamed
away, or are tracked dotfiles skipped by `fs-to-graph`; they are where history
and the working tree disagree.

```cypher
MATCH (f:File)
WHERE NOT EXISTS { MATCH (:Folder)-[:CONTAINS]->(f) }
  AND EXISTS { MATCH (:Commit)-[:TOUCHED]->(f) }
RETURN f.path AS file
ORDER BY file LIMIT 10
```

### Which files churn most per KB?

Normalizing commits by file size separates small files that change constantly
from large files that merely exist.

```cypher
MATCH (c:Commit)-[:TOUCHED]->(f:File)
WHERE f.size_bytes IS NOT NULL AND f.size_bytes > 0
RETURN f.path AS file, count(c) AS commits, f.size_bytes AS bytes,
       1000.0 * count(c) / f.size_bytes AS commits_per_kb
ORDER BY commits_per_kb DESC LIMIT 10
```

### Which file contents mention a term?

Full-text search over `File(name, content)` finds code and docs by a word,
across the tree rather than just commit messages.

```cypher
MATCH (f:File)
SEARCH f IN (FULLTEXT INDEX file_content FOR $term LIMIT 10)
RETURN f.path AS file
```

### Which directories have the most churn?

Joining `Folder` containment to `TOUCHED` shows which areas collect the most
changes, not just which files do.

```cypher
MATCH (folder:Folder)-[:CONTAINS]->(f:File)
MATCH (c:Commit)-[:TOUCHED]->(f)
RETURN folder.path AS directory, count(DISTINCT f) AS files, count(c) AS touches
ORDER BY touches DESC LIMIT 10
```

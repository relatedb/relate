# ADR-0376: The git-to-graph import contract

Status: Accepted
Claim: 5d4c39232aa32a00de4adc354258b68e247b58bc
Later references: ADR-0377 bounds the import's git-log bytes, CO_CHANGED pairs, --max-changeset maximum and path components. ADR-0380 scopes the no-new-Cargo-dependency rule to external crates and corrects the pair-memory arithmetic. ADR-0383 renames the importer's --db flag to -d / --database on both lab actions; the supersession is the flag spelling only. ADR-0392 moves the write to one explicit transaction per 1,000-row batch over lazily generated rows: the range-index-before, full-text-after order, schema, path rule and MERGE/SET semantics stand, and the idempotent re-run replaces the whole-import rollback at the scales the store's WAL payload cap makes uncommittable. ADR-0597 adds the --until window flag to the command shape and the git argv — the flag table and the invocation each gain one optional element; everything else pinned here stands.

## Context

`cypherlite lab fs-to-graph` (ADR-0157) imports the working tree; it cannot say
who wrote a file, when a change landed, or what changes together. Those facts
live in git's history, so `cypherlite lab git-to-graph` — the second visible
`lab` action (ADR-0279) — reads the checkout's history into a graph over the
same `:File.path` keys, joining "what is here" to "how it got here".

The command is positional in the ADR-0069 grammar and ships under the existing
`lab` feature (full bundle only, ADR-0104). It takes `--db` as fs-to-graph does
(ADR-0157), including ADR-0286's explicit-`--db` exit — not the statement
session's `--database` (ADR-0200). The contract below is pinned before
implementation, as ADR-0157's was.

Constraints: no new Cargo dependency (AGENTS.md R6; git is a runtime
requirement), a path that joins fs-to-graph (ADR-0157), a safe re-import, and
a bound plus indexes for co-change (see the alternatives).

## Decision

`cypherlite lab git-to-graph [DIR] [--db PATH] [--max-commits N]
[--max-changeset N] [--since DATE]` is a **visible** `lab` action (ADR-0279):
listed in the hand-written `--help` and `docs/api/cli/spec.md`, covered by
tests, under the existing `lab` feature (full bundle only, ADR-0104), with no
feature of its own.

### Command shape

| Argument / flag | Default | Meaning |
| --- | --- | --- |
| `DIR` | current directory | Work tree; `std::env::current_dir()` when omitted, exactly as fs-to-graph resolves it (ADR-0157). At most one positional. |
| `--db PATH` | `:memory:` | Target. An explicit `--db` prints the summary and exits after the import (ADR-0286); omitting it imports into `:memory:`. |
| `--max-commits N` | all history | Positive integer passed to git as `--max-count=N`. |
| `--max-changeset N` | `50` | Positive integer; a larger commit still gets `Commit`/`TOUCHED` but no `CO_CHANGED` pairs. |
| `--since DATE` | none | Non-empty string passed to git as `--since=DATE`; git filters by **committer** date. |

`--max-commits` and `--max-changeset` reject `0`, negative and non-numeric
values; `--since` rejects a missing or empty value. Any usage error — a bare
`lab`, unknown action or flag, extra positional, missing or bad value — prints
the `lab` usage line and a `--help` hint and exits **2** (ADR-0279/ADR-0157);
runtime failures exit **1**.

### Graph schema

```cypher
(:Person {email, name})-[:AUTHORED]->(:Commit {sha, subject, authored_at, files_changed})-[:TOUCHED]->(:File {path})
(:File)-[:CO_CHANGED {count}]->(:File)
```

- **`(:Person {email, name})`** — `MERGE`d on `email`, the lowercased
  mailmap-resolved `%aE`; `name` is the mailmap-resolved `%aN` of that
  person's newest imported commit (greatest `authored_at`).
- **`(:Commit {sha, subject, authored_at, files_changed})`** — `MERGE`d on
  the full `sha`; `subject` is `%s`, `authored_at` is `datetime(%aI)` (the
  engine's datetime value, not a string), and `files_changed` is the number
  of paths git listed for the commit in this import.
- **`(:File {path})`** — `MERGE`d on `path`; `name` is the final path
  component, set **`ON CREATE` only**; `extension`, `size_bytes` and
  `content` are **never** written (fs-to-graph's facts, ADR-0157).
- **`AUTHORED` and `TOUCHED`** — `MERGE`d on their endpoints, no properties;
  a commit's `files_changed` equals its `TOUCHED` edge count in a graph built
  by this import.
- **`(:File)-[:CO_CHANGED {count}]->(:File)`** — one edge per unordered pair
  of distinct files a qualifying commit touched. `count` is the number of
  commits read by this import that touched both files, counting only commits
  with `files_changed <= --max-changeset`; a larger commit still contributes
  its `Commit`/`AUTHORED`/`TOUCHED`, only no pair.

Every `CO_CHANGED` edge runs from the lexicographically smaller **stored
path** (`File.path`, Rust `str` ordering) to the larger, so a pair is written
once and the graph never holds both `a->b` and `b->a`; either-direction
queries use an undirected pattern. Nodes `MERGE` on keys, edges on endpoints,
and `count` is **`SET`** — overwrite, never increment — so the same re-import
is idempotent.

### Paths

`File.path` starts at **DIR exactly as given** (`std::env::current_dir()` when
DIR is omitted, exactly as fs-to-graph resolves it; not canonicalized, not
repo-rooted), pushes each `/`-separated component of the git-printed path in
order, and renders with `to_string_lossy()`. With `--relative` and the
pathspec `.`, git prints DIR-relative paths with no DIR prefix, so the string
equals the path fs-to-graph stores for the same DIR argument (also
`to_string_lossy()`, with the platform separator on Windows) and `File.path`
joins the two graphs; repo-root-relative paths would not, which is why
`--relative` is in the invocation. Tracked **dotfiles** (skipped by fs-to-graph
unless `--include-hidden`) and files **deleted in history** become git-only
`:File` nodes.

### Reading git history

One `std::process::Command` with **no shell** and **stdin null**
(`Stdio::null()`), so every value is one argv element and git cannot prompt:

```
git -C DIR -c log.showSignature=false --no-pager log \
  --no-merges --no-color --no-ext-diff --no-textconv --relative \
  --name-only -z --format=%x1e%H%x1f%aN%x1f%aE%x1f%aI%x1f%s \
  [--max-count=N] [--since=DATE] -- .
```

- `-C DIR` reads that work tree; `log.showSignature=false`, `--no-pager` and
  `--no-color` keep signature output, paging and ANSI escapes out;
  `--no-ext-diff`/`--no-textconv` stop external diff and textconv filters from
  changing `--name-only`.
- `--no-merges` excludes merge commits (below); `--relative` prints
  DIR-relative paths and limits the diff to DIR; `-- .` is the pathspec, and
  `--` keeps a flag-shaped path out of the parser.
- `--name-only -z` NUL-terminates each path, so spaces, tabs and newlines
  cannot split a record. Records start with `0x1E`; `%H`, `%aN`, `%aE`, `%aI`,
  `%s` are separated by `0x1F`; the parser splits records on `0x1E`, fields on
  `0x1F`, paths on NUL (the newline before the first name is not a path).
- `--max-count=N` and `--since=DATE` are added only when present, each one
  argv element; `--since` uses **committer** date, which can disagree with
  `authored_at` (author date). No revision argument means `HEAD`, not every
  ref.

`git` missing from PATH, a DIR git refuses (not a repository, missing, not a
directory), or any non-zero exit fails the import with exit **1**; git's
stderr is printed verbatim under the CLI's `Error:` label (a spawn failure
prints the OS error), and a success's warnings are not printed.

### Writes, indexes and transactions

Before any write, three range indexes are created `IF NOT EXISTS`:

```cypher
CREATE INDEX file_path IF NOT EXISTS FOR (f:File) ON (f.path)
CREATE INDEX commit_sha IF NOT EXISTS FOR (c:Commit) ON (c.sha)
CREATE INDEX person_email IF NOT EXISTS FOR (p:Person) ON (p.email)
```

They are load-bearing: every node `MERGE` and every `TOUCHED`/`CO_CHANGED`
endpoint lookup is an equality on one of these properties, and an unindexed
label scan makes the write quadratic (see the alternatives). They precede the
write transaction, in their own statements, so its MERGEs use them from the
first row; `IF NOT EXISTS` keeps a re-import and a retry safe.

All node and edge writes then run in **one explicit write transaction** that
rolls back as a whole on failure: a mid-import error leaves the graph exactly
as it was, never a half-applied history. Paths, subjects and dates are bound
as parameters, never interpolated into Cypher text (ADR-0157).

After the transaction commits, it creates the full-text index
`commit_subject` over `:Commit(subject)`; no import statement consults it, and
creating it after the bulk write avoids maintaining it on every commit row.

### UX

On success stdout prints exactly:

```
imported C commits, P people, F files (T TOUCHED, X CO_CHANGED edges) into DB
```

C is the commits read, P the `Person` nodes merged, F the distinct `File`
paths merged, T the `TOUCHED` edges written, X the `CO_CHANGED` edges written,
and DB the target (`:memory:` or the `--db` path); the numbers describe this
import's rows, not store totals.

Then three starter queries:

```cypher
MATCH (a:File)-[r:CO_CHANGED]->(b:File)
RETURN a.path AS file, b.path AS co_changed, r.count AS times
ORDER BY times DESC LIMIT 10;

MATCH (p:Person)-[:AUTHORED]->(c:Commit)
RETURN p.name AS person, count(c) AS commits
ORDER BY commits DESC LIMIT 10;

MATCH (c:Commit)
SEARCH c IN (FULLTEXT INDEX commit_subject FOR 'fix' LIMIT 10)
RETURN c.sha AS sha, c.subject AS subject, c.authored_at AS authored_at;
```

Then a pointer to the question catalog at `references/lab-git-to-graph.md` in
the CLI skill bundle (`cypherlite skill install`). The catalog lists the
plain-language questions the graph answers — who owns a file, what changes
together, what landed recently — each with a paste-ready query; it ships with
the implementation, listed in SKILL.md and the BUNDLE_FILES drift guard.

The telemetry action dimension is `git-to-graph` (ADR-0079 §4). The
interactive shell opens only when `--db` is **omitted**, stdin is a TTY, and
`repl` is compiled in — exactly the fs-to-graph rule (ADR-0157, ADR-0286).
An explicit `--db`, including `--db :memory:`, prints the summary and starter
queries and exits 0, checkpointing a persistent target; no TTY or no `repl`
does the same.

## Alternatives considered

- **libgit2 or gix as the history reader.** libgit2 brings a native C build to
  every target; gix brings a large pure-Rust tree. Either is the heavy
  dependency class AGENTS.md R6 makes an explicit decision about, and neither
  is needed for one experimental action: git already applies `.mailmap`, parses dates,
  and emits `--relative` paths in the shape the keys need. Rejected: a
  build-time dependency on every platform to replace a runtime program every
  targeted machine already has.
- **Repository-root-relative paths** (drop `--relative`, resolve
  `git rev-parse --show-toplevel`, prefix the root). `File.path` would read
  `src/x.rs` while fs-to-graph stores `DIR/src/x.rs`; the join would fail for
  every DIR that is not the root, and making it join would mean changing
  ADR-0157 and invalidating every graph already built. Rejected: a second git
  invocation for a path shape the working-tree graph cannot consume.
- **Compute co-change at query time from `TOUCHED`.** Every query would
  self-join each commit's file set; that is the same quadratic pair product,
  paid per query, and no index can serve a count derived at read time.
  Rejected: the import is the one place to pay it, and a stored `count` is
  what makes the question a single indexed pattern.
- **Import merge commits, with `PARENT` edges.** A merge has no single change
  set: `--name-only` suppresses its diff, so importing it means picking a
  parent (`-m`), which re-lists the branch's changes and double-counts work
  the merged commits represent; the DAG would also need a second edge type
  and a parent list the catalog does not use. Rejected: `--no-merges` keeps
  the graph to changes a person authored and leaves the DAG to
  `git log --graph`.
- **Increment `CO_CHANGED` counts on re-import** (`SET r.count =
  coalesce(r.count, 0) + 1`, or `+=`). A second import of the same history
  would double every count, making the graph depend on how often the command
  ran. Rejected: `MERGE` plus `SET` of the recomputed count makes the same
  import produce the same graph.
- **Write without the range indexes.** Measured at plan time: 60,000 pair
  `MERGE`s over 5,000 `:File` nodes took **102 s** without the `file_path`
  range index and **0.9 s** with it, because each unindexed `MERGE` scans the
  label. Rejected: the indexes are the difference between an import that
  finishes and one the user abandons, so they precede the write transaction.

## Consequences

- The schema, path rule, three range index names, full-text index name, and
  `CO_CHANGED` direction and count semantics are pinned. Changing any of them
  — a label, a property, a merge key, the path rendering, an index name, the
  edge direction, or what `count` counts — needs a new ADR, because user
  queries and every earlier import depend on them.
- AGENTS.md R6 is **not** triggered: no Cargo dependency is added. `git` is a
  runtime requirement of this action only; the workspace builds without it,
  `lab` is full-bundle only (ADR-0104), and a missing git fails with exit 1.
- The import reads `HEAD` only, with `--no-merges`: merged changes appear
  through the commits that carry them, and branches, tags and the DAG are not
  modeled. `--since` filters by committer date while `authored_at` is the
  author date, so the two can disagree after a rebase or cherry-pick.
- `files_changed`, `TOUCHED` and `CO_CHANGED` are scoped to DIR: `--relative`
  and the `.` pathspec drop changes outside DIR, so running at the root and at
  a subdirectory yields different graphs for the same commit, by design.
- The import never deletes: it `MERGE`s and `SET`s. Re-importing the same DIR
  with the same flags is idempotent; a narrower window (`--since`,
  `--max-commits`) overwrites the pairs it recomputes and leaves edges
  outside the window in place. Use a fresh `--db` when the window changes.
- The implementation change adds the action to `docs/api/cli/spec.md` and the
  hand-written `--help`, and adds `references/lab-git-to-graph.md` with its
  SKILL.md row and BUNDLE_FILES entry. The range indexes survive a
  rolled-back import (they are outside the transaction), and a retry finds
  every index `IF NOT EXISTS`.

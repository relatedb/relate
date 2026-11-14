# 0157. `fs-to-graph` import contract

- **Status**: Accepted
- **Date**: 2026-10-01
- **Deciders**: CypherLite maintainers
- **Later references**: ADR-0286 supersedes the unconditional TTY-shell
  rule below. An explicit `--db` never opens the interactive shell; the
  shell still opens only when `--db` is omitted, on a TTY with `repl`.
  ADR-0383 renames the importer's `--db` flag to `-d` / `--database`; the
  supersession is the flag spelling only. ADR-0602 supersedes the
  whole-import rollback later ADRs (ADR-0392, ADR-0430) attribute to
  this contract: the extension-mounted importer commits each writing
  call as its own transaction, so a failed import keeps the batches
  that committed and a re-run completes the graph. The schema, walk
  semantics, content capture, index and MERGE-by-path idempotence
  below stand.

## Context

`cypherlite lab fs-to-graph` (the visible `lab` action ADR-0279 introduces)
turns a directory tree into a property graph that users then query with
Cypher. The value of the command is entirely in the graph it produces: the
node labels, the property names, the relationship type, and the import
semantics are the public contract users write queries against, so they must
be pinned before the implementation lands, not inferred from it afterwards.

The design constraints are:

- The import must be a plain `std::fs` walk. No new heavy dependency
  (AGENTS.md R6) and no async runtime: the walk is blocking and the graph is
  built with ordinary Cypher writes against `cypherlite::Database`.
- Re-running the import must be safe. Users will point the command at the
  same directory more than once (a second import after adding files, or an
  accidental repeat), and the graph must not accumulate duplicates.
- The graph is meant to be searched. File contents are the interesting text,
  so a fulltext index over the content is part of the contract, not an
  optional extra.
- The command must be usable with no arguments beyond a path, and must also
  persist when the user asks for it.

## Decision

### Graph schema

- **One `(:Folder {path, name, depth})` node per directory** the walk
  visits, including the walked root. `path` is the directory path as the walk
  produced it, `name` is its final path component, and `depth` is its
  distance from the walked root (`0` for the root).
- **One `(:File {path, name, extension, size_bytes, content?})` node per
  file.** `path` is the file path as the walk produced it, `name` is its
  final path component, `extension` is its extension without the leading dot,
  and `size_bytes` is its size on disk. `content` is present only when
  captured (below).
- **One `(:Folder)-[:CONTAINS]->(:Folder|File)` relationship** from a
  directory to each of its direct children, folder or file.

### Walk semantics

- The walk is an iterative `std::fs` walk over an explicit stack of open
  directories: depth-first in `read_dir` order, with no recursion, so nesting
  depth is bounded by the filesystem's path-length limits, never by the call
  stack. An adversarially deep tree fails with an ordinary I/O error (a path
  too long to address), not a stack overflow mid-import.
- **Symlinks are never followed**, for files or directories. A symlink is
  seen for what it is (its `DirEntry` type) and is not descended into; this
  avoids cycles and escaping the walked root.
- **Dotfiles and dot-directories are skipped** unless `--include-hidden` is
  given. "Hidden" means the final path component starts with `.`.
- **`--ignore-patterns PAT` prunes entries by name** (repeatable, one pattern
  per occurrence). A pattern matches a single path component — a file or
  directory name — by exact string equality; there are no globs. A trailing
  `/` marks the pattern **directory-only** (`node_modules/`); a bare pattern
  (`debug.log`) matches files and directories alike. A matched directory is
  pruned entirely: no `Folder` node, no descendants, no `CONTAINS` edges. A
  matched file gets no `File` node. Ignored entries are absent from the
  import summary counts. The filter composes with the dotfile rule: both
  apply. A value that is empty, or empty after trimming a trailing slash
  (`/`), is a usage error (exit 2).

### Content capture

- A file gets a `content` property only when its **first 8 KiB are valid
  UTF-8 with no NUL byte**. A NUL byte or invalid UTF-8 marks the file as
  binary, and a binary file gets metadata only.
- Content capture is capped at **`--max-content-bytes`**, default **65536**.
  The cap is all-or-nothing: a file larger than the cap gets no `content`,
  even when its first 8 KiB look like text, and is imported with its metadata
  only. (The 8 KiB sniff decides *whether* to capture; the cap decides
  *whether the whole file fits*.)
- Binary and oversized files therefore appear as `(:File …)` nodes with
  `path`, `name`, `extension`, and `size_bytes`, and no `content`.
- I/O errors opening or reading a file's content **fail the import**: they
  propagate as import errors, exactly like the walk's `read_dir`/`stat`
  errors. Metadata-only means binary or oversized — never "unreadable".

### Idempotent re-import

- Nodes are written with **`MERGE` by `path`** (files and folders alike), and
  the `CONTAINS` relationship is merged on its endpoints. Re-importing the
  same tree into the same database is idempotent: node and relationship
  counts are unchanged.

### Index

- After the import the command runs
  `CREATE FULLTEXT INDEX file_content IF NOT EXISTS FOR (f:File) ON EACH [f.name, f.content]`,
  so the imported graph is immediately searchable by name and content.

### UX

- **`--db` defaults to `:memory:`.** With no `--db`, the command imports into
  an in-memory database.
- On a TTY **with the `repl` feature** compiled in, the interactive shell
  opens on the imported graph after a successful import, so the user can
  query it immediately. Without `repl`, or when not on a TTY, the command
  imports and exits.
- **`--db PATH` persists** the imported graph to that path and checkpoints
  it, so the graph outlives the process.

## Alternatives considered

- **One node per path, with no `Folder`/`File` split.** Rejected: it makes
  every query filter on a kind property and cannot express
  `(:Folder)-[:CONTAINS]->(:File)` as a typed pattern; the split lets a query
  walk the tree and filter folders from files by label, which is the whole
  point of importing a tree.
- **Follow symlinks.** Rejected: a symlink cycle would make the walk
  non-terminating, and a symlink out of the walked root would silently
  import content the user did not point at. Not following them is the safe
  and predictable default.
- **Recursive walk.** Rejected: recursion depth is bounded by the call
  stack, so an adversarially deep tree could overflow it mid-import and
  crash the process; an explicit stack of open directories keeps the same
  depth-first `read_dir` order with depth bounded only by the filesystem's
  path-length limits.
- **Treat an unreadable file's content as absent.** Rejected: mapping an
  I/O error (permissions, EIO) to "no `content`" is indistinguishable from a
  binary or oversized file and would silently import an incomplete graph;
  the walk already fails on `read_dir`/`stat` errors, so content read errors
  fail the import the same way.
- **Index content without a cap.** Rejected: a single large file (a log, a
  dataset) would balloon memory and the index with content the user did not
  ask to search; the cap keeps imports bounded and predictable, and
  `--max-content-bytes` lets a user who needs more raise it.
- **Honour `.gitignore`.** Still deferred, not rejected on its merits:
  parsing ignore files correctly needs a dependency (or a hand-written
  matcher) and a decision about precedence with `--include-hidden` and
  `--ignore-patterns`; that is its own dependency decision and gets its own
  ADR before it lands. `--ignore-patterns` now covers the common case (build
  output and dependency directories, stray logs) without gitignore syntax.
- **Store content for binary files.** Rejected: binary bytes are not valid
  Cypher string data, cannot be meaningfully fulltext-indexed, and would
  bloat the graph; metadata-only keeps binary files present in the graph
  without pretending their bytes are text.

## Consequences

- The schema (`(:Folder {path, name, depth})`,
  `(:File {path, name, extension, size_bytes, content?})`,
  `(:Folder)-[:CONTAINS]->(:Folder|File)`) and the MERGE-by-path idempotence
  are pinned: changing either requires a new ADR, because user queries and
  re-import safety depend on them.
- The text heuristic (first 8 KiB valid UTF-8, no NUL) and the
  `--max-content-bytes` cap (default 65536; larger files get no `content`) are
  part of the contract; a file at either boundary can flip between "content
  captured" and "metadata only", so both are documented rather than left
  implicit.
- The walk is iterative, so import depth is limited by the filesystem's
  path-length limits, not the call stack: an adversarially deep tree fails
  with an ordinary I/O error rather than crashing the process.
- An unreadable file (permissions, I/O error) fails the whole import with
  the underlying error, matching `read_dir`/`stat` behavior; only binary and
  oversized files import metadata-only.
- `docs/api/cli/spec.md` documents the `lab fs-to-graph` usage, its
  positionals, and every flag (`--db`, `--include-hidden`,
  `--max-content-bytes`, `--ignore-patterns`) with defaults, in the same
  change as the implementation.
- The fulltext index `file_content` is created by the command; a later change
  to its name or indexed properties is a breaking change for queries that
  reference it.
- The walk has no ignore-file support until a later ADR adds it;
  `--ignore-patterns` covers explicit name-based pruning, and users who need
  `.gitignore` semantics today filter in Cypher after importing.
- The command depends only on `std::fs` and the public `cypherlite::Database`
  surface; it adds no heavy dependency, so AGENTS.md R6 is not triggered.

## Later references

ADR-0286: an explicit `--db` does not open the interactive shell. The TTY
shell in the UX section applies only when `--db` is omitted.

ADR-0383: `-d` / `--database` replaces `--db` on both `lab` importers. The
supersession is the flag spelling only — the schema, walk, content, index and
default above stand, and ADR-0286's explicit-flag rule now applies to `-d` /
`--database`.

ADR-0602: the extension-mounted importer commits each writing call as its own
transaction (`per-batch`, ADR-0392's framing). A failed import keeps the
batches that committed, and a re-run completes the graph; the schema, walk
semantics, content capture, index and MERGE-by-path idempotence above stand.

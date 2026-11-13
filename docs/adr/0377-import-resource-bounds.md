# ADR-0377: Import resource bounds for `lab git-to-graph`

Status: Accepted
Claim: 3da31eeb8266d64db2f36842612d9fa923a641bd
Later references: ADR-0380 scopes the no-new-Cargo-dependency rule to external crates and corrects the CO_CHANGED pair-memory arithmetic recorded in §2 and Consequences. ADR-0392 raises §2's pair cap to 5,000,000 and supersedes §2's "no partial import to resume" with the idempotent re-run; every refusal keeps firing before the range-index DDL and the first write batch. ADR-0597 adds --until to §1's refusal hint — the LogTooLarge message names it among the flags that read a smaller window; the caps and every other refusal text stand.

## Context

ADR-0376 pinned the schema, path rule, flags, index set and transaction of
`cypherlite lab git-to-graph`. It did not pin a resource envelope. The
security review of the implementation found three paths where a large or
hostile checkout exhausts the host or leaves DIR:

1. `git_log::run_command` buffers the child's entire stdout with
   `Command::output()`. `--max-commits` defaults to all history, so the
   allocation is proportional to the whole log before the first record is
   parsed. A repository with a 2 GiB `git log` needs more than 2 GiB to
   read it, and a hostile repository controls that size.
2. `count_co_changes` keeps one map entry per distinct file pair, and
   `build_rows` then materializes one parameter row per pair.
   `--max-changeset` (default 50) bounds a commit, not the import: 10,000
   commits touching 50 files each produce 12,250,000 pairs, roughly 6 GiB
   at ~0.5 KiB per pair, before the write transaction starts.
3. `mapped_path` accepts `.`, `..` and root or prefix components; `push`
   then resolves them. A crafted tree can store a `File.path` outside DIR,
   and that path is the key the fs-to-graph graph joins on (ADR-0157), so
   the escape is visible to every query, not just to the importer.

The design constraints are: no new flag, because `--max-commits`, `--since`
and `--max-changeset` already shrink the work; no new dependency (AGENTS.md
R6); ADR-0376's schema, accepted-path rendering, `MERGE`/`SET` semantics
and index names unchanged; and every refusal before the write transaction,
so a rejected import writes nothing.

This ADR amends ADR-0376's resource consequences only. It does not change
the graph schema, the path rendering for an accepted path, the flag
meanings, or the exit-after behaviour of an explicit `--db` (ADR-0286).

## Decision

Three numeric bounds, each a compile-time constant of the `lab` feature.

### 1. `git log` stdout buffer: 134,217,728 bytes (128 MiB)

`read_history` never buffers more than **134,217,728 bytes** of the child's
stdout. It reads into one buffer instead of calling `Command::output()`;
the byte that would make the buffer 134,217,729 fails the import. On that
byte the reader terminates the child, reaps it, and returns
`ImportError::LogTooLarge`; no partial output is parsed and no record is
written.

Memory arithmetic: the buffer is 128 MiB, and `parse_log` keeps it alive
while it builds one `GitCommit` per record. A one-path record is ~200 bytes
of stdout; its parsed form is five `String`s (5 x 24-byte headers plus ~154
bytes of heap) and a `Vec<String>` (~24 + 24 + ~50 bytes), ~1.8x the raw
bytes. The parsed history is therefore ~230 MiB and the record-slice vector
~10 MiB at ~200-byte records, so peak read-and-parse is ~0.4 GiB. 128 MiB
holds ~300,000 five-path commits (~420 bytes each) or ~650,000 one-path
commits, which covers a normal checkout's whole history; histories above it
are the ones `--since` and `--max-commits` exist to window.

Error text (exit 1, `Error:` label):

```text
git log output exceeds the 134217728-byte import limit (128 MiB); pass --max-commits or --since to read a smaller window
```

### 2. `CO_CHANGED` pairs: 500,000 distinct pairs per import

The counting loop fails on the **500,001st distinct pair**. It checks the
map length before inserting a new key (an existing key still increments),
so the check is part of counting, not a pass over a finished map, and
`Rows::co_changed` is never materialized. The result is
`ImportError::TooManyPairs`.

Memory arithmetic: a `BTreeMap<(String, String), i64>` entry is ~200 bytes
(two 24-byte `String` headers, ~80 bytes of path heap, an 8-byte count and
node slack); the materialized `Value::Map` row is ~300 bytes (a three-node
`BTreeMap` holding two `Arc<str>` paths and an `i64`). At ~0.5 KiB per pair,
500,000 pairs cost ~250 MiB. At 10 files per commit (45 pairs) the cap
admits ~11,100 commits; at the default 50-file changeset (1,225 pairs) it
admits ~408. It is about 24x below the 12,250,000-pair default-flags
blow-up in Context.

Error text (exit 1):

```text
import would write more than 500000 CO_CHANGED pairs; lower --max-changeset or pass --max-commits to read fewer commits
```

The count is a pure function of the window: ADR-0376's import recomputes
counts and `SET`s them, it never increments stored state. Re-running the
same command therefore fails at the same pair, and there is no partial
import to resume. Only a smaller window (`--max-commits`, `--since`) or a
lower `--max-changeset` changes the count; a re-import that does not shrink
the window cannot recover.

### 3. `--max-changeset` maximum: 1,000

`--max-changeset` accepts `1..=1000`. A larger value is rejected at parse
time as a usage error: exit 2, the `lab` usage line and a `--help` hint,
exactly ADR-0376's usage-error shape. It is not clamped and not silently
lowered.

Memory arithmetic: one commit at the maximum contributes
1,000 x 999 / 2 = **499,500** pairs, below the 500,000 pair cap, so a single
commit can never trip limit 2 on its own; the per-commit dedup
`BTreeSet<&str>` is ~1,000 x 48 bytes ~ 48 KiB, and the pair loop is ~500
thousand map probes. A larger maximum would make the two limits
inconsistent: one commit could exhaust the whole pair budget, leaving no
per-commit recourse.

Error text (exit 2):

```text
invalid --max-changeset: '1001' is above the maximum 1000
```

### 4. Error-text contract

The three runtime checks are typed `ImportError` variants (`LogTooLarge`,
`TooManyPairs`, `UnsafePath`), not formatted strings assembled at the call
site. The CLI renders a variant as one `Error: {message}` line and exits 1;
the parse check is a `ParseError` and exits 2. The wording above is the
contract: every message names the limit as a number and the flag that
changes it, and tests assert the exact text (with only the offending value
substituted). No limit drops data, truncates output or continues with a
default. Each number lives in exactly one constant (`MAX_LOG_BYTES`,
`MAX_CO_CHANGED_PAIRS`, `MAX_CHANGESET`) in the lab import module; the
runtime checks, the parse check, `--help` and the tests read those
constants, never a repeated literal.

### 5. Path components

`mapped_path` validates each `/`-separated component **before** pushing it.
A component is accepted only when all of these hold:

- it is non-empty (so a leading `/`, an absolute path or `a//b` fails);
- it is not `.` and not `..`;
- it contains no `\`, the Windows separator, and does not begin with an
  ASCII letter followed by `:`, a Windows drive prefix;
- `Path::new(component).components()` yields exactly one
  `Component::Normal`, so root and platform prefix components fail on every
  host.

A failing path is `ImportError::UnsafePath`, exit 1, naming the commit
record:

```text
malformed git path "<path>" in commit <sha>: component "<component>" is not allowed
malformed git path "" in commit <sha>: path is empty
```

The two lines are the component case and the empty-path case. Path and
component use Rust debug quoting, so control characters cannot forge a
second message line.

This closes the gap by construction: `PathBuf::push` of a `Normal` component
cannot remove an accepted component, every accepted component is one
`Normal`, and no `.`, `..`, root or prefix can be pushed, so the stored
`File.path` is DIR plus ordinary components and cannot leave DIR. The check
runs before the row exists, so no escaping path reaches `File.path` or the
write transaction. The parser's own empty-path error (naming the record
ordinal, ADR-0376) stays and fires first on raw output; `mapped_path` keeps
its empty check because it is the component-level API.

### 6. Order of checks and inert refusal

The phases stay in ADR-0376's order: read history (limit 1) -> build rows
(limit 2 and the path rule) -> create the range indexes -> open the write
transaction. Limits 1 and 2 therefore run before any store write: a refused
import creates no index, writes no `MERGE` or `SET`, commits no
transaction, and leaves a target that already holds an older import exactly
as it was. It prints one `Error:` line and exits 1, never the success
summary or the starter queries.

## Alternatives considered

- **Stream the child's stdout instead of capping it.** Streaming would cut
  the 128 MiB buffer to one record, and it is the better end state. It is
  deferred, not rejected as an idea: the import still materializes every
  `GitCommit`, every `TOUCHED` row and every pair before the transaction
  opens, so the stdout buffer is at most ~0.4 GiB of a peak that stays
  linear-plus-pairs after streaming; and it replaces ADR-0376's pure
  `parse_log(&[u8])` parser, its record-ordinal errors and its separator
  handling in a change whose subject is a bound. A later ADR can stream the
  same byte limit as a work bound and lower the peak without changing this
  contract. Rejected for this change: the parser rewrite is a larger,
  separately reviewable decision, and the remaining unbounded pair term
  would still need this ADR's limit.
- **Leave the `CO_CHANGED` aggregate uncapped.** Rejected: the default
  flags on a 10,000-commit, 50-file history build 12,250,000 entries, ~6
  GiB of map and rows, and fail in the allocator instead of with a message
  a user can act on; the transaction has not started, so the only result is
  an unhelpful abort after minutes of work.
- **Add one new flag per limit (`--max-log-bytes`, `--max-pairs`).**
  Rejected: `--max-commits` and `--since` already shrink the log bytes and
  the commit set, `--max-changeset` already shrinks each commit's pair
  product, and a safety rail that every user must remember is not a tuning
  knob. Two more flags would also have to land in `--help`, the flag table
  and every bundle test for no capability the existing flags lack.
- **Validate the rendered path after the fact.** The rendered path is
  `to_string_lossy()` output, so distinct invalid-UTF-8 byte paths collapse
  to the same string with U+FFFD, and `\` is a separator only on Windows; a
  post-hoc check would re-split an already lossy, platform-dependent string
  after the row was built. Rejected: the check is lossy, platform-dependent
  and too late to guarantee that no escaping path reaches a row, while
  component validation is the only point where the input is the exact git
  path and the join is one component at a time.
- **Bound the aggregate as `--max-commits × (--max-changeset choose 2)`.**
  Rejected: that product is a worst case no real history reaches (most
  commits touch few files), and distinct pairs dedupe across commits, so
  the product overestimates the memory by an unbounded factor; the map
  length is the exact quantity that consumes memory.
- **Adopt one import-wide byte budget instead of shape limits.**
  Rejected: a byte budget is an allocator- and platform-dependent estimate
  rather than a checkable invariant, the same reasoning ADR-0352 records for
  processing-memory limits. The three bounds above are exact counts of the
  two superlinear structures and one parse value, testable at their
  thresholds; a budget estimated from rows and bytes is neither.

## Consequences

- The three numbers — 134,217,728 bytes of `git log` stdout, 500,000
  distinct `CO_CHANGED` pairs, and a maximum `--max-changeset` of 1,000 —
  are part of the CLI contract. Changing any of them needs a new ADR, with a
  `Later references` note here, because tests and user scripts depend on the
  exact thresholds and messages.
- An import that succeeded before may now fail with the cap's advisory
  message instead of completing: a history above 128 MiB of `git log`
  output, or one whose distinct pairs exceed 500,000 at the chosen flags.
  The message names the limit and the flag that shrinks the work, so the
  refusal is actionable; a `--max-changeset` above 1,000 that previously
  ran now exits 2 at parse.
- The hand-written `--help` (`surfaces/cypherlite-cli/src/help/lab.rs`) and
  the `lab git-to-graph` flag table in `docs/api/cli/spec.md` must name the
  `--max-changeset` maximum (1000) and both runtime failure modes (the
  134,217,728-byte log cap and the 500,000-pair cap), including the flags
  that change them. CLI tests assert the same wording, so help, spec and
  error text cannot drift.
- ADR-0376 gets a `Later references` note pointing here. Its schema, path
  rendering for accepted paths, index names, `MERGE`/`SET` semantics,
  usage-error shape and explicit-`--db` exit (ADR-0286) are unchanged.
- The fs-to-graph join (ADR-0157) is unchanged for every path that imported
  before: accepted components render exactly as ADR-0376 specifies. The
  price is that a Unix filename containing `\` or beginning with a drive
  designator (`X:`) is now an import error; that is what makes the rejection
  platform-independent rather than a Windows-only check.
- The import's own transient heap stays under ~0.7 GiB at the caps: ~0.4
  GiB for the raw buffer plus parsed history, ~0.25 GiB for the pair map and
  its rows, and the linear `TOUCHED` rows per path. The store's own nodes
  and edges are the database's memory, not the import's; the two superlinear
  terms are what these caps bound.
- No Cargo dependency and no new flag are added, so AGENTS.md R6 is not
  triggered; the bounds are `lab`-feature constants and every unchanged
  build keeps the same import surface.
- Streaming stdout remains the better end state and stays open; a later ADR
  can lower peak memory further while keeping the 128 MiB byte limit as the
  work bound and the two data caps unchanged.

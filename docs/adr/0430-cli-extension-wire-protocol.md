# ADR-0430: The extension wire protocol v1 (LABS-470)

Status: Proposed
Claim: d31cc31e20106ac2143e29241da25ed21a9478e6

Later references: **superseded by [ADR-0447](0447-extension-single-wire.md)**
(2026-10-29). The mount speaks MCP stdio only: the records, the `hello`
handshake, the manifest `wire` field and the v1 child kit are retired.
The caps (§4), the commit-intent vocabulary (§7), the request envelope
(§6) and the exit classification (§5) survive in substance under the MCP
mount. ADR-0598 narrows §4's total stream cap to `whole-import`
sessions; a `per-batch` session keeps the per-call caps and the store's
live-graph limit.

## Context

ADR-0429 (minted beside this one) fixes where a command-kind extension
lives, how the host discovers it, and how it mounts as
`cypherlite run <name> [flags]`. This ADR fixes what the hosted child
process says while it runs: the wire v1 records, the protocol
versioning, the host-side resource caps, the exit-code mapping, and the
rule everything else follows — the child never gets a store path.

The core decision is LABS-466's: **the host owns the `Database`.**
Everything the `lab` import lifecycle already owns stays owned — the
`:memory:` default with interactive REPL handoff (ADR-0286), WAL and
memory limits, resource caps (ADR-0377), the fresh-store check
(ADR-0394), typed error rendering and output sanitization, telemetry,
exit codes — and the child is a pure source walker. The design maps 1:1
onto the existing importer decomposition: `lab/write.rs` separates
`RowSource` (generation) from `run_phases` (execution); the wire is a
`RowSource` over a pipe.

The host builds on the P0 seam — the static descriptor registry in
`surfaces/cypherlite-cli/src/lab_cmd.rs`, merged as #502 (LABS-467) —
whose descriptor shape the manifest-loaded `run` actions join
(ADR-0429), and it reuses `run_import`'s lifecycle end to end (§7).
Spawning a child and decoding a capped stdout stream is established
in-tree behavior, not a new risk class: `lab git-to-graph` shells out to
`git log` and reads its stdout through the crate's `read_capped` guard
under ADR-0377's 128 MiB bound. The deciding tickets are LABS-470 (this
phase) and LABS-552 (the mount and surface the host serves).

## Decision

### 1. The transport and the six records

The child writes NDJSON to stdout — one JSON object per line — and reads
one request envelope from stdin (§6). Wire v1 has six record kinds,
decoded as `serde` tagged enums, one variant per kind:

```text
{"hello":{"protocol":1,"name":"…","version":"…"}}
{"batch":{"statement":"UNWIND $rows AS row MERGE …","commit":"per-batch"}}
{"rows":[{…},…]}
{"index":"CREATE FULLTEXT INDEX …"}
{"summary":"imported …","starters":["MATCH …"]}
{"error":"…"}
```

- `hello` — the child's identity echo: the protocol version it speaks,
  its name and version. It must be the first record.
- `batch` — one write phase: a parameterized statement and its commit
  intent, framing the `rows` that follow.
- `rows` — the parameter rows of the preceding `batch`, one object
  each, at most the host row cap per record (§4).
- `index` — a batch-phase DDL record, executed at the import's
  index-creation point.
- `summary` — the child's one-line result, plus optional starter queries
  for the REPL handoff.
- `error` — the child's own failure report, the alternative to a
  non-zero exit.

### 2. Strict decode, fail-closed

An unknown record kind, an unknown field, or a malformed line is a loud
protocol error — never a skipped record, never a silent fallback (the
code-shape rules). `hello` must arrive first, and its `protocol` must
equal the manifest's `protocol` value; a mismatch is a protocol error
naming both versions, not a negotiation.

### 3. Per-mount-point protocol versioning

`protocol` in the manifest (ADR-0429 §4) is the wire's version at that
mount point. The wire never negotiates: the manifest declares, the host
enforces. Later strata (`callables` P2, LABS-468; `starter` P3,
LABS-533; `engine-plugin` P4, LABS-472) carry their own protocol
versions at their own mount points, so their evolution cannot break
`command` hosts. A `command` protocol bump is a new number, fail-closed
on old hosts.

### 4. Host-side caps, ADR-0377 semantics

The host caps what an untrusted child can make it hold, with numeric
bounds enforced host-side, each a named constant, each refusal a typed
error variant with pinned text a test asserts (ADR-0377's error-text
contract):

- a per-record byte cap — the child's stdout is read through the
  existing `read_cap` bounded-read mechanism (`read_capped`), so a
  hostile or growing stream cannot allocate past the cap before the
  refusal;
- rows per `rows` record (the host row cap);
- a statement cap — the count of `batch`/`index` records;
- a total stream cap across the whole run.

Every cap refuses before the store write it would have fed; none
truncates, drops a record, or continues with a default. On the first
refusal the child is killed and reaped, exactly ADR-0377's behavior
toward an oversized `git log`.

### 5. The error mapping: protocol exit 1, manifest/flag exit 2

- **Runtime failures — exit 1**, one `Error:` line: a protocol violation
  (unknown record, unknown field, malformed line, `hello` mismatch, a
  cap exceeded), a killed child, a non-zero child exit, a store error
  during a batch. These match ADR-0377's runtime refusals: the work was
  invited and failed.
- **Usage errors — exit 2**, the synopsis/usage shape (ADR-0069): a
  manifest problem (unknown kind, unknown field, bad flag kind, a name
  collision, an escaping executable path — ADR-0429's load refusals) or
  a flag problem (unknown flag, a value that does not parse as its
  kind). These match the parse-time refusals: the invocation was
  malformed before anything ran.

A child's `error` record and summary render through the host's
`print_error`-grade sanitization — a child can never forge a second
line, ANSI, or control characters into the host's output.

### 6. The child never gets a store path

The child receives exactly one parsed JSON envelope on stdin — its
resolved flag values and positional from the manifest flag table —
never raw argv, and never the `--database` value, a store path, or any
other host flag. All `--database` handling is host-side (ADR-0383), so
the child cannot even see the flag. Every store write flows through the
host-owned `Database`; the child can neither address nor name the store
it feeds. This is the rule the trust model (ADR-0429 §7) and the caps
(§4) both lean on: the child's only power is the rows it emits.

### 7. Host execution

Batches execute against the host-owned `Database` through the existing
`lab/write.rs` phase-runner shape: `RowSource` framing (rows stream
batch by batch, never copied), `CommitPolicy` from the manifest
(`whole-import` ADR-0157 / `per-batch` ADR-0392), reusing
`run_import`'s lifecycle — the `:memory:` default, the optional
interactive handoff (`run_lab_interactive`, ADR-0286), the
`final_checkpoint` — with the summary printed through sanitization and
the starters offered to the REPL.

## Alternatives considered

- **dlopen C-ABI in-process plugins (no wire at all).** Rejected: the
  in-process route needs ABI versioning and a platform matrix, and it
  puts untrusted code inside the host process, where no cap, kill, or
  sanitization boundary exists; command-kind extensions never need
  engine vocabulary, so the ABI buys nothing. The wire keeps the child
  outside the process and the host the sole store writer.
- **WASM as the isolation boundary.** Rejected: the dependency and
  author-toolchain cost ADR-0429 records, and command-kind actions need
  fs/git access whose capability grants re-open the hole WASM was meant
  to close; process isolation reaches the same boundary with std pipes.
- **exec-full-command, git-style (`cypherlite-x-foo`).** Rejected: with
  no wire, the child owns the store — the host could not enforce caps,
  the fresh-store check (ADR-0394), commit policies, the REPL handoff
  (ADR-0286), or uniform summary/telemetry/checkpoint behavior; every
  helper would re-implement the lifecycle the lab importers already
  share.
- **Direct top-level mounting of installed actions.** Rejected with
  ADR-0429: it is a grammar-level alternative, but it would also take
  the host out of the execution path the wire pins — the mount and the
  wire are one design.
- **Raw argv passthrough to the child.** Rejected: the child would
  re-implement flag parsing and every convention (help, did-you-mean,
  exit-2 usage errors, `--database`), and the host could not enforce
  §6 — a passthrough child sees the store path, which is the one thing
  it must not.
- **A single global protocol version.** Rejected: every stratum would
  move in lockstep; per-mount-point versioning lets `command` stay
  stable while `callables` (P2) and the later kinds evolve.
- **A framed binary protocol.** Rejected: NDJSON is printable and
  debuggable line by line, language-agnostic by construction, and
  `serde_json` is in-tree; a binary frame buys neither — the caps, not
  the framing, bound the resource risk.

## Consequences

- Fixture extensions run in CI: a tiny Rust fixture and a POSIX `sh`
  fixture (proving any language that can print NDJSON can extend the
  CLI; the sh one gated to unix lanes), plus the spawn/isolate/assert
  harness, malformed-stream tests, and cap tests with pinned text.
- Every fail-closed path has a pinned authored error text (LABS-470
  acceptance): protocol violation, killed child, cap exceeded, `hello`
  mismatch — tests assert the exact wording.
- A new record kind, a renamed field, or a changed record shape is a
  `protocol` bump at the mount point; old hosts fail closed on the
  mismatch, naming both versions. Unknown records are errors, not gaps:
  there is no skip list.
- The host trusts nothing the child says beyond what §2 and §4 bound:
  `hello`'s identity is echoed in errors and telemetry, never a grant.
- No new dependency: `serde_json` and std pipes (AGENTS.md R6 not
  triggered).
- An Oplane threat model runs on the implementation diff before merge
  (subprocess spawn + untrusted stream parsing), per LABS-466's rules.

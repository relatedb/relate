# Changelog

All notable changes to Relate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `UNWIND` of a list comprehension (`[x IN range(…) WHERE … | …]`) streams
  its elements: each element is bound, filtered and projected as the
  consumer pulls it, so the projected list is never built and an
  aggregating query's memory no longer scales with the list length
  (ADR-0555, REL8-1372). A pattern-comprehension source still materializes
  — the eval walk is not resumable; tracked on the same ticket.

### Fixed

- List and map literals are charged to the per-allocation expression
  budget: a query-sized literal (bounded by the query-text cap, so up to
  hundreds of MB of allocation) now refuses under a small expression
  memory limit instead of allocating silently.
- `randomUUID()` no longer panics in the WASM build: the UUID's timestamp
  now comes from the same JavaScript-backed clock as `datetime()` and
  `timestamp()` instead of Rust's unimplemented `std::time` on
  `wasm32-unknown-unknown` (REL8-2001).

### Notes

- Release tooling: the Windows signing leg preflights its DigiCert
  credentials with `smctl healthcheck` before signing and classifies the
  report (`scripts/check-smctl-healthcheck.sh`), so a failed client
  certificate surfaces as the secret to fix instead of DigiCert's bare
  sign-time 403; the echoed report redacts the API-key and
  client-certificate-password lines.
- Release tooling fix: the x86-64 baseline ISA gate accepts the pinned
  build container's note-less baseline link under an explicit
  `--pinned-toolchain` vouch — the container's rustc+gcc link emits no
  "x86 ISA needed" ELF property note at baseline, so the gate's
  note-must-exist arm had refused every release build — and the lane also
  pins `-C target-cpu=x86-64` explicitly. A note listing
  x86-64-v2/v3/v4 still fails the release.

## [1.3.0] — 2026-11-25

### Added

- The Windows release binary (`relate-windows-amd64.exe`) is signed with an
  Authenticode signature carrying an RFC 3161 timestamp at release time, so
  SmartScreen and enterprise download policies see a signed publisher
  instead of an unknown one (ADR-0594).
- The agent plan document (`relate.plan/v1`: the MCP `plan` and the FFI
  result plan) gains two always-present node fields, `fused_into` and
  `fused_metrics`, for the display-only steps an index-seek-started `MATCH`
  now breaks into in `EXPLAIN` and `PROFILE`. Both are `null` on a real
  operator; `fused_metrics` carries `{seek_ms, node_reads}` on a `PROFILE`
  seek step only.
- `SHOW CURRENT GRAPH TYPE AS CONSOLIDATED` and `AS FLATTENED` (CIP-275,
  ADR-0638), behind the new `RELATE_PREVIEW=graph-type-show-variants`
  opt-in. `AS CONSOLIDATED` shows independent property-existence and
  property-type constraints as the element types they would be; `AS
  FLATTENED` shows each node element type with its inherited labels,
  properties, defaults and undesignated constraints inlined. Both are
  display-only, work with `AS GRAPH`, `YIELD` and `WHERE`, and leave the
  stored graph type untouched; without the opt-in they fail with the
  authored preview error.
- Preview: `PATH [n, r, m]` builds a path value from an alternating
  node, relationship, node list (`RELATE_PREVIEW=path-construction`), and `||`
  over two paths that share a boundary node concatenates them into one path
  (`RELATE_PREVIEW=path-concatenation`). With a flag off, existing statements
  behave as before: the constructor fails with the authored preview error, and
  `||` on paths keeps its operand type error (ADR-0637).
- Preview: with `RELATE_PREVIEW=path-construction`, a one-element `PATH [n]`
  builds a zero-length path when the statement uses no variable named `path`
  elsewhere; with one, it stays a subscript of that variable. With the flag
  off nothing changes (ADR-0637).
- A markdown vault can carry its schema in version control: a committed
  `ontology.yaml` at the vault root declares key properties, the
  relationship vocabulary, the ontology document (`db.ontology.get`
  serves it; `set`/`drop` refuse while it is present — edit the file),
  the graph type, and the indexes (the spec's native
  `extensions.indexes` envelopes, validated and defaulted the way a
  `CREATE INDEX` would be). Every machine and fresh clone opens with
  the declared schema installed — fulltext and vector search live out
  of the box; a schema change is a PR (ADR-0649).
- Rust API: `Database::checkpoint_if_pending()` — the close-path
  checkpoint shape; explicit `checkpoint()` is unchanged.
- The .NET client is published to nuget.org as `Relate.Net`: every release builds
  one package carrying all five supported RIDs, and the release pipeline pushes it
  with a short-lived OIDC-issued key instead of a stored credential (ADR-0652).
  Install it with `dotnet add package Relate.Net`.
- The executor's per-statement memo budget is now an open-time setting:
  `OpenOptions::expression_memo_budget` or the
  `RELATE_EXPRESSION_MEMO_BUDGET` environment variable, in whole MiB
  (default 16, unchanged). `0` disables the budgeted memo tables for a
  no-memo control run; a negative or unparsable value fails the open
  naming the setting. Results never depend on the budget.
- `PROFILE` now reports what the expression memos did. A statement that used
  a memo gets a `Memo` column in the plan table (`hits/probes`, suffixed `*`
  when a memo gave up at its hit-rate review), and the plan carries the
  counters on the operator that ran them: `MemoProbes`, `MemoHits`,
  `MemoInserts`, `MemoRefused`, `MemoBytes` and `MemoDisabledCount` in the
  Bolt and HTTP plan args, and `actual.memo` in the `relate.plan/v1`
  document (C ABI, Python, MCP, CLI JSON; `ClauseMetrics::memo` in Rust,
  `PlanActual.memo` in Java). The datetime constructor memo's counters sit
  on the plan root beside a `kinds` list. The group is present only when a
  memo was probed, and `MemoBytes` is never part of `MemoryBytes`.
- The CLI installs and upgrades through six package managers: Homebrew
  (`brew install relatedb/tap/relate`), APT (`apt-get install relate`,
  amd64 and arm64 from the signed repository at relatedb.ai/apt), RPM
  (`dnf install relate` from the signed repository at relatedb.ai/rpm),
  winget, Chocolatey and Scoop — every channel's package built from the
  release's own signed, checksummed artifacts. On a package-managed
  install, `relate upgrade` names the package manager's own upgrade
  command and exits non-zero instead of replacing the binary, and the
  curl-pipe installers refuse to overwrite a package-managed install.
  The release gate now derives the Linux binary's glibc floor from its
  ELF symbol versions (refusing anything above the documented 2.35 cap)
  and pins the x86-64-baseline ISA note, so a .deb/.rpm install always
  starts on the declared-supported distributions.
- Every completed checkpoint reports its duration and its trigger on
  stderr.
- `OpenOptions::load_threads` and `RELATE_LOAD_THREADS` cap how many
  threads opening a store may use (default: the machine's available
  parallelism; zero or invalid values fail the open). A new
  `Database::open_peak_transient_bytes` reports the most temporary memory
  the open's snapshot decode and adjacency rebuild held at once, and a
  follower's gap reload refreshes it.
- `relate::params_from_map_with` is the per-entry parameter walk
  `params_from_map` now shares: the `parameter <name>:` error prefix, the
  value-depth check and the collect, with the value decoder supplied by the
  dialect (the HTTP typed fold passes its own).
- `CALL db.index.fulltext.awaitEventuallyConsistentIndexRefresh()` now
  executes as a void no-op instead of failing `ProcedureNotFound` — the
  engine applies committed index updates synchronously, so the wait is
  trivially complete, exactly like `db.awaitIndexes()`
  (ADR-0680; the 2026-11-24 corpus sync put the call in the full-text
  acceptance suite's setup)

### Changed

- `LOAD CSV` feeding an aggregation (or an ordered window) streams its
  records: the rows are handed to the consumer as they are read and never
  collected, so peak memory stops scaling with the file's record count and
  a part-sized file loads under the default per-allocation cap where the
  materializing form refused (ADR-0555, LABS-1845).
- The Java client's Maven Central deploy now rides the release's
  `registries` opt-in like npm, PyPI and crates.io, and a deployment
  publishes itself once Central validates it — no manual
  publish-registries dispatch afterwards, no Portal button. An
  unchecked dispatch (the default) publishes nothing anywhere, the
  Java client included, where the v1.2.0 cut attempted the jars from
  every release and failed importing an empty signing key. The release's
  chained call now forwards the four repository-level Central
  credentials a called workflow cannot see on its own, and the
  publishing plugin moves 0.7.0 → 0.12.0,
  whose deployment model reads the `warnings` field the Portal API
  newly returns — 0.7.0 crashed polling a validation the upload had
  already landed. A direct `publish-registries.yml` dispatch against
  the tag remains the recovery arm.
- `relate upgrade` no longer installs the agent skill into every detected
  agent when only some have it: the post-upgrade refresh updates only the
  agents whose bundle is installed and leaves the rest untouched (no
  prompt, no install, no hint, `--yes` included). The skill reaches new
  agents only when nothing is installed anywhere (the first-install
  prompt, or `--yes`) or through an explicit `relate skill install`
  (ADR-0632, REL8-1875).
- The Docker image's HTTP and Bolt startup log lines now match the Neo4j
  server's format — `Bolt enabled on 0.0.0.0:7687.` and `HTTP enabled on
  0.0.0.0:7474.` — so log-waiting tooling built for the Neo4j image
  (testcontainers and friends) treats the image as a drop-in replacement;
  the auth banner lines and the MCP startup notice are unchanged.
- A Ctrl-C during `serve`'s startup window is now handled cleanly — the
  shutdown watcher is armed before any port binds, so an early SIGINT
  shuts the server down with its final checkpoint instead of killing the
  process mid-startup.
- A `CALL` feeding an aggregation or an ordered window holds constant
  memory however wide its input-rows × procedure-rows cross product is:
  the composition streams into folding consumers (ADR-0555), where it
  previously materialized every composed row. A `CALL` whose consumer
  cannot fold now bills its composed rows against the expression memory
  cap instead of silently ballooning (ADR-0463).
- Three classification corners align with the facade's compile-time
  validator: a user-registered procedure that declares no yields is void
  at execution too; a NULL fulltext query on `db.index.fulltext.query*`
  answers no rows (the policy the SEARCH clause and the vector CALL
  already answer) instead of erroring; a YIELD of the non-declared entity
  spelling on a vector call reads Null instead of a phantom alias, and a
  per-row builtin's produced rows are no longer re-filtered through the
  stored-set argument predicate.
- A `LOAD CSV` seeding a per-row-independent tail (`MATCH … CREATE …`) runs
  its tail batch-major: the records are pulled in bounded batches and the
  remaining clauses run once per batch, so the driving rows never exceed one
  batch instead of the whole record stream. A bulk load of this shape now
  answers under the default per-allocation cap where it refused before (and
  peaked at 13.1 GiB for a ~490 MB store with the cap lifted); the tail's own
  containers stay charged as before, and the statement remains one
  transaction — an oversized commit is the explicit `CALL (a, b) { … } IN
  TRANSACTIONS` form the WAL-payload refusal names (ADR-0641, REL8-1845).
- Warm plan-cache hits no longer re-run the front end per statement: repeated statements with fresh literals see a 9× smaller planning window (point read 6.5 µs → 0.7 µs p50 in-process). A value-dependent compile verdict still surfaces as the compile's own error — a cached plan that fails with a type error re-runs the statement once uncached (REL8-1120, ADR-0651).
- The per-commit WAL record cap — a hard 64 MiB constant since ADR-0086 —
  is now 256 MiB by default and configurable at open:
  `OpenOptions::wal_payload_limit` for embedders, the
  `RELATE_WAL_PAYLOAD_LIMIT` environment variable (same `SIZE` grammar as
  every memory knob) for the CLI and served surfaces. A zero, negative,
  unparsable, or over-4-GiB value fails the open loudly naming the
  source; unset keeps the 256 MiB default; the refusal message names the
  configured value and keeps the smaller-transaction
  (`CALL { … } IN TRANSACTIONS`) remedy. One big commit in one statement
  no longer requires the rewrite — for example
  `RELATE_WAL_PAYLOAD_LIMIT=512mb relate --database ./mydata`
  (ADR-0650).
- Markdown vaults at scale open and close dramatically faster: a clean
  session (open + exit) no longer re-walks and re-renders every vault
  file at exit — a checkpoint with nothing to publish writes nothing
  (ADR-0646) — and both the directory walk and the file read run
  concurrently. On a 27k-file vault: unchanged session 4.1s → ~1.25s
  warm, cache-evicted open 7s → 1.8s, fresh-clone bootstrap ~1.65s; a
  100k-file vault now works at all (the sidecar's own 256 MiB bound,
  ADR-0648) and bootstraps in ~5.7s.
- `relate --read-only` opens skip the exit checkpoint by design; the
  first open of a fresh clone needs `--snapshot-format markdown` once
  (then plain opens work).
- A `CALL { … }` subquery feeding an aggregation (or a clause that may
  take the ordered window) streams its outer-rows × body-rows product
  instead of materializing it: the consumer pulls one merged row at a
  time, so peak memory is bounded by one outer row's body result rather
  than the input-row count times it (ADR-0555). The materializing form —
  and the `WHEN`/`ELSE` conditional's output buckets, which have no
  parkable consumer by grammar — now charge what they retain to the
  running total (ADR-0463), where the pre-conversion forms held their
  whole output unbilled (REL8-1845 items 3, 4 and the chargeable half
  of 5).
- **Memory:** the adjacency delta tier is a bounded transient buffer — delta hop vectors reserve at a 256-hop quantum above the power-of-two band (supernode-scale updates between checkpoints hold up to 37.5% less delta heap), and settled 4–7-hop chunks seal into the compact arena (spread workloads: -64% settled delta bytes). Write latency and read paths unchanged.
- Warm point-lookup statements (an index seek or label scan with a simple
  return or count/sum/avg fold, and their one-hop forms) run on the
  calling thread from their second execution: the per-connection
  query-worker handoff — the largest fixed cost left on a warm statement —
  no longer applies to them (ADR-0663).
- Every clause takes a bounded batch stream: `UNWIND`, `MATCH`, `CALL` and
  `LOAD CSV` no longer materialize their output for the consumer, so
  statements that previously refused against the workspace cap now run, and
  `PROFILE` reports a streamed `UNWIND` clause's own row count instead of 0
  (ADR-0660).
- Sustained writes and bulk imports no longer stall behind a WAL-rotation
  checkpoint. A checkpoint now freezes its cut — WAL pin, snapshot pin,
  schema and dirty-id image — and dumps beside in-flight commits instead
  of excluding them (ADR-0643); a commit group waits for a checkpoint
  only when the closed-segment backlog exceeds its bound (ADR-0644).
- A Bolt client writing a CREATE-only stream commits once per batch
  instead of once per statement, so write throughput is no longer
  bounded by per-statement commit cost (ADR-0642).
- A `tools/call` whose `arguments` is not an object is refused with the one
  wording `"arguments" must be an object` on both the product MCP server and
  the extension mount (including `report_output`), and `null` is now absent
  for every optional member: `{"parameters": {"rows": null}}` is no longer a
  shape error.
- `SHOW TRANSACTIONS YIELD *` now carries all forty live columns in alphabetical order, so `YIELD * RETURN elapsedTime` and `YIELD * WHERE ...` resolve every live name. Columns the embedded engine has no state for (timings, lock/page/heap counters, query id, planner/runtime, ...) are Null; `parameters`, `metaData` and `currentQueryStatus` are filled. A bare `SHOW TRANSACTIONS` still renders the seven defaults.
- A UUID operand beside a string no longer concatenates through `+`:
  `'a' + UUID()` and `UUID() + 'a'` now fail with a type mismatch, matching
  Neo4j, where they previously returned the UUID's text glued into a string.
  A UUID is still a property value, a list element and a `toString()`
  argument; `UUID() + []` and `[] + UUID()` still build a list.
- The per-statement nested-evaluation budget is 1 000 000 passes (was
  100 000, ADR-0670): million-row loads through nested `CALL` bodies no
  longer refuse at the 100 001st pass.
- The `SEARCH` clause's complex patterns and entity-constrained window join
  the default dialect (ADR-0678): a searching `MATCH` keeps every pattern
  shape the general `MATCH` machinery accepts — the
  `RELATE_PREVIEW=complex-search-patterns` opt-in and the pattern-shape
  rejections are retired with the reference implementation's own
  graduation — and `SKIP`/`LIMIT` page over the entities the whole MATCH
  statement keeps (its labels, property predicates, outer `WHERE` and
  pre-bound bindings restrict candidates before the window; the `LIMIT`
  counts survivors, not raw hits), for both index kinds and every pattern
  shape. The inner `WHERE` now serves full-text searches too, and its
  predicate family grows the entity match filter (`n IN <expr>`) and its
  id-spelled twin (`id(n) IN <expr>`), with a correlated `COLLECT`
  subquery allowed on the right-hand side. An index created inside the
  open transaction fails a search with the transient `IndexStillPopulating`
  rejection until the transaction ends.
- The `BINDINGS` spellings of the match modes parse and run (ADR-0679):
  `MATCH REPEATABLE ELEMENT BINDINGS` / `MATCH DIFFERENT RELATIONSHIP
  BINDINGS` (also after the plural forms) map onto their element twins'
  relationship-uniqueness semantics.
- a SEARCH over an eventually-consistent full-text index now fails with
  `EventuallyConsistentIndexNotSupported` instead of silently scanning;
  the `db.index.fulltext.query*` procedures remain the supported surface
  for such indexes (ADR-0680)
- the full-text corpus memo keeps one corpus per distinct index and
  analyzer (up to sixteen, least-recently-used) instead of a single live
  corpus, charging every retained corpus to the statement's memory meter
  and the query's shared live-bytes counter under the kernel memos' own
  ceiling, releasing on eviction and drop — a `COMBINE` alternating
  sources stops rebuilding each corpus per row, an `UNWIND` over index
  names into the procedures can never pin unbounded uncharged memory,
  and the retained corpus bytes are counted into the statement's joint
  memo budget and folded into PROFILE's memo group at the clause's end
  (ADR-0680)

### Fixed

- `string.regexReplace` now expands a `$n` reference to an existing
  capture group that did not participate in the match as an empty
  string, matching Neo4j: `string.regexReplace('abc', '(a)|(x)', '$2')`
  returns `"bc"` where it used to return `"nullbc"`. A group number past
  the pattern's group count is still rejected.
- Number literals glued to FLOAT32 suffix runs (`1.0F32`, `1.0f32`,
  `1.0e0F32`, `.4d32`) now fail as one `InvalidNumberLiteral` malformed
  literal (Neo4j's 42I07 shape) instead of a confusing "unexpected
  token" parse error, and a combining mark glued to any number is the
  same malformed run. The bare suffix spellings (`1.0f`, `.4d`, `1e4D`)
  and the `FLOAT32` vector type expression are unchanged.
- `duration({quarters: …})` now rejects the `quarters` map field with
  `Unknown field: quarters` (Neo4j's 22G0I) instead of summing it into
  the months lane — the integral, float and mixed years/months
  spellings all reject. The `d.quarters` accessor, the `q`/`Q`
  formatting-pattern letter and every other duration map field are
  unchanged.
- The vector index's `vector.quantization.type` config now accepts only
  `SCALAR`, `BINARY` and `NONE` (case-insensitive) and rejects every
  other spelling — `INT8` included — with an error listing the
  supported values. The legacy `vector.quantization.enabled` spelling
  now maps onto the type (`true` → SCALAR, `false` → NONE), and the
  default search-expansion factor follows the type: 1.0 for NONE, 1.5
  for SCALAR, 3.0 for BINARY. Indexes stored by older builds with the
  once-accepted `INT8` spelling keep working: the stored read maps it
  onto `SCALAR`, while `CREATE` rejects the spelling going forward.
- A list or string slice whose dynamically-evaluated FLOAT bound exceeds
  the int32 range now saturates the bound into that range instead of
  raising `NumberOutOfRange` — `[1,2,3][head([1.0/0.0, 'x'])..2]`
  returns `[]`, matching Neo4j (ADR-0631, superseding ADR-0175's
  dynamic-bound rule). Float-literal bounds still fail before execution,
  and `substring()` / `left()` / `right()` and list indexing keep the
  rejection.
- The `relate upgrade --yes` help line states what the flag does — install
  the skill when none is installed, without asking, while installed
  bundles update regardless — instead of the ambiguous
  "Auto-accept the post-upgrade skill refresh".
- Whitespace between the relationship dash and the arrowhead now parses —
  `MATCH (a) - [:R*2] - > (b)`, the spaced left head `< - [:R] - (b)` and
  the bracketless `-- >` all plan and run with identical plans to the
  glued forms, instead of failing with `expected node pattern after
  relationship`. A single bare dash with an arrowhead (`(a) - > (b)`,
  `(a)->(b)`) stays rejected as before, and comparisons like
  `RETURN (a) < -1` are unaffected.
- An unaliased map projection in `WITH` (`WITH m{.*}`) now plans and runs,
  with the output column named after the base variable — the same implicit
  name `RETURN` already gave it — instead of demanding `AS`
  (`NoExpressionAlias`). The variable is shadowed by the projected map, the
  aliased form is unchanged, and `WITH m{.*}, m` still rejects as a
  duplicate column name.
- `CALL p() YIELD *` on a procedure body with no terminal RETURN (a FINISH
  body) now reports its own rejection — "a procedure body with no terminal
  RETURN cannot be yielded with `*`" — instead of the shared
  must-name-every-item message.
- `NOT(b)` and `(b)` over a boolean or scalar alias in WHERE now validate
  and execute — the parens read as grouping, matching Neo4j — instead of
  failing with `UndefinedVariable` (the alias invisible to its own WITH's
  WHERE) or the single-node pattern rejection. The same spellings over
  node and path variables keep the single-node pattern-predicate rejection.
- A grouping-key variable read through a scalar function call beside an
  aggregation now validates and runs — `WITH count(n) AS c MATCH (m) RETURN
  toFloat(c) / count(m) AS coverage, c` executes with `c` as the group key
  instead of failing with `AmbiguousAggregationExpression`. Calls over
  non-grouped variables and aggregates hidden inside list literals keep
  rejecting.
- `ORDER BY` now reads the base variable of a bare projection item under
  `RETURN DISTINCT` — `RETURN DISTINCT m AS node ORDER BY m.p1 DESC` plans
  and runs (dedup on the projected value, ordering through the base
  variable) instead of failing with `UndefinedVariable`. Variables only
  derived from (`m{.p1} AS node`) or absent from the projection stay
  ORDER-BY-inaccessible under `DISTINCT`, matching Neo4j; and the
  unaliased spelling over a path/scalar/list-bound variable now rejects
  uniformly instead of passing by insert-order accident.
- `relate pull` (and `push`, the same `NEO4J_URI` resolution) now reaches
  a self-hosted server through a TLS Bolt `NEO4J_URI`: a
  `neo4j+s://host:7687` URI (also `neo4j+ssc`, `bolt+s`, `bolt+ssc`; the
  port may be omitted) resolves through ordered Query API origin
  candidates — `https://host:7473` then `https://host` (443), a
  plaintext Bolt URI `http://host:7474` then `http://host` (80) — the
  server's own port first, the generic web port as the proxied fallback,
  the first origin answering at the HTTP layer winning (ADR-0633). A
  failure that does not prove the request was undelivered (a timeout, a
  reset connection, a TLS or protocol error) is never retried on
  another origin, so a delivered request is never re-POSTed.
  Previously the TLS spellings built `https://host` (port 443 only,
  where a self-hosted server has no Query API) and a `+s` URI with an
  explicit non-default bolt port reused that port verbatim as the Query
  API port. Malformed ports (`neo4j://host:abc`, previously silently
  remapped to :7474) and scheme-less or unknown-scheme Aura-suffixed
  URIs now fail loudly with fixed text instead of being accepted
  silently; an `http://` URI to an Aura host is used as-is instead of
  being silently upgraded to `https`. Valid Aura and plain-Bolt URIs
  are unchanged.
- Wire envelope row objects now serialize with keys in the query's column
  order instead of alphabetically sorted (ADR-0634): every JSON surface —
  the WASM `execute` envelope, the C ABI envelope, the CLI
  `--mode json`/`jsonl`/`envelope` modes, MCP tool results — keys rows as
  the statement's `columns` array lists them, and every wire object
  follows its documented member order. `columns` remains the
  authoritative column order.
- A FULLTEXT index's configured `fulltext.analyzer` now analyzes its stored
  tokens and the query the procedures and a `SEARCH` without `WITH
  ANALYZER` run: an `english` index drops English stop words and
  Porter-stems both sides, so `refunds` finds `refund` and `charge` finds
  `charged` — previously the option was echoed but its stemming never ran.
  `WITH ANALYZER null` is no override (the query follows the index's
  analyzer) rather than a silent switch to the standard analyzer. Default
  (`standard-no-stop-words`) and `standard-folding` indexes are unchanged
  (ADR-0636).
- A registered call failing its own signature at runtime — `duration_between('abc','def')`,
  `duration(123, "s 'seconds'")`, `date(123)`, `date(duration('P1D'))`, the truncate
  spellings and a non-string timezone — now reports Neo4j's `ProcedureCallFailed`
  class (`ProcedureError`, GQLSTATUS 42001) instead of a TypeError or SyntaxError,
  matching Neo4j 2026.10.1 (ADR-0639).
- `PROFILE` runs the streaming path it reports: a `MATCH` feeding later
  clauses streams under `PROFILE` too instead of materializing its whole
  output, so the statement completes where the profiled form used to
  refuse with the per-allocation cap (a 1.66M-row cross product now
  profiles under the default limit), and every consumed clause reports
  its own rows, time and reads — the head clause's record states its own
  work, never the run's (REL8-1904).
- A predicate function's own variable now shadows a same-named outer
  variable in the compile-time kind checks: `ALL(p IN nodes(p) WHERE
  size(labels(p)) > 0)` over a path `p` no longer fails with
  `labels() requires a node, got PATH` — the bound element is the
  runtime's to check, and the unshadowed spellings keep their rejection.
- A subquery expression whose body pattern declares a name the enclosing
  projection dropped no longer fails to plan: `WITH DISTINCT p0 WHERE
  COUNT { MATCH (n0) … } >= 0` rejected the body's own `MATCH (n0)` as a
  read of the projected-away outer `n0`. The body's fresh pattern
  declarations are skipped; a name the projection still exposes stays a
  checked correlation, and direct reads of dropped names keep their
  rejection.
- A mixed aggregating projection whose non-aggregation part is arithmetic
  over a projected grouping key now plans and runs: `MATCH (n) WITH n.x + 1
  AS nx RETURN nx, nx - 1 + count(nx)` no longer fails with
  `AmbiguousAggregationExpression`. The front-end ambiguity check judges
  the variables such an expression reads, not the shape around them, and
  the planner's grouping-key leaf discipline decides the rest — a mixed
  part reading a variable no grouping key covers keeps its rejection.
- Under a configured logical graph size limit, concurrent writes no longer fail intermittently with `capacity exceeded`: a commit group now charges a dense id-array growth reservation once per group instead of once per group-mate crossing the same growth boundary. Single-writer admission is unchanged, and writes that genuinely exceed the headroom are still refused.
- A vault copied through a normalizing tool (macOS `tar`, zip, some
  sync tools) opens and heals to the canonical filename spelling:
  Unicode normalization no longer makes the vault unopenable
  (ADR-0647).
- Deleting, inside one explicit transaction, an entity an earlier
  statement of that transaction created no longer poisons the connection
  with `internal error` (`update counters are non-negative`): the delete
  statement now counts `nodes_deleted` / `relationships_deleted` (and
  `properties_removed`) instead of driving the statement's create counter
  negative. Same-statement create+delete pairs still net to zero, and a
  `CALL { … } IN TRANSACTIONS` batch deleting a node an earlier batch of
  the same statement committed reports both the create and the delete.
- `db.nameFromElementId` over a well-formed element id that names an unknown
  database now fails with the `EntityNotFound` class (GQLSTATUS `42001`),
  as Neo4j does, instead of a semantic error; a malformed id keeps its
  unexpected-format error.
- `datetime()` and `localdatetime()` reject a space-separated date-time text
  (`'2024-01-02 03:04:05'`) as Neo4j does; only the `T` separator parses.
- `vector()` integer coordinates are truncated toward zero before the
  kind's width test, matching Neo4j: `vector([127.9], 1, INT8)` returns
  `[127]` instead of raising an overflow. An out-of-range coordinate
  still overflows, and INT64's upper bound stays inclusive at 2^63 (the
  value `i64::MAX as f64` rounds up to), which the saturating cast clamps
  to `i64::MAX` (REL8-1897).
- A column alias spelled with a `__` prefix (`WITH i AS `__x` RETURN *`) is
  no longer dropped by a star projection; the column and its rows now come
  back as they do on Neo4j.
- The markdown vault's key-choice refusals — a node missing its label's
  key property, a non-string key value, a key value with no representable
  characters — now name the remedy: declare `key: true` on the label's
  node type in the vault's committed `ontology.yaml` (or `ontology.json`)
  to pin a better key property, instead of stating only the requirement.
- A function call as the map expression of a comprehension whose WHERE
  filters by label now parses and runs —
  `[x IN xs WHERE x:L2 | substring(x.p1, 0, 1)]` executes like its
  property-access and projection siblings, instead of failing with
  `expected RightBracket, got "("`. Label disjunctions in a comprehension
  WHERE keep their reading (`x:A|B AND …` still commits on operators).
- A match-mode phrase after a path variable declaration —
  `MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b)` — now rejects with
  `InvalidPathModePosition` (the phrase is legal only directly after
  `MATCH`, matching Neo4j), instead of executing with relationship
  uniqueness silently lifted. The mode-first spelling
  (`MATCH REPEATABLE ELEMENTS p = …`) and the CIP-51 path modes
  (`p = TRAIL (…)`) are unchanged.
- A whole-map parameter inside a pattern an expression carries —
  `WHERE (a:L1 $props)-[:R]->(b)`, its `exists(…)` spelling, a pattern
  comprehension, a `shortestPath()` argument — now rejects with the
  parameter-use error the MATCH/MERGE clause spellings already had,
  instead of executing and filtering by the parameter's map. CREATE
  keeps the shorthand, and a literal map with a parameter value
  (`{t: $v}`) keeps executing.
- A single `|` after a label chain inside a comprehension WHERE now reads
  as the comprehension's map separator, matching Neo4j: `[x IN xs WHERE
  x:A|B]` fails with `UndefinedVariable: variable 'B' not defined`
  instead of silently returning the label disjunction's rows, and the
  operator tail (`x:A|B AND y]`) yields the same way. The disjunction
  reading survives when a later pipe in the same comprehension keeps it
  alive (`x:A|B | x.p1` is unchanged), as are the map-expression
  spellings that already worked.
- `date.transaction()`, `time.transaction()`, `localtime.transaction()`,
  `datetime.transaction()` and `localdatetime.transaction()` now return one
  instant for the whole explicit transaction, matching Neo4j, where they
  used to return the current statement's instant. `.statement()`, the
  default `date()`/`datetime()` constructors and `timestamp()` still read
  one instant per statement, `.realtime()` the wall clock per call, and an
  autocommit statement behaves as before.
- A composed `SHOW CURRENT USER` statement (for example `SHOW CURRENT USER YIELD user MATCH (n) RETURN n`) is now rejected off the system database with the same administration-command error as `SHOW DATABASES`; the standalone command still runs anywhere.
- A `point()` argument rejection now reports the GQLSTATUS Neo4j sends for its
  site (`22N20` dimension mismatch, `22N21` unknown srid or crs, `22N23`
  latitude out of range, `22N24` non-finite coordinate, `22G03` wrong type)
  beside the `Neo.ClientError.Statement.ArgumentError` code and the runtime
  stage, instead of the general `42000`.
- `duration()` with a fractional sub-second field beside a seconds field at
  or above 2^53 (for example `{seconds: 9223372036854775807, nanoseconds: 0.1}`)
  rounds the seconds as Neo4j does, so the rendered duration matches.
- A `FOREACH` body's `CREATE (n:L)-[:R]->(m)` now declares a fresh `n` that shadows an enclosing `n`, instead of relabelling the enclosing node.
- Opening a store no longer fails with `storage error: truncated WAL
  magic` when the newest WAL segment is a zero-length file: an
  interrupted rotate or store reset that left an empty segment now has
  its header completed on the next open, instead of leaving the store
  unopenable until manually deleted. Sole, closed and
  predecessor-corrupt segments still fail exactly as before, and a
  read-only open or live follower poll now refuses a mismatching short
  newest segment it previously skipped as frameless.
- A markdown vault's committed ontology now pins the schema it declares:
  `key: true` installs the enforced key constraint and labelled
  relationship endpoints install their endpoint-label constraints — both
  listed by `SHOW CONSTRAINTS` and enforced — and a write session
  checkpoints cleanly. Before, the first write checkpoint after adding the
  ontology failed with `invalid checkpoint enforced-label constraint`.
- A `DELETE` whose `MATCH` held an empty pattern it did not name (`MATCH (a:A), (b:B) DELETE a`
  with no `:B` node) deleted every node of the named label once that label crossed the
  10,000-target batching threshold; the row space is the cross product of every pattern, so an
  empty factor now yields no targets at any size. A `WITH` between the `MATCH` and the write
  that drops a matched variable (`MATCH (a:A), (b:B) WITH a DELETE a`) now declines batching
  entirely — the dropped factor still empties or scales the row space, which the batched path
  cannot see.
- `EXISTS { MATCH … MATCH … }` and `count(DISTINCT x) > 0` over two `MATCH` clauses answered
  false when only the same relationship could satisfy both clauses (relationship uniqueness
  was applied across the clauses); `EXISTS` now agrees with `COUNT`, and a `NOT EXISTS`
  feeding a `DETACH DELETE` no longer deletes the node.
- A `WHERE` comparison on a node variable repeated inside one path pattern (a cycle) dropped
  the other occurrence's labels and property map, returning rows no node satisfies and
  letting `SET` / `DELETE` act on them; every occurrence's constraints are now checked on
  every pattern engine.
- Markdown vault full-form wikilinks (`[[Label/<key>]]`) resolve at the
  label they name whenever a node carries that label as primary; a
  label also carried as an extra label by an earlier-sorted file no
  longer silently redirects the link into that file's label namespace —
  hand edits to frontmatter link lists (container rewires) survive a
  load/checkpoint cycle instead of being silently rewritten back
  (REL8-2039)
- Variant key spellings (case, Unicode normalization) in a full-form
  link resolve to the same node instead of stubbing a duplicate, a
  repeated link to a not-yet-existing target resolves to the first
  stub instead of creating a second node under the same key, and two
  bare links to one target are one relationship at load (set
  semantics); unresolved links materialize their stub under the label
  the wikilink names
- An ontology relationship `type` declared across several endpoint pairs
  (one type, several `relationships` entries — a shape the Ontology
  Graph Spec allows) no longer breaks `CALL db.ontology.apply(…)`
  (`duplicate identifying label`) or the vault-committed
  `ontology.yaml` open (where every declared pair was refused with a
  sibling entry's expected label): such a type now defines no graph
  type at all — its relationships stay writable under any labels, the
  type still seeds the vault's link vocabulary, and types declared once
  enforce exactly as before (ADR-0668, REL8-2042)
- Two node entries sharing one identifying label are refused at the
  ontology mapping with a named diagnostic (`one label, one node type`)
  instead of a late generic `duplicate identifying label` at apply; an
  entry restating an earlier entry's endpoint pair with a different
  declaration is likewise refused naming both entries
- `CALL { … } IN TRANSACTIONS OF n ROWS` no longer refuses bulk loads past
  100 000 rows: an admissible body (e.g. `CREATE`/`MERGE` per row, reads
  included — the body evaluates the batch as a unit, in pipeline order)
  now runs as one engine pass per batch — one nested-evaluation-budget
  tick per batch instead of one per row — so the per-commit WAL payload
  cap's suggested remedy works at any row count and batched bulk loads
  run markedly faster (ADR-0669).
- A bulk-batched auto-commit write no longer changes its answer with data
  size: `MATCH ()-[r]-()` writes each relationship once per orientation,
  and two relationship patterns of one `MATCH` clause keep relationship
  uniqueness, on the batched and the unbatched path alike.
- `toStringOrNull()` of a homogeneous list literal (`toStringOrNull([1, 2])`)
  now renders `'[1, 2]'` instead of failing during semantic analysis. Neo4j
  renders every list for this function, and the same value read from a
  property, parameter or variable already rendered. `toString()` keeps
  rejecting a homogeneous list literal at compile time — Neo4j's own
  static-type boundary — and a property or parameter list still renders for
  both functions.
- The HTTP Query API v2 emits Neo4j-shaped values: an entity's `elementId`
  is the ADR-0554 form `elementId()` returns (`4:rl-id:n5`), so a returned
  id can be sent back as a parameter; a point is its EWKT string
  (`SRID=7203;POINT (1.0 2.0)`); a non-finite float spells `Infinity`,
  `-Infinity` or `NaN` instead of `Inf`.
- Typed request parameters on the HTTP Query API keep their type: a
  non-finite `Float` binds a FLOAT (it bound the STRING `"Inf"`, so a
  comparison against it selected nothing) and a `Point` EWKT payload binds
  a POINT (it was rejected with a 400).
- The fulltext query syntax rejects the Lucene constructs the engine does
  not implement — grouping parentheses and field groups, ranges, regular
  expressions, the `+`/`-`/`!` clause modifiers, a quoted phrase of more
  than one term, a dangling or doubled operator, and an empty query — with
  a syntax error, where they previously returned a request-shaped wrong set
  (`(gamma OR one) AND beta` answered `one AND beta`, `alpha AND !beta`
  required `beta`). The forms `docs/reference/cypher-syntax.md` lists are
  unchanged, and a quoted term keeps working after a field prefix
  (`title:"alpha"`), which previously dropped the field.
- Chinese and Japanese text in a fulltext index is searchable by its
  words: the tokenizer emits one term per Han ideograph and per Hiragana
  character and keeps a Katakana run whole, and the `cjk` analyzer emits
  overlapping bigrams over those runs instead of sharing the standard
  tokenizer (ADR-0672).
- `db.index.fulltext.queryNodes` and `db.index.fulltext.queryRelationships`
  accept their documented optional `options` map: `skip` and `limit` page
  the ranked hits, and `analyzer` names the query-side analyzer exactly as
  `SEARCH ... WITH ANALYZER` does. An unknown key is an argument error.
- The string-literal escapes `\r`, `\b` and `\f` decode to their control
  characters, in plain strings and in interpolated literals, so `'\r'` is
  one character and a stored `'a\r\nb'` holds a carriage return; an unknown
  escape still keeps its two-character form.
- Four cross-clause reads walks and the subquery-expression stage loop each
  missed a construct: a name only a quantified group's element property map
  or dynamic label, a procedure's `YIELD … WHERE`, a `CALL (vars) { }` import
  list, or a comprehension/quantifier/`reduce` binder read from a subquery
  body keeps it live instead of failing `UndefinedVariable`, and
  `EXISTS { … NEXT … }` (plus the omitted-RETURN `COUNT { … NEXT … }`) decides
  on the whole stage chain rather than its first stage (REL8-2028).
- `ORDER BY`, `min`/`max` and `coll.sort` now read one total order over
  every value kind: maps order by size, then keys, then the values under
  them (so a null or a cross-type pair under a key decides the order
  instead of making the maps equal), points by SRID then coordinates, and
  nodes, relationships and paths by entity id. The output of a sort over
  these kinds is now ordered, `min`/`max` answer the real extremes, and
  sorting more than 20 maps no longer fails the statement with an internal
  error.
- `percentileDisc` returns the input value it picks instead of an F64
  image: an INTEGER sample answers an INTEGER, exactly, above 2^53.
- A far-year `date` (beyond year ±262142) now compares and sorts with
  in-range dates instead of answering null for `<`/`>` and keeping the
  input order.
- A cancelled statement now stops inside the contracted chain and hub-star
  kernels instead of running to the end of their expansion and row
  conversion: a CLI `--timeout` or Bolt `--query-timeout` bounds cancel-to-
  return on those kernels the way it already did on the per-row walker.
- `MATCH`-fed `ORDER BY … LIMIT` and a named `DISTINCT` now fold into their
  bounded forms instead of holding the whole input: the ordered page folds
  into the bounded top-k window and the `DISTINCT` into a first-seen pass as
  rows land, so memory scales with the page or the distinct keys, not the
  input row count. A provably empty page (`LIMIT 0`) answers without
  collecting its input. Answers are unchanged.
- A variable an earlier clause bound to null (`WITH null AS x`, a failed `OPTIONAL MATCH`) now
  matches nothing in a later pattern. On a variable-length, quantified or shortest pattern the
  engine treated the null as its own unbound placeholder and rebound it to whatever node the walk
  reached, so row counts, `EXISTS` / `COUNT { }` subqueries and the writes behind such a `MATCH`
  covered values Neo4j never matches; a `SHORTEST`/`shortestPath` with a null end now also answers
  instantly instead of enumerating every trail.
- A `CALL { … }` body with neither a scope clause nor a leading importing `WITH` now imports
  nothing, so a name its pattern binds is a new variable even when the outer row binds one of the
  same name (`MATCH (n:A) CALL { MATCH (n) RETURN count(n) AS total } RETURN total` now answers 3,
  not 1). The legacy importing `WITH`, `CALL ()` and `CALL (*)` forms are unchanged.
- Inside a `CALL (k) { … }` body, and inside a `COUNT` / `COLLECT` / `EXISTS` subquery, an
  imported or enclosing name now stays readable after a body `WITH` — a `MATCH … WHERE`, `SET`,
  `CREATE`, `REMOVE` or `DETACH DELETE` there no longer raises `UndefinedVariable` on a name other
  clauses accept.
- After a `DISTINCT` or aggregating `WITH` / `RETURN`, the variables the projection does not
  project are out of scope, so a subquery in the following `WHERE` / `ORDER BY` that re-uses one of
  those names binds a fresh variable. Relate pinned it to the dropped value — silently dropping
  groups and the writes behind them — or rejected the aggregating spelling.
- Large HTTP Query API results no longer build a JSON tree of every row
  before the first byte is written, which used about 20x the response size
  in memory; rows now encode one at a time from the engine's values.
- A `WHERE` comparison whose bound side is a subquery — `p.flag = EXISTS { … }`
  or `a.prop = COUNT { (b)… }` — no longer evaluates the subquery before
  the pattern's variables are bound and serves the result as an index
  seek. The EXISTS form kept only rows where both the comparison and the
  subquery held (a subquery with a free imported name matches anything,
  yielding `true`); the COUNT form returned 0 rows whenever the planner
  expanded the other pattern first (the free name made the count cover
  the whole side). Both now compare the value the subquery yields for
  each row.
- `valueType()` over a list mixing flat and nested list elements rendered the nested member ahead of the flat one (e.g. `valueType([1, [], [2, [null]], [2.0, 2]])`); the flat member now sorts first, matching Neo4j's canonical order in either input order.
- `isWasmTrap` (the WASM bundle's trap classifier) now recognizes Firefox's
  stack-overflow wording (`too much recursion`) and the `recursive use of an
  object detected…` error an overflowed instance throws on its next call, so
  a host following the documented contract replaces the dead instance instead
  of continuing to use it. Previously both were classified as non-traps on
  Firefox (and on any host that did not stop at the first trap), leaving a
  broken instance in service. The classifier's boundary is now explicit:
  only Error values are classified — a non-Error value never came from the
  bundle and is never a trap (previously a raw string whose text matched a
  trap wording classified as a trap).

### Notes

- Breaking (pre-1.0, ADR-0634): installed bash-kit extensions parse the
  MCP tool-result member order and must be regenerated after upgrading
  (`relate extension init` again, or reinstall from the first-party
  channel) — the mount now serializes `{"type":"text","text":…}` as
  documented. Recordings are unchanged: map literals and entity
  properties keep their code-point key order, now sorted explicitly at
  the decode boundary. HTTP notification entries and the MCP tool result
  serialize in the Neo4j member order their specs document. The markdown
  vault's parse boundary now sorts frontmatter object keys explicitly —
  the canonical value form ADR-0406 always specified, previously
  inherited from serde_json's sorted map — so the link sort/dedup key
  and rendered files stay spelling-independent.
- The macOS CI lanes (`os-smoke`, `swift`) run on the org's
  `macos-5-core-arm64` runner instead of the free-tier `macos-latest`;
  `scripts/check-os-matrix.py` now guards both labels via per-OS
  runner-label families.
- The never-sealed adjacency seal floor adapts: a chunk whose delta length
  held across a checkpoint interval now seals at 8 hops instead of the
  32-hop knee (ADR-0635), so a long-running store's steady mid-degree
  chunks fold instead of staying delta. The delta chunk stride grows 32 to
  40 B, and the Small memory shape's pins and the loadtest logical bytes
  move with it — the committed envelopes re-freeze through the ADR-0071 CI
  dispatch.
- The adjacency-payload checkpoint experiment (#621, REL8-1256) was
  removed from main before any release cut on its measured verdict: the
  parquet-row payload never paid at any measured store size (10M
  mid-degree: 17% open win against a recurring per-checkpoint penalty),
  and the cost sits in the parquet column layer in both directions. Its
  env vars (`RELATE_ADJACENCY_PAYLOAD`,
  `RELATE_ADJACENCY_PAYLOAD_MIN_RELS`) never shipped; the measurement
  record lives on REL8-1256 and the code is recoverable from the revert
  commit and PR #621.
- ADR-0372's Later-references note records the 2026.10.1 re-probe: the
  serving-pair conflict rule the ADR set is parity with current Neo4j,
  which now accepts the non-serving coexistence (a FULLTEXT index beside
  existence, type and uniqueness constraints and a graph type's
  `STRING NOT NULL` property, in both creation orders). Records and a
  parity pin test only — the engine behavior is unchanged since the
  serving-pair narrowing.
- CI: the spec-harness lane is now a required regression gate (ADR-0661) — a pull request fails when a scenario fails that the machine-written baseline in the private harness repository does not already carry as failing. The baseline refreshes on every merge to main by construction (a fix-N-break-N swap is red; the passing number cannot go stale), and each PR run checks out the harness at the revision the baseline records, so upstream corpus churn never reds an engine PR. The traffic-light coverage report is unchanged; scenario ids and corpus paths never render in this repository.
- New report-only CI lane: the REL8-211 load ground truth (1M nodes + 1M rels through one `UNWIND ... CREATE`, 3 rounds on a fresh store) now runs on the loadtest runner class for every ready PR, every `main` push, and on-demand dispatch, reporting execute/wall/user/RSS per round in the job summary (`scripts/load-perf.sh`; `make load-perf` runs the identical measurement locally). Reporting only, never a merge gate (the sqlite-gate rule, ADR-0482 §4): the point is a clean-host number on every PR instead of one-off readings from a shared dev box — the 2026-11-23 re-measure never found a load window under 4 in 4.75 hours.
- Toolchain: the workspace moves to Rust 1.99.1 (MSRV 1.99, ADR-0671,
  superseding ADR-0443). Pins, Sonic and baseline images, and the wasm
  toolchain (wasm-bindgen 0.2.130, wasm-pack 0.15.1) move together; every
  frozen loadtest envelope re-freezes with the `rust:1.99-bookworm`
  environment class in the stacked re-freeze PR.
- The baseline images' digest pins have one home (`scripts/baseline-env.sh`,
  read by name via `--print-image build|run`): the ceremony no longer carries its own
  floating defaults (which is how the memory lanes drifted off the pinned
  class while the make-driven lanes stayed on it), the Dockerfile's ARG
  defaults are gone so a standalone build fails loudly, and the fingerprint
  stamp no longer fabricates a floating image default. The run image is
  re-pinned to the dated tag `debian:bookworm-20261123-slim@sha256:e67eeead…`.
- Revised the preview-gated `db.ontology.*` procedure family's contract
  (ADR-0667; behind the `RELATE_PREVIEW=ontology` opt-in);
  `SHOW PROCEDURES` renders the current signatures under the opt-in.
- Cleared the open CodeQL alert backlog: 43 alerts fixed in code (least-privilege workflow `permissions:`, assert messages that print types instead of values, two ReDoS-prone regexes rewritten), 37 dismissed with per-alert reasons. Maintainer record, not release-notes material.
- A nightly `baseline-image-drift` cadence compares the pinned baseline
  images with Docker Hub and files, or comments on, one issue per moved
  image — Debian republishes `bookworm-slim` every 1–3 weeks,
  security-driven, so a drift is a re-pin candidate rather than a failure.
  The pin grammar (`repo:tag@sha256:<64 hex>`) gets one home
  (`scripts/baseline-pin.sh`), shared by the drift check, its test and the
  pins guard.

## [1.2.0] — 2026-11-20

### Added

- Mistral Vibe joins the `relate skill install` agent catalog. A machine
  with `~/.vibe` gets a `vibe:` row in `relate skill list`, and
  `relate skill install --agent vibe` (or a bare `relate skill install`,
  which targets every detected agent) writes the skill bundle to
  `~/.vibe/skills/relate` — detection, install, `skill check` drift
  detection and the `relate upgrade` refresh treat it like every other
  agent.
- `EXPLAIN` warns with `01N60` when the statement references a parameter
  the caller did not supply, one notification naming every undefined
  parameter — `EXPLAIN RETURN $foo` reports `The undefined parameter(s)
  $foo prevent plan reuse.` A fully supplied statement warns nothing.
  `PROFILE` and the plain path are unchanged: both refuse the statement
  with the ADR-0308 error instead, and `EXPLAIN` keeps planning without
  parameter values, as in the reference server.
- New .NET client `Relate.Net` under `clients/dotnet` (ADR-0625): an
  embedded C#, F# and Visual Basic API over the typed C ABI v2, with
  disposable databases, connections and transactions, typed parameters,
  graph and temporal values, result metadata and structured errors. CI
  builds the native library from the same checkout and runs the
  three-language tests on Linux, macOS and Windows.

### Changed

- `EXPLAIN`/`PROFILE` plan trees show display-only child steps under an
  index-seek-started `MATCH` — the seek step plus one `Expand` step per
  hop, in the CLI's plan table and as fused-flagged children in the
  structured plan (Bolt `queryPlan`/`profiledQueryPlan`, HTTP v2, the
  CLI's machine modes). Execution is unchanged: the clause stays the
  executed unit; consumers that require per-operator metrics skip the
  children.
- `MATCH (n:L) RETURN n.p, count(*)` and the min, max and `count(n.p)`
  aggregates over one labeled property now answer from the existing
  `(L, p)` index instead of scanning every node of the label — the
  `group_by_age` query of the SF0.1 latency suite drops from 17.3 ms to
  6.2 ms. Results are unchanged: whenever the index cannot serve the
  answer exactly, the aggregate falls back to the scan's exact answer
  (ADR-0611).
- Two parameter-name spellings Neo4j accepts now plan and run: the
  backtick-quoted name (``$`p1``, ``$`p 1``) and whitespace between `$`
  and the name (`$ p2`) — both previously failed at the lexer with
  `InvalidParameterName`. Every `$name` render now goes through one
  canonical quoting policy, so two different parameter names can never
  collide in a plan-cache key; `$ €`, a bare `$` and `$ (x)` keep their
  rejections.
- Dense id-span stores reserve the smaller of a 1.5-growth ladder rung
  and the old power-of-two doubling, and the memory floor charges
  exactly that reservation — the worst-case slot slack in a restored
  store drops from below 2× to below 1.5× (ADR-0617).
- A constraint, index or database can now be literally named `null`:
  `CREATE CONSTRAINT null …`, `CREATE INDEX null`, `DROP CONSTRAINT
  null`, `DROP INDEX null`, `ALTER DATABASE null` and
  `SHOW DATABASE null` parse and run, as in Neo4j. String-literal names
  and the continuation keywords stay rejected.
- Redundant parentheses around a projection item no longer change its
  meaning: `WITH (n)`, `WITH DISTINCT(n)`,
  `RETURN DISTINCT(n.p1) ORDER BY n.p1` and `GROUP BY (a)` now behave
  exactly like their unparenthesized forms — alias rules, ordering under
  `DISTINCT`, grouping keys and exemptions, `CALL` imports and the
  rendered column names — as Neo4j treats them. A property access keeps
  Neo4j's verbatim column name (`(n.p1)` names its column `(n.p1)`), and
  no restriction is loosened: every pinned refusal keeps its verdict.
- The Java client now publishes to Maven Central by default from a
  Publish-to-registries workflow dispatch, over a repaired natives gate
  (the `release` environment still gates the credentials, and
  `autoPublish=false` still leaves the Central Portal release in hand;
  uncheck `maven` to publish only the other registries).
- Cypher text may carry `\uXXXX` unicode escapes in the whitespace
  between tokens and in identifier text — `\u2028` between two tokens
  reads as whitespace, `\u0041` as the identifier `A` — matching the
  reference server's decoding. Parameters, numbers, comments and
  backtick names keep their rejections: an escape cannot spell a
  parameter name or any other token (ADR-0619).
- A fixed-length clause entered by two or more rows now runs on the
  frontier kernel, one run per entering row, instead of falling to the
  per-row walker — the walker enumerates trails, exponentially on a
  dense cyclic component. A single row whose run declines at run time
  still drags the clause to the walker.
- A checkpoint reopen now decodes and installs across all cores — one
  Parquet reader per row-group span, a slice-based bulk install that
  reads rows in place — so reopening a large store drops from 1225 ms
  to 461 ms on the measured fraud shape (2.66×; 1.36× on a shape whose
  restore work was already sharded). Restore rows are read in place and
  installed in bounded slices, so peak restore memory is
  `max(rows, graph)`, not `rows + graph`.
- Every release now publishes the Java client to Maven Central
  automatically through the release pipeline, and the publish profile
  gains a dry run that rehearses it against `main` without uploading.
- The engine's sidecar directory inside a store is `.relate/`, not
  `.cypherlite/` (the schema-files mirror moves with it): a writable
  open adopts an existing store in place with one directory rename,
  run as the last step before the lease so an open refused by any
  earlier check never renames the user's directory — natively, and
  through a caller-supplied `Vfs` (`Database::open_vfs`,
  `StoreTarget::from_vfs_with_codec`) the same way. A read-only open
  never adopts: a store still carrying the legacy `.cypherlite/`
  markdown sidecar refuses closed, naming the one writable pass that
  adopts it, and a legacy `.cypherlite/` without the manifest (a
  classic store's schema mirror alone) is inert and opens unchanged.
  The lock and writer marker files keep their names.
- `USING RANGE INDEX n:L1(p1)` now parses and validates like the plain
  `USING INDEX` hint, as in Neo4j: the hinted variable, label and WHERE
  predicate pass the same fulfillability checks (42N62/42N76), and the
  plan-time 01N31 warning names the hint as written
  (`'RANGE INDEX :L1(p1)'`). The hint still steers nothing — Relate's
  planner is not cost-based, so the kinds differ only in the rendered
  spelling.
- Label expressions now parse in every node pattern a query can write,
  not only MATCH patterns: a `WHERE` pattern predicate
  (`WHERE (a:L1|L2)<-[:R]-(b)`) and a pattern comprehension's leading
  node (`[(x:L1&L2)-[:R]->(y) | y]`) accept the full grammar — `|`, `&`,
  `!`, `%`, parenthesized groups, colon-chains and dynamic atoms — as
  Neo4j does. Both positions previously failed at the parse: the
  predicate read as a parenthesized `a:L1|L2` the relationship then
  broke, and the comprehension read as a list literal (ADR-0626).
- A filtered list comprehension's map expression may now be a map
  projection: `[n IN nodes(p) WHERE n:L1 | n{.p1}]` keeps the WHERE's
  label predicate and projects each surviving element through `n{.p1}`,
  as in Neo4j. The `|` was previously eaten by the label disjunction
  (`n:L1|n`), leaving the projection's `{` an unexpected token; a
  genuine disjunction with no map expression (`[m IN list WHERE m:A|B]`)
  keeps its reading.
- `SET n.p1 = EXISTS((n)-[:R]->(:L2))` now plans, executes and stores the
  predicate's boolean per row, as in Neo4j: a SET value accepts the same
  pattern-expression positions a projection does — the `exists(<pattern>)`
  argument, a comprehension's WHERE filter, and predicate positions
  (`NOT`, boolean operands, a `CASE WHEN` condition). A bare pattern as
  the value (`SET n.p1 = (n)--()`) keeps its rejection with the SET
  spelling.
- Property access on an aggregation result now plans and evaluates, as
  in Neo4j: `RETURN max(n.p1).p2` and `RETURN collect(n)[0].p1` treat a
  property whose target carries an aggregation as part of the
  aggregating expression instead of a non-grouping reference — the
  aggregate folds first and the property applies to its result (a
  node-typed `collect(n)[0].p1` reads the node's property; a scalar
  result keeps the existing property-on-scalar type error). A genuinely
  non-aggregated leaf beside the property (`max(n.p1).p2 + n.q`) keeps
  its grouping-key rejection.
- Aggregations grouped by a relationship's property with a selective
  predicate on the start node — for example `MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention)
  WHERE e.id IN $seeds RETURN e.id AS ent, r.method AS method, count(m) AS n` — no
  longer scan every node of the start label and walk every one of its
  edges: the predicate's seeks now answer the fold's start scan, walking
  only the sought nodes' adjacencies. Results are unchanged — every
  predicate still applies on the fully bound row.
- A GQL type predicate inside a comprehension's WHERE filter now leaves
  the trailing `|` to the comprehension's map separator, as in Neo4j:
  `[k IN keys(n) WHERE n[k] IS :: STRING | k]` plans and runs (a genuine
  `IS :: STRING | INTEGER` union inside the filter still parses
  greedily), and every `IS [NOT] ::` / `IS TYPED` / bare `::` spelling
  works in list, pattern, and CIP-272 map comprehensions.
- `USING TEXT INDEX n:L1(p1)` now parses and validates like the plain
  `USING INDEX` and `USING RANGE INDEX` hints, as in Neo4j: the hinted
  variable, label and WHERE predicate pass the same fulfillability
  checks (42N62/42N76), and the plan-time 01N31 warning names the hint
  as written (`'TEXT INDEX :L1(p1)'`). The hint still steers nothing —
  Relate's planner is not cost-based.
- A pattern predicate inside a quantifier's WHERE (`all`, `any`, `none`,
  `single`) is now accepted in WITH/RETURN projections and SET values,
  as in Neo4j: `RETURN all(x IN $p1 WHERE (:L1 {p1: x})<-[:R]-(:L2
  {p1: $p2}))` evaluates the pattern per element; bare pattern
  projection items and patterns in value subexpressions keep their
  rejections.

### Fixed

- EXPLAIN of a long clause chain (roughly 249+ chained clauses) no
  longer panics the plan-wire walk (`plan wire node is a map, got
  Null`): the plan tree is engine-produced metadata, so its crossing to
  the wire form no longer rides the ADR-0462 value-nesting depth cap
  (ADR-0628); the cap still governs runtime data values.

### Notes

- The 1.1.0 Swift entries were misfiled under [1.0.2] by their pull requests and moved to their release (ADR-0618).
- The shortest-route fan, anchor, selector and class-walk work landed as consolidation campaigns — no user-facing change (#499, #497).
- The PR cycle is now stated as one loop in the repository's agent contract, the ready flip the agent's own signal and the merge a separate authorization (#525).
- The decision-record corpus is now a queryable markdown vault (ADR-0621), and the adr-numbers CI gate reads the frontmatter `claim:` mint proof from every added ADR.
- Changelog entries now arrive as `.changes/` fragment files — one per
  pull request, named `<PR number>-<slug>.md` — collected into the
  version section at the release cut; a pull request no longer edits
  CHANGELOG.md (ADR-0627).
- The Linear workspace was renamed LABS → REL8: REL8-NNNN is the ticket-id
  spelling, an old LABS-NNNN reference means REL8-NNNN, and the
  ADR-number and release-notes reference checks refuse both spellings.

## [1.1.0] — 2026-11-17

### Added

- Swift: managed asynchronous access. `try await db.read { tx in … }` and
  `try await db.write { tx in … }` run a synchronous body off the caller's
  thread on connections the database owns (one writer, up to
  `Options.maxReaders` readers), with single-statement forms and
  `checkpointInBackground()`. A poisoned pooled connection is replaced.
- Swift: `Database.setTraceHandler` reports every statement with its
  timings and parameter names, not values (its statement text holds any
  literal spliced into it, and its error may quote a value), and every
  statement is an `os_signpost` interval. `Database.commits()` is an
  `AsyncStream` of
  committed writes with their bookmark, summed counters and the tags the
  writer attached (`db.write(tags:)`, `transaction(tags:)`).
- Swift: live values. `Database.values(tags:_:)` is a stream of what a
  read returns, now and again after each commit that may have changed it;
  `@LiveQuery` is the same observation as a SwiftUI property, reading the
  database set with `.relateDatabase(_:)`.
- Swift: `Database.defaultStoreURL(named:)` and
  `Database.provision(at:from:)` for seeding an app's store from its bundle,
  and `Migrator` for idempotent schema statements plus versioned steps.
- A guide, "Relate in an iOS app" (`docs/guides/ios-apps.md`), and the
  `examples/ios-snotel` app moved to live queries and the asynchronous,
  typed Swift API.
- Swift: bridges between `CypherValue` and platform types.
  `Foundation.Date` to and from `DATETIME`, `LOCAL DATETIME` and `DATE`;
  `Swift.Duration`; `[Float]` and Core ML arrays to `FLOAT32` vectors;
  points from a latitude and longitude or a `CLLocationCoordinate2D`.
  The bridges check their input and refuse, with `invalidParameter`, a date
  outside the years -999,999,999 through 999,999,999, a UTC offset beyond
  18 hours, date components that are not a calendar day, a duration beyond
  `Int64` seconds, a latitude or longitude off the globe, and a vector with
  a non-finite coordinate or more than 4,096 coordinates.
- Swift: `CypherValueConvertible`, so parameter dictionaries take Swift
  values (strings, numbers, optionals, arrays, dates, raw-value enums)
  without wrapping. A `QueryResult` is a collection of `Record`s with strict
  typed accessors (`record.get("title", as: String.self)`), plus `single()`
  and `scalar(as:)`. `QueryResult` is `Equatable`.
- Swift: `Statement`, a value holding statement text and parameters. As a
  string literal it binds interpolated values as parameters
  (`try conn.run("MATCH (b {key: \(key)})")`), with `\(identifier:)` and
  `\(raw:)` for what is not a value.
- Swift: `CypherValueEncoder` and `CypherValueDecoder`. An `Encodable`
  model binds as a map; rows, nodes and relationships decode into
  `Decodable` types (`result.decode(Book.self)`).
- Swift: `Connection.transaction(retries:_:)` re-runs its body on a
  retryable conflict; `Connection.read(_:)` runs a consistent snapshot that
  is always rolled back; `runInTransaction(_:retries:)` runs a list of
  statements all-or-none; `Connection.isPoisoned()` probes for a poisoned
  handle.

### Changed

- A shortest-path endpoint whose `WHERE` no index serves (`WHERE b.id + 0 =
  7`, a function over the property) is now listed by scanning its label with
  that predicate, and the pair search runs for the few matches, instead of a
  search that reaches every node and builds a row and a path for each. On a
  1.6M-node social graph `SHORTEST 1 (a {id: …})-[*..6]->(b) WHERE b.id + 0 =
  …` drops from 16 s to 0.5 s, the cost of the same predicate in a separate
  `MATCH`. Results are unchanged; a predicate matching more than 16 nodes, a
  nondeterministic one, one that reads another variable, or one that raises
  keeps the earlier plan.
- `SHORTEST k` (k above 1), `SHORTEST k GROUPS` and `ANY k` between two
  fixed endpoints now walk the trails one length class at a time instead of
  enumerating every trail up to the hop bound, so a bounded pair query that
  never finished on a social graph answers in milliseconds. Results are
  unchanged; a count selector with one open end still enumerates. A start
  with a large neighbourhood is no longer scanned neighbour by neighbour:
  the walk reads its steps near the end from the distance ball, and a full
  top-k heap rejects a worse trail before building it.
- Swift: `QueryResult.records` returns `[Record]` instead of
  `[[String: CypherValue]]`. Reading a record by column name
  (`record["name"]`) is unchanged; `record.dictionary` is the old form.
- Swift: `RelateError` has two new cases, `unexpectedResult` and `misuse`;
  an exhaustive `switch` over it needs them. `QueryResult.scalar()` now
  throws `unexpectedResult`, rather than `decoding`, for a result that is
  not one row and one column.
- Swift: a `transaction(_:)` body receives a `Transaction` instead of the
  `Connection`. A body that only runs statements on its parameter compiles
  unchanged; one that called `begin`, `commit`, `rollback` or `setNamespace`
  on it does not.

### Deprecated

- Swift: `connect(namespace:)` and `setNamespace(_:)` taking a namespace
  name as a `String`. The namespaces are fixed; pass a
  `DatabaseNamespace`.
## [1.0.2] — 2026-11-14

### Added

- Swift: `Database(url:readOnly:)`, `connect(namespace:)` and
  `setNamespace(_:)` taking the typed namespace, and error predicates on
  `EngineError` (`isConstraintViolation`, `isSyntaxError`,
  `isMissingParameter`, `isWriteConflict`, `classification`) plus
  `RelateError.isRetryable`.

### Changed

- `fs-to-graph` commits each rows record as its own transaction instead of
  one transaction around the whole import (ADR-0602): an import whose
  total payload exceeded the 64 MB WAL record cap (a large checkout at
  default flags) no longer fails at commit with nothing landed — every
  bounded batch commits as it lands. A failed import now keeps the
  batches that already committed, and re-running the same command
  completes it; a session's bounds are the per-call caps (1000 rows /
  8 MB per rows record) and the store's live-graph limit — the whole-run
  stream cap applies to whole-import framing only (ADR-0598).
- A `MATCH`-clause `WHERE` that reads the path (`length(p)`, `relationships(p)`,
  a relationship variable) now filters the selected paths afterwards for the
  keyword shortest selectors (`SHORTEST 1`, `ANY SHORTEST`, `ALL SHORTEST`,
  `SHORTEST 1 GROUPS`), as Neo4j does; it no longer promotes a longer path
  that satisfies it. `shortestPath()`, `allShortestPaths()` and inline
  `WHERE`s are unchanged (ADR-0600).
- The `system_ontologies` namespace is now visible only under
  `RELATE_PREVIEW=ontology` (ADR-0593), the same preview row that gates the
  `db.ontology.*` procedures (ADR-0378). Without the opt-in the namespace
  is invisible: `SHOW DATABASES` lists the two Community Edition rows, and
  `USE system_ontologies`, session connect (`--namespace`, the client
  `connect`/`setNamespace` APIs, Bolt's `db` routing) and
  `ALTER DATABASE system_ontologies` all fail with the site's ordinary
  unknown-database rejection (`Graph not found: system_ontologies`,
  42N00) — never the authored preview error. With the opt-in set, the
  namespace is visible and behaves exactly as before (ADR-0229: not
  queryable, only the administration commands run from it). Stored
  databases are unaffected: the durable catalog row stays and the listing
  returns when the opt-in is set.
- Swift: the result-metadata types are renamed so they no longer collide
  with Apple's. `Notification` is now `QueryNotification` and `Namespace`
  is now `DatabaseNamespace`; both old names are removed, a source-breaking
  change for code that spelled them. `Severity` and `Category` are now
  `NotificationSeverity` and `NotificationCategory`, with deprecated aliases.
- Swift: calls that can only fail with the package's own error are
  `throws(RelateError)`.

## [1.0.1] — 2026-11-12

### Added

- `SECURITY.md` at the repository root: the vulnerability reporting
  channel, the supported-surface scope, and the security allowances the
  project accepts.

## [1.0.0] — 2026-11-11

### Changed

- Opening an `s3://` URI reports that object storage belongs to clients
  implementing embedded Relate.

### Removed

- CDC replication slots. `register_cdc_slot`, `ack_cdc_slot`,
  `drop_cdc_slot`, `cdc_slots`, `cdc_records_after`, and
  `set_apply_only` / `apply_only` are gone, along with `cdc/slots.json`.
  Checkpoint recycle no longer waits on a consumer ack (ADR-0574).

## [0.1.3] — 2026-11-11

## [0.1.2] — 2026-11-09

## [0.1.1] — 2026-11-09

## [0.1.0] — 2026-11-09

### Initial release

First release of Relate — the embedded, in-memory property graph engine
with an openCypher query contract, WAL + snapshot durability, and Bolt
serving. See the README for the full surface list.

[Unreleased]: https://github.com/relatedb/relate/compare/v1.3.0...HEAD

[1.3.0]: https://github.com/relatedb/relate/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/relatedb/relate/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/relatedb/relate/compare/v1.0.2...v1.1.0
[1.0.2]: https://github.com/relatedb/relate/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/relatedb/relate/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/relatedb/relate/compare/v0.1.3...v1.0.0
[0.1.3]: https://github.com/relatedb/relate/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/relatedb/relate/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/relatedb/relate/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0

[Unreleased]: https://github.com/relatedb/relate

# Embedded driver providers — findings (LABS-447 spike wave, 2026-11-03)

The three spike reports verbatim, one per language leg. The summary and
cross-leg comparison live in
[`embedded-driver-provider-analysis.md`](embedded-driver-provider-analysis.md).

Spike branches (local, not merged): `spike/labs-447-java-provider`,
`spike/labs-447-python-provider`, `spike/labs-447-js-provider`.

---

## Java leg

# Java embedded provider spike (LABS-447)

Date: 2026-11-03. Branch `spike/labs-447-java-provider` (worktree
`.claude/worktrees/labs-447-java-spike`, off `main` @ 531730617). Commits
`d46ef7d3c` (client seam + two client bug fixes) and `507b70afe` (provider +
test program). Not pushed, no PR, no ADR.

Stack: stock `org.neo4j.driver:neo4j-java-driver:6.3.0`, which resolves
`org.neo4j.bolt:neo4j-bolt-connection:12.1.0` (not 12.2.2 as the research
report assumed for "today"). JDK 25, pom at Java 17. FFI:
`cargo build --release -p relate-ffi`, loaded through `RELATE_LIB`.
Client adapted to: the `main` JNA client (`com.relate.Database`), whose
public API is the same as LABS-830's fat-jar branch (`feat/labs-830-java-packaging`,
6712e79f4). The fat jar only adds `NativeLoader`/`NativePlatform`, so the
provider ports onto it with no code changes.

## Verdict

**The hydration-level seam works.** The stock driver, unpatched, runs
`executableQuery`, managed and unmanaged transactions (including a retried
write-write conflict), async, reactive, fetch-size paging, EXPLAIN/PROFILE
plans, notifications, counters and `db` routing against Relate
in-process. Records hold the driver's own `InternalNode`, `InternalRelationship`,
`InternalPath` and temporal values, built by the driver-passed `ValueFactory`.
No PackStream and no socket are involved.

The JSON-only C ABI is the only thing that limits fidelity. Three gaps need
string-sniffing hacks today:

- **Type tags.** Temporals, non-finite floats and paths carry none in the wire
  JSON.
- **Element ids.** The wire JSON has none.
- **Error codes.** Failures carry message text only, no Neo4j or GQL code.

All 8 spike scenarios pass (`mvn test`: 20/20, including the 12 existing
`DatabaseTest`s).

Size: about 1,000 lines of Java for the provider, value mapping, error mapping
and summaries, plus a 54-line seam class. That matches the research estimate of
"smaller than Query API".

### Messages: what the driver actually sent

Histogram across the whole suite: `RUN=38 PULL=36 RESET=38 BEGIN=28 COMMIT=22
ROLLBACK=1 DISCARD=1`. ROUTE, LOGON/LOGOFF and TELEMETRY were never sent.

| Message | Status | Notes |
| --- | --- | --- |
| HELLO (`BoltConnectionProvider.connect`) | works | No handshake. Any `AuthToken` is accepted. `minVersion > 4.4` → `MinVersionAcquisitionException`. |
| GOODBYE (`BoltConnection.close`) | works | Rolls back an open tx and calls `relate_disconnect`. Provider close calls `relate_database_close`, so a file store reopens cleanly in the next driver (s7). |
| RUN | works, with hacks | Params are converted to ADR-0087 JSON. Result metadata comes through the seam. Errors are classified by message sniffing (see gaps). The whole result is materialised at RUN. |
| PULL n | works | Pages the materialised rows. `fetchSize=2` gives real `hasMore` paging, but no memory or latency benefit. |
| DISCARD | works | `consume()` after a partial read. |
| BEGIN | works | `db` → `conn.setNamespace`. Timeout, metadata, bookmarks and impersonation are ignored. |
| COMMIT | works, degraded | No commit sequence on the ABI → **no bookmark** returned (`Optional.empty()`). |
| ROLLBACK | works | |
| RESET | works | The pool sends it on every release, and after every failure. Rolls back if a tx is open. FAILURE → OPEN. |
| ROUTE | implemented, never sent | The non-routing scheme uses `SingleRoutedBoltConnectionSource`. Answered with a single-member table. |
| LOGON / LOGOFF / TELEMETRY | implemented, never sent | `telemetrySupported()=false`. |

The provider tracks the failure state machine (OPEN/FAILURE/CLOSED, with
IGNORED after a failure until RESET), and the driver's retry and cleanup paths
behaved correctly on top of it.

### Scenario results

1. **`executableQuery("RETURN 1 AS one")`**: works. `ServerInfo` is
   `agent=Relate/0.1.0-embedded protocol=4.4 address=relate.embedded:7687`,
   `db=neo4j`, `queryType=READ_ONLY`. `verifyConnectivity()` works.
2. **Value types**: see the fidelity table below.
3. **`executeWrite`**:
   - Commit works. Explicit rollback leaves no trace.
   - A mid-tx `1/0` gives `ClientException Neo.ClientError.Statement.TypeError`, is **not
     retried** (attempts=1), and the tx's earlier `CREATE` is gone.
   - The session recovers via RESET.
   - A syntax error gives `Neo.ClientError.Statement.SyntaxError`.
   - Disjoint concurrent write txs both commit. Relate's commit is
     optimistic, not a single-writer lock at BEGIN.
   - A same-node conflict makes the second committer fail with `write-write conflict`.
     The provider maps that to `Neo.TransientError.Transaction.Outdated` →
     `TransientException`. `executeWrite` across an interleaved conflicting
     commit **retried and succeeded** (attempts=2, final value 1110 = all three
     increments applied).
4. **Async** (`executeReadAsync`, `listAsync`) and **reactive**
   (`ReactiveSession.run` + `Flow` subscriber requesting one at a time): both
   work.
5. **Parameters**: string, int, list and map bind correctly. Temporal and point
   parameters bind **degraded**: as the ISO string or a map. `date($d)` and
   `point($p)` in the query recover them. `byte[]` and NaN/Inf parameters
   are refused with a typed client error. A missing param gives `ParameterMissing`.
6. **Metadata**:
   - The write summary has `WRITE_ONLY` and full counters.
   - EXPLAIN gives a plan tree (`operatorType`, `Details`, children) plus the
     `CartesianProduct` notification. The driver synthesises the GQL status
     objects `00001`/`03N42` from the 4.4-legacy notification.
   - PROFILE gives a profile with dbHits, rows and time.
   - `keys()` and `consume()` work.
   - `SessionConfig.forDatabase("system")` routes, and `SHOW DATABASES` returns
     3 rows. An unknown db gives `FatalDiscoveryException DatabaseNotFound`.
   - `resultAvailableAfter` is `round(elapsed_ms)`, so it is 0 for fast queries.
     `t_last` is faked as 0.

### Value-type fidelity (results)

| Type | Status | Mechanism |
| --- | --- | --- |
| INTEGER, FLOAT, STRING, BOOLEAN, NULL | correct | JSON scalars |
| NaN / ±Inf | correct **only at top level with a FLOAT column stamp** | Wire sends the strings `"NaN"`/`"Inf"`. Re-typed via the ADR-0224 stamp. A nested NaN stays a string. |
| LIST, MAP | correct | Recursive |
| NODE | degraded | Numeric `id` only. **elementId is synthesised** as `Long.toString(id)`. Detected by sniffing `kind:"node"`. |
| RELATIONSHIP | degraded | Same: synthesised element ids for the rel and both endpoints |
| PATH | correct at top level, broken nested | The wire path is a bare `[node, rel, node]` array, identical to a list. Only the column stamp `PATH` tells them apart, so a path inside a list or map comes back as a LIST. |
| DATE, LOCAL TIME, TIME, LOCAL DATETIME, DATETIME (offset), DATETIME (named zone, `[Europe/Berlin]`), DURATION | correct **only at top level** | ISO strings re-typed using the column stamp. `TIME` and `DATETIME` stamps cover the local and offset variants, so the offset is sniffed from the string. Durations are parsed to months/days/seconds/nanos. Nanosecond precision survived. |
| Temporal nested in a list, map, or **node/relationship property** | **broken** (returned as STRING) | No stamp below the top level. A `born: date(...)` node property arrives as `"1990-01-02"`. This is the most visible user-facing defect. |
| Temporal in a later row of a heterogeneous column | broken | The ADR-0224 stamp describes row 0 only. A string in row 1 of a DATE column would be mis-typed, or a date mis-read as a string. |
| POINT (cartesian/WGS-84, 2D/3D) | correct | Sniffing `srid`+`crs`. Longitude/latitude map to x/y correctly. |
| User map that looks like an entity (`{kind:'node', id, labels, properties}`) | **broken** | It hydrates as a NODE. Shape-sniffing cannot be made safe. |
| VECTOR, geometry, UUID, BINARY | not exercised | The wire has shapes for vector (`{type,dim,values}`) and geometry (GeoJSON), but no mapper was written. They would come through as maps. |

## Gap list: requirements for the typed-value C ABI ADR

✅ = hit in this spike, with the evidence above.

1. ✅ **Typed result values.** A per-value type tag at every depth, not a
   first-row column stamp: DATE, LOCAL TIME, TIME (offset), LOCAL DATETIME,
   DATETIME with offset *or* zone id, DURATION as months/days/seconds/nanos,
   non-finite floats, paths distinct from lists, points, vectors and bytes.
   Without it, nested temporals and entity properties are broken.
2. ✅ **Element ids.** Nodes and relationships need a stable element-id string,
   with the start and end element ids on relationships. Today the provider
   fabricates them from the numeric id.
3. ✅ **Unambiguous entity encoding.** Graph entities must be distinguishable
   from user maps at every depth. The `kind` key collides with user data.
4. ✅ **Typed parameters.** The provider needs to pass LocalDate, ZonedDateTime,
   Duration, Point, `byte[]`, non-finite floats and (later) vectors. Today they
   degrade to strings or maps, or are refused. A `{"__type":"date"}` shape binds
   as a map (ADR-0087), so a typed builder is required, not a JSON convention.
5. ✅ **Error classification on the ABI.** Expose the Neo4j status code (and the
   GQLSTATUS) from the Bolt surface's `failures::error_class`, single-sourced.
   Today the provider string-matches `"syntax error:"`, `"type error:"`,
   `"ParameterMissing"` and `"conflict"`. Retry correctness rests on that hack:
   it happened to make the write-write conflict transient and `1/0`
   non-retryable. Also expose the compile-vs-runtime class (ADR-0319), so
   errors can surface at RUN vs PULL as Bolt does.
6. ✅ **Bookmarks.** COMMIT and auto-commit need a commit sequence (ADR-0310's
   `relate:<n>`). Today no bookmark is returned, so cross-session causal
   chaining is a silent no-op.
7. ✅ **Streaming results.** RUN materialises the whole result before the
   first record, so `fetchSize` paging is cosmetic. This needs a cursor ABI
   (open, fetch n rows, close) for big results and real backpressure in reactive
   sessions.
8. ✅ **Accurate timing.** `elapsed_ms` exists, but there is no
   result-consumed-after (`t_last`), and "available after" is coarse.
   Low priority.
9. ✅ **Notification positions and GQL objects.** The engine emits
   `gql_status`, but at 4.4 the driver synthesises GQL objects from the legacy
   shape anyway. At 5.5+ the provider would need `statuses` with
   `diagnostic_record`. That is only needed if the advertised protocol goes up.
10. **Single-writer lease vs transient errors.** *Partly refuted.* Two
    connections on one `Database` do not block each other at BEGIN. Disjoint
    writes both commit. Same-node writes fail the second committer with
    `write-write conflict`, which must map to a TransientError (it does via the
    hack, and `executeWrite` retries). The real requirement is gap 5.
    Cross-*process* lease contention was not exercised.
11. **Open-config knobs.** `Database.open(uri)` takes a URI only. A driver
    `Config` cannot pass read-only mode, memory limits or durability. The
    provider would need an open-options seam. Not hit.

### Gaps in the Java client, as a proposed internal seam

The public `Database` API stays unchanged. The provider consumes
`com.relate.DriverSeam`, a public final class in the spike. In a product
build it would sit in a non-exported package, with a qualified JPMS export to
the provider module. Everything on it is something the provider needed and the
public API cannot express:

- `query(conn, cypher, paramsJson)`: **the public Java client has no
  parameterised query at all.** That is a public-API gap in its own right.
- `columnTypes(result)`, `statsJson(result)`, `planJson(result)`,
  `elapsedMs(result)`: the C ABI has all of these, but `Result` drops them.
- `close(db)`, `disconnect(conn)`: **the public client has no
  `Database.close()` or `Connection.close()`.** Without them a file store is
  never released, the final checkpoint never runs, and a reopen in the same
  JVM would fail. The provider needs both for GOODBYE and driver close.

Once the typed ABI exists, the seam becomes a visitor over typed values that
targets the driver's `ValueFactory` directly. Its shape is close to the bolt
`ValueFactory` interface itself, which is a good template. The JSON decode then
disappears.

### Pre-existing client bugs found (fixed on the spike branch)

CI runs only `DatabaseTest#returnOne`, so both bugs are latent on `main`:

- `Result.Notification` has no Jackson creator. **Any EXPLAIN whose plan
  carries an operator warning throws** while decoding the plan (`DatabaseTest`
  `explainReportsCartesianProductNotification` and
  `explainReportsThePlanDocument` fail on main).
- `PlanActual` rejects unknown fields. **Every PROFILE throws**, because the
  engine now emits `batched`, `loops`, `frontier`, … in plan/v1 actuals.

Both deserve a standalone ticket and fix. The spike fixes are
`@JsonCreator(DELEGATING)` and `@JsonIgnoreProperties(ignoreUnknown=true)`.

## What the research report got wrong about the SPI

- **`relate:///path` does not "just work".** The report inferred that a
  host-less URI yields a harmless `null:7687`. In fact driver 6.3's
  `PooledBoltConnectionSource` calls `new BoltServerAddress(uri)` →
  `requireNonNull(host)` and **NPEs before the provider is ever called**. The
  same happens for `relate::memory:`, `relate:file:///…` and
  `relate:///abs/dir` (asserted in s7). The URL needs a host:
  `relate://memory` or `relate://localhost/abs/dir`. Relative paths have
  no clean spelling. The ADR must design the URL grammar around this, for
  example a dummy authority, or an upstream ask to tolerate host-less URIs.
- **Version.** Driver 6.3.0 ships against bolt-connection **12.1.0**, not
  12.2.x. The SPI signatures in the report (`connect(...)` with 11 args,
  `ResponseHandler`, `ValueFactory`) match 12.1.0. 12.1.0 also has
  `onIgnored()`/`onComplete()` on `ResponseHandler`. **A provider must call
  `onComplete()` once per `writeAndFlush` batch**, which the report did not
  mention.
- **Protocol 4.4 is fine** (open risk in the report, now closed). Driver 6.3
  only gates `legacyNotifications` on `< 5.5`
  (`ResultCursorImpl`/`RxResultCursorImpl`). With 4.4 it reads `notifications`,
  not `statuses`, and synthesises GQL status objects itself. Nothing else
  inspects `protocolVersion()` on the non-routing path. Advertising 4.4 is
  strictly easier than 5.4, which is the Query API's choice.
- **Messages not sent.** The report listed ROUTE as something the provider
  "must answer". On a non-routing scheme it is never sent. Instead, **RESET**
  dominates: it is sent on every pool release, not only after failures.
- **Server agent.** No `Neo4j/` prefix check exists in driver 6.3
  (`MetadataExtractor` passes the agent through). `Relate/<ver>` works
  without a `--bolt-identity`-style shim.
- **Threading.** One single-thread executor per connection is sufficient and
  satisfies the C ABI's "do not share a connection across threads" rule, plus
  the thread-local `relate_last_error`. No large-stack thread is needed (the
  engine's worker thread owns the stack, ADR-0259). Confirmed.

## Packaging (steer from Michael)

The provider is a thin adapter over the existing `Database`/`Connection`/`Result`.
There is no second JNA binding and no parallel result path: the seam adds 3 JNA
method declarations to the one `CLib` interface and reads the one decoded
`Result`. In the spike it lives in test sources, because the task scoped
neo4j-java-driver to test.

| | (a) Provider inside `com.relate:relate` (fat/classifier jar) | (b) Separate `relate-driver` artifact depending on the fat jar |
| --- | --- | --- |
| User story | One dependency: direct API, plus `GraphDatabase.driver("relate://…")` once the user adds the neo4j driver | Two dependencies. Explicit opt-in. |
| Dependency weight | bolt-connection must be `optional`/`provided`. Direct-API users get no neo4j classes on the classpath, but the jar carries provider classes that are inert without them. ServiceLoader never loads `META-INF/services/org.neo4j.bolt…` unless the bolt SPI is present, so this is harmless on the classpath. | Clean: the direct-API jar has zero neo4j references |
| Driver-compat versioning | **Coupled.** The bolt SPI is "no support, no API guarantees". It has moved 3 times in about 12 months (and 6.3 ships 12.1, not 12.2). Every driver-SPI break forces an engine-client release, and one jar can target only one SPI major. | **Decoupled.** `relate-driver` versions track driver majors (for example `relate-driver-6.x`) and pin the bolt-connection range. The engine client releases on its own cadence. |
| Classifiers (LABS-830) | Every classifier jar duplicates the provider classes and the services file. ServiceLoader de-duplicates by provider class name within a loader, so mixing classifiers is safe, but the provider ships N times. | Pure-Java jar, no classifier. It depends on whichever relate classifier the user picks. |
| JPMS | With no `module-info` (LABS-830 has none, so it is an automatic module), `provides` is declared only via `META-INF/services`. With a real module, `provides org.neo4j.bolt…Factory` plus `requires static org.neo4j.bolt.connection` is risky when the bolt module is absent *(inferred, not tested)*. | The provider module `requires org.neo4j.bolt.connection` normally. The seam package gets a qualified export `to com.relate.driver`. |
| Seam visibility | The seam can stay package-private: provider and client share the jar, but not the package, so the seam still needs to be public-but-internal or same-package | The seam must be public in a non-exported (or qualified-export) package. That is the `DriverSeam` shape this spike used. |

**Recommendation: (b).** The deciding factor is versioning. The bolt SPI is
explicitly unsupported and churns, and (a) would chain every engine-client
release to it. (a)'s "one dependency" story is weaker than it looks, because
users must add the neo4j driver anyway. Ship (b) with a small `relate-driver`
pure-Java jar, and keep the seam as an internal, qualified-export package in the
client.

## What I'd cut for a v1 product provider

Keep: the message set as built here (including RESET-on-release and
FAILURE→IGNORED semantics), Bolt 4.4, auth accepted as-is, the single-member
ROUTE answer, `db` → namespace, and the plan/profile and notification mapping.

Cut or defer:

- `file:` scheme claims and relative paths. Ship exactly
  `relate://memory` and `relate://localhost/<abs path>`.
- Routing schemes (`neo4j://`-style), impersonation (reject with a client
  error), tx timeout/metadata (ignore, documented), notification filters, and
  re-auth (LOGOFF/LOGON).
- Protocol ≥ 5.x and GQL `statuses`, until the ABI carries GQLSTATUS.
- Every string-sniffing hack: column-stamp re-typing, `kind`/`srid` shape
  detection, and message-prefix error classification. **v1 should wait for the
  typed-value + error-code ABI** rather than ship them. Shipping them makes
  nested temporals silently wrong and retry correctness depend on message
  wording.
- Real streaming. v1 can document "results are materialised; `fetchSize`
  pages a buffer", the same caveat the Python HTTP precedent ships with, and
  take the cursor ABI later.
- Bookmarks. v1 can document them as a no-op only if the ABI cannot return the
  commit sequence in time. Better to add it to the typed-ABI ADR, because it is
  small.

## Files

- `clients/java/src/main/java/com/relate/DriverSeam.java`: internal seam
- `clients/java/src/main/java/com/relate/Database.java`: seam hooks +
  two Jackson fixes
- `clients/java/src/test/java/com/relate/neo4j/`:
  - `RelateBoltConnectionProviderFactory.java`
  - `RelateBoltConnectionProvider.java` (URL mapping, Database lifecycle)
  - `RelateBoltConnection.java` (message dispatch, state machine, summaries)
  - `ValueMapping.java` (both directions + every hack)
  - `ErrorMapping.java` (the message-sniffing hack)
  - `Summaries.java`
  - `DriverSpikeTest.java` (the 8 scenarios)
- `clients/java/src/test/resources/META-INF/services/org.neo4j.bolt.connection.BoltConnectionProviderFactory`
- `clients/java/pom.xml`: `neo4j-java-driver:6.3.0` (test scope), surefire 3.5.4

Run with:
`RELATE_LIB=<worktree>/target/release/librelate_ffi.dylib mvn -f clients/java/pom.xml test -Dtest=DriverSpikeTest`.

---

## Python leg

# Python embedded provider spike (LABS-447)

Date: 2026-11-03. Branch `spike/labs-447-python-provider` (worktree
`.claude/worktrees/labs-447-python-spike`, off `origin/main` @ 6230ab314).
Commits `e0fd316ae` (native seam + provider) and `044edfe39` (sendable sessions,
GQL statuses, test program). Not pushed, no PR, no ADR.

Stack: stock, unpatched `neo4j` **6.4.1** from PyPI (pin `>=6.4,<6.5`), CPython
3.12 (uv venv), pyo3 binding built with `maturin develop --release` from
`surfaces/relate-python`. Run with:
`.venv/bin/python -m pytest python/tests/test_neo4j_driver_spike.py`
(53 scenarios pass; the full binding suite stays green, 348 passed / 8 skipped).

## Verdict

**The hydration-level seam works, and the typed binding removes most of the
Java spike's value-fidelity gaps, but not all of them.**

`relate.neo4j.driver("relate://memory")` returns a real `neo4j.Driver`
subclass (`async_driver()` returns a `neo4j.AsyncDriver`). On top of it the
stock driver runs all of the following in-process, with no socket, no PackStream
and no JSON:

- `execute_query`;
- managed and explicit transactions, sync and async;
- retried write-write conflicts, sync, async, and 8 threads sharing one store;
- `fetch_size` paging, `consume()`/DISCARD, and `Result.graph()`;
- EXPLAIN/PROFILE plans and GQL status objects;
- `database_` routing, bookmarks, and file-store reopen.

Records hold the driver's own `neo4j.graph.Node`/`Relationship`/`Path`,
`neo4j.time.*`, `neo4j.spatial.*Point` and `neo4j.vector.Vector`.

What the typed binding fixes compared with Java's JSON ABI:

- Temporals are correct at every depth, including inside node and relationship
  properties. That covers nanoseconds, named zones and offsets, which was the
  Java spike's worst defect.
- Paths stay paths when nested.
- A user map shaped like a node stays a map.
- NaN/Inf are correct at every depth and bind as parameters.
- Errors are classified from a typed `.kind`, with no message sniffing.

What it does **not** fix:

- **Durations** are lossy. Month-free durations are normalised through
  `timedelta`, so `PT36H` comes back equal to `P1DT12H`. Month-bearing and
  sub-µs durations arrive as **strings**, which are wrong at depth and in
  properties.
- **Element ids** disagree with `elementId()`.
- **Nested vectors** are plain dicts.
- There are **no Neo4j status codes or GQLSTATUS** on errors, and the `.kind`
  vocabulary is lossy exactly where retry decisions live.
- There is **no commit sequence**, so there are no real bookmarks.
- The public binding cannot open **more than one connection per store**. A
  small native seam was required for that.

Size: about 1,030 lines of Python (provider, value mapping, summaries, error
table, pools) plus a 130-line Rust seam, with a 530-line test program.
Overhead per trivial statement:

| Path | Time per statement |
| --- | --- |
| Binding, direct | 12 µs |
| Driver `session.run` (auto-commit) | 34 µs |
| Driver `execute_query` (managed transaction: BEGIN/RUN/PULL/COMMIT) | 64 µs |

## How it plugs in

- **Entry point:** `relate.neo4j.driver(uri, *, auth=None, **config)` and
  `async_driver(...)`, with the same shape as `GraphDatabase.driver`. The factory
  parses its own URI and never calls `neo4j._api.parse_neo4j_uri`. It builds
  `PoolConfig`/`WorkspaceConfig` via `Config.consume_chain` and then constructs
  `RelateDriver(_Direct, neo4j.Driver)` around our pool. This mirrors
  `HttpDriver._open`.
- **Pool:** `AsyncIOPool`/`IOPool` subclasses. They need only `acquire`,
  `release`, `kill_and_release`, `close`, `is_direct_pool=True` and
  `ssr_enabled=False`. The `opener` is never used. One native `Database` (one
  store) per driver, and an idle list of engine connections.
- **Connection:** subclasses of `AsyncConnection` and of the unasync'd sync
  `Connection`, sharing one state-machine core.
  - `run`/`pull`/`discard`/`begin`/`commit`/`rollback` queue request thunks.
  - `send_all` executes them as typed engine calls.
  - `fetch_message` delivers one queued response to the driver's handlers.
  - `reset` is synchronous.

  This is the HTTP transport's design (`_http/_http2.py`) without the HTTP.
- **Hydration:** `new_hydration_scope()` returns a `HydrationScope` whose
  `hydration_hooks` dict holds one hook, keyed on the binding's `QueryResult`
  type. That hook closes over the scope's `GraphHydrator.graph`, so every entity
  of one result shares one `Graph`, and `Result.graph()` works.
- **Parameters** are walked once. Driver-native types (`neo4j.time.*`,
  `neo4j.spatial.Point`) become what the binding accepts: stdlib `datetime`
  types and `relate.Point`.

### Messages (what the driver sent)

| Message | Status | Notes |
| --- | --- | --- |
| connect / HELLO | works | No handshake. The pool hands out a connection; auth is accepted and ignored. `ServerInfo(protocol (5, 8))`, agent `Relate/0.15.0-embedded`. |
| RUN | works | Typed params through the binding. The whole result is materialised and hydrated at RUN. A failure terminates the tx (engine rolled back immediately) and later requests get IGNORED until the FAILURE is consumed. |
| PULL n | works | Real `has_more` paging over the buffer: 3 PULLs for 5 rows at `fetch_size=2`. There is no memory or latency benefit. |
| DISCARD | works | `consume()` after a partial `fetch(1)`. |
| BEGIN | works | `db` → `set_namespace`. metadata/timeout/bookmarks/impersonation are ignored (impersonation is *silently* accepted: v1 should reject it). |
| COMMIT | works with a hack | No commit sequence on the binding → the provider mints ADR-0310 `relate:<n>` itself. |
| ROLLBACK | works | Idempotent when the engine tx is already gone (after a FAILURE). |
| RESET | works | `reset()` is called by the pool release path and after failures. It rolls back an open tx. |
| FAILURE / IGNORED | works | FAILURE is `{code, message}` → `Neo4jError._hydrate_neo4j`. IGNORED is sent for requests queued behind a failure. |
| ROUTE / LOGON / LOGOFF | not applicable | No such methods on the 6.4 ABC. A direct pool never routes. Re-auth is reported as supported and ignored. |
| TELEMETRY | no-op | `telemetry()` is accepted and dropped. |

## Scenario results

1. **`execute_query("RETURN 1 AS one")`**: works. `keys == ["one"]`,
   `query_type "r"`, `database "neo4j"`. `verify_connectivity()`,
   `get_server_info()` and `supports_multi_db()` work. The managed tx sends
   BEGIN, RUN, PULL and COMMIT, then RESET on release.
2. **Values**: see the fidelity table.
3. **Transactions**:
   - `execute_write` commits. An explicit `tx.rollback()` leaves no trace.
   - A mid-tx `RETURN 1/0` raises `CypherTypeError`
     `Neo.ClientError.Statement.TypeError`, is not retried (attempts=1), and the
     tx's earlier `CREATE` is gone. The session recovers.
   - Syntax errors raise `CypherSyntaxError`. A missing parameter raises
     `ClientError` `ParameterMissing`.
   - A same-node write-write conflict fails the **second committer at COMMIT**
     with `TransientError` `Neo.TransientError.Transaction.Outdated`.
   - Async (`AsyncSession.execute_write`, `async for`) works. 8 threads × 25
     `execute_write` increments end at exactly 200, with conflicts retried and
     only 5 engine connections opened.
4. **Parameters**: see the parameter table.
5. **Summaries**:
   - Write counters are complete, and `contains_updates` is derived by the
     driver.
   - EXPLAIN gives a Bolt-shaped `plan` (`operatorType`, `args.Details`,
     `EstimatedRows`, children).
   - PROFILE gives `profile` with `dbHits`, `rows` and `time` in ns. The page
     cache fields are zero.
   - `notifications` come in the legacy shape. `gql_status_objects` carry the
     **engine's own GQLSTATUS** (`00001`, `03N90`) because the provider passes
     Bolt-5 `statuses`. The Java spike at 4.4 got the driver's synthesised
     `03N42`.
   - `keys()`, `consume()`, `peek()` and `fetch()` work.
   - `result_available_after` is the engine's elapsed ms. `t_last` is faked as 0.
   - `database_="system"` routes: `SHOW DATABASES` returns 3 rows and
     `summary.database == "system"`. An unknown db raises `ClientError`
     `DatabaseNotFound`.
6. **Bookmarks**:
   - The committing write returns `relate:1`. An auto-commit read returns
     the last value, and an auto-commit write advances it.
   - A bookmarked session reads the write. That is trivially true, because there
     is one in-process store.
   - The values are minted by the provider, not the engine (gap below).
7. **File store** (`relate:///abs/dir`):
   - Data persists across driver close and reopen.
   - A second *writable* driver on the same directory is refused (`storage
     locked`), but it raises the binding's raw `RelateNativeError` out of
     `relate.neo4j.driver()`, not a driver exception.

### Value fidelity (results)

| Type | Top level | Nested in list/map | In node/rel property | Mechanism |
| --- | --- | --- | --- | --- |
| INTEGER, FLOAT, STRING, BOOLEAN, NULL | correct | correct | correct | typed |
| NaN / ±Inf | correct | correct | correct | typed float (Java: top level only) |
| LIST, MAP | correct | correct | correct | typed |
| NODE / RELATIONSHIP | correct, but the element id needs a **hack** | correct | n/a | typed `relate.Node`/`Relationship` → `neo4j.graph`. The binding's `element_id` is `str(id)` (`"0"`, and node 0 and rel 0 collide), while Cypher's `elementId()` returns `4:rl-id:n0` / `5:rl-id:r0`. The provider re-spells the engine's format so `WHERE elementId(n) = $id` round-trips. |
| PATH | correct | **correct** (Java: broken) | n/a | typed `relate.Path` |
| User map shaped like an entity | correct (stays a map) | correct | n/a | typed (Java: mis-hydrated as a NODE) |
| DATE, LOCAL TIME, TIME (offset), LOCAL DATETIME, DATETIME (offset), DATETIME (named zone), nanoseconds | **correct** | **correct** | **correct** | stdlib `datetime` when lossless, `relate.Temporal` otherwise. `Temporal.text` is the Query-API ISO form, parsed by the driver's own HTTP hydrators. Zone survives (`tzinfo.zone == "Europe/Berlin"`). Cosmetic: tzinfo is stdlib `timezone` or `pytz.FixedOffset` depending on the path (Bolt always gives pytz). |
| DURATION, month-free, µs-exact | **degraded** | degraded | degraded | Normalised through `timedelta`: `PT36H` == `P1DT12H` == `Duration(days=1, seconds=43200)`. Cypher distinguishes them. |
| DURATION with months, or sub-µs | correct only with a **hack** | **broken (STRING)** | **broken (STRING)** | The binding returns ISO text. At the top level the ADR-0224 `DURATION` column stamp re-types it. Nested, `[duration('P1Y')]` is `["P1Y"]`. |
| POINT (cartesian/WGS-84, 2D/3D) | correct | correct | correct | typed `relate.Point` → `neo4j.spatial` |
| VECTOR | correct only with a **hack** | **broken (dict)** | not exercised | A self-describing `{dim, type, values}` dict at every depth. Only the top-level `VECTOR` stamp distinguishes it from a user map. Engine dtype names are `FLOAT32`… and `INTEGER8`…. |
| BYTES, UUID | n/a | n/a | n/a | The engine has no such scalars on this surface (bytes → `LIST<INTEGER>`, UUID → STRING). |

### Retry behaviour (the error-code evidence)

| Case | Code the driver saw | Driver behaviour |
| --- | --- | --- |
| Interleaved same-node conflict, classified (`.kind == "WriteConflict"` → `Neo.TransientError.Transaction.Outdated`) | Transient | **Retried**: attempts=2, with the "will be retried in ~1s" log. Final value 101: no lost update. Same for async and for 8 threads. |
| The same conflict **without classification** (`_errors.CLASSIFY=False`, so every engine error is `Neo.DatabaseError.General.UnknownError`, which is what a message-only ABI gives without sniffing) | DatabaseError | **Not retried**: attempts=1, `execute_write` raises `DatabaseError`, `is_retryable()` is False. Final value 100: the user's increment is lost. |
| `RETURN 1/0` mid-tx | `Neo.ClientError.Statement.TypeError` | Not retried. Correct outcome, but Bolt says `ArithmeticError`: the binding folds `Error::Arithmetic` into `TypeError`. |
| Syntax error, missing parameter | `SyntaxError`, `ParameterMissing` | Not retried |
| Retryable resource cap (`concurrent-logical-workspace-limit`) | *inferred, not exercised* | `.kind` is `ResourceLimitExceeded` for **both** retryable and non-retryable caps, so the provider must pick one code. Bolt sends `Neo.TransientError.General.MemoryPoolOutOfMemoryError` for the retryable one. A mapping from the binding can only be wrong for one of them. |
| Any error | GQLSTATUS `50N42` | The driver's generic fallback. No GQLSTATUS crosses the binding. |

Conclusion: retry correctness depends entirely on a Neo4j code being present,
and today it depends on a provider-side kind→code table. That table is a second
copy of `surfaces/relate-bolt/src/failures.rs::error_class` and is lossy:
`Arithmetic` → TypeError, `Cancelled` → SemanticError (Bolt:
TransactionTimedOut), `ProcedureNotFound` → SyntaxError, and the retryable
resource cap is unrecoverable.

### Parameters

| Parameter | Result | Notes |
| --- | --- | --- |
| str, int (i64), bool, None, list/tuple, nested map | binds | `2**64` gives a typed out-of-range error |
| float NaN / ±Inf | **binds** | Java refused these |
| stdlib `date`, `datetime` (naive/aware), `time` (naive/aware), `timedelta` | binds | Native pyo3 conversion |
| `neo4j.time.Date`, `Time`/`DateTime` (µs-exact), `Duration` (no months, µs-exact) | binds | Via `.to_native()` / timedelta in the provider |
| Nested temporal params (map/list) | binds | |
| `neo4j.spatial.CartesianPoint`/`WGS84Point` (2D/3D), `relate.Point` | binds | Via `relate.Point(srid, …)`. Java got a map. |
| Named-zone `DateTime` (pytz/zoneinfo) | **degraded** | Binds as an offset datetime. The zone id is lost. |
| `Duration(hours=36)` | **degraded** | Binds as `P1DT12H` (timedelta normalisation) |
| `bytes` | degraded | `LIST<INTEGER>` (ADR-0219) |
| `uuid.UUID` | degraded | STRING (CIP-246) |
| Sub-µs `DateTime`/`Time`, `Duration(months=…)`, `Vector` | **refused** | `CypherTypeError`. The binding has no carrier for these. |

## Corrections to the research report

- **The public binding is not enough by itself.** The report assumed the
  connection "calls the relate pyo3 module". But `relate.Database` is
  **one store fused with one engine connection**. Two `Database(":memory:")`
  objects are two unrelated stores, and a second writable `Database(path)` is
  refused by the writer lease. A driver pool needs N independent connections,
  each with its own explicit transaction, on one store. That is impossible
  through the public API, so the spike adds `Database._driver_session()` (see
  the binding gaps below).
- **Thread model.** The public `Database` is thread-affine (DB-API
  threadsafety 1). Driver pools hand connections to any thread, and
  `driver.close()` closes idle connections from the closing thread. A first cut
  with `#[pyclass(unsendable)]` sessions **panicked on driver close**. The seam's
  sessions must be sendable (a `Mutex<Option<Connection>>`). The engine
  `Connection` is `Send`, so this is safe.
- **The temporal fidelity claim** ("full temporal fidelity, LABS-756") holds for
  date/time/datetime, including nanoseconds and zones, but **not for
  durations**. `convert.rs::duration_to_py` returns a `timedelta` or an ISO
  string. **`datetime` params are accepted**, but named zones degrade to
  offsets, sub-µs values cannot be bound, and month durations cannot be bound.
- **The ABC is larger than listed.** Besides run/discard/pull/begin/commit/
  rollback/reset/send_all/fetch_message/fetch_all/new_hydration_scope/supports_*/
  defunct/closed, it also has abstract `log_id`, `auth`, `auth_manager`,
  `ssr_enabled`, `telemetry` and `is_reset`. The workspace and result also
  read `server_info`, `unresolved_address` and `pool`, which are not in the ABC.
  Class-level defaults satisfy the abstract properties (as HTTP does).
- **Hydration hooks are keyed by type, and the scope is not passed to `run`.**
  The driver passes only `hydration_hooks` (a dict) and `dehydration_hooks`, not
  the `HydrationScope`. To build entities on the scope's `Graph` (needed for
  `Result.graph()`), the hook must close over it, keyed on your own result type.
  `HydrationScope`/`GraphHydrator`/`DehydrationHooks` live in
  `neo4j._codec.hydration._common`; they are not exported from
  `neo4j._codec.hydration`.
- **The HTTP transport cannot be imported as a library.** Importing
  `neo4j._async.io._http` (for example its `ResponseHandler`) pulls in `aiohttp`,
  an optional extra. The provider carries its own 40-line handler instead. The
  HTTP *codec* (`neo4j._codec.hydration.http.v2.temporal`) imports cleanly, and
  its ISO parsers hydrate `Temporal.text` and duration strings for free.
- **"Needs both variants" is cheap.** One shared core covers both. The
  async/sync split is about 60 lines (`reset`/`send_all`/`fetch_message`/
  `fetch_all`/`close` plus the handler class).
- **URIs are not a problem in Python.** The factory parses its own URIs, so
  host-less forms work: `relate://memory`, `relate::memory:`,
  `relate:///abs/dir`, and `relate:rel/dir`. This differs from Java,
  where the driver's `BoltServerAddress` NPE'd. The `Address` is cosmetic
  (`memory:7687`).
- **Protocol (5, 8) is fine**, so the provider can send Bolt-5 `statuses`. With
  them the engine's real GQLSTATUS reaches `gql_status_objects`. Nothing in
  6.4.1 gates on the advertised version along the direct path.
- **The async driver blocks the event loop.** The GIL is released inside the
  engine call, but the call is synchronous on the loop thread. Sessions are now
  sendable, so `run_in_executor` would be possible, but the spike did not do it.

## Requirements for the typed-value + error-code ABI ADR

✅ = hit in this spike, with the evidence above.

1. ✅ **Error classification on the binding/ABI.**
   - Carry the Neo4j status code, single-sourced from the Bolt surface's
     `failures::error_class`, plus the GQLSTATUS and the compile/runtime class.
   - Today the binding's `.kind` is typed but lossy exactly where retries are
     decided: retryable vs non-retryable `ResourceLimit`, `Arithmetic`,
     `Cancelled`, `ProcedureNotFound`.
   - Without a code, the driver silently does not retry a write-write conflict
     (measured).
2. ✅ **Commit sequence → bookmark.** `relate::Connection::commit` drops the
   commit timestamp that `try_wal_commit` returns. Return it, as ADR-0310's
   `relate:<n>` or the raw ts, from commit and from auto-commit writes, so
   bookmarks are real. The provider counter is the same stand-in the Bolt
   listener uses.
3. ✅ **Lossless durations.** Expose (months, days, seconds, nanos) as a typed
   value at every depth, for example a `relate.Duration` in the binding and
   a tagged duration in the C ABI. Today `PT36H` and `P1DT12H` collide, and
   month or sub-µs durations become strings. The same type is needed as a
   parameter, because month durations cannot be bound.
4. ✅ **Element ids.** Entities must carry the engine's `elementId()` string
   (`4:rl-id:n0`), and relationships their endpoints' element ids. The
   binding's `element_id = str(id)` disagrees with Cypher and collides between
   node 0 and rel 0.
5. ✅ **Tagged vectors.** A vector type distinct from a map at every depth.
   Today it is a `{dim, type, values}` dict, recoverable only with the top-level
   stamp. Vectors must also be bindable as parameters.
6. ✅ **Typed temporal parameters beyond stdlib.** Named-zone datetimes (zone
   id kept) and sub-µs time/datetime need a carrier. A constructible
   `relate.Temporal`, or the C ABI's typed builder, would do it.
7. ✅ **Multiple connections per store.** The binding has 1:1
   store:connection. The C ABI already has `relate_connect` on a database
   handle, so this is a **Python-binding** requirement, not an ABI one.
8. Streaming/cursor: hit only as "`fetch_size` is cosmetic", the same as in
   Java. Not required for v1.
9. Timing (`t_last`): low priority, the same as in Java.

Already satisfied by the Python binding (no ABI work for Python): typed values
for scalars, lists, maps, nodes, relationships, paths, points, and
date/time/datetime with nanoseconds and zones; typed params for those; query
type; resolved namespace; counters; elapsed ms; notifications **with
GQLSTATUS**; and the plan/profile document.

## Python-binding gaps (the `DriverSeam` analogue)

The spike's seam is one private method plus one private class in the native
module (`surfaces/relate-python/src/driver_seam.rs`):

- `Database._driver_session(namespace)` → `DriverSession`. It is an independent
  `relate::Connection` on the same store, opened **without** the owner-thread
  check, and **sendable**. Its methods are `execute(query, params)` (GIL
  released, returns the public typed `QueryResult`), `begin`, `commit`,
  `rollback`, `in_transaction`, `set_namespace`, `namespace`, `close` and
  `closed`.

Everything else the provider needed is already public on the binding: the
`QueryResult` metadata, `RelateNativeError.kind`/`.tck_class`, and the
graph/temporal pyclasses. Missing from the binding, and worked around in the
provider:

- `commit()` returns no commit sequence (requirement 2).
- No Neo4j code or GQLSTATUS on errors (requirement 1).
- `element_id` ≠ `elementId()` (requirement 4).
- Durations and vectors are lossy (requirements 3 and 5).
- `Database(...)` open errors surface as raw `RelateNativeError`. The
  provider should translate them to `ServiceUnavailable`/`ClientError`; the
  spike does not.
- EXPLAIN returns `columns == []`. Neo4j returns the statement's keys with no
  records. Minor.
- No open-options seam beyond `read_only` (memory limits, durability). Not hit.

## Packaging

The provider rides the existing `relate-db` wheel as the subpackage
`relate.neo4j`. `import relate` stays driver-free, because the
subpackage is imported only on demand. The spike installed `neo4j` by hand; a
product would declare an extra: `pip install relate-db[neo4j]` →
`neo4j>=6.4,<6.5`.

Recommendation: **ship it inside the wheel, behind the extra, with an
import-time driver-version guard.** The Python situation differs from Java's:

- **No dead weight for non-users.** There is no classpath or classifier
  duplication. An unused subpackage costs a few KB and never imports `neo4j`.
- **The seam stays private.** A separate pure-Python package would have to
  depend on `Database._driver_session` across a package boundary, which turns a
  private native method into a cross-package contract versioned on both sides.
  Inside one wheel it stays private.
- **Driver-version coupling is real, and worse than Java's.** The provider
  touches about 20 private driver names:
  - `neo4j._async.io._connection.AsyncConnection` and
    `neo4j._sync.io._connection.Connection`
  - `neo4j._async.io._pool.AsyncIOPool` and `neo4j._sync.io._pool.IOPool`
  - the `AsyncDriver`/`Driver` constructors and `_Direct`
  - `neo4j._async_compat.util`
  - `neo4j._codec.hydration._common`
  - `neo4j._codec.hydration.http.v2.temporal`
  - `neo4j._work.summary` status constants
  - `neo4j._conf`, `neo4j._async.config`, `neo4j._sync.config`
  - `neo4j._addressing.Address`, `neo4j.spatial._srid_table`
  - `Neo4jError._hydrate_neo4j`
  - the `Graph._nodes`/`_relationships` and entity `_properties`/`_labels`/
    `_start_node` internals

  The whole seam is a 6.4.0 **preview** that "does not follow semver" (PR #1319).
  So pin to one driver minor (`>=6.4,<6.5`) and fail loudly at import on any
  other version. Every driver minor then needs a wheel release, a cost accepted
  in exchange for the private seam. If driver churn turns out to dominate, split
  later into `relate-db-driver` (pure Python, versioned per driver minor),
  once the seam has become a documented binding API.
- **Upstream ask, the same as in the research report.** A hook equivalent to
  rust-ext's optional import (`try: import neo4j._embedded`) in the scheme/pool
  dispatch would let `GraphDatabase.driver("relate://…")` work and turn the
  private-API surface into a supported one.

## What I'd cut for a v1

Keep:

- the request/response state machine as built: FAILURE → IGNORED until
  consumed, failure terminates the tx, RESET rolls back;
- direct pool, auth accepted and ignored, `db` → namespace, sendable pooled
  engine connections;
- protocol (5, 8) with `statuses` (the engine GQLSTATUS reaches the user);
- plan/profile mapping;
- the stdlib/`Temporal` → `neo4j.time` hydration, which is lossless today.

Cut or defer:

- **Every hack.** That means the element-id re-spelling, the duration and vector
  column-stamp re-typing, the kind→code table, and the provider-minted
  bookmarks. v1 should wait for requirements 1–5 rather than ship
  silently-wrong nested durations or retries that depend on a second, lossy
  copy of the Bolt error policy. Unlike Java, the remaining hack set is small:
  durations, vectors, element ids, error codes and bookmarks.
- **Impersonation, tx metadata/timeout, notification filters.** Reject them with
  a `ConfigurationError`, as the HTTP transport does, instead of silently
  ignoring them.
- **The async driver.** Either document that it blocks the event loop, or run
  engine calls with `run_in_executor`, now that sessions are sendable.
- **Real streaming.** Document "results are materialised; `fetch_size` pages a
  buffer", which is the HTTP transport's own documented caveat.
- **Relative-path URIs** (`relate:rel/dir`). Ship `relate://memory` and
  `relate:///abs/dir`.

## Files (branch `spike/labs-447-python-provider`)

- `surfaces/relate-python/src/driver_seam.rs`: internal native seam
  (`DriverSession`)
- `surfaces/relate-python/src/lib.rs`: `Database._driver_session`, class
  registration
- `surfaces/relate-python/python/relate/neo4j/`:
  - `__init__.py` (factories, URI parsing)
  - `_pool.py` (store, pools, driver classes)
  - `_connection.py` (state machine, async and sync connections, hydration
    scope, bookmarks counter)
  - `_values.py` (both directions, every hack marked)
  - `_summary.py` (counters, plan/profile, notifications, statuses)
  - `_errors.py` (kind → code, the `CLASSIFY` switch)
- `surfaces/relate-python/python/tests/test_neo4j_driver_spike.py`: the 53
  scenarios

---

## JavaScript leg

# JavaScript embedded provider spike (LABS-447)

Date: 2026-11-03. Branch `spike/labs-447-js-provider` (worktree
`.claude/worktrees/labs-447-js-spike`, off `origin/main` @ b7b329d8f). One
commit, `d023bec46` (client seam + provider + test program). Not pushed, no
PR, no ADR.

Stack: stock, unpatched `neo4j-driver` **6.3.0** from npm (the current
`latest`; exact dev dependency). It pins `neo4j-driver-core` 6.3.0 and
`neo4j-driver-bolt-connection` 6.3.0 exactly. Node 24.21, koffi 2.16. The FFI
was built with `cargo build --release -p relate-ffi` and loaded through
`RELATE_LIB`. The provider runs on the existing koffi client
(`clients/javascript/index.js`). It adds no second binding.

Run with:
`RELATE_LIB=<worktree>/target/release/librelate_ffi.dylib npm --prefix clients/javascript run test:neo4j-spike`.
That runs 16 scenarios, all passing. The existing `npm test` (client and
loadtest) stays green.

## Verdict

**The hydration-level seam works.** The JavaScript leg lands in the same place
as Java: the JSON-only C ABI limits fidelity, and the driver does not.

`driver("relate://memory")` returns a real `neo4j.Driver`. With a one-line
`install()`, the stock `neo4j.driver("relate://…")` returns one too. On top
of it, the stock driver runs all of the following in-process, with no socket,
no Bolt bytes and no PackStream:

- `executeQuery`;
- `session.run` auto-commit;
- managed `executeRead`/`executeWrite`, including a retried write-write
  conflict;
- explicit transactions;
- `fetchSize` paging with `consume()`/DISCARD;
- EXPLAIN/PROFILE plans, notifications and GQL status objects;
- `database` routing to a namespace;
- file-store close and reopen;
- RxSession;
- 8 concurrent async sessions on one store.

Records hold the driver's own core `Node`, `Relationship`, `Path`, `Integer`
(or `BigInt`/`number` under `useBigInt`/`disableLosslessIntegers`),
`Date`/`Time`/…/`Duration`, `Point` and `Vector`. The driver's own
`ResultStreamObserver` builds the records and summaries.

The value gaps are exactly Java's, because both legs sit on the same wire JSON:

- **Type tags.** Temporals, durations, non-finite floats, vectors and paths
  are correct only at the top level. Nested in a list, a map or a node
  property, they come back as strings, lists or maps.
- **Element ids.** The wire JSON carries none.
- **Error codes.** Failures carry message text only.
- **Bookmarks.** None are returned.

JS adds three gaps of its own:

- **The client's JSON decode is lossy for numbers.** It rounds int64 beyond
  2^53 and folds `1.0` into the integer `1`. This is a **pre-existing
  public-client bug**: `RETURN 9007199254740993` returns
  `9007199254740992` on `main`. The provider works around it with a
  source-text reviver, which needs Node ≥ 21. The client's `engines` floor
  is 18.
- **Every engine call is synchronous** and blocks the event loop.
- **The C ABI's thread-local `relate_last_error` loses the error message**
  under koffi's async call form, which is the only way to stop blocking the
  event loop. I measured this.

Size: about 880 lines of JavaScript, made up of:

- `provider.js`: 429 lines (connection state machine, provider, factory,
  monkey-patch);
- `values.js`: 333 lines (both directions, every hack marked);
- `summary.js`: 116 lines (summaries, plans, the error-sniffing hack);
- a 30-line internal seam in `index.js`.

The test program is 344 lines. That is the same order as Java (about 1,000)
and Python (about 1,030).

Overhead for a trivial statement (`RETURN 1`, 5,000 iterations after warm-up,
release build):

| Path | Time per statement |
| --- | --- |
| Client, direct (`conn.query`) | 13–16 µs |
| Driver `session.run` (auto-commit: RUN/PULL) | 47 µs |
| Driver `executeQuery` (managed tx: BEGIN/RUN/PULL/COMMIT) | 79 µs |

These are close to Python's 12 / 34 / 64 µs. The driver adds about 30 µs per
statement. Most of that is the driver's own promise and observer machinery,
plus one `setImmediate` hop per request batch.

## How it plugs in

**The seam is neo4j-driver-core's `ConnectionProvider` + `Connection` pair.**
It is injected through the `@protected` `Driver` constructor. The
bolt-connection layer is *below* the seam:

```
neo4j.Driver (stock)                                       ← unchanged
  └─ core Session / Transaction / Result / ConnectionHolder ← unchanged
       └─ core ConnectionProvider.acquireConnection()       ← RelateConnectionProvider
            └─ core Connection.run/beginTransaction/…        ← RelateConnection
                 └─ bolt-connection ResultStreamObserver     ← reused as-is
                 ✗ bolt-connection ChannelConnection, BoltProtocol v4x4,
                   transformer, PackStream, channel          ← bypassed
```

Why this is the narrowest hydration-level seam:

- Everything at or below bolt-connection's `ChannelConnection` traffics in
  PackStream structs or bytes. The transformer hydrates structs into core
  types *inside* the protocol layer.
- A fake `channel` would therefore need PackStream encoding.
- A fake `BoltProtocol` would still need a `ChannelConnection`, a channel and a
  pool around it.
- Core's `Connection` is the first interface whose inputs are a query plus
  params and whose output is an observer that emits **already-hydrated**
  values.

This is the JS analogue of Java's `BoltConnectionProvider` SPI and Python's
`AsyncConnection` plus pool.

Python's provider wrote its own response handler. This one instead *reuses*
bolt-connection's `ResultStreamObserver`. The provider feeds it the Bolt 4.4
response shapes as JS objects:

- `onCompleted({fields, t_first})` for RUN;
- `onNext([…hydrated values])` for each record;
- `onCompleted({has_more:true})` or `onCompleted({type, stats, db, t_last,
  notifications, plan|profile})` for PULL.

So the driver's own code does fetch-size paging, the high/low watermarks, the
reactive pull, `consume()`→DISCARD and summary assembly.

The provider emulates two things the bypassed layer normally does:

- **Asynchronous, in-order responses.** Requests queue per connection and drain
  on `setImmediate`. Answering synchronously from inside the observer's
  `moreFunction` corrupts its state, because `_more()` sets `STREAMING` *after*
  calling it.
- **Failure handling.** On FAILURE, requests already queued behind the failure
  are IGNORED: they receive the same error, as `ResponseHandler` does. Then the
  implicit RESET runs, which `ChannelConnection._resetOnFailure` sends after
  every failure. That RESET rolls the engine transaction back.

**Entry points.** There are two:

- **`driver(uri, auth, config)`, the factory.** It builds
  `new neo4j.Driver({address, typename:'Direct', routing:false}, config,
  createProvider, undefined)`. It parses its own URI, so host-less forms work:
  - `relate://memory`
  - `relate::memory:`
  - `relate:///abs/dir`
  - `relate://localhost/abs/dir`
  - `relate:rel/dir`

  The fourth argument, `boltProvider`, can be `undefined`. Only the
  client-side encryption preview (`driver.encryption`) uses it, so that
  feature is unavailable.
- **`install()`, the monkey-patch the steer asked for.** It replaces
  `require('neo4j-driver').driver` and `.default.driver` with a wrapper that
  routes `relate:` URLs to the factory and everything else to the original.
  The driver's scheme `switch` is never patched; `uninstall()` restores the
  original. **Measured limits:**
  - It reaches CJS `neo4j.driver(…)` and ESM `import neo4j from
    'neo4j-driver'; neo4j.driver(…)`.
  - It does **not** reach ESM `import { driver }` or `import * as ns`, which
    get a snapshot of the CJS exports taken at import time.
  - It does not reach a CJS `const { driver } = require(…)` destructured before
    `install()`.

**Private and non-semver names touched:**

| Package | Name | Status |
| --- | --- | --- |
| `neo4j-driver` | `Driver` constructor `(meta, config, createConnectionProvider, boltProvider)` | `@protected`, "you should not be calling this directly" |
| `neo4j-driver` | the `driver` export, reassigned by `install()` | public export, mutated |
| `neo4j-driver-core` | `ConnectionProvider`, `Connection` (abstract base classes) | exported, but documented as internal |
| `neo4j-driver-core` | `internal.constants.FETCH_ALL`, `BOLT_PROTOCOL_V4_4` | `internal` namespace |
| `neo4j-driver-core` | `internal.serverAddress.ServerAddress.fromUrl` | `internal` namespace |
| `neo4j-driver-core` | `ServerInfo`, `newError`, `int`, the graph, temporal, spatial and `Vector` classes | public |
| `neo4j-driver-bolt-connection` | `ResultStreamObserver`: constructor options, `prepareToHandleSingleResponse()`, the `onNext`/`onCompleted`/`onError` feed protocol and the meta keys `fields`/`has_more`/`t_first`/`t_last`/`db`/`type`/`stats`/`plan`/`profile`/`notifications`/`statuses` | `@access private` |
| `neo4j-driver-core` | the call shapes core uses on a `Connection`: the `run`/`beginTransaction` config objects (`beforeError`, `afterComplete`, `onDb`, `reactive`, `fetchSize`, watermarks, …), plus `release()`, `hasOngoingObservableRequests()`, `isOpen()`, `getProtocolVersion()` and `resetAndFlush()` | undocumented contract between core and bolt-connection |

**Driver-version coupling.**

- The three packages release in lockstep with exact pins:
  `neo4j-driver@6.3.0` depends on `neo4j-driver-core@6.3.0` and
  `neo4j-driver-bolt-connection@6.3.0`.
- The provider must use **the same core instance the driver uses**.
  `instanceof Neo4jError`, `Record` and the graph classes are compared across
  modules. The spike relies on npm hoisting (`require('neo4j-driver-core')`
  resolves to the driver's copy).
- Under pnpm's strict layout, or with a second core version in the tree, a
  product must resolve `neo4j-driver-core` and `neo4j-driver-bolt-connection`
  *through* `neo4j-driver` (`require.resolve(…, {paths:[dirname(require.resolve('neo4j-driver'))]})`)
  *(inferred, not tested)*.
- `ResultStreamObserver` is the most private piece. The provider could carry
  its own observer (about 150 lines, the shape PR #1474's HTTP connection
  needs too) to drop the bolt-connection dependency, at the cost of
  re-implementing paging and watermarks.

## Messages the driver sent

Histogram across the whole suite:

`HELLO=28 BEGIN=230 RUN=462 PULL=462 COMMIT=226 ROLLBACK=1 DISCARD=1 RESET=1 FAILURE=14 IGNORED=5 RESET(on failure)=14 GOODBYE=28`

s12's 8×25 managed writes dominate it. Per-scenario message sequences are
asserted:

- `executeQuery`: HELLO, BEGIN, RUN, PULL, COMMIT.
- `session.run`: HELLO, RUN, PULL.
- `fetchSize=2` over 5 rows: RUN plus 3 PULLs.

ROUTE, LOGON/LOGOFF and TELEMETRY were never sent.

| Message | Status | Notes |
| --- | --- | --- |
| HELLO (`acquireConnection`, new connection) | works | No handshake. Auth is accepted and ignored. One `relate_connect_namespace` per pooled connection. `verifyConnectivity()` opens the store but borrows no connection. |
| GOODBYE (provider close) | works | Rolls back any open tx and calls `relate_disconnect`, then `relate_database_close`, so a file store reopens cleanly (s10). |
| RUN | works, with hacks | Params are hand-serialized to ADR-0087 JSON. The whole result is materialised at RUN. Errors are classified by message sniffing. |
| PULL n | works | Real `has_more` paging over the buffer: 3 PULLs for 5 rows at `fetchSize=2`. No memory or latency benefit. |
| DISCARD | works | Breaking out of `for await` and then calling `summary()`. |
| BEGIN | works | `database` → `setNamespace`. Bookmarks, tx timeout/metadata, mode and notification filters are ignored. |
| COMMIT | works, degraded | No commit sequence on the ABI → **no bookmark** (as in Java). |
| ROLLBACK | works | Idempotent when the engine tx is already gone. After a mid-tx FAILURE the driver sends **no** ROLLBACK, because the implicit RESET already did it. |
| RESET | works | Two kinds. **On failure:** emulated, 14 times, which is `ChannelConnection._resetOnFailure`. **On release:** sent only when requests are still pending or a tx is open (1 time). **Differs from Java**, where the pool RESET on every release (38/38). |
| FAILURE / IGNORED | works | FAILURE → `newError(msg, code)`. Requests queued behind a failure get the same error (IGNORED). |
| ROUTE / LOGON / LOGOFF / TELEMETRY | not applicable | A direct provider never routes. Session auth and impersonation are **refused** at acquire. `apiTelemetryConfig` is dropped (4.4 has no TELEMETRY). |

## Scenario results

1. **`executeQuery("RETURN 1 AS one")`** works:
   - `keys == ["one"]`, the value is an `Integer`, `queryType "r"`, and
     `database.name "neo4j"`.
   - `verifyConnectivity()`, `getServerInfo()` (`agent
     Relate/0.15.0-embedded`, `protocolVersion 4.4`, address
     `relate.embedded:7687`) and `supportsMultiDb()` all work.
2. **`session.run` auto-commit** works:
   - Counters are complete (`nodesCreated 2`, `relationshipsCreated 1`, …).
   - `containsUpdates()` is derived by the driver.
   - `queryType "rw"`, and `keys()` works.
3. **Managed transactions:**
   - `executeWrite` commits.
   - A mid-tx `RETURN 1/0` gives `Neo.ClientError.Statement.TypeError`, is not
     retried (attempts=1), and the tx's earlier `CREATE` is gone. The session
     recovers.
   - `foo(` gives `SyntaxError`; `$missing` gives `ParameterMissing`.
4. **Write-write conflict**, retried:
   - The interleaved conflicting commit fails the second committer at COMMIT
     with `write-write conflict`. The hack maps that to
     `Neo.TransientError.Transaction.Outdated`, so `executeWrite` **retried
     and succeeded** (attempts=2, final value 11: both increments applied).
   - Without classification, see Retry behaviour below.
5. **Explicit transactions:**
   - `beginTransaction`/`commit` work, and `rollback` leaves no trace.
   - After a mid-tx failure, `tx.run` rejects client-side ("rolled back"), and
     `close()` works.
6. **`fetchSize` paging + consume**: 3 PULLs for 5 rows. A partial read
   followed by `summary()` sends DISCARD.
7. **EXPLAIN/PROFILE:**
   - EXPLAIN gives a plan tree: `ProduceResults` → child with
     `arguments.Details "(a), (b)"`.
   - The `CartesianProduct` notification comes in the legacy 4.4 shape. The
     driver synthesises GQL status `03N42` from it.
   - **New in JS:** core never gates `statuses` on the protocol version, so the
     provider's `gqlStatuses` option passes the engine's own `03N90` through
     even at 4.4.
   - PROFILE gives `profile.rows`, `dbHits` and `time`.
8. **`database` routing:**
   - `session({database:'system'})` and `executeQuery(…, {database:'system'})`
     both run `SHOW DATABASES` and get 3 rows, with
     `summary.database.name "system"`.
   - `database:'nope'` raises `Neo.ClientError.Database.DatabaseNotFound`.
   - Default sessions stay on `neo4j`.
9. **Bookmarks:**
   - `lastBookmarks()` is `[]` after a committed write, and the
     `executeQueryBookmarkManager` stays empty.
   - A session created with `bookmarks:['relate:42']` is accepted and the
     bookmark is ignored. Its read sees the write, which is trivially true for
     one in-process store.
10. **File store** (`relate:///abs/dir`):
    - Data persists across driver close and reopen; the reopen used
      `relate://localhost/abs/dir`.
    - A second writable driver on the same directory is refused with
      `ServiceUnavailable` (`storage locked`).
    - **Finding:** `ServiceUnavailable` is retryable, so `executeQuery` on that
      second driver re-attempted the open for `maxTransactionRetryTime` and
      rejected only after **35–65 s**. The plain client refuses in 2 ms. A v1
      must classify "storage locked" deliberately: either a non-retryable
      client error, or a retryable one with documented intent.
11. **RxSession** works: `rx.run(…).records()` with `fetchSize:2` and
    `rx.executeWrite(…)`.
12. **Concurrency** works:
    - 8 async sessions × 25 read-modify-write `executeWrite`s end at exactly
      200.
    - 207 attempts in total: conflicts were classified and retried, with no
      lost update.
    - 8 engine connections were opened, one pooled connection per concurrent
      session.
    - This is cooperative interleaving on one JS thread, not parallel engine
      work (every call is synchronous).
13. **Monkey-patch, URIs, refusals:**
    - `install()` makes `neo4j.driver("relate::memory:", neo4j.auth.basic(…))`
      work.
    - `bolt://` still goes to the stock factory, and `uninstall()` restores
      `Unknown scheme: relate`.
    - Impersonation and session auth are refused with a client error.
14. **Integer config**: `useBigInt` gives a `BigInt`
    (`9007199254740993n`, exact), and `disableLosslessIntegers` gives a plain
    `number`.
15. **Top-level temporal detail:**
    - A named-zone datetime keeps `timeZoneId "Europe/Berlin"`, offset 7200
      and nanoseconds 123456789.
    - `duration('P1Y2M3DT4H5M6.5S')` comes back as months 14, days 3, seconds
      14706, nanos 500000000.
    - A WGS-84 point comes back as srid 4326 with x=1, y=2.

## Value fidelity (results)

Produced by `neo4j-spike/fidelity.js`. "Expected" is what the Bolt path
yields.

| Type | Expected | Top level | Nested in list | Nested in map | Node property |
| --- | --- | --- | --- | --- | --- |
| INTEGER | Integer | Integer | Integer | Integer | Integer |
| INTEGER > 2^53 | Integer | Integer (exact, via reviver **hack**) | Integer | Integer | Integer |
| FLOAT, FLOAT integral (`1.0`) | number | number (via reviver **hack**) | number | number | number |
| NaN / +Inf | number | number (**hack**: FLOAT stamp + `"NaN"`/`"Inf"` string) | **string** | **string** | **string** |
| STRING, BOOLEAN, NULL | as is | correct | correct | correct | correct (null property: absent) |
| LIST, MAP | Array, Object | correct | correct | correct | correct (LIST) |
| DATE, LOCAL TIME, TIME, LOCAL DATETIME, DATETIME (offset), DATETIME (zone) | core temporal | correct (**hack**: ISO re-typing via column stamp) | **string** | **string** | **string** |
| DURATION | Duration | correct (**hack**) | **string** | **string** | **string** |
| POINT (cartesian, WGS-84 3D) | Point | correct (**hack**: `srid`+`crs` sniffing) | correct | correct | correct |
| VECTOR | Vector | correct (**hack**: VECTOR stamp) | **Object** | **Object** | **Object** |
| PATH | Path | correct (**hack**: PATH stamp) | **Array** | **Array** | n/a |
| NODE / RELATIONSHIP | Node / Relationship | correct (**hack**: `kind` sniffing; element id re-spelled) | correct | correct | n/a |
| User map shaped like a node | Object | **Node** | **Node** | **Node** | n/a |
| DATE column, row 1 holds the STRING `'2020-01-02'` | string | **Date** (the stamp describes row 0 only) | | | |
| `elementId` round trip (`WHERE elementId(n) = $id`) | 1 match | 1 match (`4:rl-id:n22`) | | | |

This matches Java's table row for row. Python's typed binding fixes all of
these rows except durations and vectors. JS-specific detail:

- **Int vs float and int64 precision.** Java's Jackson reads these losslessly
  for free. In JS the public client's bare `JSON.parse` loses both, so the
  provider needs a lossless decoder, built with the `JSON.parse` reviver's
  `context.source`.
- **Element ids.** The provider re-spells the engine's own `elementId()` format
  (`4:rl-id:n<id>` / `5:rl-id:r<id>`), as Python did, so
  `elementId(n) = $id` round-trips. Real Bolt 4.4 sends no element id, and the
  driver would then expose `String(identity)`. That breaks the round trip
  against Relate's `elementId()`. **"Advertise 4.4" and "element ids that
  match `elementId()`" pull in opposite directions.** The ADR should pick the
  engine's format explicitly.

## Retry behaviour

| Case | Code the driver saw | Driver behaviour |
| --- | --- | --- |
| Interleaved same-node conflict, classified (message contains `conflict` → `Neo.TransientError.Transaction.Outdated`) | Transient | **Retried**: attempts=2, about 0.8 s backoff, final value 11. No lost update. 8 sessions × 25 ended exactly at 200 (207 attempts). |
| The same conflict **without classification** (`errorClassification.enabled=false`, so every engine error becomes `Neo.DatabaseError.General.UnknownError`, which is what the message-only ABI gives without sniffing) | DatabaseError | **Not retried**: attempts=1, `executeWrite` rejects, `isRetryableError()` is false. Final value 10: **the user's increment is lost** (surfaced as an error, never applied). Matches Python. |
| `RETURN 1/0` mid-tx | `Neo.ClientError.Statement.TypeError` | Not retried. The engine says `type error: / by zero`; Bolt would say `ArithmeticError`. |
| Syntax error, missing parameter | `SyntaxError`, `ParameterMissing` | Not retried |
| Second writer on a locked file store | `ServiceUnavailable` (provider's choice) | **Retried** by `executeQuery` for about 30 s+, re-attempting the open each time (s10 finding) |
| Any error | GQLSTATUS `50N42` | The driver's `newError` fallback. No GQLSTATUS crosses the ABI. |

## Parameters

| Parameter | `valueType($p)` | Comes back as | Notes |
| --- | --- | --- | --- |
| JS number `1` | FLOAT | number | **Correct driver semantics only via the hand-written serializer.** PackStream sends every JS number as a float. The client's `JSON.stringify` would send `1` → INTEGER. |
| JS number `1.5` | FLOAT | number | |
| `neo4j.int(1)`, `neo4j.int(2^62)`, `BigInt` 2^62 | INTEGER | Integer (exact) | Exact digits, beyond 2^53 |
| string, boolean, null, `undefined` | as is (`undefined` → NULL) | as is | |
| list `[1, 'a']` | `LIST<STRING NOT NULL \| FLOAT NOT NULL>` | Array | The 1 binds as a float, which is correct driver semantics |
| nested map with an `Integer` leaf | MAP | Object | |
| NaN, ±Infinity | **refused** | `Neo.ClientError.Statement.TypeError` | No params-JSON spelling (same as Java; Python binds them) |
| `neo4j.types.Date` | **STRING** | string | **Degraded**: the ISO string. `date($d)` recovers it. |
| `neo4j.types.DateTime` (zone) | **STRING** | string | Degraded. The zone id survives inside the string. |
| `neo4j.types.Duration` | **STRING** | string | Degraded |
| `neo4j.types.Point` | **MAP** | Object | Degraded. `point($p)` recovers it. |
| JS `Date` | **refused** | TypeError | The provider's choice: the driver's temporal types are the supported spelling |
| `Int8Array` (bytes), `neo4j.vector(…)` | **refused** | TypeError | No carrier |

## Corrections to the research report

- **The seam is core's `Connection`, and it needs bolt-connection's observer.**
  The report placed `ResultStreamObserver` at
  `core/src/internal/observers.ts:73`. That is only the TypeScript
  *interface*. The core runtime ships only `CompletedObserver` and
  `FailedObserver`. The only concrete result observer is bolt-connection's
  `@access private` `ResultStreamObserver`. So a provider either imports a
  private bolt-connection class (this spike) or writes its own observer.
- **`boltProvider` is not needed.** The report's
  `new Driver(meta, config, () => provider, boltProvider)` works with
  `undefined`. In 6.3, `boltProvider` feeds only the client-side encryption
  preview.
- **Monkey-patching works, with limits.** The report recommended "do not patch
  the module". The steer was to patch, and patching the `driver` export works
  for CJS and ESM default imports. It is invisible to ESM named or namespace
  imports and to pre-destructured CJS bindings (measured). The factory export
  remains the reliable entry point.
- **The response protocol has an ordering constraint the report did not
  mention.** Responses must be asynchronous: the observer sets its state
  *after* calling `moreFunction`, so a synchronous PULL answer corrupts it.
  And after any FAILURE, bolt-connection's `ChannelConnection` sends an
  implicit RESET itself (`_resetOnFailure`). The core layer relies on that:
  it sends no ROLLBACK after a mid-tx failure.
- **RESET is not sent on every release** (unlike Java's pool). `ConnectionHolder`
  resets only when `hasOngoingObservableRequests()` is true or a tx was open.
- **Protocol 4.4 is fine, and core does not gate on it.** Core's
  `ResultSummary` reads whatever `notifications`/`statuses` the metadata
  carries. The 4.4-vs-5.x choice is entirely the provider's, so the engine's
  GQLSTATUS for notifications can reach users at 4.4. That is not true for
  error GQLSTATUS, which needs ABI work.
- **6.x retries auto-commit too** (`disableAutoCommitRetries` defaults to
  false). Together with a retryable open error, this is why a locked store
  costs 30+ s instead of failing fast.
- **The threading inference was confirmed, and it is worse than inferred.**
  The synchronous koffi call blocks the event loop for the query's duration.
  The suggested fix, `fn.async`, **does not work with the current ABI**:
  `relate_last_error` is thread-local. When I ran `RETURN 1/0` through
  `query_params.async`, it returned rc=1, and `relate_last_error()` read on
  the main thread returned `null`. The message stays on the libuv worker
  thread.
- **The scheme switch is hard-coded, as reported.** `neo4j.driver("relate://…")`
  throws `Unknown scheme: relate` without the patch. Draft PR #1474's
  `http` case adds a second hard-coded arm rather than a registry.

## Requirements for the typed-value + error-code ABI ADR

✅ = hit in this spike, with the evidence above. Numbering follows the Java
report. JS matches Java on 1–8, refines 9 and 10, and adds 12–14.

1. ✅ **Typed result values at every depth.** Nested temporals, durations,
   NaN/Inf, vectors and paths are broken today; the fidelity table matches
   Java's row for row. **Same as Java.**
2. ✅ **Element ids** in the engine's `elementId()` format, with start and end
   element ids on relationships. The provider re-spells them; the round trip
   works only because of that hack. **Same as Java and Python**, plus the 4.4
   tension noted above.
3. ✅ **Unambiguous entity encoding.** A map shaped like a node hydrates as
   `Node`. **Same as Java.**
4. ✅ **Typed parameters.**
   - Temporal → string, point → map, NaN/bytes/vector refused: **same as
     Java.**
   - **JS refinement:** the params JSON *does* distinguish `1.0` (FLOAT) from
     `1` (INTEGER), but only a hand-written serializer can produce the
     difference. The typed builder must carry int vs float explicitly, plus
     int64 beyond 2^53.
5. ✅ **Error classification on the ABI** (Neo4j code + GQLSTATUS + the
   compile/runtime class). Without it, a write-write conflict is not retried
   and the increment is lost (measured). **Same as Java and Python.**
6. ✅ **Commit sequence → bookmark.** None today. **Same as Java.**
7. ✅ **Streaming/cursor.** `fetchSize` is cosmetic.
   - **JS adds:** materialisation is synchronous on the event loop, so a large
     result also stalls every other request in the process.
   - A cursor ABI plus off-thread calls (see 12) is what makes the async
     driver honest.
8. ✅ **Timing.** `t_first` is engine elapsed ms rounded; `t_last` is faked as 0.
   Low priority. **Same as Java.**
9. **Notification GQLSTATUS: already satisfied.** The engine's `gql_status`
   reaches `gqlStatusObjects` at 4.4, because core does not gate (measured,
   `03N90`). This refines Java's item 9: no ABI work is needed for
   notifications.
10. **Single-writer lease.** Same as Java: commits are optimistic, and the
    second committer fails. **New:** the *open* of a second writer fails fast
    at the ABI (2 ms). The provider must classify it deliberately, or the
    driver's retry turns it into a 30–65 s stall.
11. **Open-config knobs.** Only `readOnly` exists (the factory passes it
    through). Not hit.
12. ✅ **New: errors must travel with the call, not only through thread-local
    `relate_last_error`.** Any non-blocking JS binding (koffi `.async`,
    a worker pool, N-API async work) runs the call on another thread, and the
    message is then unreachable (measured: `null`). Options:
    - an error out-param on every fallible entry point
      (`relate_error **err`, owning code + GQLSTATUS + message);
    - an error-bearing result handle.

    This pairs naturally with requirement 5's code and GQLSTATUS.
13. ✅ **New: a lossless number representation**, if any JSON path survives the
    typed ABI. int64 beyond 2^53 and `1.0` vs `1` survive only if the
    consumer's JSON parser keeps the source text. A typed ABI with explicit
    INTEGER/FLOAT tags removes the question.
14. ✅ **New (client, not ABI): the plain koffi client cannot run off-thread
    today**, and the C ABI's "do not share a connection across threads" rule
    holds trivially only because everything runs on the main thread. Once 12
    lands, the client needs one serial executor per connection, the same model
    as Java's single-thread executor.

## JS-client gaps (the `DriverSeam` analogue)

The seam is an internal, non-enumerable export:
`require('@relate/relate')[Symbol.for('relate.driverSeam')]`.
It is about 30 lines in `index.js`, reuses the module's one set of koffi
bindings and the one `materializeResult` (now parameterised by a cell
decoder), and adds no new FFI symbol. It provides:

- `queryJson(conn, cypher, paramsJson, decodeCell)`. The public `query()`
  cannot express either half:
  - **Params:** it runs `JSON.stringify` over a plain object. It cannot spell
    a float-typed integral number (`1` → INTEGER, while driver semantics say
    FLOAT) or an int64 beyond 2^53. It also rejects every non-plain object,
    so the provider must pre-serialize anyway.
  - **Results:** it decodes cells with bare `JSON.parse`. **This is a
    pre-existing public-client bug.** `conn.query("RETURN 9007199254740993 AS x")`
    returns `9007199254740992`, and `RETURN 1.0` is indistinguishable from
    `RETURN 1`. It deserves its own ticket. The fix needs either a typed ABI
    or a reviver with `context.source`, and the reviver raises the client's
    `engines` floor from Node 18 to 21+.
- `disconnect(conn)` and `close(db)`. **The public client has no
  `Connection.close()` or `Database.close()`** (same as Java). The koffi
  bindings exist (`relate_disconnect`, `relate_database_close`), but
  nothing calls them. Without them a file store is never released: the
  reopen in s10 depends on `close(db)`.

Already public and sufficient (richer than Java's client): `columnTypes`,
`stats`, `plan` (plan/v1), `notifications` (with `gql_status`), `queryType`,
`namespace` and `elapsedMs`.

Missing, and worked around in the provider:

- **Errors are a bare `Error(message)`.** There is no kind and no class (Python
  at least has `.kind`), so classification is pure message sniffing.
- **No commit sequence** (requirement 6).
- **Synchronous only.** The event loop blocks, and async is blocked by
  requirement 12.
- **EXPLAIN returns `columns == []`.** Neo4j returns the statement's keys.
  Minor; same as Python.
- **No open options beyond `readOnly`.**

## Packaging recommendation

| | (a) Subpath export `@relate/relate/neo4j` inside the existing package | (b) Separate `@relate/rl-id-driver` package |
| --- | --- | --- |
| User story | One install. `import { driver } from '@relate/relate/neo4j'`, or `install()` | Two installs |
| Dead weight | None. The subpath is never loaded unless imported. Platform binaries stay in the per-platform optional deps, and the JS is a few KB. | None |
| Seam | Stays internal: same package, `Symbol.for` export, no cross-package contract | The `Symbol.for` seam becomes a cross-package API versioned on both sides (Python's argument) |
| Driver coupling | `neo4j-driver` as an **optional peer dependency** pinned to one minor (`~6.3.0`), plus a load-time version guard. Every driver minor needs a client release. | Versions track driver minors independently of the engine client |
| Core instance identity | Resolve `neo4j-driver-core` and bolt-connection *through* the user's `neo4j-driver`. Same in both options. | Same |

**Recommendation: (a), with an optional peer dependency and a load-time
version guard.** This follows Python's reasoning rather than Java's:

- npm has no classifier duplication, and the optional peer dependency costs
  non-users nothing.
- Keeping the seam private outweighs the release-cadence argument.

The coupling is real, though. The provider touches a `@protected` constructor,
an `@access private` observer, two `internal` namespaces, and an undocumented
core↔bolt-connection call contract, across three lockstep-pinned packages. So:

- Guard on `require('neo4j-driver/package.json').version` matching `6.3.x` and
  fail loudly otherwise.
- Consider owning the result observer to shed the bolt-connection dependency.
- Split into (b) only if driver churn starts to dominate engine-client
  releases.

The upstream ask is the same as in the research report: a scheme registry in
`neo4j-driver`'s `driver()` (PR #1474 is about to add a second hard-coded
arm). That would make both the `@protected` constructor and the monkey-patch
unnecessary.

## What I'd cut for a v1

Keep:

- core `ConnectionProvider` + `Connection` as the seam, with in-order
  asynchronous responses;
- FAILURE → IGNORED → implicit RESET;
- the direct pool, with auth accepted and ignored and `database` → namespace;
- Bolt 4.4, but with the engine's `statuses` passed through (it costs nothing
  and gives users the real GQLSTATUS);
- the plan/profile mapping;
- the factory as the primary entry point, with `install()` as an opt-in
  convenience documented with its ESM limits.

Cut or defer:

- **Every hack:**
  - column-stamp ISO re-typing;
  - `kind`/`srid` shape sniffing;
  - element-id re-spelling;
  - message-prefix error classification;
  - the reviver-based number decoding.

  v1 should wait for requirements 1–5, 12 and 13 rather than ship nested
  temporals as strings and retries that depend on message wording.
- **Unsupported features: refuse loudly instead of ignoring silently.** This
  covers impersonation and session auth (refused already), plus tx
  timeout/metadata and notification filters.
- **Bookmarks.** Document them as a no-op until the ABI returns the commit
  sequence. It is small; put it in the same ADR.
- **The async story.** Document that calls run on the event loop until
  requirement 12 lands and calls can move off-thread.
- **Real streaming.** Document "results are materialised; `fetchSize` pages a
  buffer".
- **Client-side encryption** (`driver.encryption`): unavailable, documented.
- **Relative-path URIs** (`relate:rel/dir`). Ship `relate://memory` and
  `relate:///abs/dir`.
- **Store-lock classification.** Decide it explicitly (see s10) before shipping.

## Files (branch `spike/labs-447-js-provider`, commit `d023bec46`)

- `clients/javascript/index.js`: the internal driver seam (`queryJson`,
  `disconnect`, `close`), with `materializeResult` parameterised by a cell
  decoder
- `clients/javascript/neo4j-spike/`:
  - `provider.js`: connection state machine, provider, `driver()` factory,
    `install()` monkey-patch, message histogram
  - `values.js`: lossless decoder, result hydration, params serializer (every
    hack marked)
  - `summary.js`: completion metadata, plan/profile, error sniffing (with the
    `errorClassification` switch)
  - `spike.test.js`: the 16 scenarios (`node --test`)
  - `fidelity.js`: produces the value and parameter tables
  - `bench.js`: per-statement overhead
- `clients/javascript/package.json` / `package-lock.json`: `neo4j-driver`
  `6.3.0` (exact, dev only) and the `test:neo4j-spike` script

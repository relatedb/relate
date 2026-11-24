# Extension system — analysis (2026-11-01)

**Question.** Should Relate offer a DuckDB-style extension system — one where an
extension can extend storage, engine, Cypher, surfaces and the CLI — and if so, how would
such extensions be loaded and linked, given Rust has no stable ABI?

**Answer in one line.** Yes, but not with one mechanism: the axis decides the mechanism.
No database in the survey extends storage/parser/engine through a dynamic ABI — DuckDB
keeps exactly those parts C++-only and version-locked, Postgres accepts recompiling per
major, SQLite and ClickHouse refuse them outright. What is worth copying is the *tiering*
and the governance around it, not a single universal plugin ABI.

Raw material: [`extension-system-findings.md`](extension-system-findings.md) (DuckDB,
SQLite, Postgres, the Rust loading landscape, and the cross-database survey), building on
the 2026-09-25 loader study (`plugin-system-*.md`).

---

## 1. What the prior art actually proves

| System | Storage | Engine/planner | Dialect | Surfaces | CLI | Mechanism |
| --- | --- | --- | --- | --- | --- | --- |
| **DuckDB** | C++ only, version-locked | optimizer/operator/parser hooks: **C++ only** | C++ only | functions/types/scans via C ABI | n/a | two tiers: stable C vtable (functions, types, scans, pushdown) + C++ `ExtensionLoader` |
| **Postgres** | table/index AMs, custom WAL rmgr | planner/executor hooks, custom scans | types/operators/casts | FDWs, logical decoding | n/a | C, **no ABI promise across majors**, recompile per major |
| **SQLite** | VFS only | **closed** (only `xBestIndex`) | closed | functions, collations, vtabs | n/a | C function-pointer table, "stable through 2050" |
| **Neo4j** | closed | closed | procedures/functions only | JAX-RS unmanaged extensions | n/a | JVM JARs, calendar-version-locked, no sandbox |
| **ClickHouse** | closed | closed | UDFs, wasm UDFs (2026) | bridge processes | n/a | no plugin API by deliberate choice |
| **Redis/Valkey** | closed | closed | commands + custom data types | n/a | n/a | one C header, API `"1"`, NULL-probing capability check |

Three lessons fall out:

1. **The stable ABI covers functions, types and scans — not structure.** DuckDB's
   `duckdb_ext_api_v1` is a 548-slot function-pointer struct; everything that needs new
   *structure* (`StorageExtension`, `ParserExtension`, `OptimizerExtension`,
   `OperatorExtension`) needs `DBConfig`, which the C ABI never exposes. The stable unit
   is "append a slot", so only things expressible as slots are stable.
2. **The parts that matter most to users are the parts nobody opens.** Postgres opens
   everything and pays with `PG_MODULE_MAGIC` recompiles and 16.8% pairwise
   incompatibility across 441 extensions (PVLDB 18, 2025). SQLite closes the parser and
   planner and is the most portable extension ecosystem in existence.
3. **Version coupling is a choice with a price.** DuckDB `C_STRUCT` = min-version ratchet
   (1.2.0 binaries load into 2.0; 1.5.6 ones are refused by 1.2.0–1.5.5); the Rust
   template opts into `C_STRUCT_UNSTABLE` v2 and therefore into **exact-version** coupling
   (`TARGET_DUCKDB_VERSION`). Either way the ecosystem is rebuilt by CI every release:
   395 community extensions, each pinned per codename.

## 2. Mechanism options for a Rust host

| Mechanism | Language reach | Safety | Per-call cost | Version coupling | Distribution |
| --- | --- | --- | --- | --- | --- |
| In-tree Rust, compile-time registry | Rust, in-repo only | None needed (trusted) | 0 (inlined) | none (one binary) | rebuild the binary |
| cdylib + C ABI vtable | C ABI only | None (trusted code) | ~34 ns | your own declared min-version | per-OS/arch, signed |
| `abi_stable` / `stabby` | Rust only | layout-checked at load | ~ns (+Drop cost) | crate major + rustc-ish | per-OS/arch |
| WASM core / component model | any wasm target | strong sandbox | ~24 µs (pooled lower) | WIT/WASI version | one `.wasm` |
| Out-of-process (stdio/IPC) | any language | OS process isolation | ~6–20 µs | message schema | any executable |

Notes that decide the choice: `abi_stable` is **unmaintained since 2023**; `stabby` is
SemVer-prime (72.x) and a niche per-type universe; **crABI is still an unmerged RFC**; a
raw Rust `dylib` requires the *exact* toolchain (Zenoh documents a SIGSEGV otherwise).
So "Rust extensions" in practice means *a C ABI boundary with Rust on the far side* — which
is exactly what `duckdb/extension-template-rs` does (`duckdb-neo` over the v2 C API,
`--abi-type C_STRUCT_UNSTABLE`).

## 3. Proposed tiering for Relate

Five tiers, each with its own contract. The tiers are ordered by how much trust and how
much coupling they demand — not by ambition.

### T0 — In-tree Rust behind feature bundles (storage, engine, Cypher, surfaces)

A trait family plus a build-time registry, selected by cargo features, extending the
existing `full` / `agent` / `minimal` precedent (ADR-0104) and the crate-group boundaries
(`cargo xtask check-boundaries`). This is where storage backends, planner/executor
operators and surface adapters belong, because they need zero-copy typed access and must
preserve invariants the TCK and the boundary rules exist to protect.

- **Loading:** none. Compile-time.
- **Why first:** it is what DuckDB's C++-only tier is, minus the ABI risk; it is already
  how the repo ships optional capability; and it costs no new trust surface.
- **Dialect caution:** keep the **parser closed** (SQLite/DuckDB precedent). "Extending
  Cypher" = procedures, functions and clauses gated behind `RELATE_PREVIEW`
  (ADR-0371) — never a per-install grammar, or the TCK floor stops meaning anything.

### T1 — Versioned C ABI vtable for third-party in-process extensions

DuckDB's model, adopted deliberately: an append-only function-pointer struct with a
**declared minimum host API version**, an ABI-type stamp and signature in a binary footer,
`dlopen` + a versioned entrypoint symbol, and **no unload**.

- **Covers:** scalar/aggregate/table functions, user-defined types and casts, replacement
  scans, config options. In DuckDB terms, the `C_STRUCT` band.
- **Rust children:** cdylib through the C boundary, with a published Rust wrapper crate;
  never a Rust ABI.
- **Mandatory design decisions up front:** panic policy (catch at every entry point; a
  panic must become a typed refusal, never a poisoned store — DuckDB's own template warns
  that an escaped Rust panic invalidates the database); allocation ownership ("the side
  that allocated frees", or an allocator in the host table); capability probing for
  forward compatibility (Redis: absent function slots are NULL).
- **Trust:** reuse the per-user trust store and signing from ADR-0449; loading off by
  default; `allow_unsigned` explicit and startup-only.

### T2 — WASM for untrusted and agent-generated code

The 2026-09-25 loader study's recommendation, now with measured numbers: ~24 µs per
component call versus ~34 ns for a cdylib — the same band as localhost HTTP, fine for
loaders and UDFs, wrong for storage or per-row engine paths. Prefer the component model
with WIT contracts; Extism is the pragmatic fallback (Helm 4 chose it). Keep it out of
storage/engine by construction.

### T3 — Out-of-process children (what shipped)

The command extensions (ADR-0429/0431/0434/0442/0447) are this tier already, and it is the
right home for two more things: **network-surface adapters** (Bolt/HTTP/MCP shapes) and
**CLI subcommands** — Grafana's backend-plugin model (subprocess over gRPC, signed
manifest, protocol majors side by side). Crash isolation and language freedom with a
message-schema version rather than a compiler version.

### T4 — Governance across all tiers

- **Manifest with contribution points and activation events** (VS Code): declare what a
  package contributes, activate lazily.
- **Per-capability stability labels** (OTel collector): development/alpha/beta/stable,
  with the promotion bar written down.
- **Typed configs, fail-closed loading** (Envoy): an unknown contribution point refuses
  rather than silently ignoring.
- **Signed artifacts + fingerprint pinning** (DuckDB 2.0 repositories): keys pinned from a
  well-known file, printed as fingerprints, verified on install *and* every load.

## 4. Constraints this repo imposes

| Constraint | Consequence |
| --- | --- |
| No stable Rust ABI; `abi_stable` stale, `stabby` niche, crABI unmerged | T1 must be a C ABI; a "Rust-native" plugin ABI would mean exact-toolchain coupling (Zenoh's cautionary tale) |
| Single-writer store; WAL + Parquet/JSON codecs are a pinned contract | A third-party *storage* backend makes the format a public promise. SQLite promises it to 2050; Postgres refuses to. Decide only with that weight in view |
| TCK floor = 3892 pinned scenarios | Closed parser; extensions add functions/procedures, never grammar |
| MSRV 1.99, pinned clippy, feature bundles | Any new tier ships feature-gated with matching `--help`/bundle tables |
| ADR-0434 R7: extension children never link engine or surfaces | A T1 in-process tier deliberately inverts R7 for a *new* group; that needs its own ADR and boundary rules |
| `full`/`agent`/`minimal` bundles already exist | T0 is an extension of shipped machinery, not a new concept |

## 5. Phased path

| Phase | Work | Gate to proceed |
| --- | --- | --- |
| 0 | Ship the T3 command extensions as they are; add "when to embed, when to extend" to `docs/api/extensions/description.md` | — |
| 1 | **T0 registry**: a Rust trait family for procedures/functions, feature-gated, registered at build time; `CALL`-reachable | TCK still 3892/3892 with the feature on and off |
| 2 | **T1 vtable v1**: scalar/aggregate/table functions + UDTs; footer + signature + min-version; one C extension and one Rust (cdylib) extension as fixtures; measure boundary cost and pin the panic policy | A third-party extension survives a host upgrade that bumped a non-stable slot |
| 3 | Storage/engine hooks — expect Rust-only, exact-version, feature-flagged (the DuckDB C++ tier), not a stable ABI | Only after the format-stability question has an ADR |
| 4 | WASM tier for untrusted/agent UDFs, with fuel and memory limits | T2 prototype measures within budget on a loader workload |

## 6. What not to do

- **No raw Rust `dylib` + Rust types.** Documented SIGSEGV class; the whole point of a
  boundary is that it is checkable.
- **No stable ABI for storage before the on-disk format is frozen.** Postgres' custom-WAL
  warning (a preloaded rmgr that disappears can stop the server starting) is the shape of
  the failure.
- **No autoload of behavior-modifying extensions** (DuckDB restricts autoload to a core
  allowlist for exactly this reason).
- **No sandbox claims for in-process native code.** Signing proves provenance, not safety
  — DuckDB says so in its own docs.
- **Don't budget only the happy path.** An extension ecosystem is a permanent CI and
  versioning tax: 395 packages rebuilt per release on DuckDB's side, 16.8% pairwise
  incompatibility on Postgres'.

## 7. Refinement (2026-11-01): an embedded API moves the T1/T2 line

Recorded after discussion on LABS-683 (in-process compute for embedded agent workloads).
The tiering above holds, but the boundary between T1 and T2 is not native-vs-WASM — it is
**what the plugin touches**, and a well-shaped embedded API moves most of the weight to T2.

| Plugin shape | Needs | Tier |
| --- | --- | --- |
| **Transform-only** — scalar UDF, predicate, aggregate step, chunker, embedder, geo predicate | values in, values out | **T2** |
| **Stateful-but-opaque** — table function with bind data, host-built index behind a handle | handle + batched host functions | **T2**, if the API is designed for it |
| **Hold-or-mutate host state** — storage backend, index, planner rule, operator with state over host pages | direct access to host structures, or a host-function protocol so chatty the boundary dominates | **T1** |

**Why transform-only collapses into T2.** The WASM penalty is *per call*: ~24 µs per
component call, ~34 ns for a cdylib. With a **batch-oriented** API (Arrow-style: N rows
per call, opaque handles instead of pointers) a 1000-row batch costs ~24 ns/row —
parity with a native call. T2 then wins outright: one `.wasm` artifact for every OS/arch,
a real sandbox, no ABI coupling, no per-release rebuild of the ecosystem.

**The design consequence.** The embedded API must be value/batch-oriented with opaque
handles and deny-by-default host functions; never raw pointers across the boundary. Get
that shape right and T2 absorbs most of what people assume needs T1. The residue — where
T1 survives — is exactly the set DuckDB keeps C++-only and version-locked (storage,
parser, optimizer, operators).

**Language targets for T2.** AssemblyScript for the JS/TS crowd; Rust via
`wasm32-wasip2` / the component model; Python via **componentize-py** (Bytecode Alliance:
CPython-in-wasm with WIT bindings). Pyodide only when the full scientific stack is
required — tens of MB and a slow cold start, which makes instance pooling a requirement
rather than an optimisation.

**Trust answers itself.** T2's sandbox is real (linear memory, deny-by-default imports, a
trap kills the instance and not the host); T1 cannot be sandboxed at all — signing proves
provenance, not safety. So anything untrusted or agent-generated is T2 by construction.

**Cheap intermediate worth trying first — "T0.5" vendored plugins.** An external crate
pulled by path/git and compiled into the binary behind a cargo feature: no ABI, no version
skew, rebuild to update. It delivers most of T1's value for trusted first- and third-party
code at zero ABI risk, and it needs no vtable, footer, signing or version ratchet. Sequence
it between T0 and T1.

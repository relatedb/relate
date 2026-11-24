# Browser/WASM client: @relate/relate-wasm

## Install

```sh
npm install @relate/relate-wasm
```

The npm packages are pending their first publish (the docs/PUBLISHING.md
runbook); until then build the bundle from a repo checkout with
`make build-wasm` (writes `surfaces/relate-wasm/pkg/`). The package is a
wasm-bindgen `--target web` ESM bundle.

## Minimal usage

```js
import init, { RelateDb } from "@relate/relate-wasm";

await init();

const db = new RelateDb(); // in-memory database
const result = JSON.parse(db.execute("RETURN 1 AS n", null));
// result.columns: string[]                    // ["n"]
// result.columnTypes: string[]                // ["INTEGER"]
// result.rows: Array<Record<string, unknown>> // [{ n: 1 }]
// result.stats: { nodes_created, ... }
// result.notifications: Array<Record<string, unknown>> // [] on a plain run
// result.error: string | null                 // null on success
```

`execute(query, paramsJson, metadataJson?)` runs one statement; `paramsJson` is a JSON
object string, or `null`/blank for none. A statement that references a
parameter missing from it fails with `ParameterMissing: Expected
parameter(s): <names>` (no `null` binding). `metadataJson` (ADR-0610)
is the per-transaction metadata map, a JSON object string decoded
exactly as params and seeded before the statement — but see the
metadata gotcha below: on the wasm build it is always the authored
preview refusal. `executeScript(script, paramsJson)`
runs several and returns one envelope per statement.

## Gotchas

- **`await init()` before any use** — it fetches `relate_bg.wasm`
  relative to the module. Bundlers that move assets need the URL handed in
  explicitly. With Vite:
  `import wasmUrl from "@relate/relate-wasm/relate_bg.wasm?url"`
  then `await init(wasmUrl)`. In Node, `fetch` has no `file:` scheme — read
  the `.wasm` from disk and pass the bytes to `init()`.
- **TypeScript lib set** — the generated `relate.d.ts` declares
  `Symbol.dispose` (wasm-bindgen's Disposable support) and the browser
  globals `init()` accepts, so checking it without `skipLibCheck` needs
  `"lib": ["esnext", "dom"]`; with `skipLibCheck` on, the types resolve
  under any modern lib set.
- **Wire-JSON envelope** — `execute` returns a **string**; always
  `JSON.parse` it. Rows are column-keyed objects (`[{ n: 1 }]`), not
  positional arrays. Engine errors (parse, planning, execution) flow through
  `error` in the envelope — they do not throw; check `result.error`.
- **`notifications` is always there** — the envelope carries an ADR-0363
  `notifications` array (`code`, `gql_status`, `severity`, `category`,
  `title`, `description`, `remedy`, plus `operator_id` on operator-scoped
  kinds). It is empty on a plain run and holds the plan-time list on
  `EXPLAIN`/`PROFILE`, e.g. `Neo.ClientNotification.Statement.CartesianProduct`
  for a disjoint multi-pattern `MATCH`. `renderResult` ignores the key.
- **OPFS is worker-only** — `await RelateDb.open("opfs://<name>")` is the
  durable browser store (`<name>`: one segment, 1–64 chars of
  `[A-Za-z0-9_-]`) and only runs in a dedicated worker
  (`FileSystemSyncAccessHandle`); on the page thread it throws.
  `new RelateDb()` (`:memory:`) may run on the page thread.
- **`isWasmTrap` for poisoned instances** — a Rust panic traps the module
  (JS sees a `WebAssembly.RuntimeError`) and the instance is dead: replace
  it (new worker or page reload). Classify with `isWasmTrap` from
  `@relate/relate-wasm/trap.js`; false means the instance is still
  usable.
- **The metadata argument is always refused on wasm** — `execute`'s
  third argument (ADR-0610) seeds the per-transaction metadata map, but
  the machinery is the `value-provenance` preview opt-in's and
  `RELATE_PREVIEW` is read once, at open, from the process environment —
  which a wasm host does not have. Every present map answers the
  envelope's `error` with the authored
  `preview feature 'value-provenance' is not enabled: set
  RELATE_PREVIEW=value-provenance` and the statement never runs; omit
  the argument (or pass `null`/blank) for today's behavior. A map that
  is not a JSON object refuses with `metadata must be a JSON object`.
- **No byte arrays** — the engine's byte array (ADR-0327) is Bolt-only and
  statement-scoped; were one in a result it would arrive as a list of
  integers 0–255 with column type `BINARY`, and it can never be stored as a
  property.
- **JSON snapshots, single-threaded** — no Parquet codec and no background
  checkpointing on wasm32; `:memory:` and OPFS both use the JSON codec.

Full contract: `docs/api/wasm/spec.md` (API, build pipeline, wire envelope).

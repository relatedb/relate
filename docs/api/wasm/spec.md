# WASM package — spec

Package: `relate-wasm` (`surfaces/relate-wasm`), published as
`@relate/relate-wasm` on npm (ADR-0278), built by a
`cargo build --profile release-wasm` + `wasm-bindgen --target web` +
`wasm-opt -Oz` pipeline (ADR-0084 reinstates the surface, ADR-0100
defines the size-tuned build). Reinstatement of the browser surface
first defined in ADR-0051.

## Install

```sh
npm install @relate/relate-wasm
```

```js
import init, { RelateDb } from "@relate/relate-wasm";

await init();

const db = new RelateDb();
const result = JSON.parse(db.execute("RETURN 1 AS n", null));
// result.columns: ["n"]
// result.columnTypes: ["INTEGER"]
// result.rows: [{ n: 1 }]
// result.stats: { nodes_created: 0, ... }
// result.plan: null                     // the relate.plan/v1 document (ADR-0381)
// result.notifications: []              // always present; plan-time list on EXPLAIN/PROFILE
// result.elapsed_ms: number
// result.error: null
```

The published package is the `pkg/` bundle the Build section below
produces; building from source is only needed for development.

Constraints (ADR-0085, ADR-0145):

- **`:memory:` by default.** `new RelateDb()` opens an in-memory
  database. Filesystem paths and IndexedDB are not accepted.
  `opfs://<name>` is the durable browser store (ADR-0145): a dedicated
  worker only, via `FileSystemSyncAccessHandle`. `Database::open` on
  the engine still rejects that scheme.
- **JSON snapshot codec.** The wasm32 build has no Parquet codec. Both
  the in-memory database and an OPFS store use `SnapshotCodecKind::Json`.
- **Single-threaded.** No background checkpoint timer. `:memory:` calls
  may run on the page thread. OPFS calls run in a dedicated worker.

## Build

```sh
# repo-root, ships-to-users pipeline (release):
make build-wasm
bash scripts/build-wasm.sh --mode release  # same thing

# fast iteration (dev, ~1.5× larger, wasm-opt -O2):
bash scripts/build-wasm.sh --mode dev

# from examples/wasm-playground/:
npm run build:wasm          # alias of build:wasm:dev
npm run build:wasm:release  # explicit release build
npm run build               # vite build only; run build:wasm:release
                            # first if you touched the wasm crate
```

All entry points call `scripts/build-wasm.sh`. In `--mode release`
(default) it runs `cargo build --profile release-wasm --target
wasm32-unknown-unknown` on `surfaces/relate-wasm`, then
`wasm-bindgen --target web`, then `wasm-opt -Oz`, and writes the bundle
to `surfaces/relate-wasm/pkg/`. In `--mode dev` it uses the workspace
`release` profile and `wasm-opt -O2`. The script also mirrors the bundle
into `examples/wasm-playground/src/wasm/` when that directory is present
(`relate.js`, `relate.d.ts`, `relate_bg.wasm`,
`relate_bg.wasm.d.ts`, `trap.js`, `trap.d.ts`, `opfs.js`,
`opfs.d.ts`, and the `snippets/` directory the glue imports), so the
playground's Vite build always picks up a fresh bundle.

Local prerequisites (CI installs these automatically):

- `wasm-bindgen-cli` matching the crate version in `Cargo.lock`
  (currently 0.2.128): `cargo install wasm-bindgen-cli --version 0.2.128 --locked`
- `binaryen` 110 or newer (provides `wasm-opt`): `brew install binaryen`
  (macOS) or `bash scripts/install-binaryen.sh DIR` (Linux x86_64 and aarch64; unpacks
  the release CI pins into `DIR`, then add `DIR/bin` to `PATH`). Ubuntu
  24.04's apt package is binaryen 108, whose `wasm-opt` breaks the dev
  bundle's externref table (it fails at init with
  `WebAssembly.Table.grow(): failed to grow table by 4`), so the script
  refuses anything older than 110.

## JavaScript API

```js
import init, { RelateDb, renderResult } from "./relate.js";
import { isWasmTrap } from "./trap.js";
await init();

const db = new RelateDb();
const wireJson = db.execute("RETURN 1 AS n", null);
const result = JSON.parse(wireJson);
// result.columns: string[]
// result.columnTypes: string[]             // engine-stamped per-column type names (ADR-0224)
// result.rows: Array<Record<string, unknown>>   // e.g. [{ n: 1 }]
// result.stats: { nodes_created, ... }
// result.plan: Record<string, unknown> | null // ADR-0381; null on a plain run
// result.notifications: Array<Record<string, unknown>> // ADR-0363; [] on a plain run
// result.elapsed_ms: number                // engine-reported statement ms (ADR-0221)
// result.error: string | null              // null on success
```

`new RelateDb(readOnly?)` — opens `:memory:` and takes one
`Connection`. `readOnly` true loads once and refuses writes (ADR-0136);
omitting it, or passing false, stays writable, so `new RelateDb()` is
unchanged. Throws a JS `Error` if the underlying open fails.

`await RelateDb.open(uri, readOnly?)` — opens a durable OPFS database
(ADR-0145). `readOnly` true loads once and refuses writes; omit it to stay
writable. `uri` is `opfs://<name>` with `<name>` one segment of 1–64
characters from `[A-Za-z0-9_-]`. The call must run in a dedicated
worker: on the page thread it throws an error that says OPFS needs a
dedicated worker. The worker prepares a catalog file and 64 slot files
under the origin's private directory (`relate/<name>/`) and then
opens them through `Database::open_vfs`. Each acknowledged commit uses
`Full` durability, so a reload replays the WAL even though terminating
the worker skips Rust destructors. A second agent that still holds the
catalog handle fails with `storage locked: opfs://<name>`. An exhausted
slot pool throws a storage error. Any other `uri` throws.

`execute(query: string, paramsJson: string | null): string` — runs one
Cypher statement and returns the `relate_model::wire` JSON
envelope as a string (same envelope as every other Relate
surface); it is `relate::execute_wire`, the statement step of
`executeScript`. `paramsJson` is a JSON object string, or `null` or blank
for none, decoded as every binding decodes parameters
(`relate::params_from_json`): each
nested object, even one with a `__type` key, is an ordinary map; build
typed values in Cypher (`point($p)`, `date($d)`). Engine errors (parse,
planning, execution, invalid params) flow through `error` in the
envelope. A Rust **panic** on wasm32 traps the module (`panic = abort`
is the wasm target default), which JS observes as a
`WebAssembly.RuntimeError`; the instance is unusable after that and
must be replaced (a new worker, or a page reload). Test for it with
`isWasmTrap` below.

`reset(): void` — drops the current database and connection. An
in-memory handle opens a fresh `:memory:` database. An OPFS handle
wipes that `opfs://` name and reopens it, so a later reload stays
empty. Throws on open or wipe failure.

`executeScript(script: string, paramsJson: string | null): string[]` —
runs a script of one or more statements (ADR-0131) and returns one wire
envelope (JSON text) per statement run, in order. It is
`relate::run_script_wire`: statements come from the canonical scanner
the CLI shell and the TCK runner use, so a `;` inside a `'…'`/`"…"`
string, a `` `…` `` identifier, or a `//` / `/* */` comment is not a
terminator, and comment-only pieces are skipped. Every statement gets the
same `paramsJson`. The run stops after the first envelope whose `error`
is not null; each statement auto-commits, so the ones before it stay
applied. A blank or comment-only script returns `[]`; parameters that do
not decode return one error envelope and run nothing. A CLI dot-command
line (`.begin`) is a statement of its own and fails as a syntax error.

```js
const envelopes = db.executeScript("CREATE (:A); CREATE (:B)", null).map((e) => JSON.parse(e));
const last = envelopes[envelopes.length - 1];
if (last.error) throw new Error(last.error);
```

`renderResult(envelopes: string[]): string` — a free function
(ADR-0131, ADR-0132) that renders a script's envelopes, as
`executeScript` returns them, as a **recording**: JSON text
`{kind, text, table, graph, stats}`.

- `kind` is `"table"` or `"gram"` (gram when a row holds a node,
  relationship, or path).
- `text` is the canonical text form of the result (what the cookbook
  records under each step): the last statement's result, with the counters
  summed over the script.
- `table` is `{columns, rows, numeric, summary}` for a table with rows
  (each cell a Cypher literal, `numeric` true for columns of numbers and
  nulls), and `null` for gram or a result without rows.
- `graph` is `{nodes, rels}` for gram: every node and relationship in the
  rows (inside paths, lists, and maps too), each once, in the order met, as
  the wire has them; `null` for a table.
- `stats` holds all eleven counters, summed over the script (zero when
  unchanged), with Neo4j's counting (ADR-0309): labels per node, one
  `properties_set` per property write, schema DDL in the index and
  constraint counters.

No envelopes render as `(no rows)`. It is `relate_formatter::recording`.
Throws on an envelope that is not wire JSON.

`isWasmTrap(err: unknown): boolean` — exported by `trap.js`, which
`scripts/build-wasm.sh` ships next to `relate.js`. True when `err`,
thrown by a call into the bundle, means the instance trapped and must be
replaced; false for any other error, after which the instance is still
usable. The playground uses it, as do the cookbook pages.

## Examples

Runnable examples live in [`examples/`](examples/): `return_one.js`
(one round-trip), `execute_script.js` (a multi-statement script and a
failing one), and `render_result.js` (a recording). CI runs them against
the release bundle (the `wasm` job):

```sh
bash scripts/build-wasm.sh --mode dev
node surfaces/relate-wasm/tests/examples.mjs [PKG_DIR]   # default: surfaces/relate-wasm/pkg
```

## Wire envelope

**Byte arrays (ADR-0327).** The engine's statement-scoped byte array enters only through Bolt or a Rust `Value::Bytes` parameter, never through this surface's own parameters. A result value holding one renders as a list of integers 0–255 (the `value_to_cypher` crossing) and its column stamps `BINARY`; byte arrays cannot be stored as properties.

Every result is the standard JSON envelope. Rows are column-keyed objects
(same shape as the C ABI / language clients), and the engine-stamped
per-column type names ride beside `columns` (`columnTypes`, ADR-0224).
Success:

```json
{
  "columns": ["n"],
  "columnTypes": ["INTEGER"],
  "rows": [{"n": 1}],
  "stats": { "nodes_created": 0, "labels_added": 0, "properties_set": 0,
             "relationships_created": 0, "nodes_deleted": 0,
             "relationships_deleted": 0 },
  "notifications": [],
  "elapsed_ms": 0.041,
  "parse_ms": 0.012, "plan_ms": 0.008, "execute_ms": 0.021,
  "lex_parse_ms": 0.006, "rewrite_ms": 0.001, "lower_ms": 0.005,
  "result_generation_ms": 0.0002, "plan_cache": "miss",
  "error": null
}
```

`notifications` is always present and follows the ADR-0363 native shape:
each entry carries `code`, `gql_status`, `severity`, `category`, `title`,
`description` and `remedy`, plus `operator_id` on operator-scoped kinds. It
is empty on a plain run (the engine derives notifications only where an
`EXPLAIN`/`PROFILE` plan tree is built); an `EXPLAIN` envelope carries the
plan-time list, e.g. the `Neo.ClientNotification.Statement.CartesianProduct`
entry. `elapsed_ms` is the engine-reported statement elapsed (ADR-0221),
with its ADR-0491 phase split (`parse_ms`/`plan_ms`/`execute_ms`) and the
LABS-1120 fine sub-split (`lex_parse_ms`/`rewrite_ms`/`lower_ms`,
`result_generation_ms`) beside it, plus the `plan_cache` outcome string
(all `0` fine parse windows on a cache hit, which the outcome explains).
The renderer in the previous section (`renderResult`) ignores the extra
envelope keys; it classifies rows from `columns` and `rows`.

Error:

```json
{ "columns": [], "rows": [], "stats": {}, "error": "Syntax error at line 1: ..." }
```

# @relate/relate-wasm

Relate — an embedded, in-memory property graph engine with an openCypher
query front-end — compiled to WebAssembly for the browser.

```sh
npm install @relate/relate-wasm
```

## Usage

```js
import init, { RelateDb } from "@relate/relate-wasm";

await init();

const db = new RelateDb(); // in-memory database
const wireJson = db.execute("RETURN 1 AS n", null);
const result = JSON.parse(wireJson);
// result.columns: string[]                  // ["n"]
// result.rows: Array<Record<string, unknown>> // [{ n: 1 }]
// result.stats: { nodes_created, ... }
// result.error: string | null               // null on success
```

`execute(query, paramsJson)` runs one Cypher statement and returns the
standard Relate wire envelope as a JSON string. `paramsJson` is a JSON
object string, or `null`/blank for none. Engine errors (parse, planning,
execution) flow through `error` in the envelope; they do not throw.

### TypeScript

The generated `relate.d.ts` declares `Symbol.dispose` (wasm-bindgen's
Disposable support) and the browser globals `init()` accepts
(`RequestInfo`, `Response`, `WebAssembly`), so a tsconfig that checks
declaration files (`skipLibCheck` off) needs `"lib": ["esnext", "dom"]`.
With `skipLibCheck` on — the common default — the types resolve under any
modern lib set.

### Bundlers: passing the .wasm URL

`await init()` fetches `relate_bg.wasm` relative to the module, which
works when the file is served next to the bundle. Bundlers that move assets
need the `.wasm` URL handed to `init` explicitly. With Vite, for example:

```js
import init, { RelateDb } from "@relate/relate-wasm";
import wasmUrl from "@relate/relate-wasm/relate_bg.wasm?url";

await init(wasmUrl);
```

### Traps: isWasmTrap

A Rust panic on wasm32 traps the module, which JS observes as a
`WebAssembly.RuntimeError`. The instance is unusable after that and must be
replaced (a new worker, or a page reload). The shipped `trap.js` exports the
classifier:

```js
import { isWasmTrap } from "@relate/relate-wasm/trap.js";

try {
  db.execute("RETURN 1", null);
} catch (err) {
  if (isWasmTrap(err)) {
    // the instance is dead; replace it
  } else {
    throw err;
  }
}
```

### Durable storage: OPFS

`opfs://<name>` is the durable browser store. It only works in a dedicated
worker (it uses `FileSystemSyncAccessHandle`), so run the database there:

```js
// inside a dedicated worker
import init, { RelateDb } from "@relate/relate-wasm";

await init();
const db = await RelateDb.open("opfs://my-graph");
```

`<name>` is one segment of 1–64 characters from `[A-Za-z0-9_-]`. On the page
thread the call throws an error saying OPFS needs a dedicated worker.
`new RelateDb()` (in-memory) may run on the page thread.

## License

Apache-2.0

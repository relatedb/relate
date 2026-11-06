# WASM package — description

`relate-wasm` is the browser-embeddable build of Relate: a
`wasm-bindgen` wrapper around `relate::Database`, built by
`scripts/build-wasm.sh` as a `--target web` bundle (ADR-0084,
ADR-0100). It ships on npm as `@relate/relate-wasm` (ADR-0278);
the in-tree build remains for development and for hosts that copy the
bundle. It is the
smallest surface for running Cypher in a page with no server.

`new RelateDb()` is still `:memory:` (ADR-0051). A dedicated worker
can `await RelateDb.open("opfs://<name>")` (ADR-0145): a synchronous
slot-pool VFS on Origin Private File System sync access handles, JSON
snapshots, no background threads (ADR-0085). IndexedDB and native
filesystem paths are not a wasm store.

`execute` runs one statement; `executeScript` runs a script of several
with the engine's own runner (ADR-0131), which splits with the same
scanner as the CLI and stops at the first error. `renderResult` turns a
script's result into a recording (a `table` or `gram` text, ADR-0132),
the canonical text form a host keeps for a result, so it reads the same
wherever it is rendered. `isWasmTrap`
(in `trap.js`, shipped with the bundle) tells hosts when a call trapped
the instance.

Beyond the snippets in `examples/`, nothing in this repository consumes
the bundle. The relate-web repository copies the built bundle (`surfaces/relate-wasm/pkg/`) and serves
it to its home page console, its cookbook pages (ADR-0528), and the
playground (ADR-0530). The cookbook pages run recipe steps through the
bundle in a Web Worker, so a runaway query can be stopped by terminating
the worker, and record each step's result with `renderResult`; the
playground opens the default in-memory database in a worker. They are
consumers, not the product.

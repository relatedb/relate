# Relate JS Playground

Browser playground for running Relate in-process via WebAssembly, with
Neo4j NVL for graph visualization. Publishing moved to the relate-web
repository with the rest of the website (ADR-0546).

## Stack

- Rust `relate-wasm` crate, built by `scripts/build-wasm.sh`
  (cargo + `wasm-bindgen --target web` + `wasm-opt`, ADR-0100)
- Vite + React 19 + TypeScript
- Neo4j Needle (`@neo4j-ndl/react`), `@neo4j-nvl/react` for the graph view,
  `@neo4j-cypher/react-codemirror` for the editor

## Build

```sh
# One-time prerequisites (see docs/api/wasm/spec.md):
#   cargo install wasm-bindgen-cli --version <Cargo.lock version> --locked
#   brew install binaryen        # Linux: scripts/install-binaryen.sh (apt's is too old)

# From this directory:
npm install
npm run build:wasm     # dev build of surfaces/relate-wasm -> src/wasm/
npm run dev            # http://localhost:5173
```

`build:wasm` shells out to [`scripts/build-wasm.sh`](../../scripts/build-wasm.sh),
the shared entry `make build-wasm` and CI also use (ADR-0084). It writes the JS
glue and `.wasm` blob into `surfaces/relate-wasm/pkg/` and copies them into
`examples/wasm-playground/src/wasm/`, with `trap.js` (the shared
`isWasmTrap`). That copy also overwrites the tracked `src/wasm/relate.d.ts`
and `src/wasm/trap.d.ts` type shims; restore them with `git checkout` unless
the wasm API changed.

## Running queries

The editor accepts several statements separated by `;`. They run with the
engine's own script runner (`executeScript`, ADR-0131), which splits with the
CLI's scanner and stops at the first error; the results pane shows the last
statement's result, with counters summed over the script. Each statement
commits on its own.

## Cookbook links

Cookbook steps link here as `#recipe=<slug>&cell=<id>` (ADR-0130). The
playground then fetches `../cookbook/<slug>/recipe.json` (format 1; any other
format is refused with a message, ADR-0134), shows the step and asks before
running anything. On confirmation it fetches every parameter file it needs,
then resets the database, runs the recipe's earlier steps, and puts the step
in the editor unrun; the replay stops at the first step that goes other than
the recipe says, by the cookbook pages' own rule (imported from
`scripts/cookbook/lib/cells.mjs`). When the step has a parameter file, those
parameters go with every query run from the editor until **Clear** or a
database reset. A recipe page's **Open in playground** links to
`#recipe=<slug>` with no step (ADR-0134): the playground runs every step of the
recipe and leaves the editor empty. See `src/lib/recipe-link.ts`,
`src/hooks/useRecipeSession.ts`, and `src/components/RecipeDialog.tsx`.

## Layout

```
examples/wasm-playground/
  index.html
  package.json
  vite.config.ts
  src/
    App.tsx
    main.tsx
    components/    # QueryWorkspace, ResultsPanel, NeedleGraphView, RecipeDialog, …
    lib/           # engine.ts (wasm bridge), recipe-link.ts, wire.ts, extract-graph.ts, samples.ts
    wasm/          # generated: relate.js + relate_bg.wasm + trap.js (tracked: the .d.ts shims)
```

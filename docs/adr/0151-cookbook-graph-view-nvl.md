# 0151. Cookbook graph view: interactive NVL with double-click expand

- **Status**: Proposed
- **Date**: 2026-09-30
- **Deciders**: CypherLite maintainers

## Context

ADR-0130 Decision item 6 draws a cookbook step's graph result with
`d3-force` into a static SVG: 300 iterations of layout, then fixed
circles and curves with `<title>` tooltips. The reader can look at it, but
not zoom into a dense result, move a node that covers another, read a
node's properties without hovering exactly on it, or see what a node
connects to beyond the rows the step returned. Neo4j's own tools (Browser,
Bloom, the playground's graph view) do all of these, and recipe readers
come from them.

ADR-0130 rejected NVL, Neo4j's visualization library and what the
playground draws with, because "NVL's layout workers rely on Vite's worker
bundling … a static-page bundle built with esbuild cannot load them
cleanly", and because the NVL chunk is large. Looking at NVL 1.2.2 closely:

- The workers are optional. `@neo4j-nvl/layout-workers` spawns them as
  module `SharedWorker`s from `new URL('./x.worker.js', import.meta.url)`,
  which esbuild does not rewrite. With `disableWebWorkers: true` NVL runs
  the same layouts on the page's thread (each layout has a `.fallback.js`
  twin), and the worker URL is never built. Cookbook results are at most a
  few hundred nodes, well within what the page's thread can lay out.
- Its telemetry, Segment Analytics (`@segment/analytics-next`), is a bare
  side-effect import that `disableTelemetry: true` leaves unused.
  `@segment/analytics-next` declares `"sideEffects": false`, so esbuild
  drops the module, but with code splitting it still emits a chunk for each
  of the module's own dynamic imports: dead files on the site.
- `@neo4j-nvl/base` plus `@neo4j-nvl/interaction-handlers`, bundled and
  minified by esbuild, weigh about 1.8 MB, 520 KB gzip, mostly NVL itself
  and cytoscape (its CoseBilkent layout). The engine the page loads on
  first Run is 2.2 MB gzip.

The page also has what an "expand" needs, which a static page usually
lacks: the recipe's database, in the engine worker, holding the same graph
the recording was made from (the WASM parity check keeps the two equal, IDs
included, when steps run in order).

## Decision

1. **NVL draws the graph view.** `scripts/cookbook/runtime/graph.js`
   renders a step's graph result with `@neo4j-nvl/base` and
   `@neo4j-nvl/interaction-handlers`, pinned to 1.2.2 (the playground's
   version), on the canvas renderer with NVL's `d3Force` layout (the one
   NVL recommends for small graphs; it settles around a dragged node as
   Browser's does) and nodes of radius 40, so a result fitted to the pane
   is drawn large enough for NVL to show captions. It is created with
   `disableWebWorkers: true` and `disableTelemetry: true`, and the build
   replaces `@segment/analytics-next` with an empty module, so no telemetry
   code ships. `graph.js` stays a dynamic import, so the NVL chunk is
   fetched only when a graph view is about to show (a gram step coming near
   the viewport, or a click on the Graph tab), and pages without graph
   results never load it.
2. **Interactions.** A pinch, or the wheel with Ctrl/⌘ held, zooms; the
   plain wheel scrolls the page past a graph until the reader clicks into
   it, then zooms until the pointer leaves it, so a long recipe never
   traps the reader's scrolling. Dragging the canvas pans; dragging a node
   moves it; hovering a node or relationship shows its labels or type and
   its properties in an inspector, and clicking one keeps it there until
   it is closed or the canvas is clicked. Buttons zoom in, zoom out, fit
   the graph to the pane, and put back the step's result after expansions,
   and the pane can be resized. Colours and captions are unchanged: labels
   are coloured as
   the recipe's data-model diagram colours them (ADR-0135), and a node's
   caption is its `name`, `title`, `text`, `id`, `label`, or `key`
   property.
3. **Double-click expands a node from the page's database.** The graph view
   asks the page's session for the node's neighbours, and the session runs
   two read statements in the engine worker:
   `MATCH (n)-[r]-(m) WHERE id(n) = $id RETURN r, m LIMIT 100`, then the
   relationships between the new neighbours and every node the view draws.
   The new nodes and relationships are added to the view, starting next to
   the expanded node. The queries and the merge live in
   `scripts/cookbook/lib/graph-model.mjs`.
4. **Expanding runs the steps the database needs first.** If the steps up
   to the one whose graph is on screen have not all run, expanding runs
   them first, in order, the way Run does (a step that has run does not run
   again), then expands. The double-click is the reader's click that
   ADR-0130 Decision item 4 requires before anything runs, and a failed
   step stops it the same way it stops a Run. Expansions are view state:
   they last while the reader switches between a step's tabs, and a run
   that gives the step a new result starts its graph view over from that
   result.
5. **Limits.** A result draws its first 300 nodes, as before. One
   expansion adds at most 100 neighbours, and a view stops growing at 1,000
   nodes, NVL's canvas-renderer comfort zone.
6. **The build checks the expand queries.** When the build replays a
   recipe through the browser build of the engine (the WASM parity check,
   ADR-0130 Decision item 2), it also expands the first node of every graph
   result right after its step runs, and fails if the queries error or miss
   one of that node's relationships the step recorded.

## Alternatives considered

- **Keep the SVG view and add `d3-zoom` / `d3-drag`.** Small, but
  inspector, expansion, re-layout around dragged nodes, and hit testing on
  curved relationships would be written by hand, and the view would still
  not look or behave like the Neo4j tools readers know. Rejected.
- **NVL with its layout workers.** Needs an esbuild plugin that rewrites
  `new URL(…, import.meta.url)` worker references inside `node_modules`
  and emits the worker modules (each with its own copy of cytoscape or
  dagre) beside the chunk. At cookbook sizes the page's thread lays a
  result out in milliseconds, so the workers buy nothing. Rejected.
- **Needle's `GraphVisualization` (`@neo4j-ndl/react-graph`), as the
  playground uses.** It wraps NVL with a side panel, but brings React and
  the Needle stylesheet into pages that ADR-0130 keeps framework-free.
  Rejected.
- **WebGL renderer.** Scales past 100,000 nodes but drops caption fidelity;
  cookbook results are small and label-rich. Rejected.
- **NVL's default `forceDirected` layout at NVL's node size.** Spreads a
  result so far that it fits the pane only at a zoom where NVL no longer
  draws captions, leaving unlabelled dots. Rejected.
- **The plain wheel always zooms.** Every graph on a recipe page would
  catch the reader's scrolling as it passed under the pointer. Rejected.
- **Expand from the recording only.** A recording holds only the elements
  the step returned, so there is nothing beyond them to expand into.
  Rejected.
- **Expand only after the reader has run the steps.** A double-click on a
  recorded result would say "run the steps first", and pressing Run does
  exactly what expanding would have done. An extra step for no benefit.
  Rejected.
- **Expand with every relationship among all drawn nodes (Browser's
  "connect result nodes").** Would also join nodes the step deliberately
  showed apart (a path, say) the moment anything is expanded. Only the new
  neighbours are connected. Rejected.

## Consequences

- `scripts/cookbook/package.json` gains `@neo4j-nvl/base` and
  `@neo4j-nvl/interaction-handlers` (1.2.2, exact). `d3-force` stays: the
  data-model diagrams lay out with it (`lib/schema-layout.mjs`).
- A recipe page with a graph result fetches the NVL chunk (about 520 KB
  gzip) when the first graph view nears the viewport, up from a few KB. The
  chunk name is content-hashed and shared by every recipe page, so it is
  fetched once per release.
- NVL is licensed under the Neo4j Visualization Library License, which
  limits it to use with Neo4j products. The cookbook shares the playground's
  position, since the playground already ships NVL 1.2.2; a change to that
  position changes both.
- `npm audit` reports `js-cookie` through `@segment/analytics-next`. Neither
  is in the published bundle (the build stubs the module out), and the
  build's npm packages run only in CI and on maintainers' machines.
- NVL's interaction handlers listen for mouse events only. On a touch
  screen a tap selects and inspects, the buttons zoom and fit, and a swipe
  over a graph scrolls the page rather than panning it.
- With many nodes on screen NVL drops captions until the reader zooms in
  (its level of detail, as in Bloom); an expansion can make a result's
  captions disappear until then.
- `lib/wasm-run.mjs`'s `runRecipe` becomes async and takes an `afterCell`
  hook, which the build uses for the expand check.
- The graph view still has no recorded form of its own: the WASM parity
  check covers the data it draws, `lib/graph-model.mjs` has unit tests, and
  the drawing itself is checked by eye.
- ADR-0130 Decision item 6 and its "Render the graph view with NVL"
  alternative are replaced by this ADR (a **Later references** note is
  added there).

## Later references

- ADR-0528 moves the cookbook to the relate-web repository: the graph view
  moves there with the pages.

# 0135. Cookbook diagrams: mermaid source, pre-rendered SVG

- **Status**: Proposed
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers

## Context

Recipes explain their graph data model and, for the longer ones, a pipeline
(retrieval, a migration rehearsal, a tracer) in prose and inline patterns
such as `(:Place)-[:IN]->(:City)`. A picture of the model, and of the flow,
reads faster. A recipe is read in two places: the cookbook website and
GitHub (ADR-0132 keeps the file readable there). GitHub renders one diagram
format natively, mermaid code blocks. The website does not.

Rendering mermaid on the website needs the mermaid library: in the page it
is several hundred KB that every recipe would load on first view (each
recipe has a diagram near the top); at build time it needs a browser,
because mermaid measures text with the DOM, and the cookbook CI job and the
Pages deploy would then need Chrome. Hand-drawn SVG files would avoid both,
but GitHub readers would lose the source, and every edit would mean
redrawing.

## Decision

1. A diagram is a ```` ```mermaid ```` block in the recipe: the source of
   truth, rendered by GitHub, readable by agents in `index.md`. Its
   `accTitle:` line names it (the website's caption and accessible name).
   Node colours for graph labels come from classes `l1`…`l12`: the palette
   Neo4j's graph tools (Bloom, NVL) draw labels in, which the cookbook's
   result graph view uses too (`scripts/cookbook/lib/palette.mjs`). The
   site's other colours come from `scripts/cookbook/lib/tokens.mjs`, the
   source of the page CSS's colour tokens too.
2. `scripts/cookbook/diagrams/render.mjs` renders each block to two SVG
   files, `docs/cookbook/diagrams/<slug>-<n>-light.svg` and `-dark.svg`, in
   the site's palette and font. It is an authoring tool with its own
   `package.json` (`mermaid`, `puppeteer-core`), run like
   `COOKBOOK_UPDATE=1` when a diagram changes; CI never installs it. It
   reads the recipes through `cookbook-export` (ADR-0132). Each file starts
   with a comment holding the SHA-256 of its source block.
3. A data model is a flowchart whose nodes are all circles, `((…))`. The
   renderer draws it as a graph, the way Neo4j Bloom and NVL draw one: each
   label a coloured circle holding its properties, each relationship type
   along its arrow, self-relationships as loops, laid out by force.
   `scripts/cookbook/lib/schema-graph.mjs` does the drawing, from mermaid's
   own parse of the block and text measured in headless Chrome in the
   site's font, with `d3-force` (already a cookbook dependency). It tries
   several deterministic starting layouts, keeps the one with the fewest
   overlaps and crossings, and warns when that one still has flaws. Mermaid
   draws every other block (pipelines, timelines) in headless Chrome. For a
   data model the renderer also writes `<slug>-<n>-labels.json`, each label's
   colour with the source's hash, and the page's graph view colours result
   nodes from it, so a label has the same colour in the diagram and in every
   result.
4. `build.mjs` replaces each mermaid block with a figure holding both SVGs
   inline (CSS shows the one for the current theme) and fails when a block
   has no rendered files or their hash does not match the block, naming the
   command to run. CI and the deploy stay browser-free.
5. Because a rendering is inlined, and its hash covers only the source, the
   build parses every rendering with `parse5` (a spec-compliant HTML parser,
   added to `scripts/cookbook`) as the content of a `<div>`, where the page
   puts it, and accepts only one `<svg>` holding the elements, attributes,
   and CSS the renderer draws: no scripts, event handlers, links, or `url()`
   other than `#fragment`s, no CSS escapes, and no @-rules but
   `@keyframes`. The page gets the checked tree serialized back, not the
   file's text, so what ships is what was checked. The renderer runs the
   same check.

## Alternatives considered

- **Render mermaid in the page.** Nothing to commit, but every recipe page
  loads the library on first view, and the page shows mermaid source without
  JavaScript. Rejected.
- **Render at build time.** Nothing to commit, but the build, the cookbook
  CI job, and the Pages deploy all need Chrome, and fonts and output would
  vary with the machine that builds. Rejected.
- **Hand-drawn SVG images only.** Full control of the drawing, but no
  source on GitHub beyond the image, and no text for agents; every change is
  a redraw. Rejected.
- **A diagram notation of our own, drawn by the build.** Browser-free and
  under our control, but GitHub would show it as code, and it is one more
  thing to maintain. Rejected.
- **Data models drawn by mermaid, with circle nodes.** Nothing of our own
  to maintain, but mermaid lays a graph out in layers, routes edges around
  them with their labels in level boxes, and draws self-relationships as
  square hooks: the result reads as a flowchart, not as the graph a Neo4j
  user knows. Rejected.
- **Data models drawn by NVL, Neo4j's visualization library.** The real
  look, but NVL draws to a canvas in a live page, not to a static SVG the
  build can inline and check. Rejected.

## Consequences

- Recipes gain rendered SVG files beside them in `docs/cookbook/diagrams/`;
  a recipe is no longer entirely one file for the website, though its
  source still is (ADR-0132).
- Changing a diagram needs Chrome and a network connection (for the site's
  font) on the author's machine; a stale or missing render fails the
  website build, not the native recipe gate.
- GitHub renders the same source with its own mermaid version and theme;
  recipes stick to flowchart syntax that older mermaid versions accept.
- Each diagram ships twice in the page (light and dark); gzip keeps that to
  a few KB.
- `scripts/cookbook` gains `parse5` (one dependency, `entities`). A mermaid
  upgrade that draws a new element or attribute fails the renderer's check
  until the allowlist in `scripts/cookbook/lib/diagrams.mjs` is widened.
- The graph drawing is ours to maintain: a force layout, loops, curved
  parallel relationships, and overlap checks, split into text, geometry,
  layout, and drawing modules and unit-tested in the cookbook CI job. A force layout also moves when a model changes, so a small edit
  can redraw the whole picture; the drawings are reviewed as images.

## Later references

- ADR-0528 moves the cookbook to the relate-web repository: the diagram
  renderer moves there and draws in the site's colours.

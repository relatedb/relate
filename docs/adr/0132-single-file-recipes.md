# 0132. Single-file recipes: embedded data, table and gram results

- **Status**: Accepted
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers

## Context

ADR-0130 made each cookbook recipe a directory: `README.md` for prose and
cells, `data/*.json` for fixtures, and `expected.json` for the recorded wire
envelopes, with each cell's rendered table repeated in an `output` block.
A recipe was therefore four to ten files, and its authoritative results lived
in a JSON file nobody reads. The `output` blocks were lossy: long values were
clipped at 60 characters, tables stopped at 25 rows, and nodes and paths were
squeezed into table cells.

We want one file per recipe that a reader can take in whole: the prose, the
Cypher, the data it loads, and the exact results, with the results in a form
people read rather than a wire dump.

## Decision

1. **One file per recipe.** A recipe is `docs/cookbook/<slug>.md`; the file
   stem is the slug and the URL. There is no recipe directory, no `data/`, and
   no `expected.json`.
2. **Data blocks.** Fixtures and parameter sets are fenced
   ```` ```json data=<name> ```` blocks holding a JSON object; its keys are the
   statement parameters. A cell names one with `params=<name>`. Parameters
   still reach the engine as the block's exact text, so `1.0` stays a float.
   Parameter blocks (up to 2 KB) sit next to the step that uses them;
   datasets (over 2 KB) go in a closing `## Data` section, each inside
   `<details>` so GitHub shows them collapsed. The gate rejects a block on
   the wrong side of `## Data`, so the section cannot hide a long recipe's
   parameters. Every block must be referenced, and none may exceed 512 KB.
   A recipe runs at most 800 lines before its `## Data` section; the section
   itself is exempt from the repository's 1000-line file guideline, since it
   is collapsed on GitHub and never rendered on the site.
3. **Result blocks are the recording.** Each cell is followed by its result:
   - ```` ```table ```` for tabular results: a cypher-shell-style table with
     every row and no truncation, values as Cypher literals (strings quoted,
     so `"446"` and `446` differ; `1.0` and `1` differ), then one summary line
     with the row count and summed mutation counters. Write-only cells and
     errors use the same block (`(no rows) · …`, `error: …`).
   - ```` ```gram ```` for results that hold nodes, relationships, or paths,
     in [gram notation](https://github.com/gram-data/tree-sitter-gram): a
     `//` summary line, then one line per row. A row that is one graph value
     is that pattern; any other row is `[{scalar columns} | graph values]`.
     Nodes and relationships carry their engine ids as identifiers
     (`n12`, `r7`), are written in full at first occurrence and referenced
     after. Values gram cannot express (empty or nested lists, vectors,
     exponent-form numbers, strings with a character gram has no escape
     for) are exact Cypher literals in a `cypher` tagged string. Names and
     strings take gram's escapes, not Cypher's: a backtick in a label is
     `` \` `` where Cypher doubles it, and gram has no `\u` escape.
   One renderer, `cypherlite_formatter::recording`, makes this text: the
   native harness calls it directly and the browser through the WASM
   `renderResult` (ADR-0131). The harness and the WASM parity check compare it
   to the block; `COOKBOOK_UPDATE=1` rewrites the blocks. Only a cell's last
   statement is rendered; the counters are summed over the cell.
4. **Website.** Data blocks and result blocks never render as code blocks on
   the site, and the `## Data` section is dropped. Result blocks fill each
   step's output pane (a `gram` result opens in the graph view); data blocks
   are published as `data/<name>.json` beside the page for the runtime and the
   playground. The site build replays each recipe through the WASM bundle to
   embed the result envelopes the graph and JSON views need.
5. **One parser.** The Rust harness (`testing/cypherlite-cookbook`) is the
   only recipe parser. The Node tools (the site build, the diagram renderer)
   read its parse as JSON from `cookbook-export`, a binary of that crate that
   builds without the engine; they do not parse recipes themselves.
6. Everything else in ADR-0130 stands: cells, CI gates, publication from the
   latest release tag, the Web Worker runtime, playground hand-off.

## Alternatives considered

- **Keep `expected.json` and make the `output` blocks readable.** Two
  sources of truth per recipe, and the authoritative one is unreadable.
  Rejected.
- **CSV result blocks.** Nested values (lists, maps, points) become quoted
  literals inside quoted fields, and CSV has no room for the summary line.
  Rejected in favor of the table form.
- **CSV and gram data blocks** (tabular fixtures as CSV loaded like
  `LOAD CSV`, graph fixtures in gram created directly). Loses types for
  vectors and lists, needs a gram loader in Rust and JavaScript, and stops the
  recipes from teaching their own load Cypher. Rejected; JSON data blocks keep
  the load steps unchanged.
- **Render graph results as tables with `(:Label {…})` cells.** Unreadable for
  paths and loses node identity across rows. Rejected in favor of gram.
- **Show data and results as ordinary code blocks on the site.** A 150 KB
  JSON block in a page, and every result twice. Rejected.
- **Parse and render in JavaScript too, kept equal by shared test cases.**
  The first version of this decision. The twins drifted within a release
  (escapes, empty list items, raw-HTML rules), and every rule had to be
  written twice. Rejected in favor of one parser and one renderer.
- **Cap whole recipe files at 1000 lines.** Would split a dataset from the
  recipe that loads it, or trim datasets to fit a line count. Rejected;
  the cap applies to the part a reader reads.

## Consequences

- Recipes are single files; the index links `<slug>.md`.
- The recording is text: whatever the table and gram renderers do not express
  is not compared. The renderer lives in `cypherlite-formatter` (a surface
  crate the harness and the WASM surface both depend on) with its unit
  tests; there is no JavaScript renderer or recipe parser.
- The Node tools need cargo to read recipes; `cookbook-export` builds in
  seconds, and the site build already needs cargo for the WASM bundle.
- Result blocks can be long: up to 200 rows (the existing cap), untruncated.
- `scripts/cookbook/build.mjs` needs the WASM bundle at build time to execute
  recipes (it already copies it).
- ADR-0130 gains a Later references note.

## Later references

- ADR-0135 adds ```` ```mermaid ```` diagram blocks to recipes. The block is
  the source and stays in the recipe file; the website uses SVG renderings of
  it kept beside the recipes in `docs/cookbook/diagrams/`, so for the website
  a recipe is no longer entirely one file.
- ADR-0528 moves the cookbook to the relate-web repository: the recipe
  format this ADR defined continues there, maintained by that repository.

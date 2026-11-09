# 0130. Cookbook: tested markdown recipes with in-page WASM cells

- **Status**: Accepted
- **Date**: 2026-09-25
- **Deciders**: CypherLite maintainers

## Context

The public website is a single static page on the `gh-pages` branch
(#74 removed the Docusaurus site on 2026-09-24). Its contract,
`gh-pages/MAINTAINING.md`, forbids a framework, a build step, and extra
pages "without a decision on `main`". The WASM playground is the only
other route; `.github/workflows/deploy-pages.yml` builds it from `main`
and layers it under `/cypherlite/playground/`.

We want a cookbook: task-shaped recipes (entity resolution, geo lookup,
dependency blast radius, hybrid recall, agent memory, tag analytics)
that a reader can study, run in the browser, and copy into the CLI or
Python. The source material is a
companion examples project, whose examples are full programs pinned to a
release and tested by
snapshot diffs in their own repository.

Constraints that shape the decision:

- AGENTS.md §2/§4: examples are tests, and every public example runs in
  CI. A recipe that drifts from the engine must fail a build.
- The browser surface (ADR-0084, ADR-0085, ADR-0100) is `:memory:` only,
  runs one statement per `execute`, has no query timeout, runs on the
  calling thread, and traps on a Rust panic. Vector, full-text, and
  spatial queries work in it.
- The site documents only shipped behavior (`MAINTAINING.md`).
- The release-triggered website agent (`update-website.yml`) edits
  `index.html` and `llms.txt` surgically and must not invent sections.

## Decision

1. **Source.** Recipes live on `main` under `docs/cookbook/<slug>/`:
   `README.md` (YAML front matter + markdown), an optional `data/`
   directory of JSON fixtures, and `expected.json`. Runnable steps are
   fenced ```` ```cypher cell=<id> ```` blocks; each is followed by an
   ```` ```output ```` block holding its recorded result. The format
   contract, including the front-matter schema, is
   `docs/cookbook/README.md`. Recipes are distilled, self-contained
   versions of the examples; each links its full program through the
   `companion` front-matter field.
2. **Tests.** A new `testing/cypherlite-cookbook` crate runs every
   recipe natively through `cypherlite::Database` (`:memory:`), in
   document order, and compares each cell's canonical wire envelope
   (`query_result_to_wire`) with `expected.json` and its rendered table
   with the `output` block. `COOKBOOK_UPDATE=1` rewrites both. The site
   build runs the same cells through the WASM bundle and compares against
   the same recordings (the WASM parity check), so native/browser
   divergence fails CI. Both halves, `make cookbook`, run in always-on CI
   jobs, because recipe edits are docs-only changes that skip the code
   jobs.
3. **Publication.** `scripts/cookbook/build.mjs` renders recipes into
   static HTML (pre-rendered outputs, no client framework) plus a
   `.md` twin per recipe and `cookbook/llms.txt`. `deploy-pages.yml`
   builds the cookbook **from the latest release tag** — recipes, generator,
   and WASM bundle all from that tag — and layers it under
   `/cypherlite/cookbook/`, the same way it layers the playground. A
   tag without `docs/cookbook/` publishes no cookbook. Nothing under
   `cookbook/` is committed to `gh-pages`; that branch gains one nav link
   and a `MAINTAINING.md` section.
4. **Runtime.** Pages run cells through a `<cypher-cell>` custom element.
   The engine runs in a module Web Worker, loaded on the first Run
   click, never on page load. All cells on a page share one database;
   running a cell first runs the earlier cells that have not run, and a
   run stops at the first cell that goes other than its recipe says (an
   error where none is expected, or success where the cell expects one),
   the rule the native harness and the playground apply too. Stop
   terminates the worker (the answer to the missing query timeout) and
   Reset discards the database. Nothing runs without a click. Fixture
   JSON is passed to the engine as raw text, never re-serialized by
   JavaScript, so `1.0` stays a float.
5. **Playground hand-off.** Each cell links to
   `/cypherlite/playground/#recipe=<slug>&cell=<id>`. The playground
   reads `/cypherlite/cookbook/<slug>/recipe.json` (emitted by the
   generator), asks the reader to confirm, replays the earlier cells, and
   places the target cell in the editor without running it.
6. **Graph view.** Cells whose result holds nodes, relationships, or
   paths offer a graph tab drawn with `d3-force` into SVG, loaded only
   when the tab is opened.

## Alternatives considered

- **Reinstate Docusaurus (or adopt Starlight).** Reverses #74 one day
  after it landed and reintroduces a second design system beside the
  hand-written homepage. Rejected.
- **Mintlify (what the TypeSafe cookbook uses).** Hosted and paid, moves
  the docs off GitHub Pages, and custom components cannot host a
  module worker that loads our WASM. Rejected.
- **Hand-written HTML recipe pages on `gh-pages`.** Untestable, so recipes
  would drift from the engine, and the website agent cannot keep a
  growing set of pages in sync. Rejected.
- **Recipe pages inside the playground SPA.** Client-rendered pages are
  poor for reading, for search, and for `llms.txt`, and would pull the
  playground's CodeMirror/NVL bundle into every recipe. Rejected.
- **Notebooks (`.ipynb`) as the source, as TypeSafe and the OpenAI
  cookbook do.** Our executable unit is Cypher, not Python; markdown is
  diffable, renders on GitHub, and is already the docs standard after
  #74. Rejected.
- **Pull recipes from the examples project at build time.** That repository
  is pinned to a release, is partly uncommitted, and is tested outside
  this repository's CI, so an engine regression would not fail here.
  Rejected; recipes link to it instead.
- **Publish the cookbook from `main` HEAD, like the playground.** Would
  show unreleased behavior, against the site's "only shipped behavior"
  rule. Rejected.
- **Render the graph view with NVL, as the playground does.** NVL's layout
  workers rely on Vite's worker bundling (see the playground's
  `vite.config.ts`), and the NVL chunk is large; a static-page bundle
  built with esbuild cannot load them cleanly. Rejected for the cookbook.
- **Run Python recipe code in the browser through Pyodide.** Needs an
  Emscripten build of the PyO3 bindings on a nightly toolchain; a separate
  decision if it is ever wanted. Rejected for now.

## Consequences

- New workspace member `testing/cypherlite-cookbook` (R4 allows it to
  depend on any workspace package). `cargo test -p cypherlite-cookbook`
  is the recipe gate; `COOKBOOK_UPDATE=1` and `COOKBOOK_ONLY=<slug>` are
  documented in `docs/cookbook/README.md`.
- New Node tooling under `scripts/cookbook/` (markdown-it, shiki, esbuild,
  d3-force; ADR-0133 adds smol-toml and ADR-0135 parse5). It only runs at
  build and test time; the page ships a small vanilla bundle plus the WASM.
- `deploy-pages.yml` builds the WASM bundle twice (release tag for the
  cookbook, `main` for the playground). Recipes added on `main` appear on
  the site with the next release.
- `gh-pages/MAINTAINING.md` and `.github/prompts/website-update.md` learn
  that `/cypherlite/cookbook/` exists, is built from the release tag, and
  must not be edited on `gh-pages`. The nav gains a Cookbook link before
  GitHub; Playground stays last.
- The link check covers `docs/cookbook/**`. `docs_routes` in
  `docs/product-capabilities.yaml` gains the cookbook route so `llms.txt`
  may link it.
- Fixture files are capped at 512 KB each and record source and license
  in front matter.
- Multi-statement cells run through the WASM `executeScript` export, and
  results render through `renderResult` (ADR-0131): the Rust code the
  native harness calls.

## Later references

- ADR-0132 replaces the recipe layout in Decision items 1 and 2: a recipe is
  one `docs/cookbook/<slug>.md` file with embedded `json data=` blocks and
  `table` / `gram` result blocks; `data/` directories and `expected.json` are
  gone.
- ADR-0133 keeps the website's links to GitHub off while the repository and
  the examples are private (while `lychee.toml` excludes them): the
  `companion` link of Decision item 1 and the nav's GitHub link render once
  those URLs are public, and the build fails on any link that does not
  resolve.
- ADR-0134 adds a whole-recipe form of the Decision item 5 hand-off,
  `#recipe=<slug>` without a cell: the playground runs every step and leaves
  the editor empty. `#recipe=<slug>&cell=<id>` is unchanged.
- ADR-0151 replaces Decision item 6 and reverses the "Render the graph view
  with NVL" alternative: the graph view is drawn with NVL, its layout
  workers off (NVL lays out on the page's thread), and it zooms, pans,
  drags, inspects, and expands a node from the page's database on
  double-click.
- ADR-0288 turns the nav's GitHub link on while the repository is internal,
  matching the landing page's navigation; the other links ADR-0133 holds back
  still wait for publication.
- LABS-999 retires the product-truth gate and its
  `docs/product-capabilities.yaml` contract: the `docs_routes` consequence
  above has no contract to land in, and the cookbook pages' links are covered
  by the link check alone.
- ADR-0528 moves the cookbook to the relate-web repository: the recipes,
  their harness, the site build, and the deploy step this ADR set up no
  longer live here, and this repository's CI no longer runs them.

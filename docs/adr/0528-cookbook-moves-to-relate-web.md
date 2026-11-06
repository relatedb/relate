# ADR-0528: The cookbook moves to the relate-web repository

Status: Proposed

Claim: 7be011d47c05e1c6fbf9879015a9fd81041bf569

## Context

ADR-0130 made the cookbook's recipes tests of this repository: each recipe
ran natively in a dedicated harness crate, and a Node build here turned the
recipes into the `/cookbook/` pages, replayed every step through the WASM
bundle, and published them from the latest release tag. Later ADRs shaped
the recipe file (ADR-0132), its links (ADR-0133, ADR-0288), the playground
hand-off (ADR-0134), its diagrams (ADR-0135), and the graph view (ADR-0151).

The website now lives in its own repository, relate-web, which builds and
publishes relatedb.ai. Keeping the recipes here split the cookbook from the
site that renders it: the pages' tooling (a Markdown renderer, a diagram
renderer, a graph library) sat beside the engine, the cookbook deployed
separately from the rest of the site, and every recipe edit ran this
repository's CI.

## Decision

1. The relate-web repository owns the cookbook: the recipes, their format,
   their tooling, and the `/cookbook/` pages. This repository no longer
   holds or builds any of it. The native recipe harness, the cookbook's
   CI jobs and change detection, its deploy step, and the playground's
   `#recipe=` hand-off are removed.
2. relate-web tests every recipe against the WASM bundle it ships: each
   step runs through `executeScript` and must reproduce its recorded
   result, rendered by `renderResult`; it runs in CI, before every deploy,
   and in a headless browser.
3. The engine contract the cookbook depends on stays here and stays
   stable: the WASM `executeScript` and `renderResult` exports (ADR-0131)
   and the recording format of `relate_formatter::recording` (ADR-0132).
   A change to either changes the cookbook's recorded results; relate-web
   sees it when it refreshes its bundle and re-records.
4. ADR-0130, ADR-0132, ADR-0133, ADR-0134, ADR-0135, ADR-0151, and ADR-0288
   stay as the record of how the cookbook was designed here; each gains a
   Later references note pointing to this ADR.

## Alternatives considered

- **Keep the recipes here and sync a copy to relate-web.** Two homes for
  one set of files; the copy drifts, and an edit has to be made in the
  right one. Rejected.
- **Move the recipes but keep the native harness here, checking out
  relate-web in CI.** Couples the two repositories' CI and needs
  cross-repository credentials, for coverage the WASM run already gives.
  Rejected.
- **Keep everything here and have relate-web publish the built pages.**
  Leaves the site's styling, layout, and runtime in two places. Rejected.

## Consequences

- This repository's CI no longer catches an engine change that alters a
  recipe's result; relate-web's check fails when it next refreshes its
  WASM bundle, and the change is reviewed there as a re-recording.
- Recipes run only on the WASM build, not natively. Both builds share the
  script runner and the renderer (ADR-0131), so the gap is a divergence
  that only the native build would show.
- The playground no longer loads recipes; a hand-off can return in
  relate-web when the playground moves there.
- Recipe authoring leaves this repository's agent instructions and docs;
  the cookbook's own README in relate-web is its contract.

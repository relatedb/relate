# 0134. Whole-recipe playground link

- **Status**: Proposed
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers

## Context

ADR-0130 (Decision item 5) defines one hand-off from the cookbook to the
playground: `/cypherlite/playground/#recipe=<slug>&cell=<id>`. The
playground replays the cells before `<id>` and puts `<id>` in the editor
unrun. That fits a link on a step: "try this step, with everything it
needs already run".

The recipe page also has one "Open in playground" link in its engine bar
(added with ADR-0133's review changes), meant to open the recipe's graph
for exploration. It pointed at the first step not marked `setup`. The
setup flag says how a step renders, not whether it loads data, so in four
of eleven recipes that step came before the data (agent trajectory
timeline and tool-call provenance open on their first step; agent memory
and bi-temporal decisions on their first write). The playground opened
with an empty or schema-only database.

## Decision

1. The hand-off accepts `#recipe=<slug>` without a cell: the playground
   fetches the same `recipe.json`, asks the reader to confirm, resets its
   database, runs **every** step of the recipe in order (a step marked
   `expect=error` may fail, as in the page), and leaves the editor empty
   with no step parameters. `#recipe=<slug>&cell=<id>` keeps its meaning.
2. The engine bar's "Open in playground" links to `#recipe=<slug>`, and so
   does the navigation's "Playground" link on a recipe page (elsewhere it
   opens the playground on its sample data); each step keeps its own
   `&cell=<id>` link.
3. `recipe.json` is the contract between the cookbook, built from the latest
   release tag, and the playground, built from `main`, so it carries a
   version: format 1 is `{format, slug, title, cells: [{id, code,
   paramsPath, setup, expectError}]}`, `paramsPath` being the step's
   parameter file relative to the recipe page. The playground checks the
   shape and rejects any other format with a clear message.
4. Loading fetches the recipe and every parameter file it needs before it
   touches the database, so a network failure changes nothing. The replay
   stops at the first step that goes other than the recipe says (an error
   where none is expected, or success where the step expects one): the
   playground imports the pages' rule, `scripts/cookbook/lib/cells.mjs`,
   and the native harness is tested against the same cases. A step link's
   parameters go only with queries run from the editor, never with the
   schema browser's queries or the sample data.

## Alternatives considered

- **Link to the last step.** Works with the existing hand-off, but the
  reader gets every step except the last, and the last step's query in the
  editor: for migration rehearsal that is the clean-up `DELETE`. Rejected.
- **Pick the step after the last data load.** Needs a way to tell which
  steps load data; the `setup` flag cannot, and a new front-matter field
  adds authoring work for one link. Rejected.
- **Drop the engine-bar link.** Every step already links to the
  playground, but none of those links means "the whole recipe", which is
  what a reader at the top of the page wants. Rejected.

## Consequences

- Opening a recipe in the playground from the top of its page gives the
  graph as the recipe leaves it, whatever the recipe's first steps are.
- The playground's `parseRecipeLink`, recipe dialog, and replay loop handle
  a link without a cell; `examples/wasm-playground/README.md` documents
  both forms.
- A change to the `recipe.json` shape needs a new format number; the
  playground on `main` then refuses an older release's recipes with a
  message instead of misreading them.
- A whole-recipe replay runs every write the recipe makes, including ones
  a reader might not expect to be part of "the data" (migration
  rehearsal's deliberate bug and repair). The dialog says so before
  anything runs.

## Later references

- ADR-0528 moves the cookbook to the relate-web repository: the playground's
  `#recipe=` hand-off this ADR extended is removed, and no page here links
  to it.

---
type: adr
area: surface
date: 2026-09-25
deciders: CypherLite maintainers
name: 0131-wasm-script-execution
status: accepted
tags:
  - wasm
title: WASM script execution and result rendering
REFERENCES:
  - "[[adr/0084-reinstate-wasm-distribution]]"
  - "[[adr/0130-cookbook-tested-recipes]]"
  - "[[adr/0132-single-file-recipes]]"
---

# 0131. WASM script execution and result rendering

## Context

`CypherliteDb.execute` in the browser surface ([ADR-0084](./0084-reinstate-wasm-distribution.md)) runs exactly one
Cypher statement; `CREATE (:A); CREATE (:B)` is a parse error. Browser
hosts still receive scripts: cookbook cells that create an index and then
query it ([ADR-0130](./0130-cookbook-tested-recipes.md)), and playground users who paste several statements.

The cookbook checks every recipe twice: natively, where the harness records
each cell's result, and through the browser build, where the page runs the
same cells for readers. Both checks, the page, and the playground need the
same two things: how a script runs (split into statements, the same
parameters for each, stop at the first error), and how its result reads (the
`table` / `gram` recording, [ADR-0132](./0132-single-file-recipes.md)). If each host implements them, the
copies drift; a review found three script runners and two renderers (Rust
and JavaScript) that already disagreed on empty input and on what counts as
a failure.

`cypherlite/cypherlite-splitter` is the canonical statement splitter. The
CLI shell and the TCK runner use it; it honours the lexer's string,
backtick, and comment rules, so a `;` inside `'a;b'` or `// x;` is not a
terminator.

## Decision

Each is implemented once, in Rust, and every host calls it.

1. **Running a script.** `cypherlite::run_script_wire(conn, script,
   params_json)` splits with
   `cypherlite_splitter::split_executable_statements` (the splitter's scan
   drops comment-only pieces), runs each statement with the same parameters
   (decoded by `cypherlite::params_from_json`, the decoder the C ABI uses
   too), and returns one wire envelope per statement run, stopping after the
   first error. A blank or comment-only script returns none. Each statement
   commits on its own, so a failure leaves earlier statements applied.
2. **Rendering a result.** `cypherlite_formatter::recording::render(envelopes)`
 is the recording renderer ([ADR-0132](./0132-single-file-recipes.md)): the text the cookbook keeps, plus
   what a page lays out (a table's cells, or a gram result's nodes and
   relationships for the graph view) and the counters summed over the
   script, so no host decodes the wire values or sums counters itself.
3. **The browser surface** exports both:

   ```ts
   db.executeScript(script: string, paramsJson?: string | null): string[]
   renderResult(envelopes: string[]): string   // JSON {kind, text, table, graph, stats}
   ```

   The native harness calls the Rust functions directly; the cookbook page,
   its build, and the playground call the exports. One statement's run,
   `cypherlite::execute_wire`, is also the WASM `execute`. `isWasmTrap(err)`, a
   small module shipped with the bundle, is the one test every host uses for
   a trapped instance.

## Alternatives considered

- **`splitStatements(script)` and a loop in each host.** The first version
  of this decision. Every host re-implemented the loop (three copies that
  disagreed within a release), and the native harness had to mirror it by
  hand. Rejected.
- **`executeScript(script)` returning only the last result.** Hides every
  statement's result but the last, and suggests atomicity the surface cannot
  deliver without a transaction API. Rejected; `executeScript` returns every
  envelope.
- **Render in JavaScript for the browser.** A second renderer, kept equal to
  the Rust one only by hand-copied test cases. The renderer adds little to
  the bundle next to the engine. Rejected.
- **Split in JavaScript.** A second implementation of the lexer's quoting
  and comment rules. Rejected.

## Consequences

- `cypherlite` gains a dependency on `cypherlite-splitter` and two public
  functions, `run_script_wire` and `params_from_json`; `cypherlite-ffi`
  decodes parameters with the latter. The WASM surface now decodes
  parameters the same way: a map with a `__type` key stays a map.
- `cypherlite-wasm` depends on `cypherlite-formatter` (R3 allows
  `surfaces/*` → `surfaces/*`), which owns the recording renderer.
- `docs/api/wasm/spec.md`, `description.md`, and `examples/` document the
  exports; `surfaces/cypherlite-wasm/tests/examples.mjs` runs the examples
  in CI, and the browser smoke tests cover the exports.
- A cell runs and renders identically in CI and in the browser by
  construction; the WASM parity check now checks the engine, not the
  harness.

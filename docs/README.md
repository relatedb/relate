# Relate documentation

Plain markdown, no site generator. The public website is a static page on the
[`gh-pages`](https://github.com/relatedb/relate/tree/gh-pages) branch
(see `gh-pages/MAINTAINING.md` for its contract; `update-website.yml` keeps it
in sync with releases).

## Layout

| Path | Contents |
|---|---|
| [`api/`](api/) | Per-surface API contracts (spec + description + examples): rust, c, cli, python, javascript, java, go, swift, bolt, http, mcp, wasm, extensions |
| [`guides/`](guides/) | Task guides: vector similarity, geospatial, CSV import, output formats, backup/restore, the schema folder |
| [`cookbook/`](cookbook/) | Tested recipes with runnable Cypher cells, published as the website cookbook (ADR-0130) |
| [`reference/`](reference/) | Deep references: Cypher syntax, functions, ontology procedures, CLI flags, storage formats, dot-commands, client libraries, Bolt, C ABI |
| [`adr/`](adr/) | Architecture Decision Records (immutable, numbered) |
| [`design/`](design/) | Design studies and research: the extension-system tiering, the loader-plugin study, memory optimization, swappable query and write engines, Swift platforms, token budgets, the embedded driver-provider spikes (LABS-447) |
| [`tck/`](tck/) | the retired in-repo TCK pin's history; language coverage lives in the spec harness (ADR-0482, ADR-0520) |
| [`testing/`](testing/) | Test notes. [Memory pins and Bolt RSS](testing/memory-testing.md) |
| [`datasets.md`](datasets.md) | Dataset / load-test binary flag tables (agent contract) |
| [`engine-architecture.md`](engine-architecture.md) | Engine overview: memory model, persistence, querying |
| [`limitations.md`](limitations.md) | Known gaps and unsupported surfaces |
| [`PUBLISHING.md`](PUBLISHING.md) | Release runbook |

The installer lives at [`../distribution/install.sh`](../distribution/install.sh)
(offline harness: `make test-install-sh`). Documented function examples are
executed in CI via `scripts/check-doc-examples.sh`.

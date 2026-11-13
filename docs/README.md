# Relate documentation

Plain markdown, no site generator. The public website lives in the separate
[relate-web](https://github.com/relatedb/relate-web) repository (ADR-0546);
the release pipeline attaches `releases.json`, `install.sh` and `install.ps1`
to each release's assets and pokes relate-web to pull them.

## Layout

| Path | Contents |
|---|---|
| [`api/`](api/) | Per-surface API contracts (spec + description + examples): rust, c, cli, python, javascript, java, go, swift, bolt, http, mcp, wasm, extensions |
| [`guides/`](guides/) | Task guides: vector similarity, geospatial, CSV import, output formats, backup/restore, the schema folder |
| [`reference/`](reference/) | Deep references: Cypher syntax, functions, ontology procedures, CLI flags, storage formats, dot-commands, client libraries, Bolt, C ABI |
| [`adr/`](adr/) | Architecture Decision Records (immutable, numbered) |
| [`design/`](design/) | Design studies and research: the extension-system tiering, the loader-plugin study, memory optimization, swappable query and write engines, Swift platforms, token budgets, the embedded driver-provider spikes (LABS-447), the kernel-composition direction, the [kernel coverage inventory](design/kernel-coverage-inventory.md) |
| [`tck/`](tck/) | the retired in-repo TCK pin's history; language coverage lives in the spec harness (ADR-0482, ADR-0520) |
| [`testing/`](testing/) | Test notes. [Memory pins and Bolt RSS](testing/memory-testing.md) |
| [`datasets.md`](datasets.md) | Dataset / load-test binary flag tables (agent contract) |
| [`pr-descriptions.md`](pr-descriptions.md) | Pull-request title and body template (agent contract) |
| [`engine-architecture.md`](engine-architecture.md) | Engine overview: memory model, persistence, querying |
| [`limitations.md`](limitations.md) | Known gaps and unsupported surfaces |
| [`PUBLISHING.md`](PUBLISHING.md) | Release runbook |

The installers live at [`../distribution/install.sh`](../distribution/install.sh)
(offline harness: `make test-install-sh`, which also drift-guards the Windows
installer's constants) and [`../distribution/install.ps1`](../distribution/install.ps1)
(offline harness: `scripts/tests/test_install_ps1.ps1` over `scripts/tests/lib/install_ps1_harness.ps1`, run on the Windows CI
lane under PowerShell 5.1 and 7). Documented function examples are
executed in CI via `scripts/check-doc-examples.sh`.

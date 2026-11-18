# Obsidian vault test plan for the markdown snapshot codec (ADR-406)

The env-gated corpus (`RELATE_MARKDOWN_VAULTS`, vaults pulled by
`scripts/pull_obsidian_vaults.sh` into `testdata/obsidian-vaults/`)
plus the committed Odyssey fixture
(`relate/relate-storage/tests/fixtures/odyssey-vault/`) are
exercised at three depths. Every vault runs the **baseline** pass;
vaults marked below also run their named passes.

## Baseline pass (every vault)

1. **Open + bootstrap**: open the vault as a markdown store; the
   generated graph type records per-label keys and the rel-type
   vocabulary; no fail-closed errors.
2. **Global stats**: node count, per-label counts (compare against
   `find`-derived expected counts), relationship count.
3. **Roundtrip**: checkpoint → reopen → identical stats; re-checkpoint
   writes zero changed files (idempotent byte-stability).
4. **Reserved names**: `index.md`/`log.md` (root and per-label) are
   skipped on load and regenerated at checkpoint.

## Per-vault query passes

| Vault | Lookups | Traversals | Why this vault |
| --- | --- | --- | --- |
| **Odyssey** (committed) | by key (`title`): `MATCH (c:Character {title:'Odysseus'})`; by body content | pattern: `-[:RULES]->(p:Place)` chains; degree ranking; full e2e vs `import.cypher` shape | owned e2e; every designed rule in one fixture |
| **kepano-obsidian** | short-form resolution stress: `[[Steph Ango]]` etc.; `tags` as property filter | `(:Clippings)`→`[[People]]` link neighborhoods | frontmatter+wikilink heavy, personal-PKM shape |
| **obsidian-hub** | global `aliases`/`tags` property queries | the `[[Buy me a coffee]]` hub node: in-degree distribution, 2-hop neighborhoods | scale (6.5k notes), uniform wiki-linked topical notes |
| **obsidian-help** | `description`/`permalink` lookups | `[[Settings]]`-rooted navigation subgraph; cross-link clustering | official docs; deep nesting under `en/` |
| **Obsidian-Books-Vault** | `title` key inference; author/series property lookups | series→book→author chains | frontmatter-heavy, structured data, 592 nested files |
| **obsidian-sample-vault** (zh) | unicode/CJK key and body round-trip | template-generated link walks | CJK filenames and content |
| **example-vault** | journal date keys | `journal` → `writing` chains | path-form wikilinks + deep nesting edge case |

## Query families each pass covers

- **Global stats**: `MATCH (n) RETURN count(n)`, per-label counts,
  `count{(n)-[r]->()}` per relationship type, degree distributions
  (`top 5 by in-degree`).
- **Individual lookups**: key equality (constraint-backed index),
  property equality (`tags`, `aliases`), body contains (fulltext when
  indexed).
- **Pattern traversals**: fixed 2-node patterns, variable-length
  (`-[:TYPE*1..3]->`), hub/neighborhood queries, plus the TCK-shaped
  compliance the engine already guarantees (these tests assert the
  *codec* preserved the data, not new Cypher semantics).

## Serve-based checks (per vault, after `relate serve --database <vault>`)

- Bolt handshake + a stats query through the Bolt surface.
- The notebook demo (`examples/obsidian-vault-demo.ipynb`, Odyssey)
  runs the same three families from Python and renders the graph.

## Invariants asserted everywhere

- No dangling links in engine state (stubs materialized).
- Idempotent re-checkpoint: zero file writes on the second pass.
- The vault remains a valid Obsidian vault (wikilinks resolve, no
  engine-owned files outside `.relate/`).

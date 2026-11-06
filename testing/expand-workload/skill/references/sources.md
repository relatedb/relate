# Sources for seed patterns

Seed queries ground the workload in queries someone actually wrote. Mine them, tag
their source, then expand.

## Docs and blog posts

- Pull the code blocks out of the material the user points at (a blog post, a docs
  page, a guide). Every fenced `cypher` block is a seed candidate.
- Keep only statements that could plausibly run against the dataset: rename labels /
  types / properties to the dataset's schema (rules in `generation-recipes.md`) or drop
  with a note. Do not force-fit a query onto data it has no shape for.
- Tag each: `source:"seed:<where-it-came-from>"`.

## openCypher TCK corpus — a reference shelf, never a bulk import

The TCK is consulted **per round**: when a round's capability needs shape ideas,
mine the matching feature files, rewrite the pattern onto the dataset's schema
(generation-recipes.md), and keep it only when the rewrite preserves the point.
A pattern that cannot live on the dataset's model is dropped — a workload full of
foreign fixture shapes tests the fixtures, not the dataset. Literal-only and
truth-table scenarios test the parser, not the data; they never enter a workload.

The corpus is not vendored in this repo any more: the shelf is the spec-harness
checkout (`relatedb/relate-cypher-compat-private`) — `features/tck/` in the
openCypher TCK layout (`clauses/`, `expressions/`, `useCases/`), the engine's own
extension corpus at `features/relate-tck/`, and the CIP evaluation suite at
`features/cip-eval/CIP-*/` with its CIP documents in `cip-docs-md/`. Any checkout
of the openCypher TCK works the same (`tck/features/`).

Mine queries mechanically:

```
python3 scripts/mine_tck.py /path/to/harness/features/tck --out tck_patterns.jsonl
```

`mine_tck.py` walks every `.feature`, extracts each scenario's When-query (and the
Given graph shape as a comment), maps the feature file to a coverage area, and emits
entries in the workload's JSONL format with `source:"tck:<Feature>::[<n>]"`. The
patterns use generic names (`A`, `B`, `T`, `name`) — the rewriting rules in
`generation-recipes.md` map them onto the dataset schema. Expectations are dropped:
execution against the real dataset is the truth.

The CIP-eval corpus (`features/cip-eval/` in the harness) mines the same way;
it leans newer (Cypher 25 proposals).

## Cheat sheet

The coverage map in `coverage-map.md` was distilled from the Cypher cheat sheet
(`neo4j/docs-cypher`, `modules/cheat-sheet/pages/`). Its pages carry per-feature
minimal examples — useful when an area has neither seed nor TCK coverage. Rewrite the
toy names onto the dataset exactly like TCK patterns.

## Repo examples

The dataset's own project often ships example queries (`docs/api/*/examples/`,
notebooks, demos). Those are the highest-quality seeds: already schema-shaped.
Prefer them over mined material when both exist.

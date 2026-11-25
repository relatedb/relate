# Run Relate on your Obsidian vault

An Obsidian vault is a folder of markdown files with YAML frontmatter.
With the markdown snapshot codec, **that folder is the database**: open
it with Relate, query it with Cypher, write through it — and keep
opening the same folder in Obsidian. There is no import step and no
separate data copy; the files you already edit are the durable store.

## What you get

- Your notes as a graph: every top-level folder is a label, every note
  is a node, and every `[[wikilink]]` in a relationship key is a
  relationship.
- A hidden `.relate/` sidecar folder inside the vault (like
  Obsidian's own `.obsidian/`) holding the engine's state: the WAL,
  indexes, and a manifest. Beside it the engine keeps two small lock
  marker files, `.cypherlite.lock` and `.cypherlite.writer`, and
  maintains `index.md` / `log.md` views (below). Everything else stays
  exactly where your notes are.
- Git-friendly storage: nodes, properties, and relationships live in
  the markdown and diff file-by-file.

## Opening a vault

Point `--database` at a folder and opt in with `--snapshot-format
markdown` — a plain folder of markdown never auto-detects as a store.
The folder can be:

- **empty** — a new vault. Write with Cypher and the notes appear:

  ```console
  $ relate --database ./stars --snapshot-format markdown \
      "CREATE (:User {name: 'Bob'})-[:STARRED]->(:Repo {name: 'relate'})"
  $ cat ./stars/User/Bob.md
  ---
  type: User
  name: Bob
  STARRED:
    - "[[Repo/relate]]"
  ---
  ```

- **an existing vault** — notes laid out as `<Label>/<name>.md`, one
  folder per label:

  ```console
  $ relate --database ~/vaults/notes --snapshot-format markdown "MATCH (n) RETURN count(n)"
  ```

`RELATE_SNAPSHOT_FORMAT=markdown` does the same, and on the Rust API it
is `OpenOptions::snapshot_codec`. Once a vault has a
`.relate/manifest.json`, it opens as a markdown store from then on
without the flag. Without the opt-in, a first open of a folder that
already holds notes refuses with `ambiguous legacy storage role …
explicit offline migration required` — add the flag and open again.

### Copying an existing store into a vault

A graph that already lives in a `.rl` file or a directory store is
copied into a vault with `clone` (`--markdown-body-properties`, below,
applies to the destination):

```console
$ relate clone graph.rl ./vault --to-snapshot-format markdown
```

The reverse packs a vault into one file — name the destination codec,
since a `.rl` bundle cannot hold the markdown codec:

```console
$ relate clone ./vault graph.rl --to-snapshot-format parquet
```

## How frontmatter becomes relationships

A frontmatter key is a relationship type when its value is a **list**
of wikilinks — one relationship per entry:

```yaml
WRITTEN_BY:
  - "[[Author/Frank Herbert]]"
SEQUEL:
  - target: "[[Book/Dune Messiah]]"
    year: 1969
```

- An entry may be a map — `target` plus the relationship's
  properties. The type must also be declared somewhere: by a bare-link
  entry in some note, or in a root `ontology.yaml`
  ([ADR-0649](../adr/0649-vault-committed-ontology-file.md));
  otherwise the open refuses, naming the note and the key.
- A **single** link written as a plain value (`INSPIRED: "[[Author/X]]"`)
  stays a string property. Make it a one-entry list to get a
  relationship.
- Wikilinks in the note **body** are prose: they stay text and never
  become relationships.
- A link whose target note does not exist creates a stub node — and,
  at the next checkpoint, its note file — the same thing that happens
  when you click a dangling link in Obsidian.

## What happens on first open

Nothing is required of you. The engine scans the vault once and
**infers** the structure:

- a **key property per label** — the note's filename. If the
  frontmatter has a `name`, `title`, or another property that matches
  the filenames 1:1, that property becomes the declared key; otherwise
  the filename itself is the key.
- a **relationship-type vocabulary** from the wikilink keys it finds.
- the inferred schema is recorded as a *generated graph type* in the
  sidecar, and is descriptive: your future edits are never refused for
  not matching it.

## The rules in plain language

- Every node needs its label's key to be present and unique. If you
  create a node without it, Cypher refuses the write with a clear
  error — the vault's filenames must stay collision-free.
- Notes at the vault root (no folder) load under the reserved
  `:Vault` label.
- `index.md` and `log.md` are regenerated views (a directory listing
  and a dated changelog); edit them all you like, the engine rewrites
  them at checkpoint.
- Case does not distinguish: two labels or filenames differing only in
  case are treated as one (with a warning).

## Which properties become the note's body

By default a note's body is the node's single `body` property
(ADR-0406). If your notes carry several long-text properties — a
`summary`, `description`, `review` — you can put them **in the note
body** where they are readable, editable, and diffed like prose
(ADR-0460):

```console
$ relate --database ~/vaults/notes --markdown-body-properties \
    "summary,notes;Book=summary,review" "RETURN 1"
```

The grammar: an ordered list of property names (`summary,notes`), with
optional `Label=list` entries overriding it per label. The list's
**first** property renders as the note's prose (the part before any
heading); each subsequent one renders as a `## <property>` section in
list order — so Obsidian's outline pane navigates them, and editing a
section in Obsidian edits that property's value:

```markdown
---
type: Book
title: Dune
---

A desert novel.

## review

Kept the spice flowing.
```

A property outside the list stays in the YAML frontmatter, exactly as
before. The heading is a **marker**: only its first word matters, casing
is ignored (`## Notes` opens the `notes` section), a longer word never
matches (`## notebook` is prose), and any text after the marker word —
`## notes Title of the notes` — opens the section with
`## Title of the notes` as its first line: the title keeps its heading,
and `n.notes` in a query returns `## Title of the notes…`, never the
marker word itself. This holds for the **first** entry too: a
hand-written note may head every section explicitly — `## summary` then
`## notes` — and each marker is stripped from its own value (the first
entry's heading opens the bare slot). At checkpoint the engine
re-renders the canonical form (`## notes` on its own line, the kept
title heading as the first content line, the first section bare) — same
values, normalized layout.

The configuration is recorded in `.relate/manifest.json` at
every checkpoint, so **you only pass the flag when changing it** — every
later open (including plain Obsidian-side edits while closed) uses the
recorded one. Two rules to know: a heading line whose first word is a
configured property *is* the section boundary (don't configure a name
you want as literal prose — after the first section has text, a
repeated heading is kept as prose), and changing a label's list
re-interprets its existing notes — no text is ever lost, but sections no
longer in the list become part of the first property's text.

## Editing while the engine is closed vs. open

- **Closed**: your Obsidian edits are picked up at the next open.
- **Open**: the engine is the writer. Edits made to the files while a
  database holds the vault open are detected and refused with an error
  asking you to close, edit, reopen. (Merging live concurrent writers
  is not supported in v1.)
- If the engine was interrupted with uncommitted changes (a dirty
  WAL), a vault digest guard fails closed rather than replaying over
  your edits. An engine-side journal recovers its own crashed
  checkpoints automatically.

## Serving the vault to Neo4j tools

`serve` exposes the vault over Bolt (and HTTP/MCP), so Neo4j Browser,
the drivers, and tools like the Neo4j Python visualization library can
connect:

```console
$ relate serve --database ~/vaults/notes --bolt 7687
```

See the [demo notebook](../../examples/python/obsidian_vault_demo.ipynb)
for the Python path — embedded and via Bolt — with visualizations.

## FAQ

**Can I keep using Obsidian?** Yes — the vault stays a valid Obsidian
vault; wikilinks resolve and the graph view works. What the engine
adds: the hidden `.relate/` folder, the two lock markers
`.cypherlite.lock` and `.cypherlite.writer` at the vault root, and the
generated `index.md` / `log.md` views.

**Does it work with git?** Well: the note files are the store, so
history is per-file and human-readable. The `.relate/` sidecar
(engine state) is usually ignored or committed alongside; the two lock
markers are per-machine and belong in `.gitignore`.

**Can it delete my vault?** `reset` refuses to run on a markdown store.
Individual files are still deleted when the corresponding node is
deleted — the store is bidirectional by design. That includes notes at
the vault root, which load under `:Vault`: in a vault that is also a
git repository, a `README.md` at the root is a `:Vault` node, and a
label-less delete such as `MATCH (n) DETACH DELETE n` removes it. Keep
repository files out of the vault root, or scope deletes by label.

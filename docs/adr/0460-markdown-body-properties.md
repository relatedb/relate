---
type: adr
area: engine
claim: ca7cf7f9bccae68a41363c381b6a2ad0bc078b33
name: 0460-markdown-body-properties
status: proposed
tags:
  - checkpoint
title: Ordered body properties for the markdown vault — the primary bare, the rest as `##` sections
REFERENCES:
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
---

# ADR-0460: Ordered body properties for the markdown vault — the primary bare, the rest as `##` sections

## Context

[ADR-0406](./0406-markdown-vault-snapshot-codec.md) pinned the vault body to exactly one property: the file body is
`properties["body"]`, rendered bare, and headings inside it are markdown
text, not structure. The lpgfs exploration it built on had a second form —
`## <name>` body sections mapping to named properties — which 0406
rejected as ambiguous (which property does a heading-less body belong
to?) and round-trip-breaking.

The rejection threw out the capability with the ambiguity. A knowledge
base whose notes carry several long-text attributes (`summary`,
`description`, `notes`, `review`) has nowhere human-readable to put them:
everything except `body` is YAML frontmatter, where multi-line prose is
hostile to read, diff, and edit. No file-per-note system solves this
today — Obsidian and Notion give a page exactly one content body with
properties as metadata; Roam and Logseq are block outlines where
"attributes" are child blocks, a model that does not transfer to one
file per node. Markdown headings are the one separator that is
markdown-native, renders in Obsidian's reading view, outline pane, and
heading links, and survives every other tool.

## Decision

An **ordered list of body properties per label, with a global default**.
The list is open-time configuration, recorded in the sidecar manifest,
and resolved one way at every site that reads or writes a node file.

- **Configuration**: `--markdown-body-properties SPEC` /
  `CYPHERLITE_MARKDOWN_BODY_PROPERTIES` / `OpenOptions.markdown_body_properties`.
  SPEC is `;`-separated segments; a bare segment is one or more
  comma-separated property names accumulating as the **global default**
  in order (`summary,notes`), a `Label=summary,notes` segment sets that
  label's list. A name may not contain `,`, `;`, or `=`. An explicit
  config at open **replaces** the recorded one wholesale (per-label
  entries not named in it are dropped).
- **Recording**: the sidecar manifest carries `bodies` — the default
  list plus the per-label map — at every checkpoint, so the vault is
  self-describing and later opens need no flag. Unlike `learned`
  (descriptive, drift-regenerated), `bodies` is store configuration, the
  same class as `codec`: never pruned by label churn, replaced only by
  an explicit open-time config. Resolution: explicit config if given,
  else the sidecar's, else the one-entry default `["body"]` — which is
 [ADR-0406](./0406-markdown-vault-snapshot-codec.md)'s rule exactly, so existing vaults are unchanged.
- **Rendering** (checkpoint): the listed properties the node carries as
  representable strings, in list order, become the body. **Zero → empty
  body; the list's first entry, when present, is the bare body (1:1,
  0406's form); every other present property renders as a
  `## <property>` section in list order — a later entry present *alone*
  keeps its heading, because rendered bare it would re-parse as the
  first entry's text.** A listed property that is absent, non-string,
  or not representable (blank or outer whitespace) stays in frontmatter,
  exactly like a demoted `body` today.
- **Parsing** (open): a line whose heading word — the first word of an
  exact ATX `## ` line, compared through the case fold — names a
  non-first list entry is that property's section boundary: `## notes`
  in any casing (`## Notes`) matches, `## notebook` never matches
  `notes`, and any text after the marker word (`## notes Title of the
  notes`) is the section's opening content line with its heading kept
  (`## Title of the notes` — a titled section stays a heading; only a
  whitespace-only remainder was an artificial marker line, contributing
  nothing), so a property's value never carries its own marker word —
  the **first** entry's marker included: a `## <first-entry>` heading on
  a still-blank preamble opens the bare slot explicitly (a hand-authored
  note may head every section), and after the first section has text the
  same heading is prose inside it, never a re-open. First occurrence
  wins; a second fails closed ("appears twice"). The text before the
  first boundary is the **first** list entry's value, so a heading-less
  body belongs to exactly one property — the ambiguity that sank the
  lpgfs form in 0406 is resolved by construction. Headings naming
  anything else are ordinary markdown text. A section with blank content leaves the
  property absent. Section order in the file need not match the
  configured order; render always emits the configured order and the
  canonical spelling — a hand-authored `## Notes Title` normalizes to
  `## notes` with `Title` as the value's first line, the same value in
  canonical layout.
- **Guards**: a body property name may not be a reserved frontmatter key
  or a relationship type (the vocabulary disjointness 0406 established
  for `body` generalizes to every listed name), and a list may not name
  the same property twice. A frontmatter key and a body section of the
  same name collide and fail closed at parse, the existing
  arrives-twice refusal.
- **Tolerances**: the raw-file (unparseable-frontmatter) body loads into
  the label's **first** body property. A bootstrap without configuration
  keeps `["body"]` — headed sections in a hand-authored vault stay body
  text until the flag declares them (inferring body properties from
  heading shapes would be the shape-sniffing LABS-311 forbids). `clone`
  into a markdown destination takes the same config.

**Documented constraint** (the one this grammar cannot remove): a
content line reading exactly `## <name>` for a configured non-first body
property is a section boundary. A note that wants such a heading as
prose must not configure that name — the same class of constraint as
the key-property constraint and the reserved view filenames.

## Alternatives considered

- **All sections headed (no bare primary).** Rejected: symmetric but
  every multi-property note opens on a heading, the single- and
  multi-property cases render differently, and the most natural human
  position for primary text — the preamble — has no owner.
- **HTML comment delimiters (`<!-- … -->`).** Invisible in Obsidian's
  reading view and hostile to hand editing; the data is real, not
  annotation.
- **Obsidian callouts (`> [!note] …`).** Obsidian-only rendering; not a
  round-trip form in any other tool.
- **Secondary text properties as frontmatter block scalars.** Buries
  the text a vault exists to hold in YAML; poor in the Obsidian
  properties UI and in diffs.
- **Inferring body properties at bootstrap from heading shapes.**
  Shape-sniffing (LABS-311): prose containing `## notes` would silently
  become a property split. Configuration is explicit and recorded.
- **Open-time config only, never recorded.** The vault would not be
  self-describing; reopening without the flag would misparse sections
  as the first property's text — silent corruption, the one failure
  class a storage codec may not have.
- **Learned-schema treatment (descriptive, drift-regenerated).** The
  list is a rendering directive, not an observation; drift-regenerating
  it could drop a declared label's list the moment its last node is
  deleted and change how the next node of that label renders.
- **lpgfs `in_TYPE` markers.** Rejected for the same reasons as in
  0406; `## <name>` headings are the markdown-native form.

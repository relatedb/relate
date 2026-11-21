---
type: adr
area: engine
claim: 51f793a8f9b4ee66377b0d577f2364c6aa4a02dd
date: 2026-11-21
deciders: the maintainers
name: 0648-markdown-sidecar-own-bound
status: proposed
tags:
  - storage
title: "The markdown sidecar rides its own metadata bound: 256 MiB on native targets, the ADR-0086 ceiling on wasm"
REFERENCES:
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
  - "[[adr/0086-persisted-id-validation]]"
---

# 0648: The markdown sidecar rides its own metadata bound

## Context

ADR-0086's 16 MiB per-metadata-object ceiling bounds every untrusted
JSON read: manifests, ontology documents, schema mirrors. The pattern
is "never trust a file's declared size for an allocation" — a planted
or corrupt object past the bound is refused before any buffer is
allocated, and writers refuse before serializing so a store is never
created that its own reader would reject.

The markdown vault's sidecar (`.relate/manifest.json`,
[ADR-0406](./0406-markdown-vault-snapshot-codec.md) §3) was spelled
through that same constant. But the sidecar is the one metadata object
whose size legitimately scales with user data: it carries per-file
stats, node ids and relationship identities for every guarded file —
measured ~220 bytes per file. The 16 MiB ceiling therefore capped the
vault itself: bootstrap and checkpoint refused around 45k files (the
sidecar's write-side check fired first, so the wall was symmetric —
the engine never created a store it could not reopen, it just refused
to hold one that big). A 100k-file vault could not exist.

## Decision

The sidecar gets its own ceiling: **256 MiB on native targets**
(`snapshot::MAX_MARKDOWN_SIDECAR`), consulted at every sidecar check
site — the load's reader, both spellings' probes, the cold
`store_info` projection and the write-side refusal — through the one
constant, never a mirrored copy. wasm32 keeps the ADR-0086 ceiling
unchanged: a browser tab cannot afford a quarter-gigabyte sidecar
parse, and browser stores are small by position.

The untrusted-input class is untouched: classic root manifests,
ontology documents, schema mirrors, bundle catalogs and Parquet footer
metadata all keep ADR-0086's 16 MiB. The distinction is authorship:
those objects' sizes are bounded by schema and catalog shape, and a
past-bound copy of one is a plant; the sidecar is engine-authored
state the engine itself just wrote, and refusing to read what it wrote
is a scale bug, not a safety win.

## Consequences

A markdown vault holds ~1.2M files before the sidecar bound binds
(~220 bytes per file measured on a 100k-file vault; a 22 MiB sidecar
bootstraps and reopens in ~6 s / ~4.5 s warm on an internal SSD).
Past the new bound the behavior is unchanged: the writer refuses
before serializing, symmetric with the reader. The inline ontology
document the sidecar may carry cannot smuggle past ADR-0120's 16 MiB
document cap, which is enforced at commit, before anything reaches
the sidecar. A vault heading past ~1M files wants a compact sidecar
format (the JSON is wasteful — repeated full path strings, pretty
printing) — a follow-up with its own ADR, not a bound raise.

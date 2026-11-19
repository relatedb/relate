---
type: adr
area: engine
claim: e507b23fbe6340c4118acce43b7f0e42d2c55c1c
name: 0623-engine-sidecar-dir-rename
status: proposed
tags:
  - checkpoint
  - storage
  - wal
title: The engine sidecar directory is `.relate/`, adopted from `.cypherlite/` at open
REFERENCES:
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
  - "[[adr/0481-schema-as-files]]"
  - "[[adr/0517-rename-to-relate]]"
---

# ADR-0623: The engine sidecar directory is `.relate/`, adopted from `.cypherlite/` at open

## Context

The markdown codec's engine sidecar ([ADR-0406](./0406-markdown-vault-snapshot-codec.md)) — the WAL, the checkpoint
journal and the sidecar manifest — and the schema-files mirror ([ADR-0481](./0481-schema-as-files.md))
live under a hidden `.cypherlite/` directory inside the store. The rename
to Relate ([ADR-0517](./0517-rename-to-relate.md) §4) deliberately kept that on-disk name, together
with the lock and writer markers, so the bytes of existing stores stayed
stable. Every other surface has since moved to the new name; this
directory is the last on-disk spelling of the old one that a fresh store
still creates, so every new store re-plants the pre-rename name.

## Decision

- The engine directory is `.relate/`. The one spelling home (`engine_dir`)
  and every path derived from it — the sidecar manifest, the markdown
  WAL directory, the checkpoint journal, the schema mirror — follow.
- A writable open adopts a legacy store in place: when
  `<root>/.cypherlite` exists and `<root>/.relate` does not, the whole
  tree is renamed with one directory rename before any sidecar probe or
  lease. There is no dual-read path.
- A store carrying both directories refuses the open closed, naming the
  hand repair (delete the stale legacy directory). A store carrying
  neither is fresh and untouched.
- Read-only opens never adopt — they cannot write. Their probes simply
  miss the un-adopted sidecar, and the next writable open adopts it.
- The lock and writer marker files and the lock role bytes are
  unchanged; renaming those is a separate decision if ever taken.
- The frozen versioned store fixtures keep their historical bytes: the
  golden-store tests copy the fixture, and the copy adopts.

## Alternatives considered

- Keep `.cypherlite/` on disk ([ADR-0517](./0517-rename-to-relate.md) §4's keep): rejected — the
  product has shipped under the new name, and every fresh store would
  keep re-creating the old one.
- Dual-read — probe both names at every site: rejected — every path
  constant, probe and identity check would carry two spellings forever,
  to serve a store population the adoption empties at first open.
- Rebuild the sidecar from store content instead of renaming: rejected —
  the WAL tail and an in-flight checkpoint journal would be discarded;
  the rename keeps their bytes identical.

## Consequences

- Existing stores migrate silently at their first writable open after
  the upgrade. A crash inside the adoption window leaves the legacy
  name, and the next writable open re-adopts — one atomic step, no torn
  state.
- The versioned golden-store sets keep opening: the copy adopts, and the
  open-time drift guard then verifies the schema mirror at its new path
  against the same canonical renders.
- A read-only open never adopts. A legacy store whose sidecar is
  markdown refuses the read-only open closed, naming the one writable
  open that adopts it — resolving it as a fresh classic directory would
  probe its WAL at the wrong spelling. A legacy classic store's sidecar
  (the schema mirror alone) is inert: the mirror guard skips an absent
  mirror and the open proceeds.
- An open refused after the adoption (a codec-configuration gate, a
  mixed-evidence resolution) leaves the store adopted: the rename is
  durable and idempotent, and the next open proceeds on the adopted
  spelling — nothing is torn.
- The rename-residuals keep-list still matches every remaining
  old-name occurrence: the marker files, the adoption's legacy spelling,
  and the frozen records.

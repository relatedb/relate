---
type: adr
area: engine
claim: 13d3d68c454aaf9f5459fbcbe57edbb4c2a96a6a
date: 2026-11-21
deciders: the maintainers
name: 0647-vault-identity-fold-composes-to-nfc
status: proposed
tags:
  - storage
title: "The vault identity fold composes to NFC: a decomposed filename is its composed spelling, and the checkpoint heals to the composed canonical form"
REFERENCES:
  - "[[adr/0406-markdown-vault-snapshot-codec]]"
---

# 0647: The vault identity fold composes to NFC

## Context

ADR-0406 §"Case-insensitive identity" defines the vault's one identity
relation as the Unicode case fold: labels and keys compare
case-insensitively, a case-only collision is a data-quality fold (first
spelling in path order wins, the later drops with a warning), and the
key property must match its filename under the fold. The fold was
`to_lowercase()` — case-aware, but not normalization-aware: two
spellings that differ only in Unicode normalization (composed `ö`,
U+00F6, against `o` + combining diaeresis) folded to different
strings.

That made a vault unopenable after an otherwise byte-perfect copy.
macOS `tar` writes decomposed (NFD) filenames while the engine writes
composed (NFC); after such a copy the load's key check read the
frontmatter's composed value against the disk's decomposed filename
and refused with `key property 'name' value … does not match its
filename` — two canonically equivalent spellings treated as different
names. The same class of failure follows any normalizing tool in the
copy chain (some sync tools, zip extractors). Canonical equivalence is
name equality; the identity model did not say so.

## Decision

1. **The fold composes.** The one identity relation
   (`relate_markdown::fold_case`) is `to_lowercase()` composed to NFC,
   so every consumer — key matching, case-only duplicate detection,
   label resolution, short-form link candidates, the reserved-label
   collision — treats normalization-only differences exactly as it
   treats case-only ones: same name, first spelling in path order
   wins, the later drops with the fold warning.
2. **The codec derives the canonical spelling.** The load splits each
   walked stem into its `(label, name)` pair composed to NFC
   (`md::compose`), so the vault key, the node's key property and the
   learned schema carry the composed form whatever spelling is on
   disk. A decomposed-named file loads as its node; the key check
   passes because both sides fold to the same composed string.
3. **Sanitization emits the composed form.** `sanitize_label` /
   `sanitize_name_path` compose their output to NFC — the engine's one
   canonical on-disk spelling. A checkpoint's plan therefore names the
   composed file, and the apply tail's existing delete-before-write
   order (the same order that makes a case-only spelling a rename on
   every filesystem) renames a decomposed file to the composed form:
   the vault heals at the next checkpoint that has something to
   publish. On a normalization-insensitive filesystem (default APFS)
   the two spellings are one physical entry, so the rename is a
   re-store under the canonical name; on a sensitive one it is a
   plain rename.

## Consequences

A vault copied through a normalizing tool opens; before this ADR it
refused. Normalization-only duplicates are reported through the same
fold warning path as case-only ones — one duplicate policy, not a
second. Engine-written names are always the composed form, so the
on-disk spelling converges to one canonical form over time. The
reserved-view name check stays ASCII-folded (`index.md` and friends
are ASCII) and is untouched. The heal rides real checkpoints: a clean
session's exit writes nothing (ADR-0646), so a decomposed spelling
persists until a write or an explicit `checkpoint()` republishes.

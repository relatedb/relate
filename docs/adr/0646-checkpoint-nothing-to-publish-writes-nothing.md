---
type: adr
area: engine
claim: 2904697b22ef6e4ecc601ca49f273a4c1f81556c
date: 2026-11-21
deciders: the maintainers
name: 0646-checkpoint-nothing-to-publish-writes-nothing
status: proposed
tags:
  - storage
title: "A checkpoint with nothing to publish writes nothing: persist-on-drop and close paths skip the dump over a clean WAL and a published snapshot"
REFERENCES:
  - "[[adr/0406-the-markdown-vault-store]]"
  - "[[adr/0105-ephemeral-stores-recycle-only]]"
---

# 0646: A checkpoint with nothing to publish writes nothing

## Context

Every durable store's close path checkpointed unconditionally: a CLI
query session's final checkpoint, the engine's persist-on-drop, and the
WAL-rotation/timed checkpoints all ran the codec's full dump whenever
their trigger fired, whether or not anything had changed since the last
checkpoint.

The cost is codec-shaped, and the markdown vault store
([ADR-0406](./0406-the-markdown-vault-store.md)) pays it hardest. Its
dump is a whole-vault pipeline: re-walk and re-read every guarded file,
re-render every node file, regenerate the derived views, serialize and
journal the sidecar manifest, publish, then remove the journal — for a
27k-file vault roughly half the session's wall clock on an otherwise
unchanged session, doubling the I/O the open's own full load already
paid. The classic codecs pay a smaller version of the same: a full
image rewrite of identical bytes.

Meanwhile the engine already owns the fact that makes all of this
redundant: `Wal::has_uncheckpointed()` — the predicate the timed
checkpoint gates on — says whether any commit this session appended or
any replayed tail still waits for a dump. A checkpoint whose WAL is
clean over a store that has already published a snapshot would rewrite
exactly the bytes it just read.

## Decision

1. **The skip.** A checkpoint whose trigger is not an explicit
   `Database::checkpoint()` skips the dump — writes nothing, recycles
   nothing, GCs nothing — when both hold:
   - the pinned WAL holds nothing uncheckpointed (`has_uncheckpointed()`,
     read under the pin's own WAL guard and carried into the dump
     decision; the dump path itself never takes the WAL mutex before
     publication, preserving the ADR-0020 lock order), and
   - the store has already published a checkpoint image — one new
     `has_published_snapshot` row in the codec ops table: the classic
     codecs' root `manifest.json`, the markdown codec's `.relate/`
     sidecar presence probe.
   Both halves are load-bearing: a clean WAL alone would skip the first
   checkpoint of a bootstrap (leaving the codec unpinned and no files
   materialized), and a published image alone would skip publishing a
   replayed tail.
2. **The triggers.** Persist-on-drop, the timed checkpoint, the
   WAL-rotation checkpoint, and the new close shape
   `Database::checkpoint_if_pending()` — the exposed persist-on-drop
   path the CLI's final checkpoint now calls instead of `checkpoint()`
   — all take the skip. An explicit `Database::checkpoint()` never
   skips: it stays the user's force-dump and keeps the codec
   validations that run only there (the Parquet manifest's
   part-metrics refusal) and the `:memory:` always-writes contract of
   ADR-0105.
3. **What the skip never touches.** The markdown codec's open-time
   re-baseline of an externally edited vault publishes at open, not at
   checkpoint, so edit pickup is unchanged. Migration-on-open
   checkpoints through the storage facade directly and never skips.
   Read-only handles never checkpointed and still do not.

4. **The accepted edge: mid-session sidecar corruption.** The
   published-image probe is an existence check at either spelling, so
   a clean session over a sidecar an external tool corrupted *after
   the open* (the sidecar is engine state, not a guarded vault file —
   the external-write guard never sees it) exits through the skip
   where the pre-skip engine's unconditional dump would have rewritten
   it. Accepted: corrupting engine state mid-session is the
   WAL-corruption class (never healed either), the window requires an
   external writer touching `.relate/` between the open and the close,
   and the recovery is any corrupt sidecar's — reopen, which names the
   corruption in its own refusal. A *removed* sidecar still heals: the
   probe answers false and the dump republishes.

## Consequences

An unchanged session over a published snapshot leaves every durable
byte untouched — not even a same-content re-publication with a fresh
mtime — which is the observable the tests pin (byte- and
mtime-identical sidecar/manifest, no staged journal). Sessions that
commit, or that replay a pending tail, checkpoint exactly as before.
The store's schema text mirror and derived views advance only when a
real dump runs; a clean session keeps the previous checkpoint's, which
by induction describe the same state. The markdown vault's
hand-formatted files likewise survive a clean exit without the
normalize-at-exit re-render a full dump would have performed — the
guard's error text already directs external-edit users through close
and reopen, so no documented contract moves.

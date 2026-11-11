# ADR-0500: Store targets and sources — `--database` + `--load`, and ephemeral opens

Status: Proposed

Claim: d8430ab36fabfde21b3f0bd2891f31371a2fdd25

## Context

The engine is memory-first: opening any store — a native directory or a
`.cl` bundle — loads the snapshot and replays the WAL into the in-memory
`Graph` and `Wal`. After the load, the durable source is only needed by the
*writable* lifecycle: the writer lease, checkpoints, demote. A read-only
open touches none of that, yet it keeps the source open and locked for the
store's whole life: the `.cl` host file (and its exclusive lock,
ADR-0074) and the WAL's open segment handle hold it.

LABS-903 (user direction, 2026-11-03) asked for the other shape, with the
wording that settled the design: **"not copy — turn into"**. A read-only
open should *turn* the store into the in-memory store while loading — the
source is released after the load, not kept as a second representation.
The motivating cases: a remote store whose download should not live on
disk at all, and environments that cannot write any file.

The same review of the CLI shape (user direction, same day) found the
flag axis at fault: ADR-0496 had made `--database` accept an `http(s)`
URL, so one flag carried two facts — *which store am I opening* and
*where does its content come from* — and a third flag (`--ephemeral`, the
first cut of this ADR's CLI half) existed only to undo one consequence of
that conflation: the materialized source's persistence. The user's
direction: drop the kept-copy remote `--database` (unreleased — never in
a release, so free to remove) and split the axes — **`--database` is the
target, `--load` is the source**.

## Decision

1. **`--database PATH` (`-d`) names the store target only** — a store
   directory, a `.cl` bundle, or `:memory:` (the default, unchanged). An
   `http(s)` URL in `--database` is a usage error (exit 2) that names the
   working form (`--load <url>`). This withdraws ADR-0496's §1 (the
   URL-in-`--database` spelling) and its §5 kept-copy disposition — the
   feature never shipped in a release. Everything else ADR-0496 decided
   about the fetch itself (§2–§4, §6–§9: the GitHub rewrite, the redirect
   and scheme policy, the `.cl` requirement, the capped streaming stage,
   the safe-mode refusal, the feature gate, the CLI-process posture)
   carries over unchanged to `--load`'s remote sources.
2. **`--load SOURCE` names the store source** — a local `.cl` bundle, a
   native directory store, or an `http(s)` URL to a `.cl` bundle — loaded
   into the target. Two legs:
   - **`:memory:` target (the default): the ephemeral open.** The engine
     loads the source as usual and then **releases** it — `OpenMode::Ephemeral`,
     `Database::open_ephemeral(uri)`. The loaded store is `:memory:`-shaped
     for its whole life: nothing holds the source open or locked, it can
     be deleted or opened writable underneath, and the loaded copy is
     frozen snapshot freshness. A remote source's scratch copy is removed
     right after the load — nothing on disk at all.
   - **Durable target (`-d PATH --load SOURCE`): store creation.** The
     target path must not exist (fail-closed; exit 2 if it does); the
     source is cloned into it through the `clone` verb's own machinery
     (the full dir/`.cl` matrix, ADR-0156), and the run then opens the
     created store with the flags' normal mode — writable by default.
     This is the one capability the old shape could not express: a URL
     materialized into a durable store **at a path the user chose**.
3. **The release is a swap, not a teardown.** After the load, every holder
   of the source lets go — the store target, the WAL's open segment handle
   (`Wal::release_source`: the replayed LSN/counters/magic stay, a fresh
   in-process WAL takes the source's place), and the store holds a
   `MemoryVfs` stand-in in its place. Any path
   that should never run on a read-only store behaves like `:memory:`'s,
   never like a released file's.
4. **Ephemeral is read-only by definition** — it is a mode variant beside
   `ReadOnly`, not an option on top of one: writes have nothing to write
   back to and a follower reads the source's WAL after load, so neither
   combination can be constructed. `:memory:` with `Ephemeral` is the
   plain memory open (the release is the no-op it never takes). The first
   cut carried an `OpenOptions::ephemeral: bool`, and the review's
   blocker was right to kill it: the flag was non-orthogonal with `mode`
   and its validation ran after the lease — a fail-before-lease violation.
   As a mode variant the illegal combinations are unrepresentable.
5. **CLI resolution.** `Flags::open_mode` stays the one open-mode
   resolution: it answers `OpenMode::Ephemeral` exactly when `--load` is
   given with the `:memory:` target, else the access slot's ADR-0480
   resolution. `--read-only=live` with any `--load` is a **parse-time**
   usage error (nothing is ever followed — the source is loaded once and
   released); `--read-only` with a creating load (durable target) is
   refused at resolution. The `--ephemeral` flag of this ADR's first cut
   died before shipping: "`--load` into the default `:memory:` target"
   says the same thing without a second flag, and the `:memory:` target
   makes the release structural rather than a mode the user must remember
   to ask for.
6. **Every fetched copy is scratch.** A memory-target load's copy is
   removed right after the load (the source was released); a
   durable-target load's copy is removed the moment the clone finishes; a
   failed fetch removes its own partial file. The kept-copy disposition
   (ADR-0496 §5's writable download kept at an announced temp path) is
   gone — a durable copy is the durable-target leg, at a path the user
   chose, not a temp path the run picked.
7. **`serve` takes the same grammar** — `serve --load SOURCE` with the
   `:memory:` target serves the ephemeral open (read-only by construction,
   no bundle lock held while serving); with a durable target it creates
   the store and serves it. The banner's access-mode line reports the
   ephemeral mode like the other non-writable modes (ADR-0306 §5).
8. **Scope.** The no-writable-disk remote case (a `Cursor` host for the
   bundle loader, so the download never touches a file) and HTTP `Range`
   lazy page fetching remain LABS-903's open tail.

## Alternatives considered

- **An `OpenOptions::ephemeral: bool`** — killed in review: non-orthogonal
  with `mode`, ~40 exhaustive-literal spellings across the surfaces, and
  validation after the lease. The mode variant makes the illegal states
  unrepresentable.
- **A `--ephemeral` CLI flag (this ADR's first cut)** — died the same
  death at the CLI layer once the axis split was seen: it existed only to
  undo `--database`'s conflation, and `--load` with the default `:memory:`
  target states the same behavior structurally.
- **A `--read-only=ephemeral` freshness value** — rejected twice over:
  ADR-0480's values are freshnesses and ephemeral's freshness *is*
  snapshot's (an ephemeral open reads exactly what a snapshot read-only
  open reads, at every moment — the difference is the source's fate, not
  the read's), so the value would be a synonym with a side effect bolted
  on; and it would put the one non-freshness arm inside the freshness
  witness.
- **Keep `-d URL` as sugar for the ephemeral load** — rejected: two
  spellings for one thing and the axis stays blurred; the target-only
  refusal teaches the grammar once.
- **An in-memory host for the bundle (`Cursor<Vec<u8>>` behind a host
  trait)** — the first sketch on the ticket: correct but more surface
  than the ask needs for local files, and unnecessary wherever a brief
  staged download is acceptable. Kept as the follow-up for genuinely
  read-only filesystems.
- **Load-then-unlink the source file** (release by deleting it) —
  rejected: it destroys someone else's store to make a private copy, and
  on a shared filesystem the deletion is visible to others.

## Consequences

- `cypherlite --load store.cl "<query>"` is a read-only,
  nothing-held-open exploration of any store; a writable open of the same
  file succeeds the moment it exits.
- `cypherlite -d out.cl --load https://…/store.cl "<query>"` materializes
  a remote store at a chosen path — previously inexpressible.
- A remote load leaves nothing behind: no kept copy, no scratch at exit.
- Telemetry classifies a memory-target load as `memory` storage, whatever
  path or URL it came from; a durable-target load as the target's kind.
- ADR-0496's URL-in-`--database` spelling and kept-copy disposition are
  withdrawn (unreleased); its fetch mechanics carry over to `--load`.
- No `OpenOptions` construction site changes — the turn-into is a value
  of the existing `mode` field.

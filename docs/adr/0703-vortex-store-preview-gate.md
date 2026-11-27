---
type: adr
area: engine
claim: 437a3a23adb4f6d1c77a6fd989a26332bd051acc
date: 2026-11-26
deciders: the maintainers
name: 0703-vortex-store-preview-gate
status: proposed
tags:
  - storage
  - preview
---

# ADR-0703: the vortex property store gates behind the first storage-representation preview row

## Context

ADR-0665's columnar property store reached answer parity and a measured
memory/evidence story behind an executor seam (`PropStore`) that the
experiment harness attaches by hand. The store's durable half (the shard
codec) installs through a process-wide registry so the storage crate
compiles without the Vortex dependency (ADR-0677). Nothing user-facing
exists: an embedding, a CLI run, or a Python session cannot ask for the
store, and every measurement so far drove it through the experiment
crate's own binary.

The wiring decision is only the gate: how a user opts in, which builds
can serve the opt-in, and what a build that cannot do about it. The
rkyv codec (ADR-0537's flag shape) already answers this class once:
a default-off cargo feature, a typed field, an environment spelling,
and a fail-closed error naming the feature when the flag is absent.

## Decision

The store is **`vortex-store`**, one runtime opt-in resolved once at
`Database::open`, served only by builds that carry the cargo feature of
the same name:

1. **The build gate** — `vortex-store` is a default-off cargo feature on
   `relate-cli` (forwarding to the `relate` crate, which forwards to the
   executor's store adapter and the storage crate's shard codec; the
   optional `vortex` dependency rides the feature, ADR-0677's registry
   seam unchanged). No bundle (`full`, `agent`, `minimal`, `serve`)
   turns it on: an experimental store does not ship in a default build.

2. **The runtime opt-in** — one fact, three spellings, one resolution:
   the typed `OpenOptions::vortex_store: bool` field, the
   `RELATE_PREVIEW=vortex-store` registry row, and the CLI's
   `--vortex-store` flag (which sets the field for the one open the CLI
   makes). The store attaches when the field is set or the row is on.
   This is the preview registry's **first storage-representation row**:
   the registry's rows to date gate Cypher syntax or procedure names at
   parse or dispatch (ADR-0371, the ADR-0497 exceptions); this row gates
   a storage representation at **open**. The registry stays the single
   feature-name table — the row lives beside the others, unknown names
   keep failing closed, and the gate-location class is the taxonomy's
   newest entry, recorded here rather than stretched from an old one.

3. **Fail-closed, both directions** — opting in on a build without the
   feature fails the open (exit 1 for the CLI) with the error naming the
   feature to enable, exactly the rkyv/parquet shape; the `--help` lists
   the flag only in feature builds (a flag whose feature is off is an
   unknown flag). The attach admits the graph's shape before it builds
   the store — a client-inflated node-id space and a crafted
   distinct-key-per-node shape each fail the open with a typed storage
   error rather than reserving attacker-sized tables — and the
   follower's reload degrades to the compact records on such a refusal
   (continuity; the open's refusal was the opt-in moment). Setting the
   row on a feature build turns the store on for **every** open in that
   process — `RELATE_PREVIEW` is read once and is process-global, which
   the client docs state as the loud caveat.

4. **Loud experimental state** — an open that attached the store says so
   in the store info the CLI prints (the `NOTE_EXPERIMENTAL` line shape
   the rkyv codec uses), so an attached run never reads as a default
   one.

The Python, JavaScript and other C-ABI clients get the environment
spelling only in this round: their open entry points bottom out in the
C ABI's optionless `relate_database_open`, and a typed opt-in means a
new ABI function plus one wrapper per client — deliberately not this
change.

## Consequences

- The experiment crate stops owning the store: the engine adapter moves
  behind the executor's feature and the shard encode/decode behind
  storage's, each feature-gated; the crate keeps the kernels and the
  A/B harness, which now build against the product feature instead of
  their own attachment glue.
- The A/B measurement discipline (ADR-0665's interleaved, shared-store
  shape) is unchanged — the flag only changes who attaches, not what
  the store answers.
- The registry gains a row whose gate is not in the parser or the
  dispatcher; a future storage-representation preview follows this row
  rather than inventing a second table.
- When the store stops being experimental, the row is retired the way
  every preview row is: a new ADR, and the feature moves toward a
  bundle decision — this ADR does not prejudge that.

---
type: adr
area: engine
claim: 2b571c83f784709de091dc8bc958471d3171bd1f
date: 2026-11-24
deciders: the maintainers
name: 0677-vortex-publish-closure
status: proposed
tags:
  - storage
title: "The Vortex experiment leaves the publish closure: two vortex-free seams and a workspace-excluded lab crate"
REFERENCES:
  - "[[adr/0033-compact-live-records-typed-csr]]"
  - "[[adr/0105-optional-parquet-codec]]"
  - "[[adr/0537-experimental-snapshot-codecs]]"
  - "[[adr/0665-vortex-columnar-property-store]]"
  - "[[adr/0671-rust-toolchain-msrv-1-99]]"
---

# 0677: The Vortex experiment leaves the publish closure — two vortex-free seams and a workspace-excluded lab crate

## Context

[ADR-0665](./0665-vortex-columnar-property-store.md) carries the Vortex
columnar property store as an experiment behind two features:
`relate-executor/vortex-store` (the store) and
`relate-storage/vortex-codec` (the snapshot codec). Both link
`vortex` as a **git** dependency of the relatedb fork.

Cargo refuses to package any crate whose manifest names a dependency
without a version requirement, whatever the target or feature gate:

```
error: all dependencies must have a version specified when packaging.
dependency `vortex` does not specify a version.
```

`--no-verify` skips the packaged tarball's build, never manifest
validation. With the entries in place, `cargo package -p relate-executor
--no-verify` and `cargo package -p relate-storage --no-verify` both exit
101, so the crates cannot be released.

[ADR-0671](./0671-rust-toolchain-msrv-1-99.md) raised the workspace MSRV
to 1.99, above the fork's 1.95 floor, so the lab no longer needs a
lane-local toolchain override. The publish closure still requires the
fork to leave the published manifests, so the lab stays excluded from the
workspace: a member would put the fork back in the root lock, and a cold
`cargo package --offline` then fails on the fork checkout.

## Decision

The Vortex implementation moves out of the publish closure. The published
crates keep only additive, vortex-free **seams**; one workspace-excluded
crate that is never published links the fork.

### The lab crate

`testing/relate-vortex-lab` stays a workspace-excluded crate
(`publish = false`, its own `[workspace]`) and holds the vortex git
dependency and every line of vortex-typed code: the `PropStore`
implementation and the snapshot codec's shard encode/decode. Its own
`Cargo.lock` is the only lock that resolves the fork; the root lock
carries no Vortex package. It is the A/B driver too: `relate-vortex-ab`
runs the query-latency suite and the pokec groups with the store attached
through the suite's hook. Its edges point outward — the lab names the
workspace crates by path, and nothing in the workspace names it.

### Seam one: the executor property store

`relate-executor` loses the `vortex-store` dependency and keeps the
existing [`PropStore`] trait (`relate/relate-executor/src/graph/prop_store.rs`),
now `pub`:

- `point`, and the optional `carries` / `serves_at`;
- `label_rows` and `equality`, the label-filter and `=`/`IN` lookup
  shapes the executor dispatches to (returning `None` leaves the lookup
  to the index route).

`Graph` gains `#[doc(hidden)] attach_prop_store(Arc<dyn PropStore>)` and
`interned_id(&str)`. The point-read and lookup dispatch sites consult the
attached store unconditionally; a graph with no store installed answers
exactly as before. The lab's `VortexStore` implements the trait from
outside the crate.

### Seam two: the storage shard codec

`relate-storage` loses the `vortex-codec` feature and dependency. The
codec's storage-general half (`codec/vortex.rs`: the payload manifest,
the row-range shard file names, the orphan retirement) compiles
unconditionally. Its two vortex-typed functions move behind a
process-wide shard-codec registry (`codec/shard_codec.rs`,
`install_shard_codec(encode, decode)`): the lab installs the
Vortex pair at startup, `SnapshotCodecKind::is_compiled` reports the
registry, and without an install the codec fails closed through the
existing `codec_unavailable` — whose Vortex text names the experiment
crate, not a Cargo feature.

### The gate

`scripts/check-package.sh` runs `cargo package --no-verify` for
`relate-executor` and `relate-storage`; it is the publish-closure half of
the pre-push gate in AGENTS.md, so a leaked unversioned dependency fails
before a release does.

## Consequences

- The published manifests carry no git dependency and no Vortex type;
  `cargo package` resolves against the registry alone.
- The fork resolves only in the lab's own lock; the root lock, the
  graded workspace build, `cargo metadata`, cargo-deny and `cargo
  package` never see it.
- The lane `.github/workflows/vortex-lab.yml` builds the excluded crate
  from its own manifest under the repo toolchain (no `RUSTUP_TOOLCHAIN`
  override) and triggers on the seam crates it depends on.

## Later references

[ADR-0665](./0665-vortex-columnar-property-store.md) is the experiment
this ADR gives its final dependency layout. If the Vortex fork publishes
a versioned release that satisfies the MSRV, the git dependency re-enters
as a plain version and this layout can collapse back into the published
crates.

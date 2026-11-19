---
type: adr
area: infra
claim: 4904513b17645931b5a1fad046593631e98d6dc2
name: 0380-first-party-dependency-scope
status: accepted
tags:
  - load
  - workspace
title: First-party dependencies are not new dependencies, and the git import's recorded pair cost
REFERENCES:
  - "[[adr/0021-opencypher-temporal-values]]"
  - "[[adr/0376-git-to-graph-import]]"
  - "[[adr/0377-import-resource-bounds]]"
  - "[[adr/0392-git-import-per-batch-commit]]"
---

# ADR-0380: First-party dependencies are not new dependencies, and the git import's recorded pair cost

Later references: [ADR-0392](./0392-git-import-per-batch-commit.md) supersedes §5's peak arithmetic — the import no longer materialises row vectors before the write; the transient overhead at the neo4j shape is measured there.

## Context

[ADR-0376](./0376-git-to-graph-import.md) and [ADR-0377](./0377-import-resource-bounds.md) pin `cypherlite lab git-to-graph` and both record that
the import adds no Cargo dependency. The implementation makes three manifest
changes, so those sentences are false as written: `cypherlite-core` as a normal
dependency of `surfaces/cypherlite-cli` for `GitCommit::authored_instant`'s
`parse_iso` ([ADR-0021](./0021-opencypher-temporal-values.md)); `cypherlite-core` again in
`testing/cypherlite-test-support` for the fixture date formatter; and
`tempfile` promoted from dev to normal in that manifest, because `git::Repo`
builds the scratch git config and fixture repositories that other crates' tests
use, so the library target needs it.

None is a new crate. `cypherlite-core` is already a normal dependency of
`cypherlite`, which the CLI depends on, so the CLI build already compiles it;
`tempfile` is a workspace dependency already declared in that manifest and
already in `Cargo.lock`. The lockfile diff is the two `cypherlite-core` edges
inside existing package entries and no package. The decision both ADRs record
— no *external* crate, AGENTS.md R6 untouched — is intact; the words around it
are not.

[ADR-0377](./0377-import-resource-bounds.md) also records the `CO_CHANGED` cost from a shape the code no longer
has: §2 describes a `BTreeMap<(String, String), i64>` of ~200-byte owned
entries with a ~300-byte row, and its Consequences add that map to a ~0.7 GiB
peak. The branch tip interns path handles into `Arc<str>`, counts each
commit's pairs while its changeset is in scope, and drops the parsed history
before the write. The map entry is smaller than recorded, but the row is not:
`write::row` wraps a capacity-11 `BTreeMap` in `Value::Map`, so a row is
~0.84 KiB and 500,000 pairs are ~0.42 GiB, not ~250 MiB.

One decision: correct the import's record to match what was decided and built.

## Decision

### 1. The dependency rule concerns external crates

The no-new-Cargo-dependency rule, and AGENTS.md R6 under it, states a fact
about the build's external surface, not about every manifest edit. A
first-party workspace edge — a `{ workspace = true }` dependency on an engine
crate already in the build graph — and promoting an already-declared
dev-dependency to a normal dependency inside the same crate are not new
dependencies, and neither needs an ADR per change. R6 keeps its text and its
force: a new *heavy external* dependency still needs an ADR. A first-party edge
still obeys R1–R5, which `cargo xtask check-boundaries` enforces.

### 2. The three instances

| Manifest | Edge | Kind |
| --- | --- | --- |
| `surfaces/cypherlite-cli` | `cypherlite-core` | normal, already compiled via `cypherlite` |
| `testing/cypherlite-test-support` | `cypherlite-core` | normal, test-only crate |
| `testing/cypherlite-test-support` | `tempfile` | normal, promoted from dev |

They add no crate to the workspace or `Cargo.lock`, change no version or
feature, and stay outside the product runtime: the test-support crate is
`publish = false`; the CLI edge calls a parser the engine already exports.

### 3. Pairs are keyed by interned handles, and a row costs ~0.84 KiB

`build_rows` counts into `BTreeMap<(Arc<str>, Arc<str>), i64>`; every path and
commit sha is interned once into `Arc<str>`. [ADR-0377](./0377-import-resource-bounds.md)'s ~200-byte owned
`String` entry becomes:

- a 32-byte key (two 16-byte `Arc<str>` handles) and an 8-byte count. After
  splits and internal nodes a resident entry measures 72.6 bytes at 500,000
  pairs (34.6 MiB) — not ~200, nor the ~48 a slot-only count suggests;
- path text once in the interner (87.7 bytes per handle at 1.8M), shared by
  map keys and every row that carries it, so no pair allocates a path;
- a row of 863 bytes, not [ADR-0377](./0377-import-resource-bounds.md)'s ~300: `write::row` builds a
  `BTreeMap<String, Value>` and wraps it in `Value::Map(Arc<_>)`. `Value` is
  48 bytes (`Temporal`, its largest variant, is also 48) and `String` is 24, so
  one leaf reserves 11 x (24 + 48) = 792 plus a 24-byte header = 816, the `Arc`
  adds 40, and each key string adds a small allocation. Two-entry `TOUCHED` and
  three-entry `CO_CHANGED` rows both measure 863 bytes; with its 48-byte
  `Vec<Value>` slot a held row is 911.

At the 500,000-pair cap the pair term is ~0.42 GiB: 34.6 MiB of map plus
434.4 MiB of rows (500,000 x 911), the map consumed into the rows as they are
built; summed rather than overlapped, ~0.46 GiB. The cap is unchanged.

### 4. No changeset list, and the history goes before the write

`count_co_changes` takes one commit's `BTreeSet<Arc<str>>`, orders it by stored
path, updates `pair_counts` and drops it, so no changeset list survives to the
write. `import_with_caps` builds the rows inside a block, so the parsed history
is dropped before `db.connect_typed`: the indexes and the write transaction
hold `Rows` only. The peak still includes every phase, not just the pair (§5).

### 5. The corrected peak, and the caps stand

At [ADR-0377](./0377-import-resource-bounds.md) §1's 128 MiB log cap (~300,000 five-path commits):

| Term | Count | Cost |
| --- | --- | --- |
| parsed history ([ADR-0377](./0377-import-resource-bounds.md) §1) | ~300,000 commits | ~230 MiB |
| `TOUCHED` rows | 1,500,000 | 1.27 GiB |
| `COMMIT` and `AUTHORED` rows | 300,000 each | 0.25 GiB each |
| `CO_CHANGED` map and rows | 500,000 pairs | 0.42 GiB |
| interner | ~1.8M handles | 0.15 GiB |
| `FILE` and `PERSON` rows | a normal checkout | ~0.1 GiB |

That is ~2.7 GiB, above [ADR-0377](./0377-import-resource-bounds.md)'s ~0.7 GiB, not below it. If every touched
path and author is distinct the same caps admit ~4 GiB (`FILE` ~1.3 GiB,
`PERSON` ~0.25 GiB).

The pair cap is not the unsafe term: 500,000 pairs buy ~0.42 GiB and remove the
quadratic blow-up (12,250,000 pairs would be ~11 GiB); the log cap sets the
peak. The caps stand as [ADR-0377](./0377-import-resource-bounds.md) pins them — 134,217,728 bytes, 500,000 pairs,
`--max-changeset` at most 1,000 — and this ADR corrects their derivation only.

### 6. The correction lands as Later references

[ADR-0376](./0376-git-to-graph-import.md) and [ADR-0377](./0377-import-resource-bounds.md) are accepted and immutable: each gains a `Later
references` note naming this ADR; their decision text is untouched.

## Alternatives considered

- **Give every first-party manifest edge its own ADR.** A decision per call
  site for a change that adds no crate, version or build unit. Rejected: R6
  names external dependencies; a first-party edge is a compile-graph fact
  R1–R5 already gate.
- **Amend [ADR-0376](./0376-git-to-graph-import.md) and [ADR-0377](./0377-import-resource-bounds.md) in place.** The smallest diff. Rejected:
  accepted ADRs are immutable, and an edit erases what was reviewed; the Later
  references form keeps record and correction visible.
- **Drop the `cypherlite-core` edges and duplicate what they call.** A second
 temporal parser and formatter would copy one engine policy ([ADR-0021](./0021-opencypher-temporal-values.md)) the
  build already links. Rejected: duplication the code-shape rules forbid.
- **Keep `tempfile` dev-only and gate `git::Repo` behind `#[cfg(test)]`.**
  Rejected: the fixture kits are a library module consumed by tests in other
  crates, and a dev-dependency is invisible to a library target.
- **Keep [ADR-0377](./0377-import-resource-bounds.md)'s ~0.5 KiB-per-pair figure as a conservative bound.**
  Rejected: the measured term is ~0.9 KiB per pair (863-byte row, 48-byte slot,
  72.6-byte map entry), so it is an undercount by more than half.
- **Lower the pair cap now that map entries are smaller.** Rejected: 500,000 is
 contract with tests and user-facing messages ([ADR-0377](./0377-import-resource-bounds.md)), the pair term is
  ~0.42 GiB of a ~2.7 GiB peak, and a smaller cap changes behaviour.

## Consequences

- AGENTS.md R6 keeps its text and meaning: a heavy external dependency still
  needs an ADR; a first-party edge or a dev-to-normal promotion does not.
- [ADR-0376](./0376-git-to-graph-import.md) and [ADR-0377](./0377-import-resource-bounds.md) keep every decision and gain one `Later references`
  note each; nothing else in either file is edited.
- The three manifest entries stand as merged, and future first-party edges
  remain subject to R1–R5 via `cargo xtask check-boundaries`: no external
  crate, version or feature changes, so the licence, MSRV and audit surfaces
  are untouched and no surface may depend on more than before.
- The import's caps, messages and behaviour are unchanged. Its peak is
 ~2.7 GiB at the caps for [ADR-0377](./0377-import-resource-bounds.md) §1's five-path history (~4 GiB if every
  path and author is distinct), and 500,000 pairs cost ~0.42 GiB. The pair cap
  is not the binding term; a host with less free memory than the peak fails
  during `build_rows`, before any write.
- A change that re-retains changesets, keys pairs on owned strings, drops the
  shared handles, gives `write::row` a different map shape, or keeps history
  alive through the write invalidates these numbers and needs a new ADR; this
 ADR is the corrected arithmetic behind their [ADR-0377](./0377-import-resource-bounds.md) citation.

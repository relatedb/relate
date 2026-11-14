# ADR-0434: The `extensions/` workspace group — first-party extension crates

Status: Proposed
Claim: aa485534a8d3704237ccba501dd4351a7e7f4784
Later references: ADR-0598 scopes the whole-run stream cap this ADR's
  importer-bounding consequence records to `whole-import` sessions; a
  `per-batch` importer runs uncapped across the session, bounded by the
  per-call caps and the store's live-graph limit.

## Context

ADR-0431 §11 makes the importer extraction mandatory: `fs-to-graph` and
`git-to-graph` leave the CLI crate as first-party extension crates
speaking protocol v1, and §12 retires the `lab` surface outright — no
stub, no compat (the 2026-10-28 user direction). The extraction change
must therefore answer a question ADR-0103's layout never had: where do
the sources of a first-party-but-installed extension live?

ADR-0103 fixed three groups — `cypherlite/` (the engine), `surfaces/`
(adapters of `cypherlite::Database`), `testing/` (conformance and
tooling) — and R5 pins every workspace member to exactly one of them.
None of the three fits a first-party extension crate:

- `surfaces/` is the wrong meaning twice over. The group's definition is
  "adapters of `cypherlite::Database`" — and the defining property of a
  child is the opposite: it carries no `Database` and never touches a
  store (ADR-0430 §6). It would also put the importer sources back
  inside the group that ships the product binaries, one directory away
  from re-introducing the compiled-in importer surface the extraction
  exists to end.
- `cypherlite/` is the engine. A child links the engine's leaf crates,
  but it is not engine code, and R1's whole point is that the engine
  group stays the engine.
- `testing/` is for test-only code; a first-party package is a product.

ADR-0431 §11 anticipated this: "The extension crate sources are
workspace crates placed under the ADR-0103 group and boundary rules;
where the three groups do not fit them, the extraction change records
the placement decision." This is that record.

## Decision

A fourth top-level group, `extensions/`, holds the first-party extension
crates — the child-process sources of the extension program:

- `extensions/cypherlite-wire` — the extension wire's shared kit: the
  bounded-read mechanism (moved from the CLI's `read_cap` module), the
  v1 wire facts (the protocol number and the four caps, stated once for
  both sides of the pipe), and the child-side emission kit (the envelope
  read, the record writer, the capped rows chunker) plus the stored-path
  rendering both importer children share.
- `extensions/cypherlite-fs-to-graph` and
  `extensions/cypherlite-git-to-graph` — the importer children: the
  walk, the history reader and the plan/row generation moved out of the
  CLI's `lab` tree as a lib (carrying no `Database`), a thin bin that
  speaks protocol v1 over std pipes, and the committed
  `package/extension.json` beside the source — the manifest the release
  pipeline will pack beside the built executable (ADR-0431 §2), and the
  one manifest the CLI e2e installs and drives.

Boundary rules (R3 and R5 amended, R7 new; all enforced by
`cargo xtask check-boundaries`):

- **R7** — normal and build deps of `extensions/*` target `cypherlite/*`
  (the engine's leaf crates only — a first-party child carries no
  `Database`; today the only engine reach is `cypherlite-core`'s
  temporal parser) or `extensions/*`; dev deps may also target
  `testing/cypherlite-test-support`. A first-party child never links a
  surface: the host is not linkable into a child, and that direction is
  the trust shape of ADR-0430 §6.
- **R3 amended** — a surface's normal/build reach into the extensions
  group is `cypherlite-wire` alone (the kit the CLI host links); dev
  deps may tie to any member (the e2e suites' pins against the importer
  crates' own constants — a dev edge never ships in a binary). A surface
  linking an importer child on the product path would re-create the
  compiled-in importer surface the extraction retired.
- **R5 amended** — every workspace member lives directly under
  `cypherlite/`, `surfaces/`, `testing/` or `extensions/`.

The importer extraction itself — the logic moves, the wire-forced
summary deltas, the fresh-store fast path's death, the `lab` retirement —
is ADR-0431's decision (§11–§12); this ADR records only where the
extracted sources live and the boundary rules that keep them children.
Its Consequences note the extraction's landed shape so the placement and
the move are read together.

## Alternatives considered

- **Place the importer crates under `surfaces/`.** Rejected: the group
  means "adapters of `cypherlite::Database`" and a child's defining
  property is carrying no `Database`; it also leaves the importer
  sources one directory away from the product binaries, the nearest
  place for the compiled-in surface to reappear.
- **Place them under `cypherlite/` (the engine).** Rejected: children
  link the engine's leaf crates but are not engine code; R1 exists to
  keep the engine group the engine.
- **A single crate with both importer bins.** Rejected: the two
  importers are separate packages on the channel (a package is one
  mount name, one manifest, one executable — ADR-0429 §2); one crate
  holding two packages' sources would fork the package shape the
  distribution ships.
- **Keep the importer sources inside the CLI crate (spawn-only bins of
  `cypherlite-cli`).** Rejected: the crates must be buildable and
  testable as the packages they ship as, with their committed manifests
  beside them — and ADR-0431 §11 names "first-party extension crates",
  plural, as their own workspace members.
- **Move `read_cap` into `cypherlite-core` instead of a new shared kit
  crate.** Rejected: the bounded reads are an IO policy shared by the
  host and the children, not engine code; the engine group is the one
  place the "move it into a surface or test-support" rule forbids
  growing new helpers.

## Consequences

- The workspace root grows `extensions/*` members (three crates;
  `default-members` includes them, so plain `cargo test` runs their
  suites) and `workspace.dependencies` grows the group's path entries.
- The wire kit is the one extensions-group artifact a surface links: the
  CLI's `read_cap` module is gone (moved), and the v1 wire facts
  (`WIRE_PROTOCOL`, the four caps) live in `cypherlite-wire` with the
  CLI's `extensions::protocol` re-exporting them — one home for numbers
  both sides of the pipe must agree on.
- The extraction landed with this placement: the walk, the history
  reader and the plan moved into the crates (no parallel copy survives;
  the CLI's `lab` tree, `lab_cmd`, the `lab` feature, the help topic and
  the built-in half of the action registry are deleted — the registry's
  one source is the XDG walk), the e2e coverage the compiled-in lab
  suites owned moved onto the mount
  (`tests/e2e_fs_to_graph.rs`, `tests/e2e_git_to_graph.rs` — the
  per-subject split of the original `tests/e2e_importers.rs` — plus
  `tests/e2e_importers_catalog.rs` and the PTY `run` legs), and the
  hidden `neo` egg is deleted entirely
  (ADR-0431 §11's user direction) — no extension crate, no remnant.
- Three wire-forced deltas on the importers' output are pinned by the
  e2e and are the extraction's semantic record: the child's summary
  names its target `the store` (the child never sees the store path,
  ADR-0430 §6); the summary's closing pointer to the question catalog
  rides as the last starter (the summary record's shape: text, then
  starters); and the git importer always `MERGE`s its edges — the
  ADR-0394 fresh-store `CREATE` fast path required reading the target's
  node count, which a child cannot, and the manifest's
  `requires-fresh-store` knob is for actions whose rows only make sense
  on a fresh store, not this completer. Two smaller grammar deltas
  follow from the manifest-driven flag parse (ADR-0429 §5): value-range
  refusals (`--max-changeset` above 1000, a negative `--max-commits`)
  are the child's `error` records (exit 1) with the old wordings, not
  exit-2 usage errors; and `--max-commits 0` now means all history (the
  envelope carries resolved values, so the absent and the explicit zero
  merge).
- The wire's 128 MiB total stream cap now bounds an importer's whole
  NDJSON output (ADR-0430 §4), where the compiled-in importer's rows
  went straight to the store: an import whose rows would exceed it
  refuses — the child pre-refuses with its own message, and under
  per-batch the batches before the refusal stay committed, so a re-run
  completes the store (ADR-0392). At the measured neo4j shape
  (ADR-0392's ~2.6M pairs) the rows exceed the cap, so that scale does
  not ride wire v1; raising the cap is a protocol bump (ADR-0430 §3),
  decided when the channel ships real history-scale imports.
- The ignored peak-RSS measurement (`peak_run`) retired with the store
  writes it measured: its `TRANSIENT_BUDGET_BYTES` figure was help-page
  data for a page that no longer exists, and the child's transient
  shape is the plan plus one wire chunk, bounded by the caps above.
- ADR-0279's namespace is superseded by `run` (its `Later references`
  note lands with this change), and ADR-0103's three-group rule gains
  this ADR as its `Later references` note.

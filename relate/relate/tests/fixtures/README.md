# Pre-ADR-0097 default WAL fixture (facade copy)

`pre_adr0084_default_wal/` is a byte-identical copy of the fixture owned
by `relate-storage` at
`relate/relate-storage/tests/fixtures/pre_adr0084_default_wal/`. That
tree's `tests/fixtures/README.md` records the provenance (written by the
`relate-storage` writer from `origin/main` at commit `615306d`), the
reproduction steps, and the SHA-256 table that pins the bytes.

The copy exists because `cargo package` cannot include files outside the
crate root: `examples/adopt_legacy_wal.rs` embeds these bytes with
`include_bytes!`, and the published crate must build its examples
standalone from the extracted tarball. If the fixture is ever regenerated
in `relate-storage`, recopy the tree here — until the two match, the
fixture-copy guard in `cargo xtask check-boundaries` fails, so drift can
never ride README prose alone.

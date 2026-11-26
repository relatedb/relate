### Notes

- Test-infrastructure change: `relate/relate` and `surfaces/relate-cli` build
  their shareable test files as one `tests/integration/` binary per crate
  instead of one Cargo test target per file, so a one-line engine edit no
  longer relinks 195 + 58 engine-sized binaries. 149 relate files merge (46
  stay separate) and 55 relate-cli files merge (5 stay separate); the layout,
  the isolation rules and the generator are in
  `docs/testing/test-binaries.md` and `scripts/merge-test-binaries.py`, and
  `scripts/tests/test_merge_test_binaries.py` holds each committed `main.rs`
  equal to the generator's output (REL8-2187, from the #563 prototype).

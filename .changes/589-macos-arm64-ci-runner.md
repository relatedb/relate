### Notes

- The macOS CI lanes (`os-smoke`, `swift`) run on the org's
  `macos-5-core-arm64` runner instead of the free-tier `macos-latest`;
  `scripts/check-os-matrix.py` now guards both labels via per-OS
  runner-label families.

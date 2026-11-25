### Fixed

- `randomUUID()` no longer panics in the WASM build: the UUID's timestamp
  now comes from the same JavaScript-backed clock as `datetime()` and
  `timestamp()` instead of Rust's unimplemented `std::time` on
  `wasm32-unknown-unknown` (REL8-2001).

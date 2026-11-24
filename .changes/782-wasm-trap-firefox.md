### Fixed

- `isWasmTrap` (the WASM bundle's trap classifier) now recognizes Firefox's
  stack-overflow wording (`too much recursion`) and the `recursive use of an
  object detected…` error an overflowed instance throws on its next call, so
  a host following the documented contract replaces the dead instance instead
  of continuing to use it. Previously both were classified as non-traps on
  Firefox (and on any host that did not stop at the first trap), leaving a
  broken instance in service. The classifier's boundary is now explicit:
  only Error values are classified — a non-Error value never came from the
  bundle and is never a trap (previously a raw string whose text matched a
  trap wording classified as a trap).

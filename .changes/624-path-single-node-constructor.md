### Added

- Preview: with `RELATE_PREVIEW=path-construction`, a one-element `PATH [n]`
  builds a zero-length path when the statement uses no variable named `path`
  elsewhere; with one, it stays a subscript of that variable. With the flag
  off nothing changes (ADR-0637).

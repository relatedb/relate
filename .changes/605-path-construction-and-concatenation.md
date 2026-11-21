### Added

- Preview: `PATH [n, r, m]` builds a path value from an alternating
  node, relationship, node list (`RELATE_PREVIEW=path-construction`), and `||`
  over two paths that share a boundary node concatenates them into one path
  (`RELATE_PREVIEW=path-concatenation`). With a flag off, existing statements
  behave as before: the constructor fails with the authored preview error, and
  `||` on paths keeps its operand type error (ADR-0637).

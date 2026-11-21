### Added

- The agent plan document (`relate.plan/v1`: the MCP `plan` and the FFI
  result plan) gains two always-present node fields, `fused_into` and
  `fused_metrics`, for the display-only steps an index-seek-started `MATCH`
  now breaks into in `EXPLAIN` and `PROFILE`. Both are `null` on a real
  operator; `fused_metrics` carries `{seek_ms, node_reads}` on a `PROFILE`
  seek step only.

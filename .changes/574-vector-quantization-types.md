### Fixed

- The vector index's `vector.quantization.type` config now accepts only
  `SCALAR`, `BINARY` and `NONE` (case-insensitive) and rejects every
  other spelling — `INT8` included — with an error listing the
  supported values. The legacy `vector.quantization.enabled` spelling
  now maps onto the type (`true` → SCALAR, `false` → NONE), and the
  default search-expansion factor follows the type: 1.0 for NONE, 1.5
  for SCALAR, 3.0 for BINARY. Indexes stored by older builds with the
  once-accepted `INT8` spelling keep working: the stored read maps it
  onto `SCALAR`, while `CREATE` rejects the spelling going forward.

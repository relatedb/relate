### Fixed

- A `point()` argument rejection now reports the GQLSTATUS Neo4j sends for its
  site (`22N20` dimension mismatch, `22N21` unknown srid or crs, `22N23`
  latitude out of range, `22N24` non-finite coordinate, `22G03` wrong type)
  beside the `Neo.ClientError.Statement.ArgumentError` code and the runtime
  stage, instead of the general `42000`.
- `duration()` with a fractional sub-second field beside a seconds field at
  or above 2^53 (for example `{seconds: 9223372036854775807, nanoseconds: 0.1}`)
  rounds the seconds as Neo4j does, so the rendered duration matches.

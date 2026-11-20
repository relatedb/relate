### Fixed

- A list or string slice whose dynamically-evaluated FLOAT bound exceeds
  the int32 range now saturates the bound into that range instead of
  raising `NumberOutOfRange` — `[1,2,3][head([1.0/0.0, 'x'])..2]`
  returns `[]`, matching Neo4j (ADR-0631, superseding ADR-0175's
  dynamic-bound rule). Float-literal bounds still fail before execution,
  and `substring()` / `left()` / `right()` and list indexing keep the
  rejection.

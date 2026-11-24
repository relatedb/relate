### Fixed

- Large HTTP Query API results no longer build a JSON tree of every row
  before the first byte is written, which used about 20x the response size
  in memory; rows now encode one at a time from the engine's values.

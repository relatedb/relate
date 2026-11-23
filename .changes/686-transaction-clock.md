### Fixed

- `date.transaction()`, `time.transaction()`, `localtime.transaction()`,
  `datetime.transaction()` and `localdatetime.transaction()` now return one
  instant for the whole explicit transaction, matching Neo4j, where they
  used to return the current statement's instant. `.statement()`, the
  default `date()`/`datetime()` constructors and `timestamp()` still read
  one instant per statement, `.realtime()` the wall clock per call, and an
  autocommit statement behaves as before.

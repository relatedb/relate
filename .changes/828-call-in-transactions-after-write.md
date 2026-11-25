### Fixed

- `CALL { … } IN TRANSACTIONS` in a `NEXT` part that follows a writing
  part is now rejected at compile time with
  `WriteBeforeCallInTransactions` (42I25) instead of running: the parts
  of a chained statement are one statement, so a writing part precedes
  a batched subquery exactly like a write clause does. A batched part
  followed by a reading or batched part keeps working.

### Fixed

- Deleting, inside one explicit transaction, an entity an earlier
  statement of that transaction created no longer poisons the connection
  with `internal error` (`update counters are non-negative`): the delete
  statement now counts `nodes_deleted` / `relationships_deleted` (and
  `properties_removed`) instead of driving the statement's create counter
  negative. Same-statement create+delete pairs still net to zero, and a
  `CALL { … } IN TRANSACTIONS` batch deleting a node an earlier batch of
  the same statement committed reports both the create and the delete.

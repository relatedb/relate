### Changed

- A `CALL { … }` subquery feeding an aggregation (or a clause that may
  take the ordered window) streams its outer-rows × body-rows product
  instead of materializing it: the consumer pulls one merged row at a
  time, so peak memory is bounded by one outer row's body result rather
  than the input-row count times it (ADR-0555). The materializing form —
  and the `WHEN`/`ELSE` conditional's output buckets, which have no
  parkable consumer by grammar — now charge what they retain to the
  running total (ADR-0463), where the pre-conversion forms held their
  whole output unbilled (REL8-1845 items 3, 4 and the chargeable half
  of 5).

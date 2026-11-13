# Bolt — compatibility matrix

The second page of the spec's [Compatibility](spec.md#compatibility)
section: the per-finding record of the compatibility probes.
Three probes compared every driver-visible behavior of `relate-bolt`
with the latest Neo4j — **Neo4j 2026.09.0 (Community)**:

- the **wire probe** `scripts/bolt_compat_probe.py` — boltkit at commit
  `687a47973c0cfb14f7f2789a4b9b494cbe938a46`, raw-socket Bolt exchanges
  at Bolt 4.4; its findings are the `F-NN` rows below;
- the **driver probe** — a one-time neo4rs probe (neo4rs pinned to
  `0.9.0-rc.10`), every scenario through the neo4rs public API; its
  findings are the `D-NN` rows.
- the **5.4 probe** — a one-time live Docker probe against the same
  Neo4j at Bolt 5.4 (2026-11-02); its findings are ADR-0484's numbered
  decisions, the rows of [Bolt 5.4](#bolt-54-adr-0484) below.

Every finding has a status and a covering test:

- **compatible** — the two replies were equal (after masking opaque
  values); a baseline `.bolt` script or another covering test pins it.
  F-02, F-04 and F-05 diverge only in the version a multi-version
  Neo4j answers (4.3 / 5.0 / 3), none of them a version this server
  speaks — it offers 4.4 and 5.4 only (ADR-0484);
  their `00 00 00 00` reply is the pinned baseline (ADR-0303).
- **fixed** — the probe found a divergence and a fix changed
  it; the row names the ADR and the `.bolt` script that pins the fixed
  behavior. The [Fixes](#fixes) table groups them, one row per fix.
- **deferred** — deliberately not matched, with the reason
  ([Deferred](#deferred)).
- **decided-not-implemented** — a divergence an ADR closed as
  deliberately not matched, forever pinned by its covering script
  (F-06, ADR-0484).

`.bolt` script paths below are repo-relative (the corpus lives under
`surfaces/relate-bolt/tests/compat/scripts/`), and every script is
replayed by `cargo nextest run -p relate-bolt --test bolt_compat`
(ADR-0304). The driver-visible fixes are additionally asserted with the
official Python driver in
`surfaces/relate-bolt/tests/compat_driver_smoke.py`
(run by `tests/python_driver_smoke.py`); findings only a raw socket can
observe (handshake bytes, HELLO metadata, chunk framing, protocol
breaches) stay in the `.bolt` corpus.

Of the 125 findings: 50 compatible, 73 fixed, 1 deferred (F-12), and
F-06 decided-not-implemented (ADR-0484).

## Fixes

One row per fix. Each id's row below names the same ADR and script, so
the fix a finding belongs to is readable both ways.

| Findings | Fixed behavior | ADR | Covering script(s) |
| --- | --- | --- | --- |
| F-11, F-13, F-18, F-19, F-20, F-40, D-52, D-55, D-56 | Result summary metadata matches Neo4j 4.4: `t_first` on RUN SUCCESS; `type`, `stats`, `t_last`, `db` on the ending SUCCESS only | ADR-0309 | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-14, F-21, D-57 | Bookmarks: the auto-commit ending SUCCESS and COMMIT SUCCESS carry a `bookmark`; received bookmarks are accepted | ADR-0310 | `surfaces/relate-bolt/tests/compat/scripts/protocol/bookmarks.bolt` |
| F-23, F-24, D-41 | Explicit transactions keep one open result per RUN, addressed by `qid` | ADR-0311 | `surfaces/relate-bolt/tests/compat/scripts/protocol/tx-two-open-results.bolt` |
| F-07 | HELLO advertises `connection.recv_timeout_seconds`; the server sends NOOP keep-alives during long requests | ADR-0312 | `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-recv-timeout-hint.bolt` |
| F-09, F-68 | HELLO requires `user_agent` (empty or absent) | ADR-0313 | `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-requires-user-agent.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-absent-user-agent.bolt` |
| F-32, F-33, F-34, F-35, F-43, F-44, F-45, F-46 | Protocol breaches answer `Neo.ClientError.Request.Invalid` and close the connection | ADR-0314 | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-commit-without-begin.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-rollback-without-begin.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-nested-begin.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-pull-without-result.bolt` |
| F-31 | Unknown message signatures are fatal: the connection closes without a FAILURE | ADR-0315 | `surfaces/relate-bolt/tests/compat/scripts/protocol/unknown-message-tag-closes.bolt` |
| F-29 | Client NOOP chunks (`00 00`) are accepted between messages | ADR-0316 | `surfaces/relate-bolt/tests/compat/scripts/protocol/client-noop-chunk.bolt` |
| F-27 | RESET answers IGNORED for work queued ahead of it | ADR-0317 | `surfaces/relate-bolt/tests/compat/scripts/protocol/reset-ignores-queued-work.bolt` |
| F-36, F-37 | ROUTE answers the home database `neo4j` and the WRITE/READ/ROUTE role order | ADR-0318 | `surfaces/relate-bolt/tests/compat/scripts/protocol/route-home-db-and-role-order.bolt` |
| D-49 | Execution-phase failures are reported at the first PULL or DISCARD | ADR-0319 | `surfaces/relate-bolt/tests/compat/scripts/protocol/runtime-error-at-pull.bolt` |
| F-41, D-46, D-47, D-50, D-51 | Every FAILURE carries a Neo4j status code | ADR-0307 | `surfaces/relate-bolt/tests/compat/scripts/failures/neo4j-status-codes.bolt` |
| F-42, F-67, D-48 | A statement referencing an absent parameter fails with `ParameterMissing` before execution | ADR-0308 | `surfaces/relate-bolt/tests/compat/scripts/failures/parameter-missing.bolt` |
| F-51, F-52, F-53, F-54, F-55, F-56, F-58, D-09, D-10, D-11, D-12, D-13, D-14, D-15, D-26, D-27, D-28, D-29, D-30, D-31, D-32 | Temporal results encode as PackStream temporal structures, at any nesting depth | ADR-0325 | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-59, F-60, D-16, D-17, D-33, D-34 | Point results encode as Point2D/Point3D structures, Float coordinates | ADR-0326 | `surfaces/relate-bolt/tests/compat/scripts/values/point-structs.bolt` |
| F-64, D-25 | Bytes parameters round-trip through a statement-scoped engine bytes value | ADR-0327, ADR-0328 | `surfaces/relate-bolt/tests/compat/scripts/values/bytes-param-roundtrip.bolt` |
| F-10, F-57 | `patch_bolt` utc: UTC DateTime structures when the client negotiates them | ADR-0329 | `surfaces/relate-bolt/tests/compat/scripts/values/patch-bolt-utc.bolt` |
| D-53, D-54 | Query type `w` vs `rw` split by returned columns (SET / DETACH DELETE without RETURN are `w`) | ADR-0332 | `surfaces/relate-bolt/tests/compat/scripts/protocol/query-type.bolt` |

## Wire findings (boltkit probe)

One row per `F-NN` finding, grouped by the probe's areas. F-68 was
appended after F-67 (the probe's stable id order), so it closes the
failures group.

### Handshake — 6 findings: 5 compatible, 1 decided-not-implemented

| Id | Behavior | Status | Covering script |
| --- | --- | --- | --- |
| F-01 | propose `[4.4]` — answer `00 00 04 04`, connection open | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-1-v44.bolt` |
| F-02 | boltkit's default proposals `[4.3, 4.2, 4.1, 4.0]` cover no spoken version — reply `00 00 00 00` and close; latest Neo4j answers `00 00 03 04`, the Bolt 4.3 it also serves (a version this server does not speak, ADR-0303, ADR-0484) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-3-only-4x-below-44.bolt` |
| F-03 | range 4.4–4.2 proposal — answer `00 00 04 04` | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-2-range-44-42.bolt` |
| F-04 | proposal `[5.0]` only — reply `00 00 00 00` and close; latest Neo4j answers `00 00 00 05`, the Bolt 5.0 it also serves (the server offers 5.4, not 5.0, ADR-0484) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-4-only-50.bolt` |
| F-05 | proposal `[3]` only — reply `00 00 00 00` and close, as latest Neo4j does for a version it does not serve (ADR-0303) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-5-only-v3.bolt` |
| F-06 | manifest-v1 slot (`00 00 01 FF`) — never answered with a manifest; a 4.4 slot after it is answered plainly | decided-not-implemented (ADR-0484) | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-6-manifest-then-44.bolt` |

### Protocol — 34 findings: 11 compatible, 22 fixed, 1 deferred

| Id | Behavior | Status | Covering script |
| --- | --- | --- | --- |
| F-07 | HELLO SUCCESS carries `hints: {"connection.recv_timeout_seconds": 120}` (and the server sends NOOP keep-alives during long requests) | fixed (ADR-0312) | `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-recv-timeout-hint.bolt` |
| F-08 | session usable after a HELLO with `user_agent` | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/hello-session-usable.bolt` |
| F-09 | reply sequence after a rejected HELLO (empty or absent `user_agent`): following messages IGNORED, a second HELLO FAILURE + close | fixed (ADR-0313, ADR-0342) | `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-requires-user-agent.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-absent-user-agent.bolt` |
| F-10 | HELLO SUCCESS echoes `patch_bolt: ["utc"]` | fixed (ADR-0329) | `surfaces/relate-bolt/tests/compat/scripts/values/patch-bolt-utc.bolt` |
| F-11 | RUN SUCCESS carries `t_first` (whole milliseconds) | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-12 | RUN SUCCESS carries `columnTypes`, the deliberate Relate extension key (ADR-0224) | deferred — see [Deferred](#deferred) | `surfaces/relate-bolt/src/protocol_tests.rs` (`run_success_carries_the_engine_column_types`); the `BINARY` stamp in `surfaces/relate-bolt/tests/compat/scripts/values/bytes-param-roundtrip.bolt` and the `ANY` stamp in `surfaces/relate-bolt/tests/compat/scripts/protocol/runtime-error-at-pull.bolt` |
| F-13 | `type` on the SUCCESS that ends the result only, never on RUN SUCCESS | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-14 | the auto-commit ending SUCCESS carries `bookmark` (a write's; the engine commit sequence, ADR-0502 §7 — a read commits nothing and carries none) | fixed (ADR-0310) | `surfaces/relate-bolt/tests/compat/scripts/protocol/bookmarks.bolt` |
| F-15 | the final PULL SUCCESS carries `t_last` and `db` | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/pull-final-success-t-last-db.bolt` |
| F-16 | RECORD stream of `RUN` + `PULL {n: -1}` | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/autocommit-run-pull.bolt` |
| F-17 | paging: records per `PULL {n: 2}`, `has_more` | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/pull-paging-has-more.bolt` |
| F-18 | a page that leaves rows answers exactly `{has_more: true}` | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-19 | DISCARD's ending SUCCESS carries `type`, `t_last`, `db` (+ `stats`, `bookmark`) | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-20 | an explicit-tx PULL SUCCESS carries `stats` | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |
| F-21 | inside an explicit tx only COMMIT SUCCESS carries `bookmark` | fixed (ADR-0310) | `surfaces/relate-bolt/tests/compat/scripts/protocol/bookmarks.bolt` |
| F-22 | ROLLBACK discards the write | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/tx-rollback-and-commit.bolt` |
| F-23 | RUN SUCCESS carries `qid` inside an explicit transaction | fixed (ADR-0311) | `surfaces/relate-bolt/tests/compat/scripts/protocol/tx-two-open-results.bolt` |
| F-24 | two open results in one tx; PULL/DISCARD address them by `qid` | fixed (ADR-0311) | `surfaces/relate-bolt/tests/compat/scripts/protocol/tx-two-open-results.bolt` |
| F-25 | failed state: IGNORED until RESET | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/failed-state-ignored-until-reset.bolt` |
| F-26 | pipelined requests answered in order | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/pipelined-requests-in-order.bolt` |
| F-27 | work queued ahead of a pipelined RESET answers IGNORED | fixed (ADR-0317) | `surfaces/relate-bolt/tests/compat/scripts/protocol/reset-ignores-queued-work.bolt` |
| F-28 | message reassembled from 4-byte and 1-byte chunks | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/split-chunks.bolt` |
| F-29 | a client NOOP chunk (`00 00`) between messages is skipped | fixed (ADR-0316) | `surfaces/relate-bolt/tests/compat/scripts/protocol/client-noop-chunk.bolt` |
| F-30 | GOODBYE closes the connection | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/goodbye-closes.bolt` |
| F-31 | an unknown message tag (`0x0E`): fatal close, no FAILURE | fixed (ADR-0315) | `surfaces/relate-bolt/tests/compat/scripts/protocol/unknown-message-tag-closes.bolt` |
| F-32 | COMMIT without BEGIN: `Neo.ClientError.Request.Invalid` FAILURE, then close | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-commit-without-begin.bolt` |
| F-33 | ROLLBACK without BEGIN: `Neo.ClientError.Request.Invalid` FAILURE, then close | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-rollback-without-begin.bolt` |
| F-34 | BEGIN inside an open transaction: `Neo.ClientError.Request.Invalid` FAILURE, then close | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-nested-begin.bolt` |
| F-35 | PULL with no open result: `Neo.ClientError.Request.Invalid` FAILURE, then close | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-pull-without-result.bolt` |
| F-36 | ROUTE that names no db answers `rt.db: "neo4j"` (the home database) | fixed (ADR-0318) | `surfaces/relate-bolt/tests/compat/scripts/protocol/route-home-db-and-role-order.bolt` |
| F-37 | `rt.servers` roles in WRITE, READ, ROUTE order | fixed (ADR-0318) | `surfaces/relate-bolt/tests/compat/scripts/protocol/route-home-db-and-role-order.bolt` |
| F-38 | rt keys, ttl and addresses per role | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/route-ttl.bolt` |
| F-39 | `rt.db` when ROUTE names a db | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/route-named-db.bolt` |
| F-40 | the auto-commit write's ending SUCCESS carries `stats` (Neo4j's kebab-case counters) | fixed (ADR-0309) | `surfaces/relate-bolt/tests/compat/scripts/protocol/summary-metadata.bolt` |

### Failures — 7 findings: 7 fixed

| Id | Behavior | Status | Covering script |
| --- | --- | --- | --- |
| F-41 | a syntax error FAILS with code `Neo.ClientError.Statement.SyntaxError` | fixed (ADR-0307) | `surfaces/relate-bolt/tests/compat/scripts/failures/neo4j-status-codes.bolt` |
| F-42 | a missing parameter FAILS with code `Neo.ClientError.Statement.ParameterMissing` | fixed (ADR-0307, ADR-0308) | `surfaces/relate-bolt/tests/compat/scripts/failures/parameter-missing.bolt` |
| F-43 | the COMMIT-without-BEGIN breach FAILS with `Neo.ClientError.Request.Invalid` | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-commit-without-begin.bolt` |
| F-44 | the ROLLBACK-without-BEGIN breach FAILS with `Neo.ClientError.Request.Invalid` | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-rollback-without-begin.bolt` |
| F-45 | the nested-BEGIN breach FAILS with `Neo.ClientError.Request.Invalid` | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-nested-begin.bolt` |
| F-46 | the PULL-without-result breach FAILS with `Neo.ClientError.Request.Invalid` | fixed (ADR-0314) | `surfaces/relate-bolt/tests/compat/scripts/protocol/breach-pull-without-result.bolt` |
| F-68 | the rejected-HELLO FAILURE code is `Neo.ClientError.Request.Invalid` | fixed (ADR-0313, ADR-0342) | `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-requires-user-agent.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/hello-absent-user-agent.bolt` |

### Values — 20 findings: 9 compatible, 11 fixed

| Id | Behavior | Status | Covering script |
| --- | --- | --- | --- |
| F-47 | node result values | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-node.bolt` |
| F-48 | relationship result values | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-relationship.bolt` |
| F-49 | path result (a forward hop) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-path-forward.bolt` |
| F-50 | path result (a hop against the relationship direction) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-path-reverse.bolt` |
| F-51 | date result: `0x44` Date structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-52 | time result: `0x54` Time structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-53 | localtime result: `0x74` LocalTime structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-54 | datetime (offset) result: `0x46` DateTime structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-55 | datetime (zone id) result: `0x66` DateTimeZoneId structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-56 | localdatetime result: `0x64` LocalDateTime structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-57 | datetime results under the utc patch: `0x49`/`0x69` UTC structures | fixed (ADR-0329) | `surfaces/relate-bolt/tests/compat/scripts/values/patch-bolt-utc.bolt` |
| F-58 | duration result: `0x45` Duration structure | fixed (ADR-0325) | `surfaces/relate-bolt/tests/compat/scripts/values/temporal-structs.bolt` |
| F-59 | 2D point results: `0x58` Point2D structure, Float coordinates | fixed (ADR-0326) | `surfaces/relate-bolt/tests/compat/scripts/values/point-structs.bolt` |
| F-60 | 3D point result: `0x59` Point3D structure | fixed (ADR-0326) | `surfaces/relate-bolt/tests/compat/scripts/values/point-structs.bolt` |
| F-61 | scalar results (int widths, float, string, bool, null) | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-scalars.bolt` |
| F-62 | list result | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-list.bolt` |
| F-63 | map result | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/value-map.bolt` |
| F-64 | a bytes parameter round-trips as PackStream Bytes | fixed (ADR-0327, ADR-0328) | `surfaces/relate-bolt/tests/compat/scripts/values/bytes-param-roundtrip.bolt` |
| F-65 | list and map parameters echoed | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/param-collections.bolt` |
| F-66 | temporal/point/duration struct parameters, read back as scalars | compatible | `surfaces/relate-bolt/tests/compat/scripts/baseline/param-structs-read-back.bolt` |

### Cypher — 1 finding: 1 fixed

| Id | Behavior | Status | Covering script |
| --- | --- | --- | --- |
| F-67 | a statement referencing an absent parameter FAILS (`ParameterMissing`), never a null RECORD | fixed (ADR-0308) | `surfaces/relate-bolt/tests/compat/scripts/failures/parameter-missing.bolt` |

## Driver findings (neo4rs probe)

One row per `D-NN` finding, from the one-time neo4rs 0.9.0-rc.10
probe; the scenario names are the probe's own. A fixed row names its
ADR, and the `.bolt` script that pins the fixed behavior is the row's
entry in [Fixes](#fixes).

| Id | Area | Probe scenario | Behavior | Status |
| --- | --- | --- | --- | --- |
| D-01 | handshake | `connect_return_1` | connect and `RETURN 1` | compatible |
| D-02 | values | `result_int` | an int result | compatible |
| D-03 | values | `result_float` | a float result | compatible |
| D-04 | values | `result_string` | a string result | compatible |
| D-05 | values | `result_bool` | a bool result | compatible |
| D-06 | values | `result_null` | a null result | compatible |
| D-07 | values | `result_list` | a list result | compatible |
| D-08 | values | `result_map` | a map result | compatible |
| D-09 | values | `result_date` | a date result hydrates as `BoltType::Date` / `NaiveDate` | fixed (ADR-0325) |
| D-10 | values | `result_time` | a time result hydrates as `BoltType::Time` | fixed (ADR-0325) |
| D-11 | values | `result_localtime` | a localtime result hydrates as `BoltType::LocalTime` | fixed (ADR-0325) |
| D-12 | values | `result_datetime_offset` | an offset datetime result hydrates as `BoltType::DateTime` | fixed (ADR-0325) |
| D-13 | values | `result_datetime_zone_id` | a zone-id datetime result hydrates as `BoltType::DateTime` | fixed (ADR-0325) |
| D-14 | values | `result_localdatetime` | a localdatetime result hydrates as `BoltType::LocalDateTime` | fixed (ADR-0325) |
| D-15 | values | `result_duration` | a duration result hydrates as `BoltType::Duration` | fixed (ADR-0325) |
| D-16 | values | `result_point_2d` | a 2D point result hydrates as `Point2D` | fixed (ADR-0326) |
| D-17 | values | `result_point_3d` | a 3D point result hydrates as `Point3D` | fixed (ADR-0326) |
| D-18 | values | `param_int` | an int parameter | compatible |
| D-19 | values | `param_float` | a float parameter | compatible |
| D-20 | values | `param_string` | a string parameter | compatible |
| D-21 | values | `param_bool` | a bool parameter | compatible |
| D-22 | values | `param_null` | a null parameter | compatible |
| D-23 | values | `param_list` | a list parameter | compatible |
| D-24 | values | `param_map` | a map parameter | compatible |
| D-25 | values | `param_bytes` | a bytes parameter round-trips | fixed (ADR-0327, ADR-0328) |
| D-26 | values | `param_date` | a date parameter round-trips | fixed (ADR-0325) |
| D-27 | values | `param_time` | a time parameter round-trips | fixed (ADR-0325) |
| D-28 | values | `param_localtime` | a localtime parameter round-trips | fixed (ADR-0325) |
| D-29 | values | `param_datetime_offset` | an offset datetime parameter round-trips | fixed (ADR-0325) |
| D-30 | values | `param_datetime_zone_id` | a zone-id datetime parameter round-trips | fixed (ADR-0325) |
| D-31 | values | `param_localdatetime` | a localdatetime parameter round-trips | fixed (ADR-0325) |
| D-32 | values | `param_duration` | a duration parameter round-trips | fixed (ADR-0325) |
| D-33 | values | `param_point_2d` | a 2D point parameter round-trips | fixed (ADR-0326) |
| D-34 | values | `param_point_3d` | a 3D point parameter round-trips | fixed (ADR-0326) |
| D-35 | values | `result_node` | a node result hydrates as `Node` | compatible |
| D-36 | values | `result_relationship` | a relationship result hydrates as `Relation` | compatible |
| D-37 | values | `result_path` | a path result hydrates as `Path` | compatible |
| D-38 | protocol | `autocommit_write_then_read` | an auto-commit write then a read | compatible |
| D-39 | protocol | `txn_commit` | an explicit-tx commit | compatible |
| D-40 | protocol | `txn_rollback` | an explicit-tx rollback | compatible |
| D-41 | protocol | `txn_two_streams` | two streams interleaved in one transaction (each RUN its own result, by `qid`) | fixed (ADR-0311) |
| D-42 | protocol | `result_larger_than_fetch_size` | a result larger than the fetch size | compatible |
| D-43 | protocol | `concurrent_queries_one_pool` | concurrent queries on one pool | compatible |
| D-44 | protocol | `db_neo4j` | database selection: `neo4j` | compatible |
| D-45 | protocol | `db_system` | database selection: `system` | compatible |
| D-46 | failures | `db_unknown` | an unknown database FAILS with `Neo.ClientError.Database.DatabaseNotFound` | fixed (ADR-0307) |
| D-47 | failures | `error_syntax` | a syntax error FAILS with a Neo4j status code | fixed (ADR-0307) |
| D-48 | failures | `error_missing_parameter` | a missing parameter FAILS with `Neo.ClientError.Statement.ParameterMissing` | fixed (ADR-0307, ADR-0308) |
| D-49 | failures | `error_during_streaming` | an error during streaming is reported at the first PULL, and the session recovers after RESET | fixed (ADR-0319) |
| D-50 | failures | `error_then_connection_reused` | the connection is reusable after a failure + RESET | fixed (ADR-0307) |
| D-51 | failures | `error_in_txn_then_rollback` | an error inside a transaction FAILS the transaction; ROLLBACK recovers | fixed (ADR-0307) |
| D-52 | protocol | `summary_create_counters` | summary counters of a CREATE (query type, stats, db, bookmark) | fixed (ADR-0309) |
| D-53 | protocol | `summary_update_counters` | summary counters of a SET / REMOVE label (query type `w`, `stats`) | fixed (ADR-0309, ADR-0332) |
| D-54 | protocol | `summary_delete_counters` | summary counters of a DETACH DELETE (query type `w`, `stats`) | fixed (ADR-0309, ADR-0332) |
| D-55 | protocol | `summary_read_stream` | the summary of a streamed read (`type: "r"`, no `stats`, `db`; no bookmark — a read commits nothing, ADR-0310) | fixed (ADR-0309) |
| D-56 | protocol | `summary_schema_index` | the summary of a schema-index CREATE/DROP (`type: "s"`, `indexes-added`) | fixed (ADR-0309) |
| D-57 | protocol | `summary_bookmark` | the summary carries a bookmark after an auto-commit statement | fixed (ADR-0310) |

## Bolt 5.4 (ADR-0484)

The 5.4 probe: a one-time live Docker probe against the same latest
Neo4j — **2026.09.0 (Community), at Bolt 5.4**, 2026-11-02 — pinned the
multi-version negotiation replies, the 5.x HELLO field contract,
LOGON/LOGOFF and their states, TELEMETRY, the notifications option
failures, and the manual's 5.0 element-id and DateTime structure
changes. ADR-0484 is the decision built on it; its numbered decisions
are the findings, each pinned by a 5.4 corpus script (replayed by the
same `cargo nextest run -p relate-bolt --test bolt_compat`), and
the driver-visible half — the official Python driver negotiating 5.4 by
first-cover-wins, its LOGON, the hydrated element ids — is asserted by
`bolt_5_4_smoke` in `surfaces/relate-bolt/tests/compat_driver_smoke.py`.

| Decision | Behavior | Covering script(s) |
| --- | --- | --- |
| 1, 2 — offered set, reply rule | the server offers Bolt 4.4 and 5.4 only; the first proposal in client order covering a spoken version decides the `00 00 mm MM` reply — the 5.8 range (`00 08 08 05`) covers 5.4, and a 5.4-exact-first set is answered 5.4 | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-7-range-58-then-range-44.bolt`, `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-9-54-then-44.bolt` |
| 3 — 5.x HELLO | `bolt_agent` (a string→string map) required beside `user_agent`; the auth fields and `patch_bolt` silently ignored, no acknowledgment key | `surfaces/relate-bolt/tests/compat/scripts/baseline/negotiation-9-54-then-44.bolt` (its own 5.x HELLO), `hello_at_5_4_requires_bolt_agent` (`surfaces/relate-bolt/src/protocol_logon_tests.rs`) |
| 4 — LOGON, LOGOFF | `basic` against the resolved credential and `none` under `--auth none`; a mismatch answers the probed Unauthorized FAILURE and closes the connection (DEFUNCT); LOGOFF returns to AUTHENTICATION and a fresh LOGON reopens the session | `surfaces/relate-bolt/tests/compat/scripts/protocol/logon-flow.bolt`, `surfaces/relate-bolt/tests/compat/scripts/protocol/logoff-round-trip.bolt` |
| 5 — TELEMETRY | the HELLO hint `telemetry.enabled: false`; every in-range `api` Int answers SUCCESS `{}`, an out-of-range Int the probed UnknownError, a non-Int `api` the probed Request.Invalid | `surfaces/relate-bolt/tests/compat/scripts/protocol/telemetry.bolt` |
| 6 — element ids | the graph structures carry the engine `elementId()` helpers' strings (`4:rl-id:n0` / `5:rl-id:r0`); a namespace without an element-id segment fails typed | `surfaces/relate-bolt/tests/compat/scripts/values/element-id-structs.bolt` |
| 7 — fixed DateTime structures | `0x49`/`0x69` unconditional in both directions; the legacy `0x46`/`0x66` rejected as parameters and never emitted | `surfaces/relate-bolt/tests/compat/scripts/values/fixed-datetime-structs.bolt` |
| 8 — notifications filter keys | `notifications_minimum_severity` and `notifications_disabled_categories` in the HELLO/BEGIN/RUN extra filter the ending `notifications` list; a statement's keys override the HELLO's; a wrong type or an unknown severity fails Request.Invalid | `surfaces/relate-bolt/tests/compat/scripts/protocol/notifications-filter.bolt` |

## Deferred

- **F-12 — RUN SUCCESS `columnTypes`.** Accepted divergence, by user
  decision 2026-10-16: `columnTypes` is the deliberate Relate
  extension key of ADR-0224, sent beside the standard `t_first`
  (ADR-0309); Neo4j drivers ignore keys they do not know. No follow-up.
  Covering tests: the protocol test `run_success_carries_the_engine_column_types`
  (`surfaces/relate-bolt/src/protocol_tests.rs`), the
  `BINARY` stamp in `surfaces/relate-bolt/tests/compat/scripts/values/bytes-param-roundtrip.bolt`
  and the `ANY` stamp in `surfaces/relate-bolt/tests/compat/scripts/protocol/runtime-error-at-pull.bolt`.

## Shared-backend interop rules (LABS-595 to 598)

Rules for one client backend — the same code, on the official Neo4j
driver — serving **both** Relate's Bolt 4.4 and a Neo4j 5.x. They
come from the GUAC interop spike (neo4j-go-driver v5.28.5 against
Relate Bolt 4.4 and Neo4j 5.26 at Bolt 5.8); each records why the
cross-engine-safe form is the rule. Engine-side, every rule is already
satisfied by the pinned behaviors above — the rules constrain the
client, not the server.

- **Never set notification filtering (LABS-595).** The driver rejects
  `SessionConfig.NotificationsMinSeverity` client-side, before any wire
  traffic, when the negotiated protocol is Bolt < 5.2
  (`FeatureNotSupportedError: requires least server v5.7`). Relate
  negotiates Bolt 4.4 only (ADR-0303; the negotiation-1 baseline pins
  it), so every Relate session would die at construction. It works
  on Neo4j 5.26 / Bolt 5.8, but a conditional set is not worth the
  branch: keep the
  default `SessionConfig` for both engines.
- **Retry on driver error classification, never on engine concurrency
  behavior (LABS-596).** The two engines have opposite concurrency
  models: Relate is optimistic — the losing transaction's COMMIT
  fails with `Neo.TransientError.Transaction.Outdated` (a
  `TransientError`, so the driver classifies it retryable; the server
  never retries, the caller must), while Neo4j 5.26 is pessimistic — a
  concurrent writer just blocks on the record lock, no error at all.
  `neo4j.IsRetryable` and the driver's ExecuteQuery-managed retry
  classify both correctly; no engine-specific retry branching. The
  Relate side of the contract — the conflict code and its
  `TransientError` class — is pinned by
  `surfaces/relate-bolt/src/failures_tests.rs` (`OUTDATED_CODE`).
- **Detect the engine only through cross-engine probes (LABS-597).**
  `SHOW SERVER` works on Relate but fails on Neo4j 5.x
  (`NotSystemDatabaseError`) — never use it in shared code. The probes
  that work on both: `ServerInfo.Agent` after connect (Relate
  reports `Relate/<version>`, ADR-0293), `CALL dbms.components()`
  (Relate reports versions `['2026.10-relate']`, ADR-0152 —
  clearly distinguishable from Neo4j's `['5.26.32']`), `CALL
  db.info()`, `SHOW DATABASES`, `SHOW INDEXES`. The procedure shapes
  are pinned by `relate/relate/tests/dbms_procedures.rs` and
  `relate/relate/tests/namespace.rs`.
- **The Bolt 5 handshake extras are harmless — no workaround
  (LABS-598).** None of neo4j-go-driver v5.28.5's Bolt 5 handshake
  extras break Relate's 4.4 path: the manifest-marker proposal
  (`00 00 01 FF`) is answered by the plain `00 00 04 04` the
  negotiation-6 baseline pins (F-06, ADR-0303), and the
  `patch_bolt: ["utc"]` HELLO extra echoes only when negotiated
  (F-10, ADR-0329). The same binary, default pool config, default
  timeouts, and basic auth ran unchanged against both engines. The
  unmodified-driver pin is the Go driver smoke
  (`tests/go_driver_smoke/`, the Go analog of
  `tests/python_driver_smoke.py`).
- **The official Java and Python drivers need the `--bolt-identity`
  shim (LABS-649).** The neo4j-java-driver (since 5.0, every 5.x/6.x;
  4.4.x predates the check) and the neo4j-python-driver (verified
  5.23 through 5.28) both validate the HELLO SUCCESS `server` agent and
  throw `UntrustedServerException` / `UnsupportedServerProduct` unless
  it starts with `Neo4j/` — the default `Relate/<version>` identity
  (ADR-0293) is rejected. The neo4j-go-driver does not gate, so it keeps
  pinning the honest agent (LABS-597) against the default-identity
  servers. Behind `serve --bolt-identity Neo4j/5.26.0` — the shipped
  compat shim, cypher-shell precedent — the pinned Java 5.28.5 driver
  negotiates down to Bolt 4.4, sees the identity verbatim, runs
  parameterized queries and explicit BEGIN/COMMIT/ROLLBACK with
  read-your-own-write, and surfaces a write-write conflict's
  `Neo.TransientError.Transaction.Outdated` as the driver's
  `TransientException` — the exact classification the driver's managed
  `executeWrite` auto-retries, which is why transactional clients that
  must observe conflicts (the Jepsen harness) use explicit unmanaged
  transactions only. The Python smoke, its compat suite, and the Bolt
  doc examples all connect to shimmed servers for the same reason.
  Both sides — the rejection of the honest identity (Java smoke) and
  the full suites behind the shim — are pinned by the driver smokes
  (`tests/java_driver_smoke/`, `tests/python_driver_smoke.py`,
  `tests/go_driver_smoke/`), with the topology's one source in
  `scripts/bolt-smoke.sh`. `neo4j-rust-ext` accelerates the Python
  driver's packstream codec (and its Bolt socket on platforms whose
  wheel carries it; the darwin wheel is packstream-only) — it is not a
  way around the identity gate, which lives in the driver's handshake
  logic.

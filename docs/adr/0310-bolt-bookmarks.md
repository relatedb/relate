---
type: adr
area: surface
claim: ee906a85ae3a0bcb0b0a4cae6451b0bbaf18a17d
name: 0310-bolt-bookmarks
status: proposed
tags:
  - bolt
  - transactions
title: |-
  Bolt bookmarks: the auto-commit ending SUCCESS and COMMIT SUCCESS carry the engine's commit bookmark; received bookmarks are accepted
REFERENCES:
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0309-bolt-summary-metadata-neo4j-4-4]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0310: Bolt bookmarks: the auto-commit ending SUCCESS and COMMIT SUCCESS carry the engine's commit bookmark; received bookmarks are accepted

## Context

Bolt drivers chain causal consistency through bookmarks. Latest Neo4j
(LABS-323 findings F-14, F-21, D-57) sends a `bookmark` string on the
SUCCESS that ends every auto-commit result, read or write, and on COMMIT
SUCCESS. Inside an explicit transaction no PULL or DISCARD SUCCESS carries
one. Drivers send collected bookmarks back in the `bookmarks` list of the
next BEGIN or auto-commit RUN extra.

cypherlite-bolt sends no bookmark anywhere, so neo4rs's `Txn::commit`
returns `None` and driver bookmark managers hold nothing. CypherLite is one
embedded server: every committed transaction is visible to the next
statement on every connection, so a bookmark the server issued is always
already satisfied.

## Decision

1. **Issued bookmarks.** The SUCCESS that ends an auto-commit result (last
 PULL or DISCARD, [ADR-0309](./0309-bolt-summary-metadata-neo4j-4-4.md)) carries `bookmark` when the statement
   committed a write. COMMIT SUCCESS is `{bookmark}` (before, `{}`) when
   the transaction wrote, `{}` when it committed nothing. ROLLBACK SUCCESS
   stays `{}`. Inside an explicit transaction no PULL or DISCARD SUCCESS
   carries `bookmark`.
2. **Value.** `"cypherlite:<n>"`, where `n` is the engine's commit
 sequence ([ADR-0502](./0502-typed-c-abi-v2.md) §7): the value `QueryResult::bookmark` carries for
   an auto-commit write and `Connection::commit` returns for a
   transaction — the same value the C ABI's `cypherlite_result_bookmark`
   and `cypherlite_conn_commit` report. Bolt mints nothing: a statement or
   transaction that committed nothing (a read, a write that changed
   nothing, an empty transaction) carries no bookmark, because the engine
   reports none.
3. **Received bookmarks** in the BEGIN or RUN extra `bookmarks` are
   accepted and ignored: there is no wait and no `InvalidBookmark`
   failure, because every bookmark is already satisfied.
4. Bookmarks are Bolt protocol data, not `QueryResult` metadata. The HTTP
 surface keeps [ADR-0232](./0232-network-surfaces-charter.md)'s empty bookmarks array.

## Alternatives considered

- **A per-listener counter** (`ConnConfig.bookmarks`, an
  `Arc<AtomicU64>`; advanced on every COMMIT and non-read auto-commit, a
  read carrying the current value) — this ADR's first revision. Rejected:
  its sequence is Bolt's own, so it diverges from the store's commit
  order the moment any other writer commits (a second listener, the
  embedded API, the C ABI), and a bookmark handed back for session
  restore can name a sequence that is not the transaction's. Once
 [ADR-0502](./0502-typed-c-abi-v2.md) §7 threaded the engine's commit sequence out, the counter was
  a second, disagreeing source of the same fact.
- **A read carries the last committed sequence.** Rejected: the engine
  reports no bookmark for a statement that committed nothing, and Bolt
  does not fabricate one; drivers keep their previous bookmarks when a
  summary carries none.
- **Validate received bookmarks and fail unknown ones.** Rejected: a
  bookmark from an earlier server run or another listener is still
  satisfied by this single store, and failing it would break driver
  retries across restarts.
- **Send the bookmark on every PULL SUCCESS, including in transactions.**
  Rejected: Neo4j sends it only where a transaction ended, and drivers
  that see a bookmark mid-transaction would record a point before the
  commit.

## Consequences

- neo4rs `Txn::commit` returns a bookmark. The Python and JS drivers'
  bookmark managers hold real values and send them back, which the server
  accepts.
- Bookmarks are the store's commit sequence: they survive a restart and
  agree across every listener, connection and surface on one store.
- A read's ending SUCCESS carries no bookmark (Neo4j sends one); drivers
  keep the bookmarks they already hold, which this store satisfies.
- Pinned by `tests/compat/scripts/protocol/bookmarks.bolt` and the
  `protocol_summary_tests.rs` unit tests.

## Later references

- Revised while Proposed (typed-abi-v2): Decision §1–2 replace the
 per-listener counter with the engine's commit sequence of [ADR-0502](./0502-typed-c-abi-v2.md) §7,
  which that ADR had left out of scope for Bolt.

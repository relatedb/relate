---
type: adr
area: engine
date: 2026-09-28
deciders: CypherLite maintainers
name: 0276-store-level-databases-catalog
status: accepted
tags:
  - catalog
  - namespaces
title: Store-level databases catalog
REFERENCES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
SUPERSEDES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
---

# 0276. Store-level databases catalog

- **Renumbered**: from 0145 (shared-number cleanup)

## Context

[ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §8 placed the databases catalog — the three fixed namespaces'
settings, today just `defaultLanguage` — in every graph's theater
(`TheaterData.databases`). That made three copies of one store-level fact:

- `ALTER DATABASE … SET DEFAULT LANGUAGE` is a database-administration
 command ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §2): it routes to the store's catalog and its
  `PutDatabase` redo lands in the `neo4j` graph's theater.
- `SHOW DATABASES … RETURN …` on a `system` session composes SHOW with
  RETURN, so the plan is not an all-`Catalog` admin command and executes on
  the `system` graph — reading that slot's statically seeded theater.

The two spellings therefore diverged on `defaultLanguage` within one store:
the write updated the `neo4j` copy while the `system` copy kept showing the
seeded `CYPHER 25`. [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §10 already established the correct home for
state that is one-per-store: the `StoreServices` bundle, `Arc`-shared into
every namespace graph via `Graph::share_services_from` (procedures, LOAD
CSV policy). The databases catalog is the same kind of state and was only
left per-graph because §8 froze the WAL/checkpoint formats around the
existing theater layout.

## Decision

The databases catalog is **store-level**: one table shared by every
namespace graph of a store.

- A crate-internal `DatabasesCatalog` (`Arc`-shared through `StoreServices`,
 like the procedure registry and CSV policy of [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §10) holds the
  `name → (defaultLanguage, composite)` table. It seeds exactly the three
 fixed namespaces at `CYPHER 25`, non-composite, and keeps the [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §8
  invariants: fixed names match case-insensitively and store canonically,
  entries for other names are ignored, a fixed namespace can never be
  dropped, and a checkpoint image is sanitized to exactly the fixed
  namespaces on load.
- `TheaterData` no longer carries `databases`. The read-admission snapshot
  (`ShowSchema`) is now a real struct that pairs a capture of the shared
  catalog with the graph's theater maps, so SHOW readers and the
  speculative overlay (`apply_overlay` on `PutDatabase`/`DropDatabase`)
  keep their admitted-copy semantics unchanged.
- **Formats do not change.** `CheckpointTheater` still serializes the table
  under the top-level `databases` key (declared first, preserving the
  historical flattened-key order); the `PutDatabase`/`DropDatabase` redo
  ops are untouched. Committed replay (`Graph::apply_redo`) writes the
  shared catalog instead of the graph theater, and checkpoint capture /
  restore read and install the shared catalog. Old stores open unchanged
  and the golden-store fixtures hold.

## Alternatives considered

- **Keep per-graph theaters, classify admin commands by command family
  instead of plan shape** (route `SHOW DATABASES … RETURN …` to the `neo4j`
  graph too). Rejected: it fixes this one symptom but leaves three copies
  of store-level state that any future routing gap can desynchronize again,
  and it widens admin routing to plans that also read session-graph data.
- **Keep per-graph theaters and replicate the write to every namespace
  slot.** Rejected: three writers for one fact, new failure modes when a
  slot is swapped (`system_ontologies` is `Arc`-swapped as a whole,
 [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §10), and no single place to capture a checkpoint image from.
- **Change the checkpoint/WAL format to a dedicated store-level section.**
 Rejected: [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §8's format freeze is still right — there is nothing
  to gain from a format bump when the existing `databases` key can carry
  the shared table verbatim.

## Consequences

- `ALTER DATABASE` and every `SHOW DATABASES` spelling, on any namespace
  session, read and write one table; the divergent-read regression is
  pinned by `ce_admin.rs`
  (`alter_and_system_session_show_databases_agree_on_default_language`).
- [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §8's placement sentence ("stays in `TheaterData.databases`") is
  superseded by this ADR; its format and invariant statements stand.
- The `system_ontologies` whole-graph swap needs no databases handling: the
  catalog survives the swap because it lives in the shared bundle.
- `DatabasesCatalog` is crate-internal, so no public API or client
  documentation changes.

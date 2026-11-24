# Retract recipe example

Run with `cargo run -p relate --example retract_recipe`.

The recipe is the audit surface's payoff (ADR-0610): two simulated
agents write with their names in the transaction metadata, then one
statement retracts everything one of them wrote. It needs the
`RELATE_PREVIEW=value-provenance` opt-in before `Database::open` —
without it `CALL db.transactions()` fails with

```text
semantic error: SemanticError: preview feature 'value-provenance' is not enabled: set RELATE_PREVIEW=value-provenance
```

and the commits record no metadata to query.

Each agent's transaction seeds its map through `CALL tx.setMetaData`
(the procedure channel; the embedded `Connection::set_tx_metadata` and
a Bolt `BEGIN`'s `tx_metadata` extra are the other two). The retract is
one statement:

```cypher
CALL db.transactions() YIELD commitTs, metadata, changedKeys
WHERE metadata.agent = 'ingest'
UNWIND changedKeys AS ck
WITH collect(DISTINCT ck.elementId) AS touched
MATCH (t) WHERE elementId(t) IN touched
DETACH DELETE t
```

`db.transactions()` lists the retained records ascending by `lsn` —
`since`, its one argument, is the inclusive `commitTs` lower bound
(default 0, every retained record) when the audit question needs only
the tail. The `WHERE` picks one agent's commits, the changed keys name
every element that agent touched (deleted elements included — the
record, not the live graph, is the truth), and the `MATCH` finds those
elements by their element ids. The example asserts the retracted state
(the other agent's work stands), and that the retraction itself is
auditable: its own retained record carries the supervisor's metadata
and its changed keys are the same elements' deletions.

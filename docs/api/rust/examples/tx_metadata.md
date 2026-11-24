# Transaction-metadata example

Run with `cargo run -p relate --example tx_metadata`.

The example opts in by setting `RELATE_PREVIEW=value-provenance` before
`Database::open` (ADR-0610, through ADR-0371's registry): the transaction
metadata machinery is preview-gated, so without that opt-in
`Connection::set_tx_metadata` fails with

```text
semantic error: SemanticError: preview feature 'value-provenance' is not enabled: set RELATE_PREVIEW=value-provenance
```

`CALL tx.setMetaData` is the void compat no-op, `CALL tx.getMetaData`
returns the fixed `{source: 'cip230'}` row the CIP-230 corpus pins, and a
store's WAL records and checkpoints carry no transaction-record bytes at
all — a store that never opts in is byte-for-byte indistinguishable from
one on a build without the feature.

With the opt-in, the example drives the metadata channels onto one
per-transaction cell (merged in call order, later keys winning):

- the embedded `Connection::set_tx_metadata(map)`;
- `CALL tx.setMetaData(map)`, whose live map `CALL tx.getMetaData()`
  returns;
- a Bolt `BEGIN`'s `tx_metadata` extra (the driver channel — exercised in
  the `relate-bolt` suite, not here);
- the typed twin `Connection::set_tx_metadata_of_type(type, map)`, whose
  transaction carries `type` as its record's family instead of the
  provenance family's own.

Every untyped channel hard-sets the **provenance** record type; the typed
twin names another family (a non-empty kebab-case name — the example uses
`audit-trail` — with `provenance` reserved for the untyped channels), so
its transaction's record derives no changed keys, joins no
`provenance()`/`history()` event, and `db.transactions` lists with its
`type` and null `changedKeys`.

The map serializes into the commit's WAL record at the next transaction
boundary — an auto-commit write's, or an explicit transaction's `commit` —
and is cleared after; a `rollback` discards it. The same record carries
the commit's derived changed property keys (`changed_keys`, distinct and
sorted — provenance-family commits only), and the retained records
(`Database::transaction_records()`, joined to a bookmark's `relate:<n>`
by `n` = `commit_ts`) survive WAL
replay and checkpoint restore — the `transactions/` object family — as
the input of the later `db.transactions` / `provenance()` / `history()`
audit surface. The merged map is bounded by the 16 MiB per-object
envelope; an over-bound seed is a typed storage error and leaves the
current map unchanged.

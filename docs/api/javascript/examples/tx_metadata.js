const { Database } = require("@relate/relate");

// ADR-0610: the transaction-metadata machinery is the `value-provenance`
// preview feature's; RELATE_PREVIEW is read once, at open. This example
// needs the opt-in — but a process that already set the variable (a test
// run's union opt-in) keeps its setting, so the first setting wins.
process.env.RELATE_PREVIEW ??= "value-provenance";

const db = Database.open(":memory:");
const conn = db.connect();

// query's third argument is the per-transaction metadata map (ADR-0610):
// it seeds the connection's map, and the statement's auto-commit boundary
// serializes it into the commit's record — where the audit surface reads
// it back. The values cross through the same builder as parameters, so
// temporals, bigints and nested maps keep full fidelity.
conn.query("CREATE (p:Person {name: $name})", { name: "Ada" }, {
  by: "app",
  run: 42,
});

// CALL db.transactions reads the retained records: the commit's metadata
// map beside its changed property keys, ascending by lsn.
const txs = conn
  .query("CALL db.transactions() YIELD commitTs, metadata RETURN commitTs, metadata")
  .rows.at(-1);
if (txs.metadata.by !== "app" || txs.metadata.run !== 42) {
  throw new Error("unexpected record metadata: " + JSON.stringify(txs));
}

// Inside an explicit transaction the live map is readable while it waits
// for the boundary, and the commit clears it after.
conn.begin();
conn.query("CREATE (:Task)", null, { step: "load" });
const live = conn.query("CALL tx.getMetaData() YIELD metadata RETURN metadata").single();
if (live.metadata.step !== "load") {
  throw new Error("unexpected live metadata: " + JSON.stringify(live));
}
conn.commit();
const after = conn.query("CALL tx.getMetaData() YIELD metadata RETURN metadata").single();
if (Object.keys(after.metadata).length !== 0) {
  throw new Error("the boundary clears the map: " + JSON.stringify(after));
}

// Seeds merge in call order — later keys win — and a read seeds without
// a boundary, so both maps ride the next write's one record.
conn.query("RETURN 1 AS n", null, { by: "loader" });
conn.query("RETURN 1 AS n", null, { run: 7 });
conn.query("CREATE (:Batch)");
const merged = conn.query("CALL db.transactions() YIELD metadata RETURN metadata").rows.at(-1);
if (merged.metadata.by !== "loader" || merged.metadata.run !== 7) {
  throw new Error("unexpected merged metadata: " + JSON.stringify(merged));
}

// provenance() joins a property value to the commit that installed it —
// the audit question the record's metadata answers.
const who = conn
  .query("MATCH (p:Person {name: 'Ada'}) RETURN provenance(p, 'name').metadata.by AS by")
  .single();
if (who.by !== "app") {
  throw new Error("unexpected provenance metadata: " + JSON.stringify(who));
}

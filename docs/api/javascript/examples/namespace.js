const { Database } = require("@relate/relate");

// ADR-0593: system_ontologies is visible only under the `ontology`
// preview opt-in; set RELATE_PREVIEW before the open (it is read once,
// at open).
process.env.RELATE_PREVIEW = "ontology";

const db = Database.open(":memory:");

// Omitting the argument connects to the default "neo4j" namespace.
const conn = db.connect();
conn.query("CREATE (n:Person {name: 'Ada'})");

// Connect straight into another fixed namespace; names match
// case-insensitively. system_ontologies is visible under the opt-in
// above (ADR-0593) and not queryable (ADR-0229): selecting it works, but
// every non-administration statement routed to it fails with 42N17 —
// SHOW DATABASES still runs there.
const ontologies = db.connect("system_ontologies");
ontologies.query("SHOW DATABASES");

// Switch an open connection's namespace between transactions.
ontologies.setNamespace("neo4j");
const result = ontologies.query("MATCH (n:Person) RETURN n.name AS name");
if (result.rows[0].name !== "Ada") {
  throw new Error("unexpected");
}

// Unknown names and in-transaction switches throw.
try {
  db.connect("nope");
  throw new Error("expected unknown-namespace error");
} catch (e) {
  if (!e.message.endsWith("Graph not found: nope")) throw e;
}
conn.begin();
try {
  conn.setNamespace("system");
  throw new Error("expected in-transaction error");
} catch (e) {
  if (!e.message.endsWith("Cannot change the namespace while a transaction is open.")) throw e;
}
conn.rollback();

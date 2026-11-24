// The metadata argument (ADR-0610): execute's third argument is the
// per-transaction metadata map — the embedded channel, a JSON object
// string decoded exactly as params, seeded before the statement runs so
// the next transaction boundary serializes it into the commit's record.
// The machinery is the `value-provenance` preview feature's, and
// RELATE_PREVIEW is read once, at open, from the process environment —
// which a wasm host does not have: the module's own environment is
// always unset, whatever the browser, worker or Node host carries. So
// on this surface a present map is always the authored preview error,
// riding the envelope's `error`, and the statement never runs — the
// channel's gate, pinned as the engine spells it.
// Assumes relate.js and relate_bg.wasm are next to this file (see
// docs/api/wasm/spec.md).
import init, { RelateDb } from "./relate.js";

await init();
const db = new RelateDb();

// Two arguments is today's call exactly: no seed, the statement runs.
const plain = JSON.parse(db.execute("RETURN 1 AS n", null));
if (plain.error !== null || plain.rows[0].n !== 1) {
  throw new Error("unexpected result: " + JSON.stringify(plain));
}

// A present map is the seed's authored preview error on this build ...
const seeded = JSON.parse(
  db.execute("CREATE (:M)", null, JSON.stringify({ by: "app", run: 42 })),
);
if (!/preview feature 'value-provenance' is not enabled/.test(seeded.error ?? "")) {
  throw new Error("expected the authored preview error: " + JSON.stringify(seeded));
}
// ... so the statement never ran, and the handle stays usable.
const count = JSON.parse(db.execute("MATCH (n:M) RETURN count(n) AS c", null));
if (count.rows[0].c !== 0) {
  throw new Error("the refused seed must not run the statement: " + JSON.stringify(count));
}

// A map that does not decode names the argument; blank text is no seed.
const refused = JSON.parse(db.execute("RETURN 1 AS n", null, "[1]"));
if (!/metadata must be a JSON object/.test(refused.error ?? "")) {
  throw new Error("expected the decode refusal: " + JSON.stringify(refused));
}
const blank = JSON.parse(db.execute("RETURN 2 AS n", null, "  "));
if (blank.error !== null || blank.rows[0].n !== 2) {
  throw new Error("blank metadata is no seed: " + JSON.stringify(blank));
}

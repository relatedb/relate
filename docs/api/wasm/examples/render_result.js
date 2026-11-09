// Render a script's result as a recording (ADR-0131, ADR-0132): the
// canonical text form of a result. Assumes
// relate.js and relate_bg.wasm are next to this file (see
// docs/api/wasm/spec.md).
import init, { RelateDb, renderResult } from "./relate.js";

await init();
const db = new RelateDb();

const envelopes = db.executeScript("CREATE (:City {name: 'Oslo'}); MATCH (c:City) RETURN c.name AS name", null);
const recording = JSON.parse(renderResult(envelopes));

const expected = '| name   |\n|--------|\n| "Oslo" |\n1 row · nodes created: 1, properties set: 1, labels added: 1';
if (recording.kind !== "table" || recording.text !== expected || recording.table.rows[0][0] !== '"Oslo"') {
  throw new Error("unexpected recording: " + JSON.stringify(recording));
}
// The counters are summed over the script, all eleven of them.
if (recording.stats.nodes_created !== 1 || recording.stats.nodes_deleted !== 0) {
  throw new Error("unexpected stats: " + JSON.stringify(recording.stats));
}

// A result holding nodes renders as gram, with its graph for a graph view.
const graph = JSON.parse(renderResult(db.executeScript("MATCH (c:City) RETURN c", null)));
if (graph.kind !== "gram" || graph.graph.nodes.length !== 1 || graph.graph.nodes[0].labels[0] !== "City") {
  throw new Error("unexpected gram recording: " + JSON.stringify(graph));
}

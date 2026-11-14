"use strict";

// Graph extraction from materialized rows (ADR-0216; ADR-0214's semantics
// on the JS side). Pure logic over the decoded value shapes (ADR-0502 §1)
// — no FFI here. The decoded-entity marker lives in entity.js.

const { decodedEntityKind, isPlainObject } = require("./entity");

/// The unique graph entities of one result, de-duplicated by element id
/// and kept in first-seen order: `nodes` and `relationships` are arrays of
/// the decoded shapes ({kind, elementId, id, labels, properties} /
/// {kind, elementId, id, type, startElementId, endElementId, startId,
/// endId, properties}).
class ResultGraph {
  constructor(nodes, relationships) {
    this.nodes = nodes;
    this.relationships = relationships;
  }
  get size() {
    return this.nodes.length + this.relationships.length;
  }
  [Symbol.for("nodejs.util.inspect.custom")]() {
    return `ResultGraph(nodes=${this.nodes.length}, relationships=${this.relationships.length})`;
  }
}

function collectGraphEntities(rows) {
  const nodes = new Map();
  const relationships = new Map();
  // Which collection an entity kind lands in is data, not control flow —
  // a Map, so the lookup consults exactly the two kinds and nothing an
  // object's prototype chain might offer under another name.
  const buckets = new Map([
    ["node", nodes],
    ["relationship", relationships],
  ]);

  function walk(value) {
    const bucket = buckets.get(decodedEntityKind(value));
    if (bucket) {
      if (!bucket.has(value.elementId)) bucket.set(value.elementId, value);
    } else if (Array.isArray(value)) {
      // A path is an alternating [node, rel, node, ...] array; every array
      // is walked, so paths and user lists both contribute their entities.
      for (const item of value) walk(item);
    } else if (isPlainObject(value)) {
      for (const item of Object.values(value)) walk(item);
    }
  }

  for (const row of rows) {
    for (const value of Object.values(row)) walk(value);
  }
  // Map preserves insertion order and the keys are unique element ids, so
  // the values arrays are the de-duplicated, first-seen-order entities
  // directly. (Element ids are opaque strings — "<db>:<store>:n<n>" — and
  // lexicographic sorting would misorder them; first-seen is the honest,
  // defined order.)
  return new ResultGraph([...nodes.values()], [...relationships.values()]);
}

module.exports = { ResultGraph, collectGraphEntities };

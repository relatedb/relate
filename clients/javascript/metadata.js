"use strict";

// The closed result-metadata vocabularies: the C ABI's `query_type`
// (ADR-0162, ADR-0332) and `namespace` (ADR-0117) accessors return exactly
// these values, so anything else is an ABI contract violation, never a
// lenient pass-through (the Go client's decodeQueryType / decodeNamespace).
// index.d.ts declares the same two unions; test.js pins them to these tables.

const { abiViolation } = require("./errors");

const QUERY_TYPES = ["r", "w", "rw", "s"];
const NAMESPACES = ["neo4j", "system", "system_ontologies"];

/// The one policy for a closed vocabulary: a listed value passes through,
/// anything else throws naming `what`.
function decodeClosed(vocabulary, what, raw) {
  if (!vocabulary.includes(raw)) throw abiViolation(`${what} ${JSON.stringify(raw)}`);
  return raw;
}

const decodeQueryType = (raw) => decodeClosed(QUERY_TYPES, "query type", raw);
const decodeNamespace = (raw) => decodeClosed(NAMESPACES, "namespace", raw);

module.exports = { QUERY_TYPES, NAMESPACES, decodeQueryType, decodeNamespace };

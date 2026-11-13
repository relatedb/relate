"use strict";

// The closed result-metadata vocabularies: the C ABI's `query_type`
// (ADR-0162, ADR-0332) and `namespace` (ADR-0117) accessors return exactly
// these values, so anything else is an ABI contract violation, never a
// lenient pass-through (the Go client's decodeQueryType / decodeNamespace).
// index.d.ts declares the same two unions; test.js pins them to these tables.
// The refusal policy is the error channel's one `decodeClosed` (errors.js),
// so no surface spells a second Set/.has() beside it.

const { decodeClosed } = require("./errors");

/// The plan-time statement classification (ADR-0162 wire code).
const QUERY_TYPES = Object.freeze(["r", "w", "rw", "s"]);
/// The three fixed namespaces (ADR-0117).
const NAMESPACES = Object.freeze(["neo4j", "system", "system_ontologies"]);

const decodeQueryType = (raw) => decodeClosed(QUERY_TYPES, "query type", raw);
const decodeNamespace = (raw) => decodeClosed(NAMESPACES, "namespace", raw);

module.exports = { QUERY_TYPES, NAMESPACES, decodeQueryType, decodeNamespace };

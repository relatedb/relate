"use strict";

// The error channel (ADR-0502 §2): the ABI's one error shape, parsed once
// into a typed exception. Pure over the FFI — the koffi decode/free of the
// error out-array stays in index.js — so the parsing contract is testable
// on its own (a malformed shape never needs a library to reproduce).

/// The error shape's field table (ADR-0502 §2): the ABI key, the
/// exception's property (a null jsKey — the message — rides the
/// constructor's first argument, Error's own channel, never assigned from
/// facts), and the required type. One vocabulary drives both halves:
/// errorFromJson validates through it, the constructor assigns through
/// it, so the two cannot drift.
const ERROR_FIELDS = [
  ["message", null, "string"],
  ["code", "code", "string"],
  ["gql_status", "gqlStatus", "string"],
  ["stage", "stage", "string"],
  ["retryable", "retryable", "boolean"],
];

/// The typed form of the ABI's error shape: the classification (code,
/// gql_status, stage, retryable) comes straight from the engine's single
/// table — never re-derived from message text. `retryable` is the drivers'
/// transient test: re-run the work only when it is true. `stage ===
/// "internal"` means a boundary failure (a poisoned handle, a caught
/// panic, an argument that failed to decode): close the handle and open a
/// new one instead of retrying.
///
/// `facts` is the complete shape, with no defaults: errorFromJson has
/// validated every field before constructing, and clientRefusalError
/// below is the one non-ABI construction.
class RelateError extends Error {
  constructor(message, facts) {
    super(message);
    this.name = "RelateError";
    for (const [abiKey, jsKey] of ERROR_FIELDS) {
      if (jsKey !== null) this[jsKey] = facts[abiKey];
    }
  }

  /// The drivers' retriable spelling, like Neo4j's Neo4jError.isRetriable().
  get isRetriable() {
    return this.retryable === true;
  }
}

/// The one spelling of an ABI-contract violation on the error path: a
/// plain Error naming the problem (the same prefix values.js's walker
/// refusals use), never a RelateError with fabricated fields.
function abiViolation(detail) {
  return new Error(`ABI contract violated: ${detail}`);
}

/// The one non-ABI construction: the sync-call refusal while an async
/// query is in flight (index.js) — a client-side refusal that carries no
/// engine classification. The only site the no-classification shape
/// (null / null / null / false) is written; every other RelateError
/// comes from a validated ABI shape.
function clientRefusalError(message) {
  return new RelateError(message, {
    code: null,
    gql_status: null,
    stage: null,
    retryable: false,
  });
}

/// The stage vocabulary of the ABI's error shape (ADR-0502 §2): where the
/// failure happened — compile (parse/plan), runtime (execution) or
/// internal (a failure the engine never saw: a NULL or poisoned handle, a
/// caught panic, an argument that failed to decode, a params-builder
/// refusal).
const ERROR_STAGES = new Set(["compile", "runtime", "internal"]);

/// Parse the error shape once into the typed exception. Input is the ABI's
/// JSON text (the one JSON that remains on the error path — metadata, not
/// cell payloads). Every field is required and correctly typed, and the
/// stage must be one of the vocabulary: a malformed or partial shape is an
/// ABI contract violation naming the problem — never a fabricated default
/// (a missing "retryable" would silently decode as false and a retry loop
/// would decline to retry). The Go and Swift clients hold the same
/// contract.
function errorFromJson(json) {
  let facts;
  try {
    facts = JSON.parse(json);
  } catch (e) {
    throw abiViolation(`the error shape is not JSON (${e.message})`);
  }
  if (facts === null || typeof facts !== "object" || Array.isArray(facts)) {
    throw abiViolation(`the error shape is not an object: ${json}`);
  }
  for (const [field, , type] of ERROR_FIELDS) {
    if (facts[field] === undefined) {
      throw abiViolation(`the error shape is missing "${field}"`);
    }
    if (typeof facts[field] !== type) {
      throw abiViolation(`error shape field "${field}" must be a ${type}, got ${typeof facts[field]}`);
    }
  }
  if (!ERROR_STAGES.has(facts.stage)) {
    throw abiViolation(`error stage "${facts.stage}" is not one of ${[...ERROR_STAGES].join(", ")}`);
  }
  return new RelateError(facts.message, facts);
}

module.exports = {
  RelateError,
  abiViolation,
  clientRefusalError,
  errorFromJson,
};

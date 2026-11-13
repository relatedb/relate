"use strict";

// The decoded-entity contract (ADR-0502 §1): the structural marker that
// makes a decoded NODE / RELATIONSHIP an entity. Its own module so the
// value codec (values.js, the writer) and the graph extraction (graph.js,
// a reader) both depend on this leaf rather than on each other.

// Decoded shapes (the value walker, ADR-0502 §1): entities are plain
// objects carrying an explicit `kind` discriminator — "node" /
// "relationship" — with the engine's elementId() spelling as the identity.
// What makes one an entity is structural, not its keys: the decoder stamps
// every NODE / RELATIONSHIP it produces with this non-enumerable marker
// (value = the entity kind), and graph() dispatches on the marker alone. A
// user MAP — even one carrying kind "node" plus every entity key — has no
// marker, so it stays a map and is walked like any other. The marker is
// module-local on purpose, never Symbol.for: the global registry would let
// any code forge or probe it, and the one invariant entity recognition
// stands on is that only a decoded entity can carry it.
const DECODED_ENTITY = Symbol("relate.decoded");

/// Build one decoded entity: the documented plain shape — the entity's
/// own fields plus `kind`, the discriminator key — stamped with the
/// non-enumerable marker (value = the kind). Non-enumerable: it never
/// shows up in Object.keys, JSON.stringify, spreads or deep-equality.
/// `kind` is written last, from the argument: this constructor is the one
/// writer of the discriminator, so a fields object cannot override it and
/// the visible kind can never disagree with the marker. Only this
/// constructor stamps: no exported function can mark an existing object,
/// so a user object can never become an entity.
function decodedEntity(kind, fields) {
  const entity = { ...fields, kind };
  Object.defineProperty(entity, DECODED_ENTITY, { value: kind });
  return entity;
}

const isPlainObject = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v);

/// The marker's value (the entity kind) on a decoded entity, or undefined
/// on anything else — the read half of the marker pair, for tests and
/// diagnostics.
function decodedEntityKind(value) {
  return value?.[DECODED_ENTITY];
}

module.exports = { decodedEntity, decodedEntityKind, isPlainObject };

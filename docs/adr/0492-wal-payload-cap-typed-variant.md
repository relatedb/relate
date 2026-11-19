---
type: adr
area: engine
claim: c4b05d16146bdc3330d4895d33a392ab8381d9bf
name: 0492-wal-payload-cap-typed-variant
status: proposed
tags:
  - errors
  - memory
  - wal
title: The WAL payload cap refusal is a typed error variant
REFERENCES:
  - "[[adr/0003-tck-named-error-kinds]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
  - "[[adr/0489-ddl-validation-error-classes]]"
  - "[[adr/0494-wal-payload-cap-memory-limit-class]]"
---

# ADR-0492: The WAL payload cap refusal is a typed error variant

## Context

The 64 MiB per-record WAL payload cap ([ADR-0086](./0086-bounded-persistence-input.md)) refuses a commit whose
serialized record would cross `MAX_PAYLOAD_BYTES`. The refusal is raised at
one site — `encode_record` in `cypherlite-storage/src/wal.rs`, the shared
encoder behind both the commit-admission reservation count and the append
preflight capture — as `Error::storage("WAL payload exceeds 64 MiB limit")`.

The cancellation ladder (PR #653) must recognize exactly that refusal: a
rung whose single-statement CREATE crosses the cap is a size-determined
`LadderStop::StoreLimit`, not a bug. It recognizes it by matching the
rendered prose prefix `"WAL payload exceeds"` — the LABS-311
stringly-typed discrimination anti-pattern. That guard was already
tightened once to the unique prefix after a collision with the WAL
scanner's corruption diagnostic (`"invalid WAL payload length …"`), which
is the recurring cost of prose matching: every new storage message is a
potential collision, and every consumer re-derives the recognition.

[ADR-0003](./0003-tck-named-error-kinds.md) sets the contract this moves inside: message text is informal,
the kind is contract, and changing a kind is a breaking change. The
refusal's kind today is the storage class — the CLI classifies it
`ErrorFamily::Storage`, Bolt `ErrorClass::Internal` (genericized at the
trust boundary, [ADR-0260](./0260-trust-boundary-error-sanitization.md)), HTTP 400, the C ABI / Python adapter
`("StorageError", "SemanticError")` via `binding_error_metadata`.

## Decision

**The refusal becomes a dedicated fieldless variant, `Error::WalPayloadCap`,
with Display byte-identical to the historical `Storage` rendering and the
storage classification unchanged on every surface — only the
discriminability moves.**

1. `cypherlite_core::Error` gains `WalPayloadCap`, rendered
   `storage error: WAL payload exceeds 64 MiB limit` — byte-identical to
   the `Storage` text it replaces, the same migration shape as
   `Error::Locked` (a storage-family refusal split out of `Storage` so
 consumers match the variant) and the [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) typed variants.
2. The cap becomes the one public constant, `Error::WAL_PAYLOAD_CAP_BYTES`;
   the variant's `Display` derives its figure from it (byte-identical
   today, single-sourced forever), and the storage crate's WAL encoder,
   scanner and tests consume the same constant, so the rendered message,
   the write refusal and the recovery bound cannot drift apart. The
   bounded sink carries the typed refusal itself — one error channel
   (`BoundedSink::finish` returns it; the serializer's io abort only stops
   the serializer) — never a separate `(limit, message)` pair.
3. Every surface classifies the variant with the storage class it had as
   `Storage`: the CLI `ErrorFamily::Storage`, Bolt `ErrorClass::Internal`,
   HTTP 400 (the reply fall-through), `binding_error_metadata`
   `("StorageError", "SemanticError")`. The exhaustive matches are
   compiler-checked — the new class is classified once per surface, never
   re-derived from text.

## Alternatives considered

- **A `CapacityKind::WalPayload` kind on the existing `Error::Capacity`
  variant.** Rejected: `Capacity`'s Display is composed by
  `capacity_detail` — "the one rendering of both capacity contracts" — so
  a WAL line would either change the message (breaking the byte-identical
  requirement) or embed the foreign `storage error: ` class prefix inside
  a capacity detail; and every surface would silently reclassify the
  refusal (Bolt `Internal` → `TransactionOutOfMemory`, HTTP 400 → 507,
  the C ABI/Python `StorageError` → `MemoryLimitExceeded`, the CLI family
 `Storage` → `Capacity`) — a kind change, which [ADR-0003](./0003-tck-named-error-kinds.md) defines as a
  breaking change, and more than the discriminability-only scope of this
  fix. Classifying the refusal as a client memory error may be the better
  long-term contract; that is a separate per-surface decision to make
  deliberately, not a side effect of typing the channel.
- **Keep the prose prefix and tighten the guard further.** Rejected: the
  LABS-311 anti-pattern this decision exists to remove; each new storage
  message re-risks the collision the prefix was already tightened for
  once, and every future consumer re-derives the recognition.
- **A `WalPayloadCap` payload carrying `used`/`limit` fields.** Rejected:
  the refusal site knows only that the bound was crossed (the bounded
  sink stops counting at the crossing write, and the admission pass
  never retains the bytes), so the fields would be fabricated values; the
  cap is already the `WAL_PAYLOAD_CAP_BYTES` constant.

## Consequences

- `cypherlite::Error` gains a public variant; every exhaustive match is
 compiler-checked (as with [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) and [ADR-0489](./0489-ddl-validation-error-classes.md)), and the classification
  sites (CLI, Bolt, `binding_error_metadata`) gain one storage-class arm
  each. HTTP and MCP need no change: their fall-through already gives
  storage errors the same treatment.
- The cancellation ladder's `refused_for_wal_payload_cap` matches the
  variant; no prose matching remains anywhere for this refusal.
- The refusal keeps its message and its storage classification on every
  surface, so pinned messages, Bolt failure codes, HTTP statuses and the
  C ABI/Python error classes do not move; only consumers that choose to
  match `Error::WalPayloadCap` see a difference.
- The WAL scanner's over-cap length diagnostic stays `Error::Storage`
  (`"invalid WAL payload length …"`): it reports a corrupt or
  newer-written segment, not a refused commit, and shares only the byte
  constant.

## Later references

[ADR-0494](./0494-wal-payload-cap-memory-limit-class.md): this ADR's Decision paragraphs 1 and 3 are superseded — the
fieldless `Error::WalPayloadCap` variant is removed in favor of the
capacity family (`Error::Capacity { used, limit, kind:
CapacityKind::WalPayload }`), and the storage classification on every
surface becomes the memory-limit class (binding kind
`MemoryLimitExceeded`, HTTP 507, Bolt
`Neo.ClientError.General.TransactionOutOfMemoryError`, CLI family
`capacity`). The kind change [ADR-0003](./0003-tck-named-error-kinds.md) would have made breaking was
green-lit while no external clients exist (LABS-862), and the
byte-identical Display requirement is lifted for the communicative
capacity line. This ADR's single-sourced `WAL_PAYLOAD_CAP_BYTES`
constant (Decision paragraph 2) and the scanner diagnostic distinction
stand unchanged.

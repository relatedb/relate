# ADR-0494: The WAL payload cap refusal is the memory-limit class on every surface

Status: Proposed
Claim: 987686a6720f960a4cf7a08d37b5a45b03fede3e

## Context

ADR-0492 typed the 64 MiB per-record WAL payload cap (ADR-0086) refusal
as the fieldless `Error::WalPayloadCap` and — deliberately, as its
scope — kept the storage classification every surface gave the `Storage`
prose predecessor: CLI family `Storage` (exit 1), Bolt `ErrorClass::
Internal` (genericized to `Neo.DatabaseError.General.UnknownError` with
the fixed message `internal error` at the trust boundary), HTTP 400 (the
reply fall-through), and binding metadata `("StorageError",
"SemanticError")` for the C ABI and Python adapter.

ADR-0492's first rejected alternative recorded why that classification
is wrong: the refusal is semantically a capacity condition — the record
cannot be journaled because building it would exhaust memory — and the
storage class misinforms every caller that branches on the class: a
client counting memory-limit refusals, an HTTP gateway mapping 507, a
driver surfacing memory conditions, an operator reading `internal
error` for a statement the user simply made too large. It was rejected
only because reclassifying changes the error's *kind*, which ADR-0003
defines as contract and therefore breaking.

The human decision (Michael, 2026-11-03, LABS-862) removes that barrier:
CypherLite has no external clients yet — the contract is young — so the
kind change breaks no one, and reclassifying now is cheaper than a compat
migration later. The decision also picked the class name:
`MemoryLimitExceeded`, the kind string the binding metadata already
gives `Error::Capacity`.

## Decision

**The refusal is a `CapacityKind::WalPayload` refusal on the existing
`Error::Capacity` variant — one memory/capacity-limit family across the
whole public API — and every surface classifies it with its
memory-limit class.** The per-surface mapping, moving ADR-0492's table:

| Surface | Was (ADR-0492) | Now |
| --- | --- | --- |
| Rust public API | `Error::WalPayloadCap`, storage class | `Error::Capacity { used, limit, kind: CapacityKind::WalPayload }` |
| C ABI + Python (`binding_error_metadata`) | `("StorageError", "SemanticError")` | `("MemoryLimitExceeded", "ArgumentError")` — the same pair every other capacity refusal carries |
| HTTP | 400 (the reply fall-through) | **507 Insufficient Storage** (the `Error::Capacity` row of `run_error`) |
| Bolt | `ErrorClass::Internal` → genericized `Neo.DatabaseError.General.UnknownError` / `internal error` | `ErrorClass::Runtime(TRANSACTION_OUT_OF_MEMORY_CODE)` → `Neo.ClientError.General.TransactionOutOfMemoryError`, the Neo4j memory code in the non-retryable `ClientError` class, engine message passed through verbatim — the same code the other two memory conditions (graph cap, `--max-result-bytes`) use |
| CLI | `(Fatal, ErrorFamily::Storage)` → envelope `kind` `storage` | `(Fatal, ErrorFamily::Capacity)` → envelope `kind` `capacity`, exit 1 |

1. `Error::WalPayloadCap` is removed; `CapacityKind` gains `WalPayload`.
   The refusal is `Error::Capacity { used, limit, kind:
   CapacityKind::WalPayload }`, with `limit` the bound the check
   consulted (`Error::WAL_PAYLOAD_CAP_BYTES` at both refusal sites) and
   `used` the counted byte total at which the bounded serializer was
   stopped — a lower bound of the record the statement tried to commit,
   never its final size. This supersedes ADR-0492 alternative 3's
   "the site knows only that the bound was crossed": the bounded sink
   now records the crossing count, so the fields carry real values.
   `Error::WAL_PAYLOAD_CAP_BYTES` stays the one public spelling of the
   cap; the encoder, scanner and tests consume it unchanged.
2. The `BoundedSink` refusal becomes a two-arm shape — the caller's
   fixed error (the snapshot manifest bound, the Parquet shard
   envelope), or a capacity refusal parameterized by the crossing
   count, the bound and the `CapacityKind` — the sink stays
   caller-agnostic, knowing the capacity family, never which
   subsystem's cap refused; the WAL encoder spells its (bound, kind)
   pair exactly once, so no bounded pass over a commit record can pair
   the kind with a different limit. Still one error channel: the
   io abort the in-flight serializer sees renders the same error
   `BoundedSink::finish` returns; the serializer's own abort is
   discarded as before.
3. Display: ADR-0492's byte-identical-to-`Storage` requirement is
   lifted. The refusal renders through the one capacity renderer
   (`capacity_detail`) as the communicative line the house style
   requires — what hit the limit (a single WAL commit record), how far
   it got (at least N MB), the knob (the per-record WAL payload cap),
   and the action (commit the data in smaller transactions) — both byte
   figures rendered through the one `human_bytes` policy from the
   `limit` the check consulted, never a second inline conversion.
   Unlike the ADR-0463 expression cap, the cap is a fixed engine bound,
   so the line offers no raise-it knob.

The WAL scanner's over-cap length diagnostic stays `Error::Storage`
(`invalid WAL payload length …`): a corrupt or newer-written segment is
not a refused commit (ADR-0492, unchanged).

## Alternatives considered

- **Keep the storage class, as ADR-0492 landed it.** Rejected: the class
  misinforms every class-branching caller, and Bolt's genericization
  hides a user-fixable condition (a too-large single commit) behind
  `internal error` — the exact communicative-error failure the house
  style exists to remove. The kind-change barrier fell with the
  no-external-clients decision.
- **Keep `Error::WalPayloadCap` as its own variant, re-rendered in the
  capacity style and reclassified per surface.** Rejected: two variants
  for one semantic class is parallel state — every surface's capacity
  arm and every future consumer matching "the memory limit" must list
  both, and the two can drift. Folding into the one `Capacity` variant
  means a consumer matching `Error::Capacity { .. }` already catches the
  refusal, and no surface grows a new arm (the storage arms shrink).
- **A payload-carrying `WalPayloadCap { used, limit }` variant (ADR-0492
  alternative 3).** Rejected for the same parallel-state reason, and
  because a refusal variant beside `Capacity` re-derives the capacity
  rendering instead of sharing `capacity_detail`'s one policy.
- **Defer the reclassification until a compat story is needed.**
  Rejected: the contract is young precisely so it can be corrected
  cheaply; every month of storage-classified refusals is a month of
  callers the later migration would break.

## Consequences

- `Error::WalPayloadCap` is gone: every exhaustive match that listed it
  (the binding metadata, the CLI classifier, the Bolt classifier) drops
  the arm, compiler-checked. The cancellation ladder's
  `refused_for_wal_payload_cap` matches
  `Error::Capacity { kind: CapacityKind::WalPayload, .. }` — still
  discriminable by variant, never by prose.
- The refusal's message text changes (the storage prefix and the
  byte-identical pinning go), and its class changes on every surface:
  HTTP 400 → 507, Bolt `Neo.DatabaseError.General.UnknownError` →
  `Neo.ClientError.General.TransactionOutOfMemoryError` (message no
  longer genericized), binding kind `StorageError` →
  `MemoryLimitExceeded` (Python's DB-API mapping is unchanged: the kind
  falls through to the `ArgumentError` class → `DataError`, the same
  exception the graph cap raises), CLI envelope kind `storage` →
  `capacity`. Pinned tests move with the change in the same commit.
- Coverage guarantee: no Cypher-level behavior moves — the TCK corpus
  (3892 scenarios) and the CIP-eval frozen set contain no scenario that
  trips a 64 MiB per-record cap, and the graph- and expression-capacity
  Displays are byte-identical to before, so the pinned TCK/CIP numbers
  and the spec-harness coverage lane (ADR-0482) are unaffected; the
  full TCK run in this change's gate confirms 3892 of 3892.
- ADR-0492's Decision paragraphs 1 and 3 (the fieldless variant, the
  storage classification) are superseded by this ADR; its
  single-sourced `WAL_PAYLOAD_CAP_BYTES` constant and its scanner
  diagnostic distinction stand unchanged.

## Later references

ADR-0604 refuses a statement run inside an explicit transaction as soon
as its redo takes the transaction's commit record over the cap, so the
refusal this ADR classifies arrives at that statement rather than only at
COMMIT. The variant, the class and the rendering decided here stand
unchanged.

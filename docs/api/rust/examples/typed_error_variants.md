# Typed error variants (ADR-0307, ADR-0308, ADR-0494, ADR-0567)

Run `cargo run -p relate --example typed_error_variants --locked`. The
[compiled example](../../../../relate/relate/examples/typed_error_variants.rs)
exercises the public `Error` vocabulary through the query API: a missing
parameter fails before execution as `Error::parameters_missing(["p"])`,
`RETURN 1 / 0` raises `Error::Arithmetic(ArithmeticFailure::DivisionByZero)`,
an unknown `USE` target raises `Error::DatabaseNotFound`, and — ADR-0494 — a
single commit whose record crosses the 64 MiB WAL payload cap
(`Error::WAL_PAYLOAD_CAP_BYTES`) is refused with the capacity refusal
`Error::Capacity { kind: CapacityKind::WalPayload, .. }` — the memory-limit
class, its message naming the record and the cap — before any frame is
written. ADR-0567's `CapacityKind::SeekCandidates` (a shortest-path
endpoint listing past its limit) is built with
`Error::capacity_seek_candidates` and its kind and message asserted. CI's engine lane builds and runs it on every push.

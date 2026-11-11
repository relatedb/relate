# Rust crate — typed error variants

Part of the [Rust spec](spec.md). These `relate::Error` variants carry
an error class typed, so a surface maps it without reading the message
(ADR-0307: the Bolt server sends a Neo4j status code per variant). Each
renders exactly the text of the error it replaced, so the C ABI, Python,
JS, Java, Go, HTTP and MCP messages are unchanged, and
`binding_error_metadata` and the CLI error envelope classify each as they
classified its predecessor — with one deliberate exception: the WAL
payload cap's capacity kind (ADR-0494), which changed both its message
and its class from the storage error it replaced, while no external
clients existed.

## The one classification — `error_info` (ADR-0502 §2)

`relate::error_info(&Error) -> relate::ErrorInfo` is the engine's
single-source classification of any `Error`: `ErrorInfo { neo4j_code,
gql_status, stage, retryable }` — the Neo4j status code the caller-facing
classes carry (the internal class carries the genericized
`Neo.DatabaseError.General.UnknownError`, ADR-0260), the GQLSTATUS, the
`ErrorStage` (`Compile` the statement is rejected before any work,
`Runtime` it is discovered while producing rows, `Internal` it is not the
statement's fault — ADR-0319), and the drivers' transient test (`true`
only for a write-write conflict and a retryable processing cap). The match
is exhaustive: a new `Error` variant does not compile until it is
classified. Bolt's FAILURE rendering, HTTP's error envelope, the CLI's
exit-code classifier, Python and the C ABI derive from it — never a
second table. The GQLSTATUS values are the codes the surfaces emit today
(the engine-documented fixed-namespace codes of ADR-0116 §9 and the GQL
general code `42000` for every other caller-facing class); a nested
`<Class>:` token in a still-stringly `Syntax`/`Semantic`/`Execution`
payload carries its own documented code, resolved by the same canonical
scan the bindings use. The per-class code constants
(`relate_error::SYNTAX_ERROR_CODE`, …) are the classification's own
data, defined and exported by `relate_error` (the `relate` facade
re-exports only `error_info`, `ErrorInfo` and `ErrorStage`); the
GQLSTATUS values HTTP's envelope restates are likewise public there
(`relate_error::GQLSTATUS_GENERAL`, `GQLSTATUS_SYNTAX`,
`GQLSTATUS_DATABASE_NOT_FOUND`).

| Variant | Contract |
| --- | --- |
| `ParameterMissing(String)` | A referenced parameter was not supplied (ADR-0308). `Display` `semantic error: ParameterMissing: Expected parameter(s): <names>`: each missing name once, in order of first appearance, joined by `, `. Raised before execution; `EXPLAIN` plans without parameter values. `Error::parameters_missing(names)` builds it. |
| `DatabaseNotFound { name }` | A session namespace (`connect_namespace`, `set_namespace`, a Bolt `db`) or static `USE` target that is not a fixed namespace (ADR-0146, ADR-0307). `Display` `semantic error: DatabaseNotFound: Graph not found: <name>`, byte-identical to the `Semantic` text it replaced. `typed_detail()` returns `("DatabaseNotFound", "Graph not found: <name>")`; `Error::database_not_found(name)` builds it. |
| `Error::undefined_variable(name)` (a `Syntax` error) | A variable with no binding in the evaluated row (ADR-0335), raised for the expressions the plan-time binding walks cannot see (a `SHOW … WHERE` predicate lowered at run time). `Display` `syntax error: UndefinedVariable: variable '<name>' not defined`, byte-identical to the front-end's scope-check error; `Error::undefined_variable(name)` builds it. An unbound variable never evaluates to `null`. |
| `Arithmetic(ArithmeticFailure)` | An arithmetic failure while evaluating an expression (ADR-0307). `ArithmeticFailure` is `DivisionByZero` (`Display` `/ by zero`), `ModuloByZero` (`% by zero`) or `RoundingNecessary` (`round(x, p, 'UNNECESSARY')` on a value that needs rounding: `ArithmeticError: rounding necessary`) — each the payload of the `Type`/`Execution` error it replaced. `ArithmeticFailure::detail()` is that payload; `Error::Arithmetic`'s own `Display` composes the legacy full text (`type error: / by zero`, …, ADR-0338). `relate::ArithmeticFailure` is exported. |
| `ArgumentValue { kind, detail }` | A run-time function-argument value rejection (ADR-0559, ADR-0565): `range()`'s zero step, the `substring()`/`left()`/`right()` range checks, list-slice bounds, `point()`'s in-map values, `bar()`'s width, the percentile aggregates' argument, the vector similarity/distance and `vector()` dimension faults, `round()`'s precision/mode checks, `replace()`'s limit, the `trim()` single-character rule and a zero `CALL … IN TRANSACTIONS` batch size. `kind` is the `ErrorKind` naming the argument fault (`NumberOutOfRange`, `InvalidArgumentValue`), `detail` the payload; `Display` composes `semantic error: <Kind>: <detail>` and `typed_detail()` returns `(kind.as_str(), detail)`. Classified as the `ArgumentError` class at the runtime stage (`Neo.ClientError.Statement.ArgumentError`), the same class as the DDL `ArgumentError` variant but its own code and stage. `Error::argument_value_kind(kind, detail)` builds it. |
| `Cancelled` | The query was stopped through its `CancelToken` (ADR-0111, ADR-0307). `Display` `query cancelled`, the text of the `Execution("query cancelled")` it replaced; `Error::cancelled()` returns it. |
| `Capacity { used, limit, kind: CapacityKind::WalPayload }` | A commit record refused by the per-record WAL payload cap (ADR-0086; typed ADR-0492; reclassified ADR-0494): its serialized payload crosses `Error::WAL_PAYLOAD_CAP_BYTES` (64 MiB), so it cannot be journaled — refused before any frame is written or LSN published. The refusal is the capacity family's memory-limit class on every surface (binding kind `MemoryLimitExceeded`, HTTP 507, Bolt `Neo.ClientError.General.TransactionOutOfMemoryError`, CLI kind `capacity`). `used` is the counted byte total at which the bounded serializer stopped — a lower bound of the record the statement tried to commit — and `limit` the bound the check consulted; `Error::capacity_wal_payload(counted, limit)` builds it. `Display` is the communicative capacity line, both figures through the one byte-size renderer: `This statement tried to commit a single WAL record of at least <used>. This is a safety limit protecting against memory exhaustion — the per-record WAL payload cap is <limit>; commit the data in smaller transactions.` (64 MiB renders `64 MB`.) |
| `Capacity { used, limit, kind: CapacityKind::SeekCandidates }` | A shortest-path (`ANY SHORTEST`, `ALL SHORTEST`, `SHORTEST k`) statement whose two endpoint property maps each match more than the listing limit (1,048,576 nodes), so the route would have to materialize both lists to choose its search side (ADR-0567). Refused rather than truncated or scanned unbounded; the memory-limit class on every surface, like the other capacity kinds. `used` is the match count the bounded scan stopped at (a lower bound, `limit + 1`), `limit` the cap; `Error::capacity_seek_candidates(counted, limit)` builds it. `Display`: `A shortest-path endpoint pattern matches at least <used> nodes; the search lists at most <limit> to choose its side. This is a safety limit protecting against memory exhaustion — bind the endpoint or narrow it with an indexed property.` PROFILE Db Hits for the statement count the nodes and index postings the bounded scan examined before it stopped. |

The runnable example is
[examples/typed_error_variants.md](examples/typed_error_variants.md) —
`cargo run -p relate --example typed_error_variants --locked`, built and
run in CI's engine lane; `relate-core`'s `error/tests.rs` pin each
`Display` against the text it replaced.

# Java Client

The Java client (`clients/java`, artifact `ai.relatedb:relate`) binds the
product **C ABI** through JNA. Java 17+. The jar bundles the native library
for darwin-arm64, darwin-x64, linux-arm64, linux-x64 and win32-x64 and
extracts the running platform's copy to a per-user cache on first use — no
environment variable or library path. Per-platform classifier jars
(`osx-aarch_64`, `osx-x86_64`, `linux-aarch_64`, `linux-x86_64`,
`windows-x86_64`) carry one platform each. `RELATE_LIB` overrides the
bundled library for development only.

## Open, query, close

```java
import ai.relatedb.Database;

Database db = Database.open(":memory:");   // or a directory / .rl bundle
Connection conn = db.connect();

Result result = conn.query("RETURN 1 AS n");
result.columns();   // List<String>
result.rows();      // List<List<Object>>
```

## Transactions

`begin()` / `commit()` / `rollback()` open and close an explicit transaction
(ADR-0028); statements outside one auto-commit.

## Errors

ABI failures throw `RelateException`. A write-write conflict carries the
message `write-write conflict` — it is not retried, so retry in the caller
(ADR-0017).

Full contract: [`docs/api/java/spec.md`](https://github.com/relatedb/relate/blob/main/docs/api/java/spec.md).

## See also

- [C FFI](c-ffi.md) — the ABI this client wraps.
- [Go](go-client.md) and [JavaScript](javascript-client.md) — the same ABI in other languages.

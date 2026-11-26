# C FFI Reference

The `relate-ffi` crate exposes a small, C-compatible API for embedding
Relate in any language with a C FFI (C, C++, and others). It is the
foundation the JS/Java/Go/Swift clients are built on. The ABI is backed by
`relate::Database`.

The ABI is **version 2** — the typed ABI of ADR-0502:
`int relate_abi_version(void)` returns `2`. There is **one** API, the
tabular database/connection pair (`relate_database_open` →
`relate_connect_namespace` → `relate_query_params` →
`relate_destroy_result` / `relate_disconnect` /
`relate_database_close`), with one canonical entry point per operation
(ADR-0220).

The crate builds as both a `staticlib` and a `cdylib`. The C header is
committed at `surfaces/relate-ffi/include/relate.h` and is
**hand-maintained C**: it carries the ownership, threading, and panic-safety
contract block, which code generation would drop (cbindgen's default output
is C++, not C). When an `extern "C"` function is added or changed, update
the header declaration in the same change and keep it compiling as plain C
(`cc -fsyntax-only -x c`). Result cells cross the boundary through the
typed value walker (`relate_value_at` + the `relate_value_*`
accessors) and parameters through the typed params builder
(`relate_params_*`); the four JSON **metadata** strings (`stats`,
`notifications`, `plan_json`, and the error shape) remain JSON.

The complete symbol table, per-kind accessor list and putter list are the
contract in [`docs/api/c/spec.md`](../api/c/spec.md) — this page summarizes
the contracts that govern every call.

## Contracts

Four contracts govern **every** call. They are stated in the header comment
and enforced in the implementation.

### Memory ownership

The return type is the rule: mutable `char *` is caller-owned, `const char *`
is owned by Relate.

- `relate_database_close` / `relate_disconnect` /
  `relate_destroy_result` free the handles (only `destroy_result`
  accepts NULL; the other two return `RELATE_ERROR` on NULL).
- Every `relate_value` handle and every string a value accessor or
  result accessor returns is **owned by the result** and dies at
  `relate_destroy_result` — never freed, never used after it.
- The `char **error_json` out-param strings and the `char *` bookmark
  `relate_conn_commit`'s `bookmark_out` hands over are
  **caller-owned** — free with `relate_free_string()`.
- A params builder root is caller-owned (`relate_params_free`); its
  children are owned by the root. Strings and arrays passed to a putter are
  copied.

### Error channel

One JSON shape — `{"code","gql_status","stage","retryable","message"}` —
travels with the call that failed (there is no thread-local slot, so the
channel is async-safe). A statement failure rides its **result**
(`relate_result_is_error` / `relate_result_error_json`, result-owned)
even though `relate_query_params` returned `RELATE_OK`; every other
failure fills the call's own trailing `char **error_json` out-param
(caller-owned). Pass NULL for the out-param to opt out of the detail —
the status code alone then reports the failure.

### Threading

A `relate_database` may be shared across threads; a
`relate_connection` must **not** be — all calls that share a connection
are serialized by the caller (one at a time). Distinct connections may be
used from distinct threads. The same one-at-a-time rule applies to a params
builder.

### Panic safety and handle poisoning

Every entry point catches Rust panics at the FFI boundary (an unwind
crossing into C is undefined behavior); argument decoding happens *inside*
the guard too. If a call panics, the handle is **poisoned** — it is not
freed, but every subsequent call on it returns a defined failure
(`RELATE_ERROR` with the `internal`-stage error shape, or a `0`/`NULL`
result accessor) instead of touching the possibly-inconsistent engine.
Recover by closing the handle and opening a fresh one; close and destroy
still free a poisoned handle.

## Functions

`relate-ffi` exposes the version symbol, one free function, the
database/connection/statement set, the result accessors, the value walker,
and the params builder. The complete table with signatures, ownership and
refusal conventions is
[`docs/api/c/spec.md`](../api/c/spec.md#symbol-table); grouped:

- **Version / free** — `relate_abi_version`, `relate_free_string`
- **Database / connection** — `relate_database_open`,
  `relate_database_open_read_only`, `relate_database_close`,
  `relate_connect_namespace`, `relate_conn_set_namespace`,
  `relate_conn_set_statement_timeout`,
  `relate_disconnect`, `relate_checkpoint`
- **Statement / transaction** — `relate_query_params`,
  `relate_conn_begin`, `relate_conn_commit` (the bookmark rides its
  `bookmark_out` out-param), `relate_conn_rollback`,
  `relate_destroy_result`
- **Result shape / metadata** — `relate_column_count`,
  `relate_row_count`, `relate_column_name`,
  `relate_result_is_error`, `relate_result_error_json`,
  `relate_result_bookmark`, `relate_result_query_type`,
  `relate_result_namespace`, `relate_result_stats`,
  `relate_result_notifications`, `relate_result_plan_json`,
  `relate_result_elapsed_ms` / `_parse_ms` / `_plan_ms` / `_execute_ms`
- **Value walker** — `relate_value_at`, `relate_value_kind`
  (`enum relate_value_kind`, 18 kinds), and the per-kind accessors
  (`_bool`, `_int`, `_float`, `_is_nan`, `_is_inf`, `_string`,
  `_element_id`, `_start_element_id`, `_end_element_id`, `_list_len`,
  `_list_get`, `_map_len`, `_map_key`, `_map_get`, the temporal components,
  `_duration`, `_point`, `_vector_dtype`, `_vector_dim`, `_vector_float`,
  `_vector_int`)
- **Params builder** — `relate_params_new`, `relate_params_free`,
  and the putters (`_put_null`, `_bool`, `_int`, `_float`, `_put_string`,
  the temporal putters, `_put_duration`, `_put_point`, `_put_vector_float`,
  `_put_vector_int`, `_put_list`, `_put_map`)

## Example

```c
#include "relate.h"
#include <stdio.h>

int main(void) {
    if (relate_abi_version() != 2) return 1;
    relate_database db = NULL;
    relate_connection conn = NULL;
    relate_query_result r = NULL;
    char *err = NULL;

    if (relate_database_open(":memory:", &db, &err) != RELATE_OK) return 1;
    if (relate_connect_namespace(db, NULL, &conn, &err) != RELATE_OK) return 1;
    if (relate_query_params(conn, "CREATE (:Person {name: 'Alice', age: 30})", NULL, &r, &err) == RELATE_OK
            && !relate_result_is_error(r)) {
        printf("bookmark: %s\n", relate_result_bookmark(r)); /* result-owned */
        relate_destroy_result(r);
    }
    relate_params params = relate_params_new();
    if (relate_params_put_int(params, "min", 18, &err) != RELATE_OK) return 1;
    if (relate_query_params(conn, "MATCH (p:Person) WHERE p.age >= $min RETURN p.name AS name",
                                params, &r, &err) == RELATE_OK) {
        if (relate_result_is_error(r)) {
            fprintf(stderr, "%s\n", relate_result_error_json(r));
        } else {
            size_t len = 0;
            const char *name = relate_value_string(relate_value_at(r, 0, 0), &len);
            printf("%.*s\n", (int)len, name);
        }
        relate_destroy_result(r);
    }
    relate_params_free(params);
    relate_disconnect(conn);
    relate_database_close(db);
    return 0;
}
```

Link against the static or dynamic library built by `cargo build -p
relate-ffi` (`librelate_ffi.a` / `.dylib` / `.so`) and add the
header directory to your include path. Compiled, runnable samples for every
part of the contract live in
[`docs/api/c/examples/`](../api/c/examples/) — CI compiles and runs each
one (`scripts/ffi-compile-example.sh`).

Full contract: [`docs/api/c/spec.md`](../api/c/spec.md).

## See also

- [JavaScript](javascript-client.md), [Java](java-client.md), [Go](go-client.md) — clients over this ABI.
- [Bolt Server](bolt-server.md) — the remote transport, for stock Neo4j drivers.

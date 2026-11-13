# C ABI: librelate_ffi (v2)

## Install

```bash
git clone https://github.com/relatedb/relate && cargo build -p relate-ffi
```

Produces `target/{debug,release}/librelate_ffi.{a,dylib,so}` (Windows:
`relate_ffi.dll`) plus the hand-maintained header
`surfaces/relate-ffi/include/relate.h` — the contract's source of
truth. Link with `-I surfaces/relate-ffi/include … -L target/release
-lrelate_ffi`, and put the library directory on `DYLD_LIBRARY_PATH` /
`LD_LIBRARY_PATH` (or set `RELATE_LIB` for the language clients). No
published package: C embedders and every language client (JS/Java/Go/Swift
over JNA/koffi/cgo/XCFramework) build from a checkout.

The ABI is **version 2** (ADR-0502, the typed ABI): check
`relate_abi_version() == 2` at startup and fail loudly otherwise.

## Minimal usage

```c
#include "relate.h"

relate_database db = NULL;
relate_connection conn = NULL;
relate_query_result r = NULL;
char *err = NULL;                                  /* out-param errors start NULL */

relate_database_open(":memory:", &db, &err);   /* or "./mydata" / "./task.rl" */
relate_connect_namespace(db, NULL, &conn, &err);   /* NULL name = neo4j */

relate_params p = relate_params_new();
relate_params_put_int(p, "min", 18, &err);
if (relate_query_params(conn, "MATCH (n:Person) WHERE n.age >= $min RETURN n.name AS name",
                            p, &r, &err) == RELATE_OK
        && !relate_result_is_error(r)) {
    size_t len = 0;
    const char *name = relate_value_string(relate_value_at(r, 0, 0), &len);
    /* … use name/len … */
    relate_destroy_result(r);
}
relate_params_free(p);
relate_disconnect(conn);
relate_database_close(db);
```

Read cells through the typed value walker (`relate_value_at` +
`relate_value_kind` + per-kind accessors); bind parameters through the
`relate_params_*` builder. Bookmarks: `relate_result_bookmark` after
an auto-commit write, `relate_conn_commit`'s `bookmark_out` for an
explicit transaction — `"relate:<n>"`, the engine's commit sequence.

## Gotchas

- **A failing statement still returns `RELATE_OK`** — the failure rides
  the result (`relate_result_is_error` / `relate_result_error_json`).
  Always test `is_error` before reading cells; checking only the status
  treats `RETURN 1/0` as a successful empty result.
- **One error shape, two channels** — statement failures are result-owned
  (never freed); every other failure fills the call's trailing
  `char **error_json` out-param (caller-owned, free with
  `relate_free_string`). Parse it once and map `retryable` to your
  transient class; never sniff message text. No thread-local slot, so the
  channel is async-safe.
- **`relate_conn_commit` reports through its status** —
  `RELATE_OK` + non-NULL `*bookmark_out` = committed (caller-owned,
  free with `relate_free_string`); `RELATE_OK` + NULL = committed
  nothing; `RELATE_ERROR` = failed (`*error_json` carries the shape).
  The `bookmark_out` out-param must not be NULL — a NULL is a refused
  call that does **not** commit. Initialize every out-param to NULL
  before the call.
- **Return type = ownership** — `const char *` is result-owned (valid until
  `relate_destroy_result`, never freed); mutable `char *` is
  caller-owned. `relate_value` handles have no per-value free: the
  result owns them all. Copy anything you keep before destroying.
- **Read string lengths** — `relate_value_string(v, &len)` /
  `_map_key` return NUL-terminated UTF-8 *and* a byte length; a Cypher
  string can contain an interior NUL, which `strlen` truncates.
- **Cypher `null` is a `RELATE_V_NULL` handle, not a NULL pointer** — a
  NULL handle means "no such value" (out of range, absent map key). Keep
  the two apart.
- **Out-of-kind access is a defined refusal** (NULL / -1 /
  `RELATE_ERROR`, out-params untouched) — never undefined behaviour,
  never poisoning. A params-putter refusal likewise leaves the builder
  usable.
- **One connection, one thread at a time** — a `relate_database` may be
  shared across threads; a `relate_connection` (and a params builder)
  must not. A caught panic poisons the handle: close it and open a new one.
- **`system_ontologies` is visible only under `RELATE_PREVIEW=ontology`
  (ADR-0593)** — without the opt-in the connect fails "Graph not found:
  system_ontologies" and `SHOW DATABASES` lists two rows; with it, statements
  routed there fail as error results (42N17 text) — use the `db.ontology.*`
  procedures on `neo4j` instead. Unknown names fail "Graph not found".

Full contract: `docs/api/c/spec.md` (symbol table, ownership, walker, params
builder, bookmarks, threading, panic safety).

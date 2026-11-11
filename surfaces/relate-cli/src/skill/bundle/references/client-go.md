# Go client: github.com/relatedb/relate/clients/go

## Install

```bash
go get github.com/relatedb/relate/clients/go   # import relate
```

Go 1.22+; cgo over the C ABI (`librelate_ffi`, ABI version 2, ADR-0502),
so `CGO_ENABLED=1` and a C toolchain are required. The module is not on a
proxy yet; until the first tagged release, build from a repo checkout: clone
the repository, `cargo build --release -p relate-ffi`, then `go build`
from `clients/go` — the cgo directives search the checkout's `target/debug`
and `target/release` for the library.

## Minimal usage

```go
import "github.com/relatedb/relate/clients/go"

db, _ := relate.Open(":memory:")            // or "./mydata" / "./task.rl"
defer db.Close()
conn, _ := db.Connect()                         // or db.ConnectNamespace("system")
res, _ := conn.QueryParams("MATCH (n:Person {name: $name}) RETURN n",
    map[string]any{"name": "Ada"})
n := res.Rows[0][0].(relate.Node)           // typed values, never JSON
n.ElementID                                     // elementId(n)
n.ID                                            // id(n) — deprecated (ADR-0506)
res.QueryType                                   // typed QueryType — "r" | "w" | "rw" | "s" (ADR-0162)
res.Namespace                                   // typed Namespace — resolved (ADR-0117)
res.Bookmark                                    // "relate:<n>" after a write
res.Stats                                       // typed Stats — the 11 mutation counters, zeros included (ADR-0216)
res.Plan                                        // *PlanDocument — EXPLAIN/PROFILE
res.Notifications                               // [] on a plain run; Severity / Category are typed, closed vocabularies

_ = conn.Begin()                                // explicit transaction
bookmark, _ := conn.Commit()                    // "" when nothing committed
```

## Gotchas

- **Metadata vocabularies are closed** — a `QueryType`, `Namespace`, notification
  `Severity` or `Category` (or error stage) outside the documented set is an
  `ABI contract violated: ...` error, like the Java and Swift clients. The
  offending value echoed into the message is sanitized — non-printable
  characters arrive escaped and the value is bounded.
- **The library must be findable** — the cgo directives search the checkout's
  `target/debug` and `target/release`; outside the build tree set
  `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux) to the directory
  holding `librelate_ffi.dylib` / `.so`. On macOS the linker needs the
  `.dylib` — a Linux `.so` left in `target/debug` fails with
  `unknown file type`.
- **Every value is typed** — `int64` (lossless, 2^53+1 round-trips),
  `string` (interior NULs kept), `[]any` / `map[string]any`, `Node` /
  `Relationship` (element ids, labels / type, properties), `Path`, temporals
  as component structs (not ISO strings), `Point`, `Vector`, `Duration`.
  Entities also carry the deprecated numeric ids: `Node.ID`, `Relationship.ID`
  / `StartID` / `EndID` are exactly `id(n)`, `id(r)`, `id(startNode(r))`,
  `id(endNode(r))` (ADR-0506); prefer the element ids for lookups.
- **Parameters are typed, not JSON** — the params builder binds signed and
  unsigned integers, floats, `bool`, `string`, slices / arrays,
  `map[string]T` (keys sorted) and structs by `json` tag; an unsupported
  type is refused before the statement runs with
  `parameter "<name>": unsupported parameter type T`. Never concatenate
  untrusted input into the statement text.
- **Errors are typed, never message-sniffed** — engine and boundary failures
  are `*relate.Error` with `Code`, `GQLStatus`, `Stage`
  (`compile` / `runtime` / `internal`) and `Retryable`; retry a unit of work
  exactly when `Retryable` (the write-write conflict). A failed `Commit`
  surfaces the conflict, so a retry loop checks both `Query`/`QueryParams`
  and `Commit`.
- **Close what you open** — `Connection.Close` is an implicit rollback;
  without `Database.Close` a file store cannot be reopened in the same
  process. One writer process per store; `OpenReadOnly` readers are
  unlimited; `:memory:` vanishes on close.

## Spec

`docs/api/go/spec.md` (types, value mapping, errors, parameters, namespaces,
library build); runnable examples in `docs/api/go/examples`.

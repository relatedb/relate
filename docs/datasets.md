# Dataset suites

Two graphs for scale tests. Load with the Rust API
(`relate-datasets`) or the JavaScript / Go C ABI clients; do not hit the
network in default CI.

## Financial fraud

Seeded synthetic generator. Full size: 10,000 accounts, 1–3 accounts per
customer, 10–1000 `TRANSFER` edges per account (log-uniform). CI uses
`scale=0.05` (~500 accounts) with the same planted patterns.

```bash
cargo run -p relate-datasets --bin fraud-generate -- --scale 0.05 --seed 42
```

Prints account/transfer counts and planted pattern ids (does not load a
database). `--help` lists the same flags.


| Flag            | Default | Meaning                                                |
| --------------- | ------- | ------------------------------------------------------ |
| `--scale <F64>` | `0.05`  | Size vs full generator (`1.0` = 10,000 accounts)       |
| `--seed <U64>`  | `42`    | Generator RNG; planted ids are stable for a given seed |


```bash
# full graph in tests (slow)
RELATE_FULL_DATA=1 cargo test -p relate-datasets --test fraud -- --ignored
```


| Env                    | Meaning                                          |
| ---------------------- | ------------------------------------------------ |
| `RELATE_FULL_DATA=1` | Run ignored full-scale (`scale=1.0`) fraud tests |




### Graph

- `(:Customer {id, name})`
- `(:Account {id, planted})` — `planted` is `fanout`, `cycle`, or null
- `(Account)-[:OWNED_BY]->(Customer)`
- `(Account)-[:TRANSFER {id, amount, ts}]->(Account)`

Planted pattern ids (stable for a given seed):

- **Fan-out:** account `fanout-src` pays `fanout_k` distinct destinations
- **Circular:** accounts `cycle-0` … `cycle-n-1` form a directed cycle

```cypher
MATCH (a:Account {id: $id})-[:TRANSFER]->(b)
WITH a, count(DISTINCT b) AS n
WHERE n >= $k
RETURN a.id, n

MATCH (a:Account {planted: 'cycle'})-[:TRANSFER*2..6]->(a)
RETURN a.id
```



## Fraud checkpoint size

Load the fraud graph, write a Parquet checkpoint, then print three sizes:
live **logical** graph + indexes (ADR-0042), process **RSS**, and **data
files** (`manifest.json` + Parquet parts; WAL is excluded).

```bash
cargo run -p relate-datasets --bin fraud-checkpoint -- \
  --uri ./target/fraud-checkpoint --reset --scale 0.10
```

```bash
make fraud-checkpoint
cargo run -p relate-datasets --bin fraud-checkpoint -- --help
```


| Flag                    | Default                         | Meaning                                                                                            |
| ----------------------- | ------------------------------- | -------------------------------------------------------------------------------------------------- |
| `--uri <URI>`           | `./target/fraud-checkpoint`     | `:memory:`, filesystem path, or `file://`. `:memory:` checkpoints into the in-process store; `files` is still that snapshot’s object size |
| `--scale <F64>`         | `0.05`                          | Graph size vs full generator (`1.0` = 10,000 accounts)                                             |
| `--graph-seed <U64>`    | `42`                            | RNG for the synthetic fraud graph                                                                  |
| `--fanout-k <N>`        | `12`                            | Planted mule out-degree (`fanout-src` destinations)                                                |
| `--cycle-len <N>`       | `4`                             | Planted directed cycle length (`cycle-0` …)                                                        |
| `--wal-dir <PATH>`      | unset                           | Local WAL directory when `--uri` is a filesystem path (ignored for `:memory:`)          |
| `--reset`               | off                             | Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:` |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index logical cap (ADR-0042). Same `SIZE` grammar as `fraud-loadtest`                 |
| `--out <PATH>`          | unset                           | Optional JSON report (`logical_used_bytes`, `rss_bytes`, `data_file_bytes`, graph counts)          |


`make fraud-checkpoint` uses `--reset` and `SCALE` / `GRAPH_SEED` / `FANOUT_K` / `CYCLE_LEN` / `URI_FRAUD_CP` / `OUT_FRAUD_CP`.

Stdout:

```
graph nodes N  accounts A  transfers T
logical X  rss Y  files Z
```


## Fraud load tests

Mixed **read and write** use cases on the fraud graph (payments plus AML).
Each use-case invocation is one timed transaction (one or more Cypher
statements). `--workers` opens that many connections and issues ops
concurrently. Independent waiting writes share one WAL `sync_all` (ADR-0019).
Results go to a JSON report for later analysis.

Same invocation: `make fraud-loadtest` (override `SCALE`, `OPS`, `WORKERS`, `MEMORY_LIMIT`, `URI_FRAUD`, `ARGS`, …).
`make fraud-loadtest` / `make identity-loadtest` / `make fraud-checkpoint`
run in the ADR-0070 Docker baseline environment (2 vCPU / 2 GB) unless
`BASELINE_ENV=host`. Freeze and gate-compare only dockerized reports.

```bash
cargo run -p relate-datasets --bin fraud-loadtest -- \
  --uri ./fraud-lt --reset --scale 0.10 --ops 800 --warmup 8 --workers 8 \
  --out target/fraud-loadtest.json
```

```bash
cargo run -p relate-datasets --bin fraud-loadtest -- \
  --uri ./fraud-lt --reset --scale 0.10 --ops 800 --warmup 8 --workers 8 \
  --out target/fraud-loadtest.json
```

```bash
cargo run -p relate-datasets --bin fraud-loadtest -- --help
```

CI runs a smaller mix (`tests/loadtest.rs`). Full graph: `--scale 1.0`.


| Flag                    | Default                      | Meaning                                                                                            |
| ----------------------- | ---------------------------- | -------------------------------------------------------------------------------------------------- |
| `--uri <URI>`           | `:memory:`                   | Database URI (`:memory:`, path, `file://`)                                                |
| `--scale <F64>`         | `0.05`                       | Graph size vs full generator (`1.0` = 10,000 accounts or users)                          |
| `--graph-seed <U64>`    | `42`                         | RNG for the synthetic graph                                                              |
| `--workload-seed <U64>` | `7`                          | RNG for use-case mix and entity picks                                                    |
| `--fanout-k <N>`        | `12`                         | Planted mule out-degree (`fanout-src` destinations)                                                |
| `--cycle-len <N>`       | `4`                          | Planted directed cycle length (`cycle-0` …)                                                        |
| `--ops <N>`             | `80`                         | Measured operations after warmup                                                                   |
| `--warmup <N>`          | `8`                          | Unmeasured operations run first (not in latency summaries)                                         |
| `--out <PATH>`          | `target/fraud-loadtest.json` | Report JSON path                                                                                   |
| `--wal-dir <PATH>`      | unset                        | Local WAL directory when `--uri` is a filesystem path (ignored for `:memory:`)          |
| `--reset`               | off                          | Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:` |
| `--workers <N>`         | `1`                          | Concurrent connections issuing ops (`1` = serial). Independent waiting writes share one WAL barrier in `full` mode (ADR-0019) |
| `--write-fraction <F64>`        | `0.5`                        | Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only). Within each kind, catalog weights still apply |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index logical cap (ADR-0042). `SIZE` is an integer (bytes) or an integer plus `b`/`bytes`/`mb`/`mib`/`gb`/`gib`. `make fraud-loadtest` defaults to `MEMORY_LIMIT=20mib` |
| `--durability-mode full\|everysecond\|snapshot` | `full` | WAL durability for persistent `--uri` (ADR-0077). Ignored for `:memory:`. `everysecond` can lose ~1s of acknowledged writes; `snapshot` can lose writes since the last checkpoint. With the default `full`, each serial autocommit pays one WAL barrier (ADR-0019) — for one-off bulk loads, `everysecond` or `snapshot` removes the per-statement fsync floor; see [Bulk import](guides/import-csv.md#bulk-import-durability-and-batch-size) |


| Env                          | Meaning                                                                                          |
| ---------------------------- | ------------------------------------------------------------------------------------------------ |
| `RELATE_COMMIT_TIMING=1`   | After measured ops, print commit-step histograms to stderr (parse+plan, overlay execute, wal_lock wait, encode+write, `sync_all`, `apply_redo`). Graph load is excluded. |
| `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`      | Live graph + index cap (`SIZE`: bytes or `mb`/`mib`/`gb`/`gib`) when `--logical-graph-size-limit` is omitted. `0` or unset is unlimited |


### JavaScript and Go

Same mix and flags as the Rust binary, over the C ABI. The clients still emit
**report schema 5** (the Rust binary emits schema 6); both the JavaScript and
Go reporters use the unified schema-6 `related` key on `ops[]`/`usecases[]`
(the Go reporter's former schema-5 `fraud_related` / `iam_related` names were
collapsed into it).
`--wal-dir` is accepted for flag parity: the engine has no WAL directory
(`relate_database_open`), so it is ignored except that `--reset` removes a
set directory.
`--reset` deletes a filesystem URI with `rm -rf`. JavaScript runs ops on one
Node thread (`--workers` is flag parity only). Go uses one goroutine per
worker and rejects `--workers < 1` with an error (the Rust binary coerces it
to 1). The Go binary normalizes out-of-range knobs once at the config
boundary and records the normalized values in the report: `--scale` is
floored at `0.05`, `--fanout-k` clamped to `[3, ordinary accounts]`, and
`--cycle-len` to `[3, 6]`; a non-finite `--scale`, or one requesting more
than 10,000,000 graph entities (scale `1000`), is rejected. Neither Go
binary accepts positional arguments: unexpected operands fail with exit 2.

```bash
# from repo root; produces librelate_ffi.dylib on macOS (.so on Linux)
cargo build -p relate-ffi
export RELATE_LIB=$PWD/target/debug/librelate_ffi.dylib   # .so on Linux

cd clients/javascript
node bin/fraud-loadtest.js \
  --uri ../../fraud-lt --reset --scale 0.10 --ops 800 --warmup 8 \
  --out ../../target/fraud-loadtest.json

node bin/fraud-loadtest.js --help
```

```bash
# same lib as above (macOS must see a Mach-O .dylib, not a leftover Linux .so)
export CGO_LDFLAGS="-L$PWD/target/debug -lrelate_ffi"
export DYLD_LIBRARY_PATH=$PWD/target/debug   # LD_LIBRARY_PATH on Linux

cd clients/go
go run ./cmd/fraud-loadtest \
  --uri ../../fraud-lt --reset --scale 0.10 --ops 800 --warmup 8 --workers 8 \
  --out ../../target/fraud-loadtest.json

go run ./cmd/fraud-loadtest --help
```


| Flag                    | Default                      | Meaning                                                                                            |
| ----------------------- | ---------------------------- | -------------------------------------------------------------------------------------------------- |
| `--uri <URI>`           | `:memory:`                   | Database URI (`:memory:`, path, `file://`)                                                |
| `--scale <F64>`         | `0.05`                       | Graph size vs full generator (`1.0` = 10,000 accounts)                                             |
| `--graph-seed <N>`      | `42`                         | RNG for the synthetic fraud graph                                                                  |
| `--workload-seed <N>`   | `7`                          | RNG for use-case mix and account picks                                                             |
| `--fanout-k <N>`        | `12`                         | Planted mule out-degree (`fanout-src` destinations)                                                |
| `--cycle-len <N>`       | `4`                          | Planted directed cycle length (`cycle-0` …)                                                        |
| `--ops <N>`             | `80`                         | Measured operations after warmup (Go: negative rejected with an error)                             |
| `--warmup <N>`          | `8`                          | Unmeasured operations run first (not in latency summaries; Go: negative rejected)                  |
| `--out <PATH>`          | `target/fraud-loadtest.json` | Report JSON path                                                                                   |
| `--wal-dir <PATH>`      | unset                        | Accepted for flag parity; the engine has no WAL directory (a set `--wal-dir` is only removed by `--reset`) |
| `--reset`               | off                          | Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:` |
| `--workers <N>`         | `1`                          | Go: concurrent connections (`0` or negative is rejected with an error). JS: accepted; ops stay serial |
| `--write-fraction <F64>`        | `0.5`                        | Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only)                           |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Sets `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` before `relate_database_open` (same `SIZE` grammar as the Rust binary)       |
| `--help`                | off                          | Print the flag table and exit                                                                     |


CI smoke: `clients/javascript/loadtest/test.js` (`npm test`) and
`clients/go/loadtest` (`go test ./...`).


### Use cases


| id                   | kind  | fraud | What it models                          |
| -------------------- | ----- | ----- | --------------------------------------- |
| `p2p_transfer`       | write | no    | Customer A pays customer B              |
| `payroll_scatter`    | write | no    | One payer, several payees               |
| `onboard_customer`   | write | no    | New customer + account                  |
| `mule_scatter`       | write | yes   | Extra scatter from planted `fanout-src` |
| `layering_cycle`     | write | yes   | 3-account directed cycle                |
| `recent_transfers`   | read  | no    | Account activity feed (`ORDER BY ts`)   |
| `customer_portfolio` | read  | no    | Accounts for a customer                 |
| `outgoing_volume`    | read  | no    | Sum of outgoing amounts                 |
| `detect_fanout`      | read  | yes   | Out-degree of `fanout-src` vs threshold |
| `detect_cycle`       | read  | yes   | Circular TRANSFER paths                 |
| `activity_page`      | read  | no    | Paginated feed (`ORDER BY` / `SKIP`)    |
| `search_customer`    | read  | no    | `CONTAINS` a generated customer number  |
| `counterparties`     | read  | no    | Undirected `TRANSFER` neighbors         |
| `payment_path`       | read  | no    | Directed `shortestPath` (`TRANSFER*1..3`) |
| `amount_band`        | read  | no    | `CASE` on amount + `count`              |
| `flag_account`       | write | no    | `SET` compliance hold                   |
| `reverse_last_out`   | write | no    | `DELETE` latest outgoing transfer       |




### Report JSON

`schema_version` is `6`. One schema covers every workload; workload-specific
fields live in the nested `workload` object. Top-level fields:

- `name` — workload name (`fraud-mixed-payments` here)
- `started_at` / `finished_at` (RFC3339 UTC), unix-ms counterparts
- `duration_ms` — whole `run()` wall (generate + load + workers + finish snapshot)
- `load_duration_ms` — generate, reset, open, and graph load
- `worker_duration_ms` — warmup + measured worker ops
- `config` — uri, scale, seeds, ops, warmup, workers, write_fraction
- `workload` — the per-workload object:
  - `kind` — `fraud` or `identity`
  - `unit` — dataset scalar that per-unit memory ratios divide by (`accounts` / `users`)
  - `config` — generator knobs (`fanout_k` / `cycle_len` here; `nest_depth` / `group_k` for identity)
  - `planted` — pattern ids from the generator
  - `dataset.start` / `dataset.finish` — node, relationship, and per-dataset counts (fraud: account, customer, transfer counts)
- `throughput` — measured use-case ops per **worker** wall-clock second (`ops_per_sec`, `writes_per_sec`, `reads_per_sec`) plus `write_ops` / `read_ops` counts. Denominator is `worker_duration_ms`, not load. Warmup is excluded from the count. A payroll scatter is one write op (several Cypher statements).
- `memory` — process RSS (bytes) and per-entity cost. RSS is **not** the `--logical-graph-size-limit` meter (ADR-0042):
  - `after_open` — after `Database::open`, before loading the generated graph
  - `start` / `finish` — after load and after measured ops; each has `rss_bytes`, `peak_rss_bytes`, and `rss_per_{node,relationship,entity,unit}` (process RSS / live counts; includes runtime, so compare across `--scale`)
  - `graph` — `start.rss - after_open.rss` and the same ratios on that delta (`bytes_per_unit` divides by `workload.unit`; the graph RSS is meaningful when the URI was empty at open, e.g. `:memory:` or `--reset`)
  - `workload_delta` — RSS and entity change from `start` to `finish` (`rss_bytes_per_new_entity` is 0 if the graph did not grow)
  - `logical_used_start_bytes` / `logical_used_bytes` / `logical_limit_bytes` — live graph + index logical size after load and after ops (ADR-0042). `logical_limit_bytes` is omitted/`null` when unlimited. Growing writes that would pass this cap fail with `capacity exceeded`
- `usecases[]` — per-use-case op counts, errors, `latency_ns` (min/max/mean/p50/p95/p99) on **measured** ops; `related` flags ops tied to the workload's planted theme (AML/fraud here)
- `environment` — sidecar environment class (ADR-0071; not a schema bump): recipe fingerprint (`class`, `os`/`arch`, image pins, CFS/memory, ops/warmup/workers, `runner`). Gate wall-clock metrics require this object to match the freeze.
- `ops[]` — every warmup and measured transaction (`latency_ns`, `ok`, `related`, `kind`, `worker`)
- `engine` / `engine_version` — `"relate"` plus the crate version for this binary

Schema 6 replaced the forked schema-5 pair (which shared `schema_version` 5
with different field names). Moves: `fraud_related` / `iam_related` →
`related`; `rss_per_account` / `rss_per_user` → `rss_per_unit`;
`bytes_per_account` / `bytes_per_user` → `bytes_per_unit`; `planted`,
`dataset`, and the workload knobs moved under `workload`.
`scripts/compare-loadtest.py` rejects a baseline/current pair whose
`schema_version` or `workload.kind` differ.

### Query API pull client (`relate-load`)

`relate pull` reads Neo4j / Aura over HTTP Query API v2 through the
`relate-load` surface (ADR-0080). Credentials come from the `--from` env
file or, when it is omitted, the process environment:

| Env              | Meaning                                                             |
| ---------------- | ------------------------------------------------------------------- |
| `NEO4J_URI`      | Source Query API origin. Required                                   |
| `NEO4J_USERNAME` | Source username. Required (`NEO4J_USER` is accepted as an alias)    |
| `NEO4J_PASSWORD` | Source password. Required                                           |
| `NEO4J_DATABASE` | Source database name (default `neo4j`)                              |

`NEO4J_DATABASE` is interpolated raw into the Query API URL path
(`/db/{database}/query/v2`). It is validated at the config boundary, before any
request: it must be non-empty, at most 63 characters, and limited to
`[A-Za-z0-9._-]`. Any other value — including one containing `/`, `?`, or `#` —
is rejected with `LoadError::Env`; it is never percent-encoded or repaired. An
absent value keeps the `neo4j` default, but a present empty value is rejected.

## Enterprise identity

Seeded synthetic IAM / directory graph. Complements fraud: typed multi-hop
access, nested groups, `OPTIONAL MATCH`, `UNION`, `MERGE`, list properties,
`date` grants, `SET` / `DELETE` / `REMOVE`, and `shortestPath`. Full size:
10,000 users, groups ≈ users/20, nested 2–4 levels. CI uses `scale=0.05`
(~500 users) with the same planted patterns.

```bash
cargo run -p relate-datasets --bin identity-generate -- --scale 0.05 --seed 42
```

Prints user/group/membership counts and planted pattern ids (does not load a
database). `--help` lists the same flags.


| Flag            | Default | Meaning                                             |
| --------------- | ------- | --------------------------------------------------- |
| `--scale <F64>` | `0.05`  | Size vs full generator (`1.0` = 10,000 users)       |
| `--seed <U64>`  | `42`    | Generator RNG; planted ids are stable for a given seed |


```bash
# full graph in tests (slow)
RELATE_FULL_DATA=1 cargo test -p relate-datasets --test identity -- --ignored
```


| Env                    | Meaning                                             |
| ---------------------- | --------------------------------------------------- |
| `RELATE_FULL_DATA=1` | Run ignored full-scale (`scale=1.0`) identity tests |


### Graph

- `(:User {id, name, email, status, hired, emails, planted})` — `hired` is a `date`; `emails` is a list
- `(:Group {id, name})`, `(:Role {id, name})`, `(:Permission {id, name})`, `(:App {id, name})`, `(:Device {id})`
- `(User)-[:MEMBER_OF]->(Group)`, `(Group)-[:MEMBER_OF]->(Group)`
- `(User)-[:HAS_ROLE {until}]->(Role)`, `(Group)-[:HAS_ROLE]->(Role)`
- `(Role)-[:GRANTS]->(Permission)-[:ON]->(App)`
- `(User)-[:REPORTS_TO]->(User)`, `(User)-[:OWNS]->(Device)`

Planted pattern ids (stable for a given seed):

- **Nested admin:** user `nested-admin` is only in leaf `nest-0`; admin is on `nest-(depth-1)`
- **Orphan privileged:** user `orphan-priv` has `admin` and no `REPORTS_TO`
- **SoD:** user `sod-user` holds both `requester` and `approver`
- **Stale grant:** user `stale-grant` has `HAS_ROLE.until` = `2020-06-01`

```cypher
MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role {name: 'admin'})
RETURN DISTINCT u.id

MATCH (u:User {id: $id})
WHERE EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'requester'}) }
  AND EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'approver'}) }
RETURN u.id
```


## Identity load tests

Mixed **read and write** use cases on the identity graph (directory plus IAM).
Each use-case invocation is one timed transaction (one or more Cypher
statements). `--workers` opens that many connections and issues ops
concurrently. Independent waiting writes share one WAL `sync_all` (ADR-0019).
Results go to a JSON report for later analysis.

Same invocation: `make identity-loadtest` (override `MEMORY_LIMIT_IDENTITY`,
`WORKERS_IDENTITY`, `URI_IDENTITY`, …). The merge gate defaults are 40 MiB
and `:memory:` (ADR-0048) with four workers (ADR-0070 lowered the count from
eight for the two-CPU container). Runs in the ADR-0070 Docker baseline
environment unless `BASELINE_ENV=host`. Both suites: `make loadtest`.

```bash
cargo run -p relate-datasets --bin identity-loadtest -- \
  --uri ./identity-lt --reset --scale 0.10 --ops 800 --warmup 8 --workers 8 \
  --out target/identity-loadtest.json
```

```bash
cargo run -p relate-datasets --bin identity-loadtest -- --help
```

CI runs a smaller mix (`tests/identity_loadtest.rs`). Full graph: `--scale 1.0`.


| Flag                    | Default                         | Meaning                                                                                            |
| ----------------------- | ------------------------------- | -------------------------------------------------------------------------------------------------- |
| `--uri <URI>`           | `:memory:`                      | Database URI (`:memory:`, path, `file://`)                                                |
| `--scale <F64>`         | `0.05`                          | Graph size vs full generator (`1.0` = 10,000 accounts or users)                          |
| `--graph-seed <U64>`    | `42`                            | RNG for the synthetic graph                                                              |
| `--workload-seed <U64>` | `7`                             | RNG for use-case mix and entity picks                                                    |
| `--nest-depth <N>`      | `4`                             | Planted `MEMBER_OF` hops from leaf group to admin (`nest-0` …)                                     |
| `--group-k <N>`         | `12`                            | Users in the nested-admin leaf (`nested-admin` plus `leaf-*`)                                      |
| `--ops <N>`             | `80`                            | Measured operations after warmup                                                                   |
| `--warmup <N>`          | `8`                             | Unmeasured operations run first (not in latency summaries)                                         |
| `--out <PATH>`          | `target/identity-loadtest.json` | Report JSON path                                                                                   |
| `--wal-dir <PATH>`      | unset                           | Local WAL directory when `--uri` is a filesystem path (ignored for `:memory:`)          |
| `--reset`               | off                             | Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:` |
| `--workers <N>`         | `1`                             | Concurrent connections issuing ops (`1` = serial). Independent waiting writes share one WAL barrier in `full` mode (ADR-0019) |
| `--write-fraction <F64>`        | `0.5`                           | Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only). Within each kind, catalog weights still apply |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index logical cap (ADR-0042). `SIZE` is an integer (bytes) or an integer plus `b`/`bytes`/`mb`/`mib`/`gb`/`gib`. `make identity-loadtest` defaults to `MEMORY_LIMIT_IDENTITY=40mib` (ADR-0048) |
| `--durability-mode full\|everysecond\|snapshot` | `full` | WAL durability for persistent `--uri` (ADR-0077). Ignored for `:memory:`. With the default `full`, each serial autocommit pays one WAL barrier (ADR-0019) — for one-off bulk loads, `everysecond` or `snapshot` removes the per-statement fsync floor; see [Bulk import](guides/import-csv.md#bulk-import-durability-and-batch-size) |


| Env                          | Meaning                                                                                          |
| ---------------------------- | ------------------------------------------------------------------------------------------------ |
| `RELATE_COMMIT_TIMING=1`   | After measured ops, print commit-step histograms to stderr (parse+plan, overlay execute, wal_lock wait, encode+write, `sync_all`, `apply_redo`). Graph load is excluded. |
| `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`      | Live graph + index cap (`SIZE`: bytes or `mb`/`mib`/`gb`/`gib`) when `--logical-graph-size-limit` is omitted. `0` or unset is unlimited |


### JavaScript and Go

Same mix and flags as the Rust binary, over the C ABI. The clients still emit
**report schema 5** (the Rust binary emits schema 6); both the JavaScript and
Go reporters use the unified schema-6 `related` key on `ops[]`/`usecases[]`
(the Go reporter's former schema-5 `fraud_related` / `iam_related` names were
collapsed into it).
`--wal-dir` is accepted for flag parity: the engine has no WAL directory, so
it is ignored except that `--reset` removes a set directory. `--reset`
deletes a filesystem URI.
JavaScript is single-threaded; Go `--workers` are goroutines, and Go rejects
`--workers < 1` with an error (the Rust binary coerces it to 1). The Go
binary normalizes out-of-range knobs once at the config boundary and records
the normalized values in the report: `--scale` is floored at `0.05`,
`--nest-depth` clamped to `[2, 8]`, and `--group-k` floored at `3`; a
non-finite `--scale` is rejected. Neither Go binary accepts positional
arguments: unexpected operands fail with exit 2.

```bash
cargo build -p relate-ffi
export RELATE_LIB=$PWD/target/debug/librelate_ffi.dylib   # .so on Linux

cd clients/javascript
node bin/identity-loadtest.js \
  --uri ../../identity-lt --reset --scale 0.10 --ops 800 --warmup 8 \
  --out ../../target/identity-loadtest.json

node bin/identity-loadtest.js --help
```

```bash
export CGO_LDFLAGS="-L$PWD/target/debug -lrelate_ffi"
export DYLD_LIBRARY_PATH=$PWD/target/debug   # LD_LIBRARY_PATH on Linux

cd clients/go
go run ./cmd/identity-loadtest \
  --uri ../../identity-lt --reset --scale 0.10 --ops 800 --warmup 8 --workers 8 \
  --out ../../target/identity-loadtest.json

go run ./cmd/identity-loadtest --help
```


| Flag                    | Default                         | Meaning                                                                                            |
| ----------------------- | ------------------------------- | -------------------------------------------------------------------------------------------------- |
| `--uri <URI>`           | `:memory:`                      | Database URI (`:memory:`, path, `file://`)                                                |
| `--scale <F64>`         | `0.05`                          | Graph size vs full generator (`1.0` = 10,000 users)                                                |
| `--graph-seed <N>`      | `42`                            | RNG for the synthetic identity graph                                                               |
| `--workload-seed <N>`   | `7`                             | RNG for use-case mix and user picks                                                                |
| `--nest-depth <N>`      | `4`                             | Planted `MEMBER_OF` hops from leaf group to admin (`nest-0` …)                                     |
| `--group-k <N>`         | `12`                            | Users in the nested-admin leaf (`nested-admin` plus `leaf-*`)                                      |
| `--ops <N>`             | `80`                            | Measured operations after warmup (Go: negative rejected with an error)                               |
| `--warmup <N>`          | `8`                             | Unmeasured operations run first (not in latency summaries; Go: negative rejected)                     |
| `--out <PATH>`          | `target/identity-loadtest.json` | Report JSON path                                                                                   |
| `--wal-dir <PATH>`      | unset                           | Accepted for flag parity; the engine has no WAL directory (a set `--wal-dir` is only removed by `--reset`) |
| `--reset`               | off                             | Delete the existing database at `--uri` (and `--wal-dir` if set) before load. No-op for `:memory:` |
| `--workers <N>`         | `1`                             | Go: concurrent connections (`0` or negative is rejected with an error). JS: accepted; ops stay serial |
| `--write-fraction <F64>`        | `0.5`                           | Fraction of ops that are writes (`0.0` = reads only, `1.0` = writes only)                           |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Sets `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` before `relate_database_open` (same `SIZE` grammar as the Rust binary)       |
| `--help`                | off                             | Print the flag table and exit                                                                     |


CI smoke: `clients/javascript/loadtest/test.js` (`npm test`) and
`clients/go/loadtest` (`go test ./...`).


### Use cases


| id                     | kind  | IAM | What it models                                              |
| ---------------------- | ----- | --- | ----------------------------------------------------------- |
| `provision_user`       | write | no  | New user + `MEMBER_OF`                                      |
| `add_to_group`         | write | no  | `MERGE` an existing user into a group                       |
| `grant_role`           | write | no  | `CREATE` `HAS_ROLE` to viewer/editor                        |
| `attach_device`        | write | no  | New device + `OWNS`                                         |
| `nested_grant`         | write | yes | Extra `MEMBER_OF` into planted `nest-0`                     |
| `effective_access`     | read  | no  | Direct `HAS_ROLE` `UNION` inherited `MEMBER_OF*`            |
| `org_walk`             | read  | no  | `REPORTS_TO*` managers                                      |
| `optional_manager`     | read  | no  | `OPTIONAL MATCH` manager and device                         |
| `detect_nested_admin`  | read  | yes | Admin via nested groups from `nested-admin`                 |
| `detect_sod`           | read  | yes | `EXISTS` requester and approver on `sod-user`               |
| `stale_grants`         | read  | yes | `HAS_ROLE.until` before a cutoff on planted `stale-grant`   |
| `search_directory`     | read  | no  | `User.email CONTAINS` a seeded `user-N` number              |
| `group_roster`         | read  | no  | Members `ORDER BY` / `SKIP` / `LIMIT`                       |
| `access_review`        | read  | no  | `CASE` on `User.status`                                     |
| `reporting_path`       | read  | no  | `shortestPath` over `REPORTS_TO`                            |
| `disable_user`         | write | no  | `SET` status (`CASE` toggle)                                |
| `add_alias`            | write | no  | `SET` list append on `emails`                               |
| `revoke_membership`    | write | no  | `DELETE` `MEMBER_OF`                                        |
| `detach_device`        | write | no  | `DELETE` `OWNS` and device                                  |
| `clear_expiry`         | write | yes | `REMOVE` `HAS_ROLE.until` on `stale-grant`                  |


### Report JSON

Same schema 6 as fraud (see **Report JSON** above). The per-workload
differences live in the `workload` object:

- `name` is `identity-mixed-iam`, `workload.kind` is `identity`, `workload.unit` is `users`
- `workload.config` is `nest_depth` / `group_k` instead of `fanout_k` / `cycle_len`
- `workload.dataset.start` / `workload.dataset.finish` — node, relationship, user, group, role, membership counts
- `related` on `usecases[]` / `ops[]` flags IAM-related ops (the identity planted theme)

## Bulk load tests

`bulk-loadtest` measures whole-graph bulk operations — one Cypher
statement, hence one commit, per repetition — across a ladder of batch
sizes (`--sizes`). `--sizes 1` is the statement-by-statement autocommit
import pattern: one row, one statement, one WAL barrier under the
default `full` durability, which is the fsync floor a bulk import pays
on real disks (the [bulk-import guide](guides/import-csv.md#bulk-import-durability-and-batch-size)
covers the user-facing recipes). Runs single-threaded by design; the
concurrent group-commit shape is the fraud/identity `--workers` lanes.

```bash
cargo run -p relate-datasets --release --bin bulk-loadtest -- --ops create --sizes 1,5000 --reps 2000
```

| Flag                    | Default                      | Meaning                                                                                            |
| ----------------------- | ---------------------------- | -------------------------------------------------------------------------------------------------- |
| `--ops <LIST>`          | every op                     | Comma-separated: `create`, `merge-with-constraint` (`merge`), `update` (`set`), `delete`           |
| `--sizes <LIST>`        | `5000,10000,50000,100000`    | Rows per statement (also `_` separators and a `k` suffix). `1` = the single-row autocommit pattern  |
| `--reps <N>`            | `1`                          | Measured repetitions per (operation, size); use a high value at small sizes for stable percentiles |
| `--warmup <N>`          | `0`                          | Unmeasured repetitions run first (each still gets its untimed setup)                               |
| `--dir <PATH>`          | `target/bulk-loadtest-data`  | Directory for the per-measurement databases (a fresh one per op and size), or `:memory:`           |
| `--logical-graph-size-limit <SIZE>` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index cap (ADR-0042). No cap by default: the bulk working set grows with batch size |
| `--durability-mode full\|everysecond\|snapshot` | `full` | WAL durability for persistent databases (ADR-0077). Ignored for `:memory:`. Under `full` every rep pays one WAL barrier — `everysecond` isolates the engine floor, `snapshot` the checkpoint-bound floor |
| `--sample <DIR>`        | off                          | Run every measurement's repetition loop under the in-process profiler (relate-test-support's, ADR-0244) and write `top.txt` / `folded.txt` / `flame.svg` per (op, size) under DIR. The profiler sees each rep's untimed setup statements too. Needs a unix build with this crate's `profiler` feature (default on); a build without it refuses the flag loudly |
| `--commit-timing`       | off                          | Record the engine's rendered commit-timing report — aggregate per-step percentiles (parse+plan, overlay execute, wal_lock wait, encode+write, `sync_all`, `apply_redo`) over every commit in the measurement, each rep's untimed setup statements included — into each report entry. A diagnostics dump for reading, not a schema-modeled structure (the row vocabulary is pinned by test). An in-memory database still records, with `sync_all` ≈ 0 |
| `--out <PATH>`          | `target/bulk-loadtest.json`  | Report JSON path (schema 3)                                                                        |

Report JSON: schema 3 — `config.durability_mode`, `config.commit_timing`,
`config.sample_dir`, and a per-entry `commit_timing` string (omitted when
`--commit-timing` is off).

The `bulk-loadtest` workflow dispatch (Actions tab) runs the ladder
across all three durability modes in the pinned ADR-0070 Docker
environment on the loadtest runner class and uploads the per-mode
reports as artifacts. Its dispatch inputs are validated against the
flags' character sets before use — they reach a command line twice
(once in the step, again through the Makefile's argument expansion). The
`sample` input runs the ladder under the in-process sampler and uploads
the stack profiles as a second artifact. Its measurement databases live on the runner's
real disk: the Docker recipe mounts `/tmp` as tmpfs, where a WAL
barrier is a memory write — a local `make bulk-loadtest` with the
default `--dir /tmp/...` therefore measures **no real fsync cost**;
point `--dir` at a disk-backed path when the barrier is the question.

## Benchmark harness: topology bytes + expansion throughput

Per-dataset baseline for the topology-compression experiments: **topology
logical bytes** and **expansion throughput** (hops/second) of the current
engine.

Datasets are existing stores opened **read-only** (`--db NAME=PATH`, or
`--graph-examples-dir DIR` to pick up every pulled store under a
graph-examples target directory — no Docker needed once the dumps have been
pulled) plus synthetic memory shapes built in-process from
`relate-datasets::shapes` (`--shape`).

Measurements per dataset:

- `nodes` / `rels` — `MATCH (n) RETURN count(n)` / `MATCH ()-[r]->() RETURN count(r)`.
- `total_logical_bytes` — `Database::memory_usage` on the dataset (live
  graph + indexes, ADR-0042; the same number the loadtests print).
- `topology_logical_bytes` — `memory_usage` of a *topology twin*: an
  in-memory database replaying every node (labels only) and every
  relationship (type and endpoints only) of the dataset, no properties, no
  indexes. The twin is built with the pull loader's batched
  `UNWIND $rows … CREATE (n:$(row.labels))` / `elementId` seek pattern
  (ADR-0155).
- `hops_per_second_1hop` — all-node 1-hop expansion
  (`MATCH (n)-[r]->(m) RETURN count(r)`); hops = traversed relationships.
- `hops_per_second_2hop` — 2-hop variable-length pass from a deterministic
  stride sample of seeds (`MATCH (n) WHERE id(n) % stride = 0 MATCH (n)-[*2]->(m)
  RETURN count(m)`); `stride = ceil(nodes / 4096)`, hops = 2 × counted paths.
- Each traversal runs one warm-up then `--rounds` timed rounds; the best
  round is reported.

The JSON report (stdout, and `--out` when given) carries `hardware` (CPU
model + OS/arch) and `repro_command` (the exact invocation).

```bash
cargo run --release -p relate-datasets --bin benchmark-harness -- \
  --graph-examples-dir testing/relate-graph-examples/target/graph-examples \
  --shape ring --shape fanout --rounds 3 \
  --out target/benchmark-harness-baseline.json
```

```bash
cargo run -p relate-datasets --bin benchmark-harness -- --help
```


| Flag                          | Default          | Meaning                                                                                  |
| ----------------------------- | ---------------- | ---------------------------------------------------------------------------------------- |
| `--db <NAME=PATH>`            | none (repeatable) | Existing store to benchmark under `NAME`; opened read-only                               |
| `--graph-examples-dir <DIR>`  | none (repeatable) | Benchmark every subdirectory of `DIR` that holds a `manifest.json`, named by directory   |
| `--shape <NAME>`              | `ring`, `fanout` when absent | Synthetic memory shape: `ring`, `fanout`, `flat`, or `dense-ring` (repeatable)  |
| `--shape-logical-mb <N>`      | `8`              | Logical graph memory per synthetic shape, in MB (1024×1024 bytes)                        |
| `--attributes <N>`            | `1`              | Integer properties on every synthetic node; the first is `id`                            |
| `--rounds <N>`                | `3`              | Timed rounds per traversal after one warm-up; the best round is reported                 |
| `--out <PATH>`                | unset            | Report JSON path; the table always prints to stdout                                      |


The harness honors no environment variables.

## Codec prototype: compressed adjacency

Compressed-adjacency codec prototype for the
topology-compression experiments. Encodes every adjacency chunk of each
dataset — one chunk per (node, rel type, direction), hops `{ts, rel_id}` in
the store's ts-DESC chunk order — with two schemes and reports encoded
bytes plus scalar and SIMD decode throughput against the recorded
benchmark-harness baseline. The SIMD decode variant (NEON on aarch64, a
scalar fallback elsewhere) is measured by default; `--simd false` gives
scalar-only numbers:

- `for128` — frame-of-reference bit-packing per 128-value id block
  (SIMD-BP128 shape, scalar); ts stream is first value raw + gaps
  bit-packed per 128-value block; a 1-bit-per-hop presence bitmap marks
  hops with an integer `ts`.
- `hybrid` — GDS-style packed tail: first id raw, zigzag deltas
  bit-packed per 64-value block (tolerates unsorted ids).

The engine's `TypedAdj` is crate-private, so hop streams are reconstructed
from one relationship scan (`id(r)`, endpoints, `type(r)`, integer `r.ts`),
grouped per (node, type, direction) and sorted with the store's
`cmp_ts_desc` order — chunk contents and order are byte-identical to the
live store. Every chunk roundtrips exactly (asserted in the bench).

Per dataset the report carries `hops`, `raw_hop_bytes` (16 B/hop),
`baseline_topology_bytes` / `baseline_1hop_hps` (recorded baseline, same
hardware, or the `--baseline` report lookup), `for128_bytes` /
`hybrid_bytes`, `percent_saved_*` against the baseline topology bytes,
`raw_ratio_*` (raw hop bytes / encoded), `scalar_decode_hps_*` (warm
best-of-`--rounds` decode of every chunk), `simd_decode_hps_*`,
`simd_vs_scalar_*` (the SIMD/scalar throughput ratio), and `verdict_*` (how
SIMD decode compares to scalar and to the live 1-hop baseline), plus
`hardware` and `repro_command`.

```bash
cargo run --release -p relate-datasets --bin codec-prototype -- \
  --graph-examples-dir testing/relate-graph-examples/target/graph-examples \
  --rounds 5 \
  --out target/codec-prototype.json
```

```bash
cargo run -p relate-datasets --bin codec-prototype -- --help
```


| Flag                          | Default           | Meaning                                                                                  |
| ----------------------------- | ----------------- | ---------------------------------------------------------------------------------------- |
| `--db <NAME=PATH>`            | none (repeatable) | Existing store to measure under `NAME`; opened read-only                                 |
| `--graph-examples-dir <DIR>`  | none (repeatable) | Measure every subdirectory of `DIR` that holds a `manifest.json`, named by directory     |
| `--shape <NAME>`              | `ring`, `fanout`, `dense-ring` when absent | Synthetic memory shape: `ring`, `fanout`, `flat`, or `dense-ring` (repeatable) |
| `--shape-logical-mb <N>`      | `8`               | Logical graph memory per synthetic shape, in MB (1024×1024 bytes)                        |
| `--attributes <N>`            | `1`               | Integer properties on every synthetic node; the first is `id`                            |
| `--rounds <N>`                | `5`               | Timed decode rounds per scheme after one warm-up; the best round is reported             |
| `--simd <true\|false>`        | `true`            | Also measure the SIMD decode variant (NEON on aarch64; scalar fallback elsewhere)        |
| `--baseline <PATH>`           | compiled-in table recorded 2026-10-11 | Baseline lookup JSON: a benchmark-harness `--out` report supplying (topology logical bytes, 1-hop hps) per dataset |
| `--out <PATH>`                | unset             | Report JSON path; the table always prints to stdout                                      |


The prototype honors no environment variables.

## Baseline gate (merge gate)

Frozen summaries under `baselines/` are a merge gate (ADR-0046).
`scripts/compare-loadtest.py` always fails when any use case has
`errors > 0` (absolute) or logical bytes exceed 110%. Throughput and p99
are compared only when baseline and current `environment` classes match
(ADR-0071): ops/s below 90% of baseline, or any p99 above max(115% of
baseline, baseline + the jitter allowance). The allowance is 250,000 ns by
default and 5 ms through `make compare-loadtests`, calibrated to the
baseline container (ADR-0070). In GitHub Actions a class mismatch is a
hard `INCOMPATIBLE` fail; locally ops/s and p99 are skipped. Missing
`environment` is incompatible. `scripts/compare-baselines.sh` compares the
in-tree `target/` reports against a baseline directory.

The comparator also renders a pair without gating: `--render-only
--markdown-out FILE` writes one baseline-vs-PR markdown table per
invocation and always exits 0 (no FAIL/INCOMPATIBLE diagnostics — the
per-row Status column carries the outcome instead). Fraud/identity pairs
render environment class, errors, ops/s, the worst-regression p99 with a
`<details>` per-use-case p99 breakdown, and logical bytes; memory-shape
pairs render per-shape logical-bytes and RSS-ratio rows; checkpoint pairs
render environment, `logical_used_bytes`, `rss_bytes`, and
`data_file_bytes`. Rows the gate would skip on an environment-class
mismatch read `skipped (INCOMPATIBLE)`; out-of-band rows read `fail
(…)`. A mismatched pair also carries a one-line explainer directly
under the environment-class mismatch blockquote — naming the rows its
kind devalues (wall-clock metrics for generic pairs, RSS-ratio rows for
memory-shape, informational sizes for checkpoint), why the runs are not
comparable (ADR-0071), and the re-freeze remedy — render output only;
the gate never prints it. Without `--render-only`, `--markdown-out FILE` writes the same
table as a sidecar while the gate's exit code and diagnostics stay
unchanged. `--markdown-out -` (or `--render-only` without
`--markdown-out`) prints the table to stdout. Report-derived free text —
use-case ids, environment classes and values, shape and index labels,
the schema/kind warnings — is HTML-escaped (`&`, `<`, `>`); a PR author
controls the report contents, and no raw markup from them reaches the
comment (the renderer's own `<details>`/`<summary>` literals and status
vocabulary are authored, not escaped).

Render mode also exposes the pair verdict machine-readably.
`--status-out FILE` writes one status token (one line, trailing
newline): `failing-rows` when any row-level failure renders (errors, a
breached band, a missing use case or shape), else `not-comparable` when
`schema_version` or `workload.kind` differ between the reports, else
`environment-mismatch` on an environment-class mismatch, else `ok` —
failing rows outrank everything. `--report-kind
generic|memory-shape|checkpoint` dispatches the table explicitly instead
of sniffing the report contents (the sniff checks both `workload.kind`
fields for `memory-shape`, then the checkpoint shape: no workload
metrics plus a top-level size field). Both flags are render-mode only:
without `--render-only` they are ignored, sidecar or not.

```bash
python3 scripts/compare-loadtest.py --render-only \
  --markdown-out target/compare-fraud.md \
  --status-out target/compare-fraud.status \
  --report-kind generic \
  baselines/fraud-loadtest.json target/fraud-loadtest.json
```

| Flag                     | Default                       | Meaning                                                                    |
| ------------------------ | ----------------------------- | -------------------------------------------------------------------------- |
| `<baseline>` `<current>` | (positionals)                 | Loadtest report JSON paths: the frozen baseline vs the current run         |
| `--render-only`          | off                           | Render the markdown comparison without gating: no FAIL/INCOMPATIBLE diagnostics, exit code always 0 |
| `--markdown-out FILE`    | stdout with `--render-only`; unset otherwise | Write the table to `FILE` (`-` for stdout); without `--render-only` it is a sidecar and the gate still runs |
| `--status-out FILE`      | unset                         | With `--render-only`: write the pair's status token — `ok`, `failing-rows`, `not-comparable`, `environment-mismatch` — to `FILE` as one line. Ignored without `--render-only` |
| `--report-kind KIND`     | sniffed from the reports      | With `--render-only`: render as `generic`, `memory-shape`, or `checkpoint`. Ignored without `--render-only` |

`scripts/compare-baselines.sh --render-only` — also `make
render-loadtest-comparison`, which sets the same `LOADTEST_P99_JITTER_NS`
bands as the gate — aggregates those per-pair tables into one document,
`target/loadtest-comparison.md`: a `# Loadtest baseline comparison` heading,
a producer-owned `**Status:**` line, a `**Pairs:**` summary line with one
`name: status` token per pair, then one `### <pair>` section per loadtest.
The pair set is declared ONCE, as the registry at the top of
`compare-baselines.sh` — one `file|report kind` record per
pair — iterated by the gate and the render step alike, so the two modes can
never disagree about membership (the registry order is the document order):

| Pair                    | Report kind    |
| ----------------------- | -------------- |
| `fraud-loadtest`        | `generic`      |
| `identity-loadtest`     | `generic`      |
Memory-shape and checkpoint are not registry members: the CI
`loadtest-baselines` job runs and reports only the fraud and identity
loadtests. Their local targets (`make memory-shape-loadtest`, `make
fraud-checkpoint`), frozen baselines, and comparator
kinds remain — re-entry is a future decision.

In render mode every pair is dispatched with its registry `--report-kind`
(no content sniffing) and classified from the comparator's `--status-out`
token — the aggregator never greps the rendered prose. Comparator tokens
map onto the `**Pairs:**` vocabulary: `ok` → `compared`, `failing-rows` →
`compared (failing rows)`, `environment-mismatch` → `compared (environment
mismatch)`, `not-comparable` → `compared (not comparable)`. The aggregator
authors three tokens itself, without a comparator run: `report-missing`,
`not-compared-no-baseline-frozen`, and `render-error` — a comparator exit
≠ 0 (its stderr is HTML-escaped into the note, so no raw `&`, `<`, `>` from
the error text reaches the document) or an unrecognized status token, which
fails loudly as a render error instead of silently classifying the pair as
healthy.

The `**Status:**` line is the document's authoritative overall verdict,
computed by the aggregator from the pair tokens: `ok` when every pair is
`compared` or `not-compared-no-baseline-frozen`; `warning` when any pair
token is `compared (failing rows)`, `compared (environment mismatch)`,
`compared (not comparable)`, `report-missing`, or `render-error`. Consumers
read this line instead of grepping `**Pairs:**` tokens, which stay in the
document for human readers. A pair without a current report renders a
`report-missing` note (a loadtest whose CI step failed — the render step
still runs); a pair without a frozen baseline renders a
`not-compared-no-baseline-frozen` note. Render mode never gates and
always exits 0. The document is appended to
`$GITHUB_STEP_SUMMARY` when set and printed to stdout otherwise.
`REPORTS_DIR` (default `<repo>/target`) relocates the reports and the
document — the aggregation tests drive it with fixture trees — and
`COMPARISON_OUT` overrides the document path.

`scripts/update-pr-comment.sh [<pr-number>] [--doc PATH]` posts that
document as ONE self-updating PR comment: it lists the PR's comments
(paginated, via `gh api` authenticated by `GH_TOKEN`) and PATCHes the
comment whose body carries the marker
`<!-- relate:loadtest-baselines -->` **and whose author is
`github-actions[bot]`** — the identity this job's `GITHUB_TOKEN` posts
as — in place, or POSTs exactly one comment when no bot-authored marker
comment exists; a rerun never adds a duplicate. The author filter is
load-bearing: a third-party comment that merely quotes the marker (a
maintainer debugging, or a squatter trying to capture the comparison) is
never edited by the bot and never suppresses the fresh bot comment. The
body starts with the marker, an overall status line read from the
document's authoritative `**Status:**` line — `ok` renders a note,
`warning` renders a warning, and a missing or unrecognized status line
is treated as warning (fail loud in presentation, never silently
healthy); the `**Pairs:**` tokens are never grepped — then the run URL
and commit SHA, then the document. The document path resolves like the
render step: `--doc` wins over `COMPARISON_OUT`, then
`$REPORTS_DIR/loadtest-comparison.md`. Without a PR number (positional
or `PR_NUMBER` — non-`pull_request` events) it no-ops with exit 0. The
comment is presentation, never a gate: `gh` missing,
`GITHUB_REPOSITORY` unset, or any `gh api` failure prints to stderr and
exits 0; only usage errors exit 2. A missing document posts a short
fallback body (marker included) saying the render step produced nothing.
GitHub caps comment bodies at 65,536 characters; an oversized document
fails the post through the same best-effort path.

```bash
# from the loadtest-baselines CI job (GH_TOKEN from secrets.GITHUB_TOKEN,
# GITHUB_* are the runner defaults there)
GH_TOKEN="$TOKEN" bash scripts/update-pr-comment.sh "$PR_NUMBER"
```

Fraud, identity, and checkpoint reports that are frozen or compared **must**
come from the ADR-0070 Docker baseline environment (`make fraud-loadtest`,
`make identity-loadtest`, `make fraud-checkpoint`, `make baseline-loadtests`:
compile in `rust:1.88-bookworm`, run in `debian:bookworm-slim` with two CPUs
of CFS bandwidth on a 10 ms period and a 2 GB cap, stores on tmpfs). The
freeze is a worst-of-`BASELINE_RUNS` (default 5) envelope, not a single run.
Repo-root `baselines/` is the only comparison set; committed envelopes come
from `.github/workflows/create-loadtest-baselines.yml` (`ubuntu-24.04`),
which opens a PR and never pushes to `main`.
Local `make baseline-loadtests` writes `target/` unless
`BASELINE_FREEZE_SOURCE=ci`. `BASELINE_ENV=host` is debug-only and is not
freezeable.

Re-freezing on a new runner class: the freeze workflow takes a `runner`
input (default `ubuntu-24.04`, the class the committed envelopes and the CI
compare gate are pinned to). When CI's `loadtest-baselines` job moves to a
new runner class, dispatch the workflow with that class's label; the chosen
label flows into `BASELINE_RUNNER`, so the new envelopes record the new
`github-actions:<label>` environment class and the PR body names it. The CI
compare hard-fails `INCOMPATIBLE` against old-class envelopes by design
(ADR-0071); merging the envelopes PR restores the gate. Only the GitHub
runner label is parameterized — the ADR-0070 loadtest environment itself
(Docker images, 2 vCPU / 2 GB container caps) is unchanged.

```bash
make fraud-loadtest      # Docker 2 vCPU / 2 GB (ADR-0070)
make identity-loadtest
make gate                # lint, tests, CIP, TCK, loadtest compare (root baselines/)
make compare-loadtests   # just the loadtest compare
make render-loadtest-comparison  # combined comparison markdown (never gates)
```

The CI `loadtest-baselines` job runs the fraud and identity loadtests
on `ubuntu-24.04` (memory-shape and checkpoint stay local targets,
not CI steps), uploads `target/fraud-loadtest.json` and
`target/identity-loadtest.json` as the `loadtest-reports` workflow artifact
(`if: always()`), then compares against root `baselines/` with ADR-0046
bands when the environment class matches.

Env vars the compare path honors:

| Env                              | Default            | Meaning |
| -------------------------------- | ------------------ | ------- |
| `BASELINES`                      | `<repo>/baselines` | Baseline directory `make compare-loadtests` / `compare-baselines.sh` read from |
| `REPORTS_DIR`                    | `<repo>/target`    | Current-report directory `compare-baselines.sh` reads; render mode also writes `loadtest-comparison.md` here |
| `COMPARISON_OUT`                 | `$REPORTS_DIR/loadtest-comparison.md` | Render-mode output document path |
| `PYTHON`                         | `python3`          | Interpreter for the compare/summarize scripts |
| `LOADTEST_MIN_THROUGHPUT_RATIO`  | `0.90`             | Minimum current/baseline `ops_per_sec` ratio |
| `LOADTEST_MAX_P99_RATIO`         | `1.15`             | Maximum current/baseline per-use-case p99 ratio |
| `LOADTEST_P99_JITTER_NS`         | `250000` (script); `5000000` via `make compare-loadtests` | Absolute p99 jitter allowance added to the baseline p99 (ADR-0070) |
| `LOADTEST_MAX_LOGICAL_BYTES_RATIO` | `1.10`           | Maximum current/baseline `memory.logical_used_bytes` ratio |
| `LOADTEST_MIN_OPS_FOR_P99`       | `8`                | Minimum measured ops in a use case before its p99 is compared |
| `PR_NUMBER`                      | (unset)           | Pull request `update-pr-comment.sh` posts to when no positional `<pr-number>` is given |
| `GITHUB_REPOSITORY`              | (unset; Actions default) | `owner/repo` addressing the GitHub API for the PR comment |
| `GITHUB_SERVER_URL`, `GITHUB_RUN_ID` | (Actions defaults) | Compose the run URL shown in the comment header |
| `GITHUB_SHA`                     | (Actions default)  | Commit SHA shown in the comment header |
| `GH_TOKEN`                       | (unset; Actions: `secrets.GITHUB_TOKEN`) | `gh` authentication for the PR-comment post |

Baseline-environment Make knobs (ADR-0070 / ADR-0071): `BASELINE_ENV` (`docker` /
`host`), `BASELINE_CPUS` (`2`), `BASELINE_CPU_PERIOD_US` (`10000`),
`BASELINE_CPU_QUOTA_US` (period x CPUs), `BASELINE_MEMORY` (`2g`),
`BASELINE_RUNS` (`5`), `BASELINE_BUILD_IMAGE` (`rust:1.88-bookworm`),
`BASELINE_RUN_IMAGE` (`debian:bookworm-slim`), `BASELINE_RUNNER`
(`github-actions:ubuntu-24.04` in CI, else `local`),
`BASELINE_FREEZE_SOURCE` (empty locally; `ci` writes repo `baselines/`).

The loadtest binaries' own `make` knobs (`SCALE`, `OPS`, `WORKERS`,
`MEMORY_LIMIT`, `MEMORY_LIMIT_IDENTITY`, `GRAPH_SEED`, `WORKLOAD_SEED`, …)
are listed with the `make fraud-loadtest` / `make identity-loadtest`
invocations above; the frozen baselines were generated at the Makefile
defaults, so the gate must run at those defaults too.

## Load test dashboard

Compare one or more `--out` JSON reports (fraud or identity, any language
client). Each report in a file is one run (object, array, concatenated JSON,
or `{ "runs": [] }`). Overlay throughput, latency by use case, memory RSS,
and a run-vs-run matrix.

```bash
cd apps/loadtest-dashboard
npm install
npm run dev
```

Open http://127.0.0.1:5173. The dev server lists `target/*.json` at the repo
root (override with `LOADTEST_WATCH_DIR`). Add the reports you want; they
reload when the load test rewrites the file. **Open files** / **Open folder**
and drag-and-drop work for reports outside `target/`. Chromium keeps file
handles and polls mtime; a dropped `File` is a snapshot until you add it
again.

Details: [`apps/loadtest-dashboard/README.md`](../apps/loadtest-dashboard/README.md).

## Profiling a load test

Profile the **in-memory** configuration (`--uri :memory:`) when looking for
code-level bottlenecks. A persistent-URI run spends most of its wall time in
`fsync`, which no engine change affects and which hides the CPU path.

Measured on the fraud load test at the same knobs (`--ops 2000 --warmup 200
--workers 8`):

| URI         | Throughput   | `mule_scatter` p50 |
| ----------- | ------------ | ------------------ |
| `:memory:`  | 16,393 ops/s | 0.59 ms            |
| on-disk     | 900 ops/s    | 24.7 ms            |

The on-disk profile is ~85% `__psynch_cvwait` + `__fcntl` (worker threads
parked in `CommitWaiter` while the group leader fsyncs). Group commit works
as specified (ADR-0019); a write p50 of ~10 ms is one shared fsync, not N
serialized ones.

### Build with symbols

Release builds strip debug info, so the profiler reports raw addresses:

```bash
cargo build --profile profiling -p relate-datasets --bins
```

### samply (macOS, no sudo, no Xcode)

```bash
samply record --save-only -o /tmp/fraud.json.gz \
  target/profiling/fraud-loadtest --uri :memory: --ops 6000 --warmup 400 --workers 8
samply load /tmp/fraud.json.gz     # prints a localhost URL; opens the Firefox Profiler UI
```

Symbolication happens in the UI only. The `.json.gz` written by `record
--save-only` has `symbolicated: false` and no function names. A profile
**exported from the UI** (Download) gains a top-level `shared` table
(`stringArray`, `funcTable`, `frameTable`, `stackTable`) with resolved
names. Notes for scripted analysis:

- Leaf/self-time counts are usable from `shared` directly (read
  `stackTable.frame` for each sample's stack index).
- `stackTable.prefixOffset` in the export is **not** a parent pointer —
  walked chains collapse to depth 2–7 with a constant root. Use the raw
  profile's own `stackTable.prefix` for caller attribution.
- `atos` resolves addresses for a local arm64 build with base `0x100000000`
  (the `__TEXT` vmaddr):
  `atos -arch arm64 -o target/profiling/fraud-loadtest $(printf '0x%x' $((0x100000000 + 0xADDR)))`.

### cargo-flamegraph (needs full Xcode)

On macOS `cargo-flamegraph` samples via `xctrace`, which requires Xcode.app
— Command Line Tools alone fails with `tool 'xctrace' requires Xcode`. In
0.6.14 there is no `--tracing-admin` flag and `--bin` does not take a path;
usage is `flamegraph -o out.svg -- <command>`. Use samply unless a
standalone SVG is required.

### Reading the result

Attribute the widest plates by crate first. On the fraud in-memory run the
front end is cheap (parser + front + planner ≈ 6% of samples); the cost sits
in the executor's property materialization path. See
[`docs/engine-architecture.md`](engine-architecture.md) for the layer map.


## Graph-examples differential (ADR-0144)

`testing/relate-graph-examples` runs the neo4j-graph-examples sandbox
walkthrough queries (17 datasets: movies, recommendations, northwind,
fraud-detection, pole, stackoverflow, twitter-v2, cybersecurity,
healthcare-analytics, fincen — plus the third wave: contact-tracing,
entity-resolution, legis-graph, network-management, twitch,
twitter-trolls, openstreetmap) **differentially**: each dataset's `data/<name>-50.dump` loads into a
`neo4j:2026.09-enterprise` docker database, the whole database is pulled into a
fresh `relate::Database` through the real `relate pull` binary (one
process: nodes, then rels carrying their endpoints, as a two-statement
`--query`; streaming rows materialize in batches, one checkpoint; ADR-0158), and
every `[source,cypher]` statement from the dataset's
`documentation/<name>.adoc` runs on both engines with the results compared
(rows as a bag, or positional with `ORDER BY`; entities compare by content,
never by id; floats with epsilon).

GDS (`gds.*`) queries are excluded; apoc queries are deferred behind
`RELATE_GRAPH_EXAMPLES_APOC=1` until the apoc work merges (the stock
reference image has no APOC, so the flag needs an APOC-carrying image first).
The opt-in gate is `RELATE_GRAPH_EXAMPLES=1` (the nightly workflow
[`graph-examples-nightly.yml`](../.github/workflows/graph-examples-nightly.yml)
sets it; regular `cargo test` never boots docker — and a gated run with
docker or `RELATE_BIN` missing fails loudly).

The run is **report-only** (ADR-0176): DIFF, order-only, skip and
inconclusive reference-timebox outcomes are reported (with stable
`dataset:adoc-line:stmt-hash` ids, so nightly reports stay diffable across
runs), never gated on. The run fails only on the infrastructure failures
listed in ADR-0176: a dataset setup failure (artifact fetch/sha mismatch,
database registration, pull, opening the pulled copy, or an adoc with zero
runnable statements), a reference-boot failure, or a pull-verification
count mismatch.

Local run:

```bash
cargo build --release -p relate-cli
RELATE_GRAPH_EXAMPLES=1 \
RELATE_BIN=target/release/relate \
cargo test --release -p relate-graph-examples --test differential -- --nocapture
```

Env vars:

| Var | Default | Meaning |
| --- | --- | --- |
| `RELATE_GRAPH_EXAMPLES` | unset (suite skips) | `1` runs the suite |
| `RELATE_BIN` | unset (suite skips with build hint) | path to the built `relate` binary the suite spawns for `pull` |
| `RELATE_GRAPH_EXAMPLES_CACHE` | `target/graph-examples` | dump/adoc cache dir (dumps are sha-pinned downloads, never committed) |
| `RELATE_GRAPH_EXAMPLES_APOC` | unset (apoc queries skip) | `1` also runs `apoc.*` queries differentially |
| `RELATE_GRAPH_EXAMPLES_DATASETS` | unset (all datasets except opt-ins) | comma-separated manifest names restricting the run (reference boot and query run) to those datasets, in manifest order; an unknown name fails before any docker command, naming the value and listing the valid names; naming an opt-in dataset always runs it (the explicit list is an explicit opt-in) |
| `RELATE_GRAPH_EXAMPLES_TWITCH` | unset (twitch skipped) | `1` includes the opt-in twitch dataset in the default run (465 MB dump, tens of seconds of pull/load) |
| `RELATE_GRAPH_EXAMPLES_CONCURRENCY` | `4` | datasets pulled/run in parallel; the default assumes a release build (the nightly workflow builds `--release`) — consider lowering for debug builds, where slower ingest under 4-way dataset concurrency can stall a pull's read loop past the server's read-stall window (~400-500 s) |
| `RELATE_GRAPH_EXAMPLES_PULL_TIMEOUT_SECS` | `900` | per-dataset `relate pull` budget; a slower pull is killed and recorded as a failure |
| `RELATE_GRAPH_EXAMPLES_QUERY_TIMEOUT_SECS` | `20` | per-statement budget on both engines; a slower reference statement is inconclusive (its abandoned transaction is terminated best-effort), a slower relate statement is a named `DIFF` via query cancellation |

### Pull scaling bench (LABS-155)

The `pull_rca` example needs no docker and takes no flags or env vars. It
first times the loader's per-statement Cypher shapes at increasing graph
sizes. It then runs the real loader (`relate_load::load_streaming`) over
loopback HTTP on synthetic whole-database pulls of 60,000 nodes + 40,000 rels
and 330,000 nodes + 100,000 rels, writing into a fresh on-disk database.
For each size it prints one line and panics if any element is missing:

```text
loader <nodes> nodes <rels> rels: <seconds> s (<micros> us/element) nodes_new=<n> rels_new=<m>
```

```bash
cargo run --release -p relate-graph-examples --example pull_rca
```

## Query latency: graph-benchmark-ldbc workload (ADR-0243)

`testing/relate-query-latency` runs the 30 Cypher queries of
[prrao87/graph-benchmark-ldbc](https://github.com/prrao87/graph-benchmark-ldbc)
(`c8eb87f`, MIT) verbatim against the LDBC SNB `CsvBasic-StringDateFormatter`
data set (CC BY 4.0), loaded with upstream's schema into `:memory:` (or a
reusable store). Every query's answer is checked against
`workloads/graph-benchmark-ldbc/expected-sf<SF>.json` with no type coercion, then the query is
timed: one untimed checked warmup, then rounds until both `--min-rounds` and
`--min-time` are reached, each under a cancellable `--timeout`. Each round
records wall time and the engine-reported statement elapsed (ADR-0221). This
is not an LDBC Benchmark; `NOTICE` in the crate has the attribution.

| Scale | Data | Load | Where it runs |
| --- | --- | --- | --- |
| SF0.1 | 19 MB archive; 327,588 nodes, 1,477,965 relationships | about 26 s (release) | locally and per PR |
| SF1 | 241 MB archive; 3,181,724 nodes, 17,256,038 relationships | about 9.5 min (release); 12 GB peak RSS, so 16 GB+ of RAM | locally only (ADR-0243) |

The data set is downloaded from `datasets.ldbcouncil.org` on first use,
verified against `workloads/graph-benchmark-ldbc/data.sha256` (a mismatch is discarded, never
extracted) and extracted with pure-Rust zstd and tar into the data cache
(`target/query-latency/data/`). The cache layout is the one the
relate-examples `fetch.sh` writes, so `RELATE_QUERY_LATENCY_DATA`
can point at that repo's `.cache/ldbc/`.

`expected-sf1.json` is upstream's, for upstream's verbatim text. Upstream
chose its literals on SF1, and at SF0.1 most do not exist, so SF0.1 runs 14
queries (q1-q6, q9, q10, q13, q16, q20, q21, q23, q27) with the literal
substitutions in `workloads/graph-benchmark-ldbc/literals-sf0.1.json` (ADR-0247): only
literals change, each with its reason, so every SF0.1 answer has the shape
of its SF1 answer. q7, q24, q25, q26 and q29 answer "nothing" at both scale
factors (upstream's negative checks) and run verbatim. `expected-sf0.1.json`
comes from one reference-engine run of those statements
(`./run.sh neo4j answers --sf 0.1 --literals <table>` in the upstream
benchmark repo);
its `_source` key holds the provenance and the table's sha256, and each
answer the sha256 of the statement that ran. Changing the table means
regenerating `expected-sf0.1.json`.

```bash
cargo run --release -p relate-query-latency -- fetch --sf 0.1
cargo run --release -p relate-query-latency -- run --sf 0.1
cargo run --release -p relate-query-latency -- run --sf 0.1 --qids 19,26 --min-time 5
cargo run --release -p relate-query-latency -- run --sf 1 --store target/query-latency/graph-benchmark-ldbc-sf1.rl
# where does q19's time go? (ADR-0244)
make query-latency-profile QID=19
cargo run --profile profiling -p relate-query-latency -- profile --qid 19 --seconds 10
# the gated tests: answers only (no latency judgement)
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test graph_benchmark_ldbc
RELATE_QUERY_LATENCY_SF1=1 cargo test --profile corpus -p relate-query-latency --test graph_benchmark_ldbc sf1
```

`query-latency fetch`:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--sf <SF>` | `0.1` | Scale factor: `0.1` or `1` |
| `--data <DIR>` | `RELATE_QUERY_LATENCY_DATA`, else `target/query-latency/data` | Data cache directory |

`query-latency run` (exit 0 when every query is `ok`, 1 when any is
`wrong`, `error` or `timeout`, 2 when the run could not happen):

| Flag | Default | Meaning |
| --- | --- | --- |
| `--sf <SF>` | `0.1` | Scale factor: `0.1` or `1` |
| `--qids <IDS>` | all 30 | Comma-separated query ids (`1`-`30`) |
| `--indexes <PROFILE>` | `upstream` | `upstream`: `ID` on all eight labels plus eight lookup indexes (`workloads/graph-benchmark-ldbc/indexes.cypher`); `text`: those plus three TEXT indexes (`text-indexes.cypher`) |
| `--store <PATH>` | unset (`:memory:`, loaded every run) | Reuse the store at `PATH`, opened read-only; when `PATH` does not exist, load into it (snapshot durability), checkpoint it, and mark it complete in `PATH.query-latency.json` (its scale factor, index profile and counts). Reuse requires that marker and a matching `--sf` and `--indexes`; a store without one (a build that failed or was killed) is refused, and a build that fails removes the store it created |
| `--min-rounds <N>` | `5` | Timed rounds per query, at least |
| `--min-time <SECS>` | `1` | Seconds of timed rounds per query, at least |
| `--timeout <SECS>` | `60` at SF0.1, `300` at SF1 | Per-execution timeout; a query past it is cancelled and recorded as `timeout` |
| `--out <PATH>` | `target/query-latency/report-graph-benchmark-ldbc-sf<SF>.json` | Report JSON path; the table always prints to stdout |
| `--data <DIR>` | `RELATE_QUERY_LATENCY_DATA`, else `target/query-latency/data` | Data cache directory |
| `--offline` | off | Fail instead of downloading a data set that is not cached |

`query-latency profile` (ADR-0244): load the graph, run the query once and
check its answer (a wrong answer fails the command), then loop it under a
sampling profiler. Exit 0 on success, 2 on any failure. Build it with the
`profiling` cargo profile (release code with line tables) so frames carry
symbols and source files; `make query-latency-profile QID=<id> [SF=0.1]
[SECONDS=10] [QUERY_LATENCY_PROFILE_ARGS=...]` does. The agent playbook is
[`docs/testing/query-latency.md`](testing/query-latency.md).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--qid <ID>` | required | The query to profile (`1`-`30`) |
| `--seconds <SECS>` | `10` | Seconds to loop the query; at least one iteration runs |
| `--frequency <HZ>` | `997` | Samples per second of CPU time |
| `--sampler <SAMPLER>` | `pprof` | `pprof`: in-process (unix, the default `sampling` feature); `none`: loop only, writing `summary.json`, so `samply` or `perf` can record the process |
| `--out <DIR>` | `target/query-latency/profile/graph-benchmark-ldbc-sf<SF>-q<NN>[-text]` | Output directory |
| `--sf`, `--indexes`, `--store`, `--data`, `--offline`, `--timeout` | as for `run` | The graph and the per-execution timeout, shared with `run` |

Outputs: `top.txt` (self time by group, inclusive time by engine crate,
the 40 heaviest functions by self and by inclusive time, each frame as
`name (source file)`), `folded.txt` (collapsed stacks, root first, for
inferno or speedscope), `flame.svg`, and `summary.json` (`schema` 1:
query, sampler, `sampling` (`frequency_hz` and `samples`; null with
`--sampler none`), iterations, engine-elapsed stats, git
state and where the graph came from). Frames are grouped by their source
file when the line tables name one (a workspace crate, `std/core/alloc`,
a crates.io crate; the global allocator's entry points as `allocator`),
else by their symbol name (`allocator`, `native`, `unattributed`). On
Linux, pprof-rs blocklists libc, libgcc, pthread and the vDSO, so samples
inside them (glibc `malloc` included) are dropped there.

| Env | Meaning |
| --- | --- |
| `RELATE_QUERY_LATENCY_DATA` | Data cache directory when `--data` is omitted |
| `RELATE_QUERY_LATENCY=1` | Runs the gated SF0.1 tests in `tests/graph_benchmark_ldbc.rs`, both index profiles (downloads SF0.1 on first use) |
| `RELATE_QUERY_LATENCY_SF1=1` | Runs the gated SF1 test in `tests/graph_benchmark_ldbc.rs` against upstream's answers. Local only: downloads 241 MB on first use, takes about 10 minutes and peaks at 12 GB RSS |
| `CARGO_TARGET_DIR` | When set, the work directory is `$CARGO_TARGET_DIR/query-latency` instead of the workspace's `target/query-latency` |

The report (`schema` 1) records the scale factor, index profile, engine
version, build, git commit and dirty flag, the run config, where the graph
came from (`graph`: `{"origin": "loaded", "store", "load"}` with seconds per
load step and per-label and per-type counts, or `{"origin": "reused",
"store", "open_seconds"}`), the process's peak RSS, and per query: `status`
(`ok`/`wrong`/`error`/`timeout`), `rows`, `rounds`, the statement's sha256,
`engine` and `wall` statistics (`min_ns`, `p50_ns`, `mean_ns`, `p99_ns`,
`max_ns`, nearest-rank) and the diff or error as `detail`.

## Pokec expansion profiling: `pokec_profile` (LABS-1116)

A dev tool (not CI-gated) in `relate-query-latency`: it runs the
traverse-bench pokec query groups against the synthetic pokec fixture as one
self-terminating process, printing wall time, total rows and a checksum per
group. The fixture is deterministic, so identical checksums before and after
a change are the correctness cross-check for a perf edit.

```bash
cargo run --profile profiling -p relate-query-latency --bin pokec_profile -- medium
cargo run --profile profiling -p relate-query-latency --bin pokec_profile -- medium \
  --group expansion_3 --sample target/pokec-prof
cargo run --profile profiling -p relate-query-latency --bin pokec_profile -- small \
  --group expansion_3 --profile
```

| Argument | Meaning |
| --- | --- |
| `small` \| `medium` (required, first) | Fixture shape: `small` = 10,000 users / 121,716 friendships (opens in seconds); `medium` = 100,000 users / 1,768,164 friendships. No default. |
| `--group NAME` | Run one group: `count`, `scan_agg_global`, `expansion_3`, `scan_expand_2hop_filter`. All groups when omitted; an unknown name is a usage error before the fixture opens. |
| `--sample DIR` | Run the selected groups under the in-process profiler (997 Hz, ADR-0244) and write `DIR/<group>/top.txt`, `folded.txt` and `flame.svg`, one directory per group. Conflicts with `--profile`; a failed round keeps its stacks. |
| `--profile` | Print the `PROFILE` result of the group's query at start id 1 instead of timing it. Needs `--group`. |
| `--help` | The generated usage and flag list. |

No environment variables beyond cargo's own. A bad argument exits 2 with the
generated usage error.

## QPP/LDBC corpus: PathDB, SNB BI, fraud-qpp, the UK railway graph and agg-stress (ADR-0479)

`query-latency corpus` (LABS-646) runs the corpora that live beside the
graph-benchmark-ldbc lane in `testing/relate-query-latency/workloads/`:

- **pathdb** — the 142 regular-path queries of
  [dbgutalca/pathdb-eval1](https://github.com/dbgutalca/pathdb-eval1)
  (`e62e33c2`, Apache-2.0; our Cypher translations, provenance in every
  file header) plus a 65-restrictor path-mode matrix, over the SAME
  pinned graph-benchmark-ldbc store as `run` (same data cache, same store reuse; no
  new data rights). PathDB's own seed ids exist in no LDBC datagen
  output, so every seed is substituted by the degree-median policy: the
  one canonical seed-key fact table is `workloads/pathdb/seed-keys.json`
  (36 keys, both scale factors, each record carrying the chosen node's
  id, degree and label — the runner checks the label against the key's
  on load; the key is the label plus the type set sorted and
  de-duplicated, so one fact has exactly one spelling, enforced on
  load), and `workloads/pathdb/params-sf<SF>.json` holds only the
  derived per-query entries (the SF1 file holds the 26-query subset).
- **snb-bi** — the 16 analytics queries of the LDBC SNB BI workload
  ([ldbc/ldbc_snb_bi](https://github.com/ldbc/ldbc_snb_bi)
  `47dd38b4`, Apache-2.0; our Cypher translations to the graph-benchmark-ldbc vocabulary,
  provenance in every file header; bi-10 APOC and bi-15/19/20 GDS are
  out of dialect permanently), over the SAME pinned graph-benchmark-ldbc store
  (`--sf 0.1` only: `workloads/snb-bi/params-sf0.1.json` is the recorded
  parameter table). No seed policy: every parameter is one Cypher
  expression the lane evaluates and binds natively; a param whose query
  answers empty at SF0.1 is substituted same-kind and the substitution
  recorded (ADR-0247 precedent). No oracle — SNB BI's expected outputs
  are for its own datagen run — so the ratchet gates completion and
  answer shape, never the answers.
- **fraud-qpp** — six in-house re-authored fraud-ring queries. f01/f02
  run over the `relate-datasets` fraud generator
  with its planted cycle as the oracle, f06 over a designed sample
  fixture answer-checked exactly; no data download.
- **ukrailway** — the two verified UK railway graph queries over the
  workload's OWN graph (ADR-0479's deferred fourth lane, landed
  2026-11-02): the 18 MB dogofbrian/ukrailway data set
  (`ce82229e`, Apache-2.0; fetched on demand from the pinned commit and
  verified per-file against `workloads/ukrailway/data.sha256`, never
  committed — same data cache root as the graph-benchmark-ldbc lanes, under `ukrailway/`)
  imported into `:memory:` by `workloads/ukrailway/import.cypher` (LOAD
  CSV confined to the fetched directory; every source root-relative,
  resolved by that policy — no path spelled in the script). No graph-benchmark-ldbc
  store, no
  parameters (constants inline), no seed policy, no scale factor. The
  queries and their answers are IN-HOUSE, verified by execution against
  the pinned data (2026-11-02): r02 1248 routePairs / 19
  changeoverStations (two-leg NEXT quantified pattern), r03 15 hops and
  the 16-station Brighton main line list (ANY SHORTEST over LINK) — both
  `ok` with machine-readable rows oracles in
  `workloads/ukrailway/oracle.json`. r02 is minutes-scale (213.8 s
  measured in the lane, release), so the lane's default per-entry
  timeout (`Lane::default_timeout`) is its own 600 s, never the
  scale-factor-scaled default the graph-benchmark-ldbc-store lanes carry.
- **agg-stress** — five IN-HOUSE aggregation-stress queries over the
  SAME pinned graph-benchmark-ldbc store (LABS-733, landed 2026-11-02): bounded `knows`
  walks whose result streams are dominated by rows flowing into a fold
  — the measuring net for the planned chunk-native streaming fold
  (LABS-685's follow-on; its 4-6x high-card estimate is for a02's
  391k-group composite fold). No parameters (constants inline), no seed
  policy; the oracle is engine-self — the answers this build returns
  over the pinned data, verified by execution 2026-11-02 and recorded
  per scale factor in `workloads/agg-stress/oracle-sf<SF>.json`
  (recorded at SF0.1 only — `--sf 1` fails loudly until an SF1 oracle
  exists). The headline bound is `knows*1..3` (2,624,450 walks):
  `*1..4` was measured at 23,050,339 walks / ~6.7 minutes under memory
  pressure — minutes-scale, out of the workload's budget. Run-and-report
  like every corpus lane, with the same ratchet; the lane's own flat
  60 s per-entry timeout (slowest entry 15.3 s) replaces the graph-benchmark-ldbc-store
  lanes' 10 s SF0.1 default. Peak RSS of the lane run is ~10.9 GiB (the
  materialized fold streams — the exact target shape), so the gated
  lane test is a local invocation, not a CI one.

Every query file carries one machine-read `// status:` marker that the
run gates on:

- `ok` — executed under the watchdog and must succeed (and answer-match
  where an oracle exists): the **ok-set ratchet**. An `ok` entry that
  newly times out, errors or answers wrongly fails the run; the ok-set
  only shrinks with a tracked reason, like the TCK allowlist.
- `kept` — parse and EXPLAIN only, never executed: the cyclic-recursion
  classes that time out today, frontier-NFA acceptance cases (LABS-644)
  and the walk-budget/memory cases (LABS-660 — snb-bi's five unbounded
  `replyOf*0..` walks belong to this class).
- `error-pin <ErrorName>` — executed and required to fail with that
  authored error (the fail-closed `UnsafeRepeatableElements` contract).

Nothing from the corpus enters the frozen gate compare set and no
latency number gates a PR (ADR-0289 memory-shape precedent): the report
records timings and row counts, the ratchet and the pins gate.

```bash
cargo run --release -p relate-query-latency -- corpus --workload pathdb --sf 0.1
cargo run --release -p relate-query-latency -- corpus --workload pathdb --sf 1 --timeout 20
cargo run --release -p relate-query-latency -- corpus --workload snb-bi --sf 0.1
cargo run --release -p relate-query-latency -- corpus --workload fraud-qpp
cargo run --release -p relate-query-latency -- corpus --workload ukrailway
cargo run --release -p relate-query-latency -- corpus --workload agg-stress --sf 0.1
# the gated SF0.1 pathdb and snb-bi corpus tests (load the graph-benchmark-ldbc SF0.1 graph):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus pathdb
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus snb_bi
# the gated ukrailway corpus test (fetches the 18 MB data set on first use):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus ukrailway
# the gated agg-stress corpus test (loads the graph-benchmark-ldbc SF0.1 graph; needs ~11 GB RAM):
RELATE_QUERY_LATENCY=1 cargo test --profile corpus -p relate-query-latency --test corpus agg_stress
# every corpus file plans, no data needed (normal test lane):
cargo test -p relate-query-latency --test corpus
```

`query-latency corpus` (exit 0 when every entry meets its expectation,
1 when any misses, 2 when the run could not happen):

| Flag | Default | Meaning |
| --- | --- | --- |
| `--workload <NAME>` | `pathdb` | `pathdb`: the PathDB RPQ corpus over the graph-benchmark-ldbc store; `snb-bi`: the LDBC SNB BI analytics queries over the graph-benchmark-ldbc store; `fraud-qpp`: the in-house fraud suite over generated and fixture graphs; `ukrailway`: the two verified UK railway graph queries over the workload's own imported graph; `agg-stress`: the five in-house aggregation-stress queries over the graph-benchmark-ldbc store (LABS-733) |
| `--timeout <SECS>` | `10` s at SF0.1 and `20` s at SF1 for pathdb and snb-bi, `10` s for fraud-qpp, `600` s for ukrailway, `60` s for agg-stress | Per-entry timeout; a timed-out `ok` entry fails the run (the ratchet), a timeout of anything else is a recorded outcome. ukrailway's r02 entry is minutes-scale (~3.5 min measured) and agg-stress's slowest entry is tens-of-seconds (a02, 15.3 s), so their lane defaults are their own |
| `--repeats <N>` | `2` | Extra timed executions of entries faster than 1.5 s, for the `min_ms`/`med_ms` columns (`0` measures once) |
| `--sf <SF>` | `0.1` | Scale factor (pathdb, snb-bi and agg-stress only): `0.1` or `1` — pathdb's params file and the store it loads (SF1 runs the 26-query subset recorded for it), snb-bi's params file and store, and agg-stress's oracle file and store (the last two recorded at SF0.1 only — SF1 fails loudly until their SF1 files exist). A passed `--sf` on a fraud-qpp or ukrailway run is refused: neither lane runs a scale factor (the ukrailway data set has one size, the fraud graphs are generator-sized); an unpassed one stays the default |
| `--store <PATH>` | unset (`:memory:`, loaded every run) | Reuse the graph-benchmark-ldbc store at `PATH` (pathdb, snb-bi and agg-stress only), opened read-only; when `PATH` does not exist, load into it, checkpoint it and mark it complete for the next run — same marker rules as `run` |
| `--data <DIR>` | `RELATE_QUERY_LATENCY_DATA`, else `target/query-latency/data` | Data cache directory the data sets fetch and load from (pathdb, snb-bi, ukrailway and agg-stress only) |
| `--offline` | off | Fail instead of downloading a data set that is not cached (pathdb, snb-bi, ukrailway and agg-stress only) |
| `--seed-policy <POLICY>` | `median` | Seed policy (pathdb only): `median`: re-resolves every seed key with the degree-median policy and report the diffs from the recorded seeds (the regeneration rule after a data re-freeze; a diff is expected, never a failure); `recorded`: run the pinned params file as-is |
| `--fraud-scale <F64>` | the datasets crate's env-gated scale (`RELATE_FULL_DATA=1` → `1.0`, else the CI scale `0.05`) | Fraud generator scale (fraud-qpp only; `1.0` = 10,000 accounts) |
| `--fraud-seed <U64>` | the datasets crate's CI seed (`42`) | Fraud generator seed; planted ids are stable per seed |
| `--out <PATH>` | `target/query-latency/report-corpus-<workload>[-sf<SF>].json` | Report JSON path; the table always prints to stdout |

Lane-scoped flags are refused on a workload that does not run that
lane (a `--store` or `--sf` on a fraud-qpp or ukrailway run — fraud graphs are
generated and fixture graphs and the ukrailway lane imports its own
`:memory:` graph, no graph-benchmark-ldbc store is reused, and neither lane runs a scale
factor; a `--data` or `--offline` on
a fraud-qpp run, no data set is read; a `--seed-policy` on a snb-bi,
fraud-qpp, ukrailway or agg-stress run, a `--fraud-scale` on pathdb,
snb-bi, ukrailway or agg-stress): a mistyped command line fails
loudly, never a silently ignored knob.

The report (`schema` 2) records the workload, the lane-scoped facts of
the lane that ran (a pathdb report carries its scale factor and seed
policy — and the seed policy's diffs; a snb-bi report carries its scale
factor but no seed policy, which it does not run; a fraud report
carries neither, only its generator knobs; a ukrailway report carries
neither — one data size, nothing seeded), the run config, git state, where the graph
came from, the process's peak RSS, and per
entry: the `// status:` classification, the outcome `status`
(`ok`/`wrong`/`error`/`timeout`), `rows` (null for planned-never-executed
entries), `first_ms` (null for every entry that never completed an
execution — planned, pinned or failed before measuring; a timed-out
entry carries the timeout itself as its bound, not a measurement), the repeat
statistics (`min_ms`/`med_ms` over the timed repeats, and
`failed_repeats` when a repeat errored or timed out, so a degraded
median is visible rather than silently smaller), the statement's sha256,
and the diff, error or plan note as `detail`.

| Env | Meaning |
| --- | --- |
| `RELATE_QUERY_LATENCY=1` | Additionally runs the gated SF0.1 corpus tests in `tests/corpus.rs` — the pathdb workload (full ratchet, seed-policy self-check) and the snb-bi workload (full ratchet over its recorded parameters), both over the graph-benchmark-ldbc SF0.1 graph, the ukrailway workload (full ratchet over its imported graph; fetches the 18 MB data set on first use), and the agg-stress workload (full ratchet over its verified engine-self oracle; loads the graph-benchmark-ldbc SF0.1 graph and peaks at ~11 GB RSS, so it needs a machine with that headroom) |
| `RELATE_QUERY_LATENCY_SF1=1` | Additionally runs the gated SF1 pathdb subset test in `tests/corpus.rs`. Local only: the same 241 MB / 12 GB RSS cost as the graph-benchmark-ldbc lane's SF1 test |
| `RELATE_FULL_DATA=1` | The fraud corpus test and the binary's fraud-qpp lane generate at the fraud generator's full scale (`1.0`, 10,000 accounts) instead of its CI scale (`0.05`) — the datasets crate's own gate, reused (the binary's `--fraud-scale`/`--fraud-seed` override it) |

Initial ok-set (2026-11-01, engine at the corpus PR, graph-benchmark-ldbc SF0.1,
release): pathdb 134 `ok` / 66 `kept` / 7 `error-pin`, all meeting their
expectation, 0 seed diffs, peak RSS 1.28 GB, slowest entry 2.4 s, median
0.3 ms; fraud-qpp 3 `ok` (answer-checked) / 3 `kept`; snb-bi
(2026-11-02, same engine class) 11 `ok` (1-100 rows each, slowest
bi-18 at 2.0 s) / 5 `kept` (the unbounded `replyOf*0..` walk class,
LABS-660), peak RSS 1.90 GB; ukrailway (2026-11-02, same engine class)
2 `ok`, both answer-checked against the verified oracle — r02 at 3 m 34 s
(213.8 s) under the lane's 600 s timeout, r03 at ~4 ms, peak RSS
1.80 GB; agg-stress (2026-11-02, same engine class) 5 `ok`, all
answer-checked against the verified engine-self oracle — a01 10.3 s,
a02 15.3 s (slowest), a03 6.1 s, a04 5.8 s, a05 0.57 s, each answering
its ten recorded rows under the lane's own 60 s timeout, peak RSS
10.86 GB (the materialized fold streams — the workload's target
shape). Ten restrictor
entries were re-measured the same day and reclassified `kept` → `ok`:
r021-r025 and r056 (4-39 ms each — bounded two-/three-hop shapes the
copied unbounded-recursion reason could not apply to) and
r006/r007/r009/r010 (~86 ms, 20 rows each — the replyOf forest is
acyclic, so the copied cyclic-recursion reason named a mechanism this
data cannot have). The 13 same-type alternation entries were collapsed
to their single quantified pattern the same day, every ok entry's row
count verified unchanged. Workload READMEs:
`workloads/pathdb/README.md` (provenance, label map, seed policy),
`workloads/snb-bi/README.md` (provenance, vocabulary map, dialect
check, substitution table),
`workloads/fraud-qpp/README.md`,
`workloads/ukrailway/README.md` (provenance, verification record,
import lane shape, timeout policy),
`workloads/agg-stress/README.md` (LABS-685/733 rationale, stream and
group measurements, bound-tuning record, dialect check, timeout and
memory policy).

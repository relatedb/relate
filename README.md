# Relate

**A graph database in a single binary — query it in Cypher, embed it anywhere, deploy nothing.**

<p align="center">
  <img src="img/relate-hex-ink.png" alt="Relate" width="800" />
</p>

Relate is an embedded, in-memory property-graph engine written in Rust, with a
command-line shell for interactive and scripted queries. A graph is a single
`.rl` file, a directory, or pure in-memory — durable through a fsync'd
write-ahead log and Parquet checkpoints.
The same engine powers the `relate` CLI, a Python package, Node.js and browser
(WASM) npm packages, a Rust crate, and an optional one-process server speaking
Bolt, HTTP, and MCP. No server to babysit, no infrastructure to operate.

📖 **Documentation: [relatedb.ai](https://relatedb.ai/)** — tutorials, guides,
and the CLI / Cypher / Rust / C API references. The deep references live in
[`docs/`](docs/) in this repository, and the honest list of what Relate does
not do yet is [`docs/limitations.md`](docs/limitations.md).

> This is an experimental Neo4j Labs project and not part of Neo4j's supported
> product lineup. See the [Neo4j Labs disclaimer](./LABS_DISCLAIMER.txt).

## Install

**CLI (macOS & Linux):**

```bash
curl -sSfL https://relatedb.ai/install.sh | bash
```

Downloads the release binary for your platform (macOS and Linux, amd64 and
arm64) to `$HOME/.local/bin`, resolving the latest release from
[`releases.json`](https://relatedb.ai/releases.json) — the same release
metadata `relate upgrade` reads
([ADR-0571](docs/adr/0571-installer-resolves-via-releases-json.md) — and
the assets themselves come straight from the GitHub release download route,
so the installer makes no GitHub API requests and no token is ever needed,
even behind shared-IP networks
([ADR-0585](docs/adr/0585-installer-downloads-via-direct-route.md))). Each
release's `SHA256SUMS` manifest is
authenticated against its SSHSIG signature with the stock `ssh-keygen` — the
public key is pinned in the script, and the installer **fails closed** on a
missing or invalid signature
([ADR-0115](docs/adr/0115-sshsig-release-signing.md)). macOS binaries are
additionally signed and notarized, so Gatekeeper raises no warning
([ADR-0123](docs/adr/0123-macos-codesign-notarize.md)). Prebuilt binaries for
Linux, macOS, and Windows are on the
[releases page](https://github.com/relatedb/relate/releases/latest), and an
installed binary self-updates:

```bash
relate upgrade          # download, verify, and replace (alias: update)
relate upgrade --check  # report current vs latest; changes nothing
```

**Python:**

```bash
pip install relate-db   # the import is `import relate`
```

**npm — Node.js and the browser:**

```bash
npm install @relate/relate        # Node.js 18+ (native bindings)
npm install @relate/relate-wasm   # browser (WebAssembly)
```

**Rust:**

```bash
cargo add --git https://github.com/relatedb/relate relate
```

> Until the PyPI and npm publishes land, Python wheels are on the
> [releases page](https://github.com/relatedb/relate/releases) and every
> package builds from source — see [Development](#development).

## Quick start

Hand the wheel to your AI agents, or drive it yourself:

```bash
# Once: install the relate skill bundle — CLI + Cypher + vector + spatial
# references — into every detected AI agent (Claude Code, Cursor, Codex,
# Gemini CLI, Copilot, Windsurf, OpenCode, ...)
relate skill install

# One-shot query on an in-memory database
relate "RETURN 1 + 2 AS sum"

# Open (or create) a store and drop into the interactive shell
relate query --database ./friends

# Script-friendly output
relate --json ./friends "MATCH (p:Person) RETURN p.name"

# Run a file of statements
relate -d ./friends < setup.cypher
```

`relate skill install` is optional — it teaches supported agents the CLI
flags, the Cypher dialect, and the gotchas, so they can drive `relate` for
you. `relate upgrade` keeps the bundle current automatically. More in
[Agent skill](#agent-skill).

An example session:

```
$ relate query --database ./friends

Relate v1.0.0
Type .help for help, .exit to quit.

cypher> CREATE (alice:Person {name: 'Alice', age: 30});
Nodes created: 1, Properties set: 2, Labels added: 1

cypher> CREATE (bob:Person {name: 'Bob', age: 28});
Nodes created: 1, Properties set: 2, Labels added: 1

cypher> MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
   ...> CREATE (a)-[:FRIENDS_WITH {since: 2020}]->(b);
Relationships created: 1, Properties set: 1

cypher> MATCH (p:Person)-[r:FRIENDS_WITH]->(friend)
   ...> RETURN p.name, friend.name, r.since;
+--------+-------------+---------+
| p.name | friend.name | r.since |
+--------+-------------+---------+
| Alice  | Bob         | 2020    |
+--------+-------------+---------+
```

## Surfaces

One engine, many front doors: the CLI (Quick start above), the libraries
below, the server surfaces (Bolt, HTTP, MCP), and the extension mount
([Extensions](#extensions) below).

### Python

Queries execute in-process — a native extension module (PyO3), no server, no
subprocess:

```python
import relate

db = relate.connect(":memory:")   # or "./friends" — a directory or .rl file

db.execute("CREATE (p:Person {name: $name, age: $age})", {"name": "Alice", "age": 30})
db.execute("CREATE (p:Person {name: $name, age: $age})", {"name": "Bob", "age": 28})
db.execute(
    "MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) "
    "CREATE (a)-[:FRIENDS_WITH {since: 2020}]->(b)"
)

for row in db.execute("MATCH (p:Person)-[:FRIENDS_WITH]->(f) RETURN p.name, f.name"):
    print(f"{row['p.name']} -> {row['f.name']}")   # Alice -> Bob
```

A native `Database` class adds `execute_arrow()` — zero-copy Arrow IPC into
pandas or Polars. Spec: [`docs/api/python/`](docs/api/python/); runnable
example: [`examples/python/python_demo.py`](examples/python/python_demo.py).

### Node.js

```js
const { Database } = require("@relate/relate");

const db = Database.open("./friends");        // or ":memory:"
const result = db.connect().query("MATCH (p:Person) RETURN p.name");

result.rows;                   // [{ "p.name": "Alice" }, ...]
result.stats;                  // mutation counters, zeros included
result.consume().elapsedMs;    // engine-reported milliseconds
```

Node 18+; the native library arrives via platform optional-dependencies — no
install scripts. Spec: [`docs/api/javascript/`](docs/api/javascript/).

### Browser (WASM)

The engine compiled to WebAssembly — run Cypher in a page, no server:

```js
import init, { RelateDb } from "@relate/relate-wasm";

await init();

const db = new RelateDb();                    // in-memory
const result = JSON.parse(db.execute("RETURN 1 AS n", null));
```

Durable browser storage via OPFS: `await RelateDb.open("opfs://graph")` —
runs in a dedicated worker only. Spec: [`docs/api/wasm/`](docs/api/wasm/).

### Rust

`relate::Database` is the engine's public API — the surface every other
binding wraps:

```rust
use relate::Database;

fn main() -> relate::Result<()> {
    let db = Database::open("./friends")?;
    let mut conn = db.connect()?;

    conn.query("CREATE (n:Person {name: 'Ada'})")?;
    let result = conn.query("MATCH (n:Person) RETURN n.name AS name")?;
    println!("{}", result.rows[0][0]); // Ada
    Ok(())
}
```

Explicit transactions, cancellation tokens, and read-only,
follow, and ephemeral opens. Spec: [`docs/api/rust/`](docs/api/rust/).

### C

Every language client wraps one typed C ABI — version 2, additive-only
after the first public release:

```c
#include "relate.h"

relate_database db = NULL;
relate_connection conn = NULL;
relate_query_result r = NULL;
char *err = NULL;            /* out-param errors: start NULL */

relate_database_open(":memory:", &db, &err);
relate_connect_namespace(db, NULL, &conn, &err);
relate_query_params(conn, "RETURN 1 AS n", NULL, &r, &err);
```

The header and library (`librelate_ffi`, static or shared) ship as
`relate-ffi-<target>.tar.gz` on the
[releases page](https://github.com/relatedb/relate/releases/latest); every
fallible call reports through one typed error channel. Contract:
[`docs/api/c/`](docs/api/c/).

### Java

`com.relate:relate` — Java 17+, JNA over the C ABI, with the native
library bundled in the jar (one fat jar, or one of five platform
classifiers):

```java
try (Database db = Database.open("./friends");
     Database.Connection conn = db.connect()) {
    Result result = conn.query("RETURN $n AS n", java.util.Map.of("n", 1));
    result.columns();   // List<String>
    result.rows();      // List<List<Object>> — typed values
}
```

Jars are on the [releases page](https://github.com/relatedb/relate/releases/latest)
until Maven Central goes live. Spec: [`docs/api/java/`](docs/api/java/).

### Go

Module `github.com/relatedb/relate/clients/go`, cgo against `librelate_ffi`
(from the release tarballs, or built with `cargo build --release -p
relate-ffi`):

```go
db, err := relate.Open("./friends")
conn, err := db.Connect()
result, err := conn.Query("MATCH (p:Person) RETURN p.name")
// result.Columns []string, result.Rows [][]any
```

Spec: [`docs/api/go/`](docs/api/go/).

### Swift

A SwiftPM package (iOS 16+ / macOS 13+, Swift 6) wrapping the C ABI, with
the checksummed binary framework attached to every release:

```swift
// Package.swift of a consumer
.package(url: "https://github.com/relatedb/relate", from: "<release>")
// target dependency
.product(name: "Relate", package: "relate")
```

```swift
let db = try Database(path: "./friends")
let conn = try db.connect()
let result = try conn.query("MATCH (p:Person) RETURN p.name")
```

Spec: [`docs/api/swift/`](docs/api/swift/).

### Server — Bolt, HTTP, MCP

When you want a socket instead of a library:

```bash
relate serve --database ./friends --bolt 7687 --http 7474 --mcp 7475
```

One process, one store, one generated token (printed at startup): Neo4j
drivers connect over Bolt — neo4j-browser works out of the box — the HTTP
Query API v2 serves JSON, and MCP (streamable HTTP) serves AI hosts. Binds
loopback by default; runs until SIGINT/SIGTERM. Specs:
[`bolt`](docs/api/bolt/), [`http`](docs/api/http/), [`mcp`](docs/api/mcp/).

Docker — `make docker` builds the server image (multi-stage, a `FROM scratch`
runtime with static binaries); `make docker-run` runs Bolt (7687), HTTP
(7474), and MCP (7475) on localhost with a data volume. Auth is on by
default; the token is in the startup banner:

```bash
TOKEN=$(docker logs <container> 2>&1 | sed -n 's/.*generated token for user relate: //p' | head -1)
python -c "from neo4j import GraphDatabase; print(GraphDatabase.driver('bolt://localhost:7687', auth=('relate', '$TOKEN')).execute_query('RETURN 1 AS n')[0][0]['n'])"
```

Or preset it with `-e RELATE_AUTH=relate:secret` (`...=none` disables it —
publish ports on trusted networks only). Configuration is env-based and
echoed at startup (`docker logs` is the debugging surface); a fresh store can
be seeded before the server starts:

```bash
docker run -p 7687:7687 -p 7474:7474 -p 7475:7475 -v "$PWD/data:/data" \
  -e RELATE_QUERY="UNWIND range(1,100) AS i CREATE (:Person {id: i})" \
  relate-bolt:latest

# fetch-and-seed from a JSON API, or mirror a Neo4j/Aura subgraph:
#   -e RELATE_LOAD='value=https://…' + -e RELATE_LOAD_QUERY='UNWIND $value.items AS q MERGE (:Question {id: q.question_id})'
#   -e RELATE_PULL_FROM=/run/secrets/neo4j.env + -e RELATE_PULL_QUERY='MATCH (o:Organization)-[r]-() RETURN o, r LIMIT 500'
```

For large seeds, two knobs matter (details:
[Bulk import](docs/guides/import-csv.md#bulk-import-durability-and-batch-size)):
batch the writes into few statements — one `UNWIND` per few thousand rows
— because each autocommit statement pays one WAL fsync under the default
`full` durability; and relax that barrier for the seed run with
`-e RELATE_DURABILITY_MODE=everysecond` (a crash can lose ~1s of
acknowledged writes — drop the variable when serving).

`INSTANCES=N` runs a test fleet of N serve processes with stepped port
ranges. The image is a dev/test image: `serve` is plaintext-only and warns
loudly about it — for encrypted Bolt, front it with a TLS-terminating proxy.

## Features

| Area | What it covers |
| --- | --- |
| **Cypher** | Reading and writing (`MATCH`, `OPTIONAL MATCH`, `CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`, `FOREACH`, `INSERT`), composing (`WITH`, `UNWIND`, `UNION`, `CALL {}` subqueries, subquery expressions), schema DDL (indexes, constraints, graph types), 80+ built-in functions, and modern pattern matching — label expressions, quantified path patterns, path selectors. One dialect: current Cypher. |
| **Language floor** | The openCypher TCK is the dialect floor, measured by the project's spec harness: Neo4j's Cypher suites — the upstream TCK among them — run against the engine's Python wheel on every ready PR, as reported coverage, never a merge gate. History: [`docs/tck/compliance.md`](docs/tck/compliance.md). |
| **Vector search** | `vector()` properties, `vector.similarity.*` scoring functions, `CREATE VECTOR INDEX` plus the `SEARCH` clause for index-backed top-K with pre-filtering, and hybrid vector + full-text queries. |
| **Geospatial** | WGS-84 and Cartesian points (2D/3D), `point.distance()`, radius and bounding-box predicates (`point.withinBBox`), and R-tree point indexes (`CREATE POINT INDEX`). The dialect covers points only — richer geometry is host-language work. |
| **Profiling** | `EXPLAIN` renders the plan tree; `PROFILE` adds per-operator rows and time. Profiling costs nothing on non-PROFILE runs. |
| **CLI shell** | Interactive REPL with readline-style editing, dot-commands, output modes `table`, `csv`, `json`, `jsonl`, `markdown`, `cypher`. |
| **Import / sync** | `LOAD CSV` directly in Cypher; `--fetch-params` binds a fetched URL/file JSON document as a parameter; `pull`/`push` mirror subgraphs between Relate and a Neo4j/Aura instance; `clone` copies a whole store across layouts — directory ↔ `.rl` bundle, optionally transcoding the snapshot codec. |
| **Read-only & follow** | `--read-only` opens an existing store without writing or checkpointing; `--read-only=live` follows a native directory's WAL while another process writes it. The same access modes — snapshot and live freshness — ride `serve` and the Rust API (`OpenMode::ReadOnly` / `Follow`, ADR-0136). |
| **Surfaces** | Rust crate, Python + Arrow IPC, JS/Java/Go/Swift over the typed C ABI, Bolt, HTTP, MCP, WASM. |

Task guides — vector similarity, geospatial, CSV import, backup/restore,
storage — live in [`docs/guides/`](docs/guides/); tested, runnable recipes for
most of the above are in the [cookbook](https://relatedb.ai/cookbook/).

## CLI reference

### Commands

Exit-after lifecycle verbs are positional commands; everything else configures
a query-engine run:

| Command | What it does |
| --- | --- |
| `query [STATEMENT]` | Interactive shell, or one-shot/stdin queries (the default surface) |
| `skill install\|remove` | Install / remove the agent-skill bundle |
| `upgrade` | Self-update the binary (alias: `update`) |
| `serve` | Host Bolt + HTTP + MCP on one store until SIGINT/SIGTERM |
| `pull` / `push` | Mirror a subgraph from / into a Neo4j/Aura remote (Query API v2) |
| `clone SOURCE DEST` | Copy a database into a new store and exit — directory ↔ `.rl` on either side, `--to-snapshot-format` to transcode the codec |
| `extension init\|install\|list\|remove\|check`, `run` | Mount and dispatch extension commands |
| `telemetry on\|off\|status` | Telemetry controls |
| `info` | Store metadata (cold read, no open) |

```bash
# pull a subgraph from Neo4j/Aura into a local mirror:
relate pull --from neo4j.env --query "MATCH (o:Organization)-[r]-() RETURN o, r LIMIT 500" --database ./mirror

# copy a store into a single-file bundle (backup, transfer):
relate clone ./friends ./friends-backup.rl
```

### Common flags

| Flag | What it does |
| --- | --- |
| `-d`, `--database PATH` | Store target: a directory, `.rl` file, or `:memory:` (default) |
| `-c`, `--command Q` | One-shot query (same as the positional form) |
| `--mode MODE` | Output: `table` (default on a terminal), `csv`, `json`, `jsonl`, `envelope`, `markdown`, `cypher` |
| `--json` / `--jsonl` / `--csv` / `--markdown` | Shortcuts for `--mode` |
| `--noheader` | Omit column headers |
| `--param N=V` | Bind a Cypher parameter (repeatable) |
| `--fetch-params N=SRC` | Fetch a URL or read a file as JSON, bind it as `$N` |
| `--max-rows N` | Print at most N rows per statement (0 = unlimited) |
| `--read-only[=FRESH]` | Open an existing store without writing; `FRESH` = `snapshot` (default) or `live` WAL tailing |
| `--snapshot-format F` | Snapshot codec: `parquet` (default) or `json` — empty stores only |
| `--durability-mode MODE` | WAL durability: `full` (default), `everysecond`, `snapshot` |
| `--logical-graph-size-limit SIZE` | Live graph + index cap in logical bytes |
| `--load-csv-dir PATH` | Allow local `LOAD CSV` under this directory (default denied) |
| `-h`, `--help` / `--version` | Usage / version (with `--json`: one JSON object) |

Full flag table, env vars, and per-command flags:
[`docs/api/cli/spec.md`](docs/api/cli/spec.md).

### For agents and scripts

Output and errors are designed to be parsed:

- **Result shape.** `--json` prints a bare array of row objects, `--jsonl`
  one object per row, and `--mode envelope` one
  `{"columns":[…],"rows":[…],"truncated":false}` object per result set — an
  empty result still lists its columns.
- **Errors.** In machine modes a failed statement also prints
  `{"error":{"code","exit_code","kind","message","retryable"}}` on stdout.
  Branch on exit code: usage `2`, rejected statement (syntax/semantic/type)
  `6`, write conflict or locked store `5`, other runtime failure `1`.
- **Row cap.** Under a detected agent harness (or explicit `--agent-mode`),
  a statement prints at most 100 rows and says so **in-band** — a capped
  result never reads as complete. Raise or lift it with `--max-rows N` /
  `RELATE_MAX_ROWS`.
- **Typos.** An unknown flag suggests the nearest real one
  (`--readonly` → `Did you mean '--read-only'?`).

### Agent skill

`relate skill install` writes the skill bundle (`SKILL.md` plus CLI, Cypher,
vector, and spatial references) into every detected agent's skills directory,
version-stamped with an ownership marker — a hand-authored directory is never
overwritten (`--force` to override). `relate skill remove` is idempotent, and
`relate upgrade` refreshes an installed bundle automatically.

```bash
relate skill install                     # every detected agent
relate skill install --agent claude-code # one agent only
relate skill remove                      # uninstall (idempotent)
```

The Python wheel bundles the same skill for library users.

## Storage

A store is a fsync'd **WAL plus one snapshot codec**
([ADR-0050](docs/adr/0050-snapshot-codecs.md)); the layout is
chosen by the path:

| Store | What it is | Open with |
| --- | --- | --- |
| In-memory | RAM only, gone at exit | `relate` |
| Directory (default) | WAL + Parquet shards under a directory | `relate -d ./friends` |
| Directory, JSON codec | WAL + a readable `graph.json` | `relate -d ./friends --snapshot-format json` |
| Single file (`.rl`) | WAL, snapshot, and catalog inside one host file | `relate -d ./friends.rl` |

Durability defaults to `full` (acknowledge after the WAL barrier);
`everysecond` can lose ~1 s of acknowledged writes, `snapshot` everything
since the last completed checkpoint. Do not mix Parquet shards and
`graph.json` in one directory. The full WAL/checkpoint story:
[storage engine guide](docs/guides/storage-engine.md).

## Extensions

Userland commands mount as `relate run <name>` — self-contained packages the
host drives over MCP stdio. The host owns the store; the extension's only
power is the `read_cypher`/`write_cypher` calls it makes. First-party
packages:

| Extension | What it does |
| --- | --- |
| `fs-to-graph` | A directory tree as a graph — one `(:File)` per file, `[:CONTAINS]` the tree, fulltext over names and contents |
| `git-to-graph` | A checkout's history as a graph — commits, parents, changed paths |
| `dungeon` | A MUD-style keep as a playable game — every verb a Cypher statement against the store |

```bash
relate extension install dungeon --from extensions/relate-dungeon/package
relate run dungeon -d keep.rl --command start
```

Scaffold your own with `relate extension init <name> [--lang sh|py|ts|rust]`.
The wire contract, trust tiers, and authoring guide:
[`docs/api/extensions/`](docs/api/extensions/).

## Telemetry

The `relate` CLI reports **anonymous** usage metrics — command shapes, feature
flags, an anonymous install id, and a coarse environment tag. It never sends
query text, file paths, or your data. Collection defaults to on, with a
layered opt-out:

```bash
relate telemetry off       # persist the opt-out
relate telemetry status    # e.g. "disabled (config file)"
```

`DO_NOT_TRACK=1` or `RELATE_TELEMETRY=0` also disable it. Only the standalone
CLI collects metrics; the embedded Rust and Python APIs do not. Full contract:
[CLI reference](https://relatedb.ai/docs/reference/cli-flags).

## Development

Requires the Rust toolchain (MSRV 1.88, pinned in
[`rust-toolchain.toml`](rust-toolchain.toml)):

```bash
git clone https://github.com/relatedb/relate && cd relate
cargo build --release
./target/release/relate --version
```

### Testing and lint

```bash
# Tests
cargo test -p relate -p relate-front -p relate-executor -p relate-cli -p relate-splitter
make test                     # the same, via make

# Lint — the CI gate
make lint                     # cargo fmt --all --check + clippy -D warnings

# WASM bundle
make build-wasm
```

Language coverage: the openCypher TCK and Neo4j's Cypher suites run in the
spec harness against the Python wheel on every ready PR — reported coverage,
never a merge gate. History and pointer:
[`docs/tck/compliance.md`](docs/tck/compliance.md).

### Python development

```bash
pip install maturin
cd surfaces/relate-python && maturin develop --release
python -c "import relate; print(relate.connect(':memory:'))"

# Python crate Rust-side tests
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo test -p relate-python
```

### Architecture

Every architecture, storage, protocol, dialect, public-API, and dependency
decision is recorded as an Architecture Decision Record in
[`docs/adr/`](docs/adr/) — the project's decision log, numbered and immutable
once accepted. Task guides live in [`docs/guides/`](docs/guides/), per-surface
API contracts in [`docs/api/`](docs/api/), and deep references (Cypher
syntax, functions, CLI flags, storage formats, the C ABI) in
[`docs/reference/`](docs/reference/).

## Contributing

We accept **issue reports only — no pull requests**. Bugs, feature ideas, and
questions all go to the [issue tracker](https://github.com/relatedb/relate/issues);
see [CONTRIBUTING.md](CONTRIBUTING.md) for what makes a useful report.

## License

Apache-2.0 ([ADR-0055](docs/adr/0055-apache-2-license.md)) — see [LICENSE](LICENSE).

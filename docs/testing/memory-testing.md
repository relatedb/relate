# Memory testing

Two suites load the same graphs and measure different meters.
[ADR-0042](../adr/0042-graph-memory-limit.md), as amended by
[ADR-0202](../adr/0202-graph-floor-memory.md), caps a logical size of the
live graph plus the indexes that enter `IndexCharge`. That counter is a
closed formula: payload bytes, plus dashmap slots, version lists,
property vectors, and adjacency vectors reserved to the next power of
two. The same nodes, relationships, property bytes, and index charge
produce the same integer on every OS and allocator.
Process RSS is a different meter: it includes the server, query scratch,
and allocator slack, and it depends on graph shape.
[ADR-0289](../adr/0289-graph-shape-memory-pins.md) pins the logical
counter exactly and records Bolt RSS beside it.

Both suites call one generator,
`testing/relate-datasets/src/shapes.rs`. The shapes are `base`,
`ring`, `fanout`, `flat`, and `dense ring`. The pin scale emits a fixed
statement list for a given `(shape, scale, indexes, attributes)`. The
loadtest uses the same batch shapes and keeps issuing them until the
database reports the memory limit. Statements are independent autocommit
queries. Writes return nothing.
Node creates and relationship creates are batched at 2,000 rows. A fanout statement writes one hub's leaves. A dense-ring statement in the pin file writes one window of sources, `2,000 / degree` of them. The loadtest creates and links that window in one statement, so a rejected batch does not leave nodes without edges. Index DDL, when present, runs
first on every filled shape. `base` emits nothing, so its RSS stays the
empty-server floor. Its logical floor is the two namespace graphs'
interned `*` and `""`, 258 bytes (ADR-0229 dropped the empty
`system_ontologies` graph every store used to allocate).

Every node carries `ATTRIBUTES` integer properties (default 1). The
first is `id`. The rest are `a1`, `a2`, and so on, set to the same value
as `id`. Each integer property is 12 payload bytes. The property vectors
reserve the next power of two, so the second attribute also grows those
vectors from one slot to two (96 bytes on top of the first, plus the
new key interned once). `0` is rejected. Three more counts default to 0
and apply to every filled shape:

- `TEXT_INDEXES` adds that many string attributes (`s0`, `s1`, …), each
  with its own `TEXT` index. The string is `abcdefghijklmnop`.
- `POINT_INDEXES` adds that many point attributes (`loc0`, `loc1`, …),
  each with its own `POINT` index.
- `RANGE_INDEXES` puts a `RANGE` index on the first integer attributes
  (`id`, then `a1`, …). It must be at most `ATTRIBUTES`.

Text and point properties are on top of `ATTRIBUTES`. Fanout creates
each index twice, once for `:Hub` and once for `:Leaf`.

The drawings below are small examples of the linking rule. Highlighted
arrows are the links that leave one node. Every other node follows the
same rule. The tests use the counts in the tables, not the counts in
the drawings.

## Running the shape loadtest

Logical pins:

```bash
make test
cargo test -p relate-datasets --test memory_shapes
```

`make test` runs the rest of the Rust suite first, then this file. The
pin file always uses the small scale. Attribute and index counts are
the ones named in each test. It does not read `LOGICAL_MB`,
`ATTRIBUTES`, or the index variables.

Bolt RSS, in the ADR-0070 container:

```bash
make memory-shape-loadtest
```

That builds the lane's binaries with `Dockerfile.baseline` (release binaries
in `rust:1.88-bookworm`), runs them on `debian:bookworm-slim`, and writes
`target/memory-shape-loadtest.json`. Ctrl+C kills the container. A
different logical size, or indexes on every filled shape:

```bash
make memory-shape-loadtest \
  LOGICAL_MB=16 ATTRIBUTES=2 TEXT_INDEXES=1 POINT_INDEXES=1 RANGE_INDEXES=1
```

`BASELINE_ENV=host` compiles and runs on the machine you are on. That
report is a debug sample. `make baseline-loadtests` refuses it, and it
must not be frozen. The host path needs Linux `/proc/<pid>/status`.

```bash
BASELINE_ENV=host make memory-shape-loadtest
```

Graph size and indexes. These five are the inputs to the generator. The pin file does not read them.

| Variable | Default | What it adds | Limit |
|---|---|---|---|
| `LOGICAL_MB` | `32` | Logical ceiling for each filled shape, passed to the database as `logical_graph_size_limit` (logical bytes). One MB is 1024×1024 bytes. The load stops at the fixed counts. Reaching this ceiling first fails the run. | `0` is rejected. `base` stays empty and does not hit the ceiling. |
| `ATTRIBUTES` | `1` | Integer properties on every node: `id`, then `a1`, `a2`, …. Each integer is 12 payload bytes, and the property vectors reserve the next power of two. | At least 1. More attributes cost more per node, so fewer nodes fit under the limit. |
| `TEXT_INDEXES` | `0` | String properties `s0`, `s1`, …, each with its own `TEXT` index. The value is `abcdefghijklmnop`. | On top of `ATTRIBUTES`. Fanout creates the index for `:Hub` and `:Leaf`. |
| `POINT_INDEXES` | `0` | Point properties `loc0`, `loc1`, …, each with its own `POINT` index. The value is `point({srid: 7203, x: 1.0, y: 2.0})`. | On top of `ATTRIBUTES`. Fanout creates the index for `:Hub` and `:Leaf`. |
| `RANGE_INDEXES` | `0` | `RANGE` indexes on the first integer properties (`id`, then `a1`, …). | At most `ATTRIBUTES`. Each posting is part of the logical size, so fewer nodes fit under the limit. |

Where the run happens, and where the report goes.

| Variable | Default | What it sets |
|---|---|---|
| `BASELINE_ENV` | `docker` | `docker` is the freezeable run. `host` is a debug sample and must not be frozen. |
| `MEMORY_SHAPE_MEMORY` | `2g` | cgroup cap for this container. At the 32 MB default, unindexed dense-ring RSS is about 1.3 GB. A larger `LOGICAL_MB` needs a higher cap. Fraud, identity, and checkpoint stay on `BASELINE_MEMORY` (`2g`). |
| `BASELINE_CPUS` | `2` | CFS quota (`cpu-period` 10 ms × this many CPUs). Docker refuses a host with fewer CPUs. |
| `CARGO_PROFILE` | `--release` | Passed to `cargo` on a host run. The image always builds `--release`. |
| `ARGS` | empty | Extra arguments appended to the binary. Host example: `ARGS=--bolt-bin=/path/to/relate-bolt` — note the standalone bolt server is retired (ADR-0368): the bolt-binary cases need serve storage flags that do not exist yet and fail loudly. |
| `OUT_MEMORY_SHAPE` | `target/memory-shape-loadtest.json` | Host path of the report. The container writes `/report/memory-shape-loadtest.json` and the script copies it out. |
| `MEMORY_SHAPE_RUN` | UTC timestamp `YYYYMMDDTHHMMSS` | Suffix of the default database directory. |
| `URI_MEMORY_SHAPE` | `/tmp/memory-shape-loadtest-$(MEMORY_SHAPE_RUN)` | Run directory. Each shape is stored at `<uri>/<shape>`, and disk MB is the full size of that directory after checkpoint, including the WAL. Docker bind-mounts this path into the container. Override the whole path with `URI_MEMORY_SHAPE=/path make memory-shape-loadtest`. |
| `BASELINE_BUILD_IMAGE` | `rust:1.88-bookworm` | Image that compiles the two binaries. |
| `BASELINE_RUN_IMAGE` | `debian:bookworm-slim` | Image that runs them. |
| `MEMORY_SHAPE_IMAGE` | `relate-memory-shape:local` | Tag of the built image. |
| `MEMORY_SHAPE_CONTAINER` | `relate-memory-shape` | Container name. A new run removes the previous one. |

Compare and freeze. These do not change the graph the loadtest builds.

| Variable | Default | What it sets |
|---|---|---|
| `BASELINE_RUNS` | `5` | Rounds in `make baseline-loadtests`. The freeze keeps the highest RSS ratio per shape. |
| `LOADTEST_MAX_RSS_RATIO` | `1.15` | Compare gate. A shape fails when its ratio is above this multiple of the frozen ratio. |

`make compare-loadtests` checks the report only when
`baselines/memory-shape-loadtest.json` exists. Until the first
`create-loadtest-baselines` freeze, compare skips the file.

## Logical pins

`make test` runs `cargo test -p relate-datasets --test memory_shapes`.
The scale is `Small`, `ATTRIBUTES` is 1, and the index counts are 0
unless a test says otherwise. Each test opens `:memory:`,
runs the generator, checks `count(n)` and `count(r)` against `census`,
and asserts `Database::memory_usage` down to the byte. The limit stays
unset. Asserts are exact. They do not use the loadtest's 10% or 15%
bands.

| Shape | Index | Nodes | Relationships | `used` |
|---|---|---:|---:|---:|
| base | none | 0 | 0 | 258 |
| flat | none | 1,000 | 0 | 276,392 |
| fanout | none | 1,000 | 996 | 640,059 |
| ring | none | 1,000 | 1,000 | 720,460 |
| dense ring | none | 1,000 | 8,000 | 2,713,465 |
| flat | range 1 | 1,000 | 0 | 338,392 |
| flat | text 1, point 1, range 1 | 1,000 | 0 | 742,526 |

### How indexes are tested

Index counts are not a separate shape. The small suite pins two facts
on flat, at one integer attribute:

- `RANGE_INDEXES=1` with text and point at 0 pins `used` at 338,392.
  That is the unindexed flat pin plus 62 bytes per node, one range
  posting on `Flat`/`id`.
- `TEXT_INDEXES=1`, `POINT_INDEXES=1`, and `RANGE_INDEXES=1` together
  pin `used` at 742,526.

That combined node is the 276-byte unindexed flat node, plus `s0` (36
bytes of string and 112 bytes of width-3 postings, 14 windows of 8
bytes), plus `loc0` (36 bytes of point and a 64-byte posting on the
4-byte key), plus the 62-byte range posting, plus the property vectors
growing from one slot to four. The empty floor and the interned names
sit on top of the 1,000 nodes.

The generator tests the Cypher, not the byte counter, for the other
shapes. One of each index on `ring` emits `CREATE TEXT INDEX Ring_s0`,
`CREATE POINT INDEX Ring_loc0`, and `CREATE RANGE INDEX Ring_id`, and
the nodes carry `s0` and `loc0`. The same text index on `fanout` is
created twice, `Hub_s0` and `Leaf_s0`, because each label is indexed on
its own. `base` still emits no statements when indexes are requested,
so the RSS floor stays an empty server. `RANGE_INDEXES` above
`ATTRIBUTES`, and `ATTRIBUTES` of 0, are errors.

Point charges use the label length and the key length, so the same
`loc0` costs a different number of bytes on `:Hub` (3 bytes), `:Flat`
or `:Ring` or `:Leaf` (4), and `:DenseRing` (9). A text index charges
8 bytes per width-3 window and does not repeat the label or the key on
each window (ADR-0207). The large
census checks that `TEXT_INDEXES=1` shrinks the node count of flat,
ring, fanout, and dense ring at the same 32 MiB target. Flat lands on
15,190 nodes. The other three are asserted only to be smaller than
their unindexed counts.

A second attribute on the 1,000-node ring must add exactly
`64 × 1,000 + 66` bytes: 12 payload bytes and the property-vector
growth for a 48-byte value slot, plus the interned key `a1`.

One file-backed `ring` graph is closed and opened again. Replay always
applies, so `used` must come back as 720,460.

A formula change updates these integers in the same commit.

## RSS loadtest

`make memory-shape-loadtest` is an [ADR-0070](../adr/0070-docker-baseline-loadtest-environment.md)
baseline target. The default logical size is `LOGICAL_MB=32` (1024×1024
bytes per filled shape), `ATTRIBUTES=1`, and `TEXT_INDEXES`,
`POINT_INDEXES`, and `RANGE_INDEXES` at 0. The container is 2 vCPU and
`MEMORY_SHAPE_MEMORY=2g`. Unindexed dense-ring RSS at that size is about
1.3 GB. A larger logical target needs a higher cap. Fraud, identity,
and checkpoint stay on the same 2 GB cap. The shared `Dockerfile.baseline`
compiles the lane's two binaries in `rust:1.88-bookworm` and runs them on
`debian:bookworm-slim`. `BASELINE_ENV=host` is a debug escape. Do not
freeze a host report.

For each large shape the binary:

1. Opens an in-process database at `$URI_MEMORY_SHAPE/<shape>` with
   `logical_graph_size_limit` set to `LOGICAL_MB` and writes batches until
   the fixed count below. Reaching the ceiling first fails the run. It
   then checkpoints, drops the database, and sums every file in that
   directory, including the WAL. That sum is disk.
2. The bolt case spawns `relate serve --bolt` with `--auth none`
   and `--logical-graph-size-limit` set to the same byte count, and
   loads the same counts (ADR-0368, ADR-0390).
   (Historically the standalone `relate-bolt` binary took `--uri`
   and `--memory-limit`. Before ADR-0390, `relate serve` had no
   graph-size flag and this case failed loudly.)
3. Samples the child's `/proc/<pid>/status` five times, 50 ms apart.
   RSS is the minimum `VmRSS`. Peak RSS is the maximum `VmHWM` in those
   samples. The test process is not the sample.

The matrix, in report order, is `base`, `flat`, `fanout`, `ring`, and
`dense ring`. The loadtest stops at:

| Shape | Nodes | Relationships |
|---|---:|---:|
| flat | 120,000 | 0 |
| fanout | 52,016 | 52,000 |
| ring | 46,000 | 46,000 |
| dense ring | 5,000 | 100,000 |

`base` stays empty. Its RSS is the empty server and is subtracted from
the others. The ratio is attributed RSS divided by logical bytes. The
report stores Given / When / Then / Outcome with each shape and prints
sizes as MB, or GB once a value reaches 1024 MB.

The gate is [ADR-0539](../adr/0539-memory-shape-gate.md). An improvement
warns and the job stays green ([ADR-0544](../adr/0544-memory-envelope-improvements-warn.md)).
`make compare-memory-shape` reads `target/memory-shape-loadtest.json`
against `baselines/memory-shape.json` and writes `target/memory-shape.md`
and `target/memory-shape-verdict.json`. Each shape section states
**Baseline** (the frozen min and max), **Validated** (this run), and
**Report** (the measured sizes and ratio against those boundaries).
`make baseline-memory-shape` runs the loadtest `BASELINE_RUNS` times
inside `create-loadtest-baselines`. Until that file is frozen, compare
reports `no-baseline` and exits 0.

Counts that miss the table are a regression. Logical bytes, disk, and
attributed RSS are lower-is-better, with a 15% band and a 1 MB floor.
The ratio is lower-is-better, with a 5% band. A numeric regression has
to clear the top by both the band and the floor. An improvement clears
the bottom the same way. Both warn and leave the job green until a new
envelope is taken. A count that misses the table still fails. An
environment-class or knob mismatch is `incompatible`: a local run skips
the numeric bands, and `CI_COMPARE=1` fails the job. The index counts
are knobs, so a run with indexes does not match a freeze taken at zero.

`baselines/memory-shape-loadtest.json` remains the worst-of-N file from
`make baseline-loadtests`. It is not this gate.

At 32 MiB and one attribute, with no indexes, the closed-form census the
unit tests pin is:

| Shape | Nodes | Relationships |
|---|---:|---:|
| flat | 121,574 | 0 |
| fanout | 51,920 | 51,904 |
| ring | 46,603 | 46,603 |
| dense ring | 5,010 | 100,200 |

That census is the pin for the formula. The RSS loadtest stops at the
fixed counts stated above this census.
`Large` in the unit tests still picks the node count whose closed form is nearest
the requested logical size. Ring, flat, and dense ring use
`(target + per/2) / per`.
Fanout searches the leaf count because a hub's adjacency vector reserves
the next power of two. One unindexed node with a 4-byte label and one
integer attribute is 276 bytes, including its version chain and property
vectors. A relationship with no properties is 236 bytes, and each
degree-1 adjacency entry is 104. Text, point, and range add the property
and the posting described above. A range index on an integer is one
posting (62 bytes on `Flat`/`id`). More attributes or indexes therefore
mean fewer nodes at the same target. Fanout keeps 16 hubs and an equal
leaf count per hub. Dense ring keeps degree 20.

## Base

![Empty graph: 0 nodes and 0 relationships](diagrams/base.svg)

No labels, no properties, no relationships. Logical `used` is 258. In the
RSS matrix this row is the server floor subtracted from every other
shape, so the ratio is the graph above that empty server.

## Ring

![Ring of 12 nodes. Node 11 links back to node 0.](diagrams/ring.svg)

A directed cycle. Node `i` is `(:Ring {id: i})` with one outgoing
`:NEXT` to `i + 1`, and the last id links back to 0. The drawing uses
12 nodes and highlights the wrap from 11 to 0. The small pin is 1,000
nodes and 1,000 relationships.

The loader creates ids in batches of 2,000, links each batch in id
order, stitches the previous batch's last node to the next batch's
first, and then closes the cycle. Nodes and relationships are always
equal.

Closed form: a 276-byte node (4-byte `Ring` label, one attribute) plus
its 236-byte relationship and two degree-1 adjacency entries (104 each),
720 bytes per node. At 32 MiB that is 46,603 nodes.

## Fanout

![Three hubs. Hub H1 links to leaves 3, 4, and 5.](diagrams/fanout.svg)

Hubs and private leaves. Hub `h` is `(:Hub {id: h})`. Each hub has the
same number of `:LEAF` relationships to `(:Leaf {id: h * leaves + l})`.
Leaves are not linked to each other. Extra attributes are set on both
hubs and leaves. The drawing is 3 hubs × 3 leaves, and hub 1's links
are highlighted: leaf ids `1×3+0`, `1×3+1`, and `1×3+2`.

The loader creates the hubs, then writes each hub's leaves on its own, in batches of 2,000. A statement does not cross hubs. The small pin is 4 hubs × 249 leaves: 1,000 nodes and 996 relationships.
The large shape keeps 16 hubs. At one attribute each hub is 275 bytes
plus one outgoing adjacency vector, and each leaf plus its relationship
and incoming hop is 616 bytes. At 32 MiB that is 3,244 leaves per hub
(51,904 leaves, 51,920 nodes).

## Flat

![Five isolated Flat nodes. Node 2 is expanded to its properties.](diagrams/flat.svg)

Nodes and no relationships. Every node is `(:Flat {id: i})` plus any
extra attributes. Nothing else is stored unless the run creates an
index that needs a property. The drawing expands node 2. The other
nodes carry the same fields with their own id.

The unindexed node is 276 bytes at one attribute. One thousand of them,
plus the 258-byte empty floor and the interned label and key, are the
276,392 pin. At 32 MiB the unindexed shape is 121,574 nodes. A range
index on `id` makes that pin 338,392. The index pin is the same 1,000
nodes with one text property, one point property, and a range index on
`id`, which is 742,526 bytes. Those counts are also legal on ring,
fanout, and dense ring.

## Dense ring

![Ring of 12 nodes. Node 11 links to nodes 0, 1, and 2.](diagrams/dense-ring.svg)

A directed ring where each node also skips ahead. Node `i` is
`(:DenseRing {id: i})` and has outgoing `:LINK` relationships to the
next `degree` ids, wrapping with modulo. The drawing uses 12 nodes and
degree 3 so the wrap is visible: node 11 links to 0, 1, and 2, and
every arrow points clockwise. Twelve nodes at degree 3 are 36 links.

The loader creates the nodes in batches of 2,000, then links them in
windows of at most 2,000 relationships (100 source nodes at degree 20).
The small pin uses degree 8: 1,000 nodes and 8,000 relationships,
2,713,465 logical bytes. The loadtest keeps degree 20 and sizes the ring
to the logical target. At 32 MiB that is 5,010 nodes and 100,200
relationships, 6,697 bytes per node.

## Memory controls and workspace memory calibration

`make memory-controls-test` verifies the memory controls and calibrates
workspace memory in the same ADR-0070 container as the shape suite
(`MEMORY_SHAPE_MEMORY`, default 2 GB). `Dockerfile.baseline`, with the
lane's bin list, builds `relate` and `memory-controls`. Every case
launches `relate serve` (ADR-0368). `BASELINE_ENV=host` is a debug
escape. Do not freeze a host RSS number. The container report is the
one the gate compares. `make processing-memory-loadtest` is the previous
name: it prints a rename notice and runs this target.

A **memory control verification** checks that a limit holds. The cases
are `connection-cap`, `unlimited-connections`,
`concurrent-logical-workspace-limit`, and the configured `workload`.
**Workspace memory calibration** is the case
`workspace-memory-calibration`. It measures RSS per live logical
workspace byte for each family.

The suite records cold and peak RSS. It does not apply a byte ceiling.
Idle `VmRSS` after later repeats must stay within 8 MiB of the first
idle sample. The child must stay alive. Stderr must not contain a
panic, stack overflow, or abort. The report also records cgroup
`memory.peak` when that file is present.

Each case prints Given, When, Then, and Outcome: the setup, the action,
the expected result, and what the run observed. It then prints Operating
RSS over Graph Logical, Graph RSS, and Processing Memory. Graph Logical
is the cold graph's admission meter.
Graph RSS is Baseline plus Floor: the cold empty process, sampled
before the graph is loaded, plus the RSS added by that graph.
Operating RSS remains Graph RSS plus Processing Memory.
Processing Min is the smallest idle sample above Graph RSS. Processing
Max is the child's later `VmHWM` minus Graph RSS. Operating RSS runs
from Graph RSS plus Min to Graph RSS plus Max.

`GRAPH_SCALE` sizes the ledger used by every case, including the
invariant probes. Scale 1 is 8 organisation levels and 12 accounts on
each level. Width grows with the scale. Depth stays at 8 until scale 5,
then gains one level every four steps. Accounts carry id, name,
currency, balance, status, opened, region, risk, and note. Organisations
are linked by `PARENT_OF`. Each account has two transfers to the next
level.

The configured workload is separate from the control verifications. It loads that
ledger with the limits off, restarts the same store with the requested
limits, samples Graph RSS, runs `WARMUP_OPS` uncounted, then `WORKERS`
Bolt clients share `OPS` operations. `READ_WRITE_RATIO` is `reads:writes`
(80:20 means 80 reads and 20 writes per 100 operations; a window that is
not a multiple of 100 is scaled so the counts sum to `OPS`).
`WORKLOAD_SEED` shuffles that mix. `INDEX_MODE` is `indexed`, `scan`, or
`both`. `READ_QUERY` is `mix` (the six-way read rotation), `deep-paths`,
or `size-sort`. The last two replace every read with that one statement.
`--max-connections` and `--concurrent-logical-workspace-limit` pass through to
the server. `0` is passed as `--max-connections 0`, the explicit unlimited
spelling (ADR-0451). An absent flag is the server default of 128. A positive
logical-workspace capacity turns admission on (ADR-0452). The value is logical bytes:
a former token capacity `N` is `N × 4096`. A `concurrent-logical-workspace-limit`
refusal counts as limited. Any other query error fails the run.

```text
make memory-controls-test \
  GRAPH_SCALE=10 \
  WORKERS=16 \
  READ_WRITE_RATIO=80:20 \
  MAX_CONNECTIONS=0 \
  CONCURRENT_LOGICAL_WORKSPACE_LIMIT=55829120
```

`CONCURRENT_LOGICAL_WORKSPACE_LIMIT` is the Makefile knob. `32768` is logical
bytes (a former token capacity of 8).

Defaults are scale 10, 16 workers, 80:20, `--max-connections 0`,
a concurrent logical workspace of 55829120 bytes, 40 operations,
8 warmup operations, seed 1, and both index modes. `WORKERS`, `OPS`, and
`WORKLOAD_SEED` are target-specific, so the fraud and identity lanes keep
their own values. The control verifications below keep their own caps and
do not inherit the workload limits. `CASE` runs one case.
`SKIP_CONTROLS=1` runs the workload only. `PROBE` and `SKIP_PROBES` still
select the same way; `connections` means `connection-cap` and
`peak-token-rss` means `workspace-memory-calibration`.

The bounded read is an id lookup and an 8-row page. The bounded write
marks one account and creates a review. The size stretch sorts every
account. The cost stretch walks the organisation spine and up to 4
transfer hops.

- Connection cap (`CASE=connection-cap`). `relate serve` with `--max-connections 2`
  hosts Bolt and HTTP on one cap. The Bolt session writes a review and
  the HTTP query reads one account. The next Bolt connection is closed,
  and the log names `bolt: --max-connections <N> reached`. A later
  connection works after one closes. A separate case passes
  `--max-connections 0` and shows connection 129 is still accepted, then
  reads and writes on one of those sessions. That case checks idle residue
  and a live child; it does not invent a session allowance for a cap that
  is off.
- Concurrent logical workspace (`CASE=concurrent-logical-workspace-limit`).
  The pool is sized for one statement, its cached plan, and one later
  statement. One session holds `RETURN 1`. The overlapping write returns
  `concurrent-logical-workspace-limit`, then the same session writes the review
  and reads the account. The running query stays in place.
- Workspace memory calibration (`CASE=workspace-memory-calibration`). Each observation starts a fresh server,
  loads and warms its graph, runs `RETURN 1` on the sessions that will
  execute the measured query, and samples parked RSS and cgroup memory.
  The same sessions then hold one named workspace family after its live
  reservation reaches a target. The matrix covers sort, expand,
  aggregation, distinct, and create workspace at three single-query
  sizes, then keeps the target total constant across two and four
  queries. Every shape repeats three times by default. The report keeps
  raw VmRSS and cgroup deltas, live held logical bytes, RSS bytes per logical byte, and
  linear fits by family. The cgroup ratio is the relevant physical
  boundary for a container; VmRSS isolates the server process.
  `CASE=workspace-memory-calibration` runs this matrix on its own.

`MEMORY_CONTROLS_PROFILE=pr` sets one repeat of every family, 25
observations. That is the pull-request lane. `MEMORY_CONTROLS_PROFILE=full`
sets three repeats, 75 observations. That is the nightly cadence and the
baseline freeze. A command-line `CALIBRATION_REPEATS` still wins.

The comparable run is the ADR-0070 Docker environment:

```bash
make memory-controls-test MEMORY_CONTROLS_PROFILE=full \
  CALIBRATION_HOLD_MS=2500
```

A bare `make memory-controls-test` runs the full matrix after the control
verifications: 5 families, 5 shapes, and 3 repeats, which is 75
observations. Each observation starts a fresh server, loads its graph, and
holds for `CALIBRATION_HOLD_MS`, so the matrix takes tens of minutes. It
prints one progress line per observation on stderr, with the elapsed time.
To skip it, use `SKIP_CONTROLS=1` (workload only) or pick another `CASE`.
To shrink it, use `CALIBRATION_FAMILY=sort` or `MEMORY_CONTROLS_PROFILE=pr`.

### The gate

`make compare-memory-controls` reads `target/memory-controls.json` against
`baselines/memory-controls.json` and writes `target/memory-controls.md`
and `target/memory-controls-verdict.json`. The document opens with
`**Status:**`.

- `ok` — every check held.
- `regression` — a behavioural expectation failed, or a lower-is-better
  metric rose above the envelope max by both the band and the floor.
  The numeric miss warns and the job stays green. A behavioural miss
  still fails the job
  ([ADR-0544](../adr/0544-memory-envelope-improvements-warn.md)).
- `improvement` — a lower-is-better metric fell below the envelope min by
  both the band and the floor. Behaviour has no improvement verdict.
  The comment warns, and the job stays green.
- `mixed` — the run has both a regression and an improvement. The job
  stays green when every regression is an envelope reading, and fails
  when one of them is behavioural.
- `incompatible` — the environment class or the knobs do not match the
  envelope. With `CI_COMPARE=1` that fails the job. A local run skips the
  numeric bands and still judges behaviour.
- `no-baseline` — the envelope is not frozen yet. The comment warns, and
  the job stays green.

RSS uses a 15% band and a 1 MiB floor. Each family is gated on its RSS
ratio and its cgroup ratio. The frozen min and max are the boundaries,
and a reading may sit 5% outside them. Slopes stay in the run report and
are not gated. Workload limited counts use a band of 0 and a floor of 2, so
fewer refusals is an improvement. An envelope miss warns and does not
fail the job. It still asks for an investigation: confirm it is real,
then re-freeze with
`create-loadtest-baselines` (`make baseline-memory-controls` inside that
workflow). `make memory-controls-gate` runs the suite and then compares.

`CALIBRATION_REPEATS` controls repeats per matrix point.
`CALIBRATION_HOLD_MS` controls how long every query remains paused while
its charged structure is live. Increase `MEMORY_SHAPE_MEMORY` rather
than accepting an OOM-truncated largest point. `BASELINE_ENV=host` is a
debug smoke test only. Do not freeze or copy a host-native quotient into
the product formula.

For one observation:

```text
rss_ratio =
  (held VmRSS - parked VmRSS) / sum(live held logical bytes)

cgroup_ratio =
  (held memory.current - parked memory.current)
  / sum(live held logical bytes)
```

The ledger log is sampled during the hold. A receipt's historical
high-water after workspace release is not a substitute for live held
logical bytes. The report's conservative cgroup ratio is the largest observed
raw ratio, not an automatic production-constant update. Inspect slopes,
intercepts, R², repeat spread, and equal-total concurrency points before
deciding whether one multiplier is adequate.

The run prints one outcome line per family: the retained memory of the
observation that set the ratio, in MB or GB, and the ratio itself. The
compare document states the frozen boundaries, the 5% slack, then that
measured size and ratio. The ratio is retained memory divided by the
live logical bytes of that observation. The envelope stores the min and
max of each family's largest RSS ratio and largest cgroup ratio.

The JSON summary still records the linear fits, including the per-holder
slope. Those fits are not gated. A large family ratio can
also identify resident executor input or transaction state that peak
logical bytes do not account for; do not hide that gap by treating the ratio as
allocator overhead without inspecting the plan. Create charges the fitted
property vector, the redo log's reserved slots, and the MATCH rows that
stay live until the write clause finishes. Grouped aggregation charges
the input rows until each one moves into the group map, and the input
vector's reserved slots until that vector drops.

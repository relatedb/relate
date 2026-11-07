# Memory management

Relate is a memory-first graph engine. Query execution reads a fully
resident property graph; disk provides durability through the WAL and
checkpoints, not an overflow tier. This gives predictable query access,
but it also makes memory a capacity boundary that the engine must expose
and enforce.

This document is the whole-system model. The linked ADRs remain the
authoritative record of individual decisions and their rationale:

- [ADR-0004](../adr/0004-memory-first-parquet-object-store.md) establishes
  the resident graph and durable checkpoint split.
- [ADR-0042](../adr/0042-graph-memory-limit.md), amended by
  [ADR-0202](../adr/0202-graph-floor-memory.md), defines and caps Logical
  Graph memory.
- [ADR-0289](../adr/0289-graph-shape-memory-pins.md) records RSS beside
  the logical meter for representative graph shapes.
- [ADR-0333](../adr/0333-processing-memory-controls.md), narrowed by
  [ADR-0352](../adr/0352-drop-processing-byte-limits.md), controls memory
  used while serving and operating on the graph. Session count is
  `--max-connections`. Query concurrency is admission by a positive
  `--concurrent-logical-workspace-limit` (ADR-0452;
  [Token budgets](token-budgets.md)).

The corresponding measurement procedures are in
[Memory testing](../testing/memory-testing.md).

## The memory model

We divide process memory by lifetime and purpose:

```text
Operating RSS
├── Graph Logical
├── Graph RSS
└── Processing Memory (RSS)
    ├── Min
    └── Max

Graph RSS     = Baseline RSS + Floor Memory (RSS)
Operating RSS = Graph RSS + Processing Memory (RSS)
```

**Graph Logical** is the cold graph's admission meter: live graph and
index bytes across every namespace. It sits beside Graph RSS so the two
can be compared. Operating RSS remains Graph RSS plus Processing Memory.

A process is **cold** when it is not processing a query or running an
operational task. WAL replay, index construction, checkpointing, and
snapshot writing are operational tasks. A process waiting after those
tasks have completed is cold.

**Baseline RSS** is the fixed cost of a cold process without
customer-owned graph data. It includes the runtime, empty-store
structures, and allocator state. The empty store interns `*` and `""`
for each namespace graph; those fixed names are treated as baseline in
RSS comparisons even though the logical counter accounts for them.

**Floor Memory (RSS)** is the additional resident memory of the cold
graph: nodes, relationships, values, adjacency, indexes, retained MVCC
versions, interned names, and the containers that own them. It is the
memory Relate keeps to make the graph immediately queryable.

**Processing Memory (RSS)** is the additional resident memory used to
serve or transform the graph. It includes sessions, query plans and
rows, transaction overlays, write and index-build scratch, and WAL and
checkpoint buffers. It is temporary by ownership, although an allocator
may retain freed pages in RSS.

**Min** is the processing RSS that remains after queries and operational
tasks have finished: parked session cost and allocator pages that were
not returned. **Max** is the high-water mark above Graph RSS while that
work is admitted. Operating RSS covers Graph RSS plus that range. The
planning formula below sizes a container for that max.

These RSS categories explain physical capacity. Admission control uses
deterministic accounting instead:

- **Logical Graph memory** estimates the resident graph and its owning
  structures. `--logical-graph-size-limit` bounds this meter.
- **Processing memory** is the RSS of query execution. With admission
  on, it is bounded by the concurrent logical-workspace pool. With
  admission off, query concurrency is unlimited.

Accounting and RSS deliberately answer different questions. Accounting
must be deterministic and computable before growth is admitted. RSS is
allocator- and platform-dependent, so it validates physical capacity
and detects gaps but is not itself an admission meter.

## Resident graph strategy

Nodes, relationships, properties, adjacency, and indexes remain resident.
The WAL makes a commit durable, and checkpoints bound replay and replace
old durable state. Neither is queried as a substitute for the resident
graph. A graph that cannot fit the configured memory envelope must be
rejected or deployed with a larger envelope; Relate does not
silently spill graph structures or query state to disk.

The logical graph meter follows ownership, not only payload:

- all retained node and relationship versions, including tombstones;
- map entries, version lists, property and label vectors, and adjacency
  vectors at their reserved capacities;
- the stub label cache, one entry per distinct label-id list, charged
  when that list is first materialized and kept after the nodes are
  deleted;
- label and opt-in index postings; and
- each distinct interned label, relationship type, and property name.

Payloads shared by query rows can use shared ownership to avoid repeated
deep copies, but sharing does not change the graph's logical charge.
Likewise, allocator fragmentation and pages retained after an allocation
is dropped do not change the logical meter. They appear in measured RSS
and in the allocator margin.

Every resident structure that grows with graph data must either be in
the logical formula or be identified as processing memory with a
separate lifetime and control. A cold, data-dependent structure missing
from both is an accounting gap.

Graph logical bytes are the owned resident graph floor. Query peak bytes
are the high-water mark of the owned resident query workspace. See
[Token budgets](token-budgets.md).

## Admission and reclamation

Memory is reserved before the operation crosses an irreversible or
expensive boundary.

For a graph mutation, Relate calculates the positive logical delta
from the redo and reserves it before appending the WAL record. Concurrent
commits see each other's reservations. A mutation that would exceed
`--logical-graph-size-limit` is rejected before it becomes durable; reads and
shrinking maintenance remain available so the operator can recover.

Open and WAL or checkpoint replay are different: recovery always
reconstructs durable state, even when that state is already above the
configured graph limit. Further growing writes are then refused. A
memory limit must never make an existing durable store impossible to
open for recovery.

Deletion does not necessarily reclaim floor memory immediately. MVCC
versions and tombstones remain charged while they remain resident, and
space becomes available when checkpoint garbage collection removes
them. This keeps the admission meter aligned with ownership rather than
claiming that a logical delete has already freed physical structure.

For processing work, admission reserves estimated peak logical bytes
before a statement runs and releases them when the statement ends,
including on error. Admission is off unless a positive concurrent
logical-workspace limit is set (ADR-0452).
Checkpoint work does not take a reservation. `EXPLAIN` does not either.

## Processing controls

Processing memory has several independent scaling vectors. A useful
limit names and bounds the cause rather than trying to impose a second,
unstable RSS counter.

| Control | Bounds | Default |
| --- | --- | --- |
| `--max-connections` | Sessions in the process; Bolt, HTTP, and MCP share one cap | Unlimited (`0` or unset) |
| `--concurrent-logical-workspace-limit` | Executing queries, by logical-workspace reservation. A positive value turns admission on (ADR-0452) | Off |

Each connection runs one query. `--max-connections` bounds how many
sessions exist. Admission, when on, bounds how much retained
work those queries may hold at once. See [Token budgets](token-budgets.md).

Each connection that has executed a query owns a query worker with a
64 MiB virtual stack
([ADR-0259](../adr/0259-query-execution-stack-provisioning.md)). Stack
pages become RSS only as they are touched. The fixed stack size is a
safety invariant calibrated with the query nesting limit, not an
operator control. Its committed idle residue is included in measured
session cost, and `--max-connections` bounds how many such session
costs can remain.

The memory request for the graph is:

```text
9 MB + Graph Logical × 1.7
```

9 MB is the empty-process baseline. Graph Logical × 1.7 sizes Graph RSS
with headroom over the admission meter. MB is 1024×1024 bytes. Sessions,
including idle ones, stay under `--max-connections`. Processing RSS for
executing queries is bounded only when admission is on; the host
then sizes that term from `concurrent_logical_workspace_limit` (logical
bytes) and its own RSS-per-logical-byte calibration. The processing-memory
loadtest measures that relationship in workspace-memory calibration: VmRSS
and cgroup memory while concurrent sort, expand, aggregation, distinct, or
create workspace holds a live reservation, minus those same sessions after
a trivial query, divided by the live logical workspace bytes held. Size
points test linearity; equal-total
one-, two-, and four-query points expose per-query overhead. The engine does
not multiply a per-query allowance. A host run is diagnostic. The controlled
container report supplies the calibration evidence, and a production formula
still uses a conservative envelope rather than rewriting itself from one run.

## Failure behavior

A resource refusal is part of normal admission, not a process failure:

- exceeding `--logical-graph-size-limit` rejects the graph-growing commit;
- a full logical-workspace pool (`concurrent-logical-workspace-limit`) is retryable;
- a running query that cannot grow inside that pool is the same retryable refusal;
- exceeding `--max-connections` closes the newly accepted connection
  before a session is created.

The error names the controlling limit. A rejected statement does not
commit, and an established session remains usable. Any path that
instead grows until allocation failure, panic, or OOM kill is a missing
admission control.

## Optimization policy

Floor and processing memory are optimized separately because they have
different lifetimes and controls.

For the floor:

- maximize useful graph data per Logical Graph MB;
- judge physical wins by cold Floor Memory (RSS), comparing the same
  graph shape in the same baseline environment;
- prefer compact ownership and shared immutable payloads over duplicate
  structures; and
- do not trade away query performance without workload evidence that
  the capacity gain is worth it.

For processing:

- stream or batch work so retention follows bounded units;
- release reservations and large temporary ownership at the end of the
  operation;
- give every input-dependent growth vector an admission control; and
- include allocator residue in measurement rather than pretending it
  was returned to the operating system.

Changes to a logical formula update exact logical pins. Changes expected
to alter physical footprint are also evaluated against shape-specific
RSS. Query workloads validate the processing controls, typed failures,
and post-work idle RSS.

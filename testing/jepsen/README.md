# Jepsen harness for Relate

A standalone Clojure project (outside the Cargo workspace) running
[Jepsen](https://jepsen.io) against Relate's network surfaces. One topology — a
control container and a single DB node (the **stock product image** `make docker`
builds, n=1 because two containers cannot write one store) — crossed by four independent
axes of lanes:

- **Workloads** (`--workload`): `set` (v1, LABS-650, the crash-durability family),
  `counter` / `bank` (LABS-652, the Elle transactional lanes), `compare` (LABS-657, the
  Neo4j differential — on demand only).
- **Fault schedules** (`--nemesis`, set family): `kill9` (v1's default), `pause`,
  `sigterm-kill9`, `mid-checkpoint` (LABS-651), `poweroff` (LABS-654, the lazyfs rig).
  The Elle family takes `kill` or `none`.
- **Durability modes** (`--lane`, set family): `full` (default), `everysecond`,
  `snapshot` (LABS-653, ADR-0470), plus the `memory-vfs` negative control.
- **Surfaces** (`--surface`): `bolt` (v1) and `http` (LABS-655, the HTTP Query API v2).

## What the harness tests

- **Topology** (docker-compose.yml): a control container (JVM 21, Clojure CLI, this
  harness) and one DB node — the **stock product image** `make docker` builds, running
  `relate serve` on a persistent `/data` volume with preset auth and
  `--bolt-identity Neo4j/5.26.0` (the official driver 5.x rejects any agent not starting
  `Neo4j/` — LABS-649; the harness asserts the value verbatim at every Bolt connection).
  The image's entrypoint appends the HTTP port itself (7474), so the http lane needs no
  compose change.
- **Bolt clients**: 5+ concurrent clients over the official Java driver (plain interop,
  pinned 5.28.5), **explicit transactions** (the ack boundary is COMMIT SUCCESS; a
  received FAILURE is a definitive fail; a connection error is `:info` — uncertain).
  Never managed `executeWrite` — it auto-retries the write-write conflict code and would
  mask it.
- **Fault verification**: every fault is **verified from the client side on the lane's
  own wire** — container state from `docker inspect`, the exit-code signature, and a
  wire probe that must fail while the fault holds and succeed with the lane's identity
  gate on recovery (against a frozen server the kernel still completes the TCP handshake
  into the process's backlog, so the probe dies at the driver's 5s connection timeout —
  bounded, never a hang) — and the fault-evidence checker fails the run unless every
  fault fired and every recovery came back. jepsen is pinned at 0.3.15 (0.3.12–0.3.14's
  `grepkill!` made kill nemeses silent no-ops).

## The lanes

### The set family (`--workload set`, v1 LABS-650)

The crash-durability test: 5+ concurrent clients CREATE nodes with unique user-visible
`id` properties, checked by **final-state set equality** — `acked ⊆ recovered ⊆
attempted`, plus no ghosts and no survivors of definitively-rejected writes — with the
contract the `--lane` selection picks (below).

A pass certifies narrowly: *for this workload, fault schedule, and duration,
acknowledged writes survived every fault in the schedule and no fabricated rows
appeared.* Jepsen proves presence of bugs, never absence.

### The Elle lanes: counter / bank (LABS-652)

Isolation under concurrency, **without faults first** (the MySQL 8.0.34 precedent —
`--nemesis none` is the lanes' default; `--nemesis kill` adds the same crash cycle as
the set lane on top). Both lanes: 5+ concurrent clients, 5+ minute runs, explicit
transactions only, ack = COMMIT SUCCESS, `Transaction.Outdated` (write-write conflict) =
definitive `:fail`, connection loss = `:info`.

- **counter** (`--workload counter`): Elle list-append over a 4-8 key pool
  (`--counters`, default 4) of `:Counter` nodes — one op = one explicit txn of 1-3
  micro-ops (`[:r k log]` reads, `[:append k v]` appends with globally unique integers;
  a txn reads key k only if it also appends to k). Ops are already in jepsen txn form,
  so elle.list-append consumes the history directly.
- **bank** (`--workload bank`): classic transfers over 16-32 `:Account` nodes
  (`--accounts`, default 16; `--max-transfer`, default 5) — read both balances,
  client-reject (ROLLBACK) unless `fb >= amount`, write both, COMMIT. Conservation and
  non-negativity via jepsen's own bank checker (there is no elle.bank namespace). For
  the Elle side, the account is an rw-register whose value is the node's `ver` tag (a
  globally unique counter), NOT the balance: balances in a conserved bank repeat, and
  elle's rw-register hard-asserts unique write values per key — the version tag
  preserves the transfer's isolation structure (read both accounts, write both accounts)
  exactly while keeping writes unique by construction.

The **curated anomaly set** (see `src/jepsen/relate/elle.clj`):
`elle.list-append/check` / `elle.rw-register/check` with `{:consistency-models
[:snapshot-isolation :strong-session-snapshot-isolation]}` — the model vector IS the G2
/ write-skew whitelist (SI permits G2; ADR-0015/0028 deliberately ship SI without SSI;
strong-session holds because one Bolt connection = one strictly ordered session). The
lanes additionally GATE on `[:serializable]`: every transaction in both workloads reads
everything it writes, so concurrent committed transactions have pairwise-disjoint write
sets and first-committer-wins (node-identity conflict granularity — measured, see below)
makes the SI history serializable in commit order. `[:strict-serializable]` runs as
**discovery only** — recorded, never failing the run.

A `client-hygiene` checker fails any run where a client completion hit a harness-class
error (invariant guard, unrecognized throw) or a connection-class error with no faults
in play — an all-erroring run is red, not quietly green.

### The compare workload (LABS-657) — on demand only

The Neo4j differential: a test.check-style random generator over the **shared openCypher
floor** (the TCK-pinned language both engines implement — no Neo4j-only syntax, no
Relate preview features) executing every statement against BOTH the Relate DB
node and a real **neo4j:5.26.0** reference container, comparing results, and shrinking
every divergence to a minimal Cypher case. It is the cheapest-bug-finder lane: no
nemesis (a fault would only manufacture spurious divergences), one client, strictly
sequential ops. **It runs on demand, never per-PR.**

What it generates (all as data, rendered to Cypher — the data is what the shrinker
shrinks):

- **Graphs**: 3–8 nodes (tiny label/name pools so duplicates and collisions are common;
  `num`/`score`/`active` properties each absent a third of the time), up to 12
  relationships (10% self-loops). Every node carries a unique `nid` and every
  relationship a unique `rid` — comparison never touches engine-internal ids.
- **Reads** (~70%): eight templates — node/chain MATCH with WHERE (comparisons, string
  predicates, IN, IS NULL, NOT), variable-length paths, OPTIONAL MATCH, aggregates
  (`count`/`sum`/`min`/`max`/`avg` over integers), a WITH aggregation stage,
  comma-separated patterns, UNWIND — with DISTINCT, ORDER BY, SKIP, LIMIT, and
  occasional parameters.
- **Writes** (~30%): SET / REMOVE / (DETACH) DELETE by nid, each followed by a canonical
  state read (every node and relationship keyed by nid/rid, deterministic order)
  compared exactly.

False-positive guards baked into the generator: rows compare as **multisets** (row order
is unspecified by Cypher), numbers compare numerically (`1 = 1.0`), floats are exact
binary fractions never accumulated (no float sum/avg, no `collect()` — those are
row-order dependent), WHERE operands are same-typed (a cross-type comparison errors on
both engines, which is a generator observation, not a divergence).

What counts as a divergence: different rows, or success on one engine and failure on the
other. An error on **both** engines is not a divergence (error text is not part of the
floor contract) but is counted and reported. On a divergence the client shrinks — drop
setup statements, WHERE predicates / RETURN items / ORDER BY / labels / property
constraints, simplify literals — re-executing every candidate against both engines,
records the minimal case in the log, on the op, and in
`store/relate-compare-<seed>/<ts>/divergences.json`, then resynchronizes both
engines from the statement history (the divergent statement excluded) and keeps going.
Found divergences are **recorded, never auto-filed**: a human reads the minimal Cypher
pair and both engines' outcomes and files what matters.

#### First run's findings (seed 657, 300 ops)

The lane's first 300-op run
([`runs/compare-657-found-divergences.log`](runs/compare-657-found-divergences.log.gz),
[`runs/compare-657-divergences.json`](runs/compare-657-divergences.json)) ran 311 ops
mechanically clean (298 ok, 13 divergences, zero both-errors, zero :info) and found
three divergence classes, each already shrunk to a zero-data minimal case:

1. **ORDER BY after aggregation** — `MATCH (b) RETURN count(*) AS c ORDER BY b.num
   DESC`: Relate returns a row; Neo4j errors ("not possible to access variables
   declared before the WITH/RETURN"). Relate accepts an ORDER BY on a
   pre-aggregation variable; Neo4j rejects it.
2. **DISTINCT + ORDER BY loses the pattern variable** — `MATCH (b) RETURN DISTINCT
   b.name AS w ORDER BY b.name DESC`: Relate errors `UndefinedVariable: variable 'b'
   not defined` (b *is* defined by the MATCH); Neo4j returns rows.
3. **Zero-argument scalar functions** — `RETURN size()` / `RETURN toString()`:
   Relate returns `null`, Neo4j errors ("Insufficient parameters"). Found by the
   shrinker emptying argument lists — degenerate input, but a real arity-validation gap.

### Fault schedules (`--nemesis`, set family)

One table in `src/jepsen/relate/schedule.clj` — the ONE home every layer consults
(the nemesis's knobs, the DB setup guard's checkpoint interval, the fault-evidence
checker's row). The Elle lanes take the other two words: `kill` (the set family's crash
cycle on top) or `none` (their default); cross-family spellings error loudly.

| Schedule | Fault cycle | Extra pin |
| --- | --- | --- |
| `kill9` (default) | `docker kill` (SIGKILL to PID 1 — the entrypoint execs into serve, so the container IS the process; no final checkpoint) / `docker start` (WAL replay) | — |
| `pause` | `docker pause` (the cgroup freezer: serve stays alive, mid-write, still holding the `.cypherlite.writer` flock) / `docker unpause` (thaw in place) | **flock pin**: while frozen, a second serve against the same `/data` store must exit nonzero with the typed `storage locked:` error (ADR-0097) |
| `sigterm-kill9` | clean-to-crash: SIGTERM (serve's clean shutdown + final checkpoint, exit 0) / restart / kill -9 / restart | the SIGKILL signature (exit 137) AND the clean-shutdown signature (exit 0 + the `serve: shutting down` line NEW in stderr since the signal — the log is cumulative across restarts, only the suffix counts) |
| `mid-checkpoint` | kill -9 against a serve checkpointing every 5s (`RELATE_CHECKPOINT_INTERVAL_SECS=5`, compose-interpolated, asserted at setup), kills jittered uniformly over the checkpoint period | the manifest's `checkpoint_lsn` must strictly advance between every restart and the next kill — under SIGKILL there is no final checkpoint and none at open, so the 5s timer is the only possible writer |
| `poweroff` (LABS-654) | the lazyfs rig's whole-machine cut — the `lazyfs::clear-cache` fault over the fault FIFO, then `docker kill` (below) | every power-off must have delivered the clear-cache fault (a plain kill certifies nothing about un-fsynced bytes) before the container died with exit 137 and Bolt unreachable |

### Durability lanes (`--lane`, set family)

One workload, one fault schedule, four contracts. `--lane` (run.sh's `LANE`) selects
both the DB configuration and the checker; run.sh passes the same values to compose
(`RELATE_DURABILITY_MODE`, `RELATE_DATABASE`,
`RELATE_CHECKPOINT_INTERVAL_SECS`) and to the harness, and the harness's setup
asserts the container really runs them (db.clj's config guard) — a lane whose mode env
never reached the container would silently test full durability and read green.

| Lane | DB config | Contract (`checker.clj`) |
| --- | --- | --- |
| `full` (default) | mode `full`, store `/data/graph` | `set-final-state`: acked ⊆ recovered ⊆ attempted, no ghosts, no survivors of definitively-rejected writes (v1 verbatim) |
| `everysecond` | mode `everysecond`, store `/data/graph` | `durability-bound 1`: acked writes acknowledged within ADR-0077's ~1s loss window before a fired kill MAY be lost; older acks must survive. The bound is the assertion — zero loss also passes |
| `snapshot` | mode `snapshot`, store `/data/graph`, small `RELATE_CHECKPOINT_INTERVAL_SECS` (lanes.sh uses 2) | `durability-bound <interval>`: acks since the last completed checkpoint may be lost; an ack older than the interval must survive |
| `memory-vfs` | mode `full`, store `:memory:` | `memory-vfs-control`: the negative control — the same fault against a store with no durable medium must LOSE acked writes. Valid only when the full-mode invariant fails that way |

The kill instants the bound checkers compare against are the nemesis's `:killed-at`
evidence — captured on the history clock immediately before `docker kill` (nemesis.clj),
not the post-verification op time.

Under `docker kill` the host and its page cache survive, so the relaxed lanes are
expected to observe **zero loss** — the WAL appends reach the file even without the
barrier, and only a host/OS/storage failure loses them. That is why the lanes assert the
BOUND, never the presence of loss; observing actual relaxed-mode loss needs a
storage-level fault (the lazyfs power-off lane, below). The memory-vfs control is what
calibrates the fault itself here: SIGKILL against a store with no durable medium loses
everything acknowledged before the last kill, so a control run that loses nothing means
the fault machinery is a no-op and every green lane above it is vacuous — the
Jepsen-Lite principle: a fault that cannot fail a known-bad config cannot validate a
good one.

A pass certifies narrowly: *under SIGKILL during commit and WAL replay, for this
workload, fault schedule, and duration, the lane's contract held.* Jepsen proves
presence of bugs, never absence.

### The http surface (`--surface http`, LABS-655)

The HTTP Query API v2 surface ([spec](../../docs/api/http/spec.md), ADR-0232), running
the set workload AND a register workload under the same kill nemesis, with every fault
verified on the HTTP wire.

- **Client choice, verified**: no official Java-driver version speaks this surface.
  neo4j-java-driver 5.28.5 — the latest on Maven Central at 2026-11-01, and the version
  the bolt lane pins — is Bolt-only: its URI scheme table
  (`org.neo4j.driver.internal.Scheme`) enumerates exactly `bolt`, `bolt+s`, `bolt+ssc`,
  `neo4j`, `neo4j+s`, `neo4j+ssc`, and neither it nor anything else in the
  `org.neo4j.driver` group ships a Query API HTTP transport (verified by unpacking the
  5.28.5 jar and searching the group's artifacts). The lane therefore speaks the wire
  itself: the JDK's `java.net.http` for HTTP/1.1 plus `org.clojure/data.json` for the
  envelopes — no other dependency.
- **Workload**: the set workload as one auto-commit `POST /db/neo4j/query/v2` per op
  (CREATE with a unique `id` parameter; ack = the fully consumed HTTP 200 envelope), and
  the register workload — auto-commit last-write-wins `SET` on one durable register node
  (created by a setup write after the hermetic wipe), with the final value checked to be
  a plausible write (an acked or uncertain write, or the initial value) and never a
  ghost or a definitively-rejected write. The HTTP surface is auto-commit per request —
  explicit transactions over HTTP are a later lane (LABS-647). A definitive non-200
  answer is `:fail`; a transport-level failure is `:info`.
- **Identity gate**: the HTTP surface has no server agent string to pin like Bolt's; the
  gate here is the public `GET /` discovery document, which must advertise a query
  endpoint ending `/db/{databaseName}/query/v2` (asserted at client open and in every
  probe), plus a probe statement whose answer is verified.

### The lazyfs power-off lane (`--nemesis poweroff`, LABS-654)

The kill schedules drop the process but keep the whole volume — a crash where the
*storage medium* never loses anything. The power-off lane makes the medium honest: the
DB node's store sits on [LazyFS](https://github.com/dsrhaslab/lazyfs) (VLDB'24, pinned
at commit `fa7d32e`), a FUSE filesystem mounted INSIDE the DB container whose backing
root is the `/data` volume. Un-fsynced bytes live only in lazyfs's userspace memory;
only fsynced bytes ever reach the backing root. The power-off nemesis is the verified
recipe: the `lazyfs::clear-cache` fault over the fault FIFO (drop every un-fsynced
byte), then `docker kill` — SIGKILL of the whole container kills serve AND lazyfs
together, so nothing further can be flushed. `docker start` remounts lazyfs on the
surviving backing root and WAL replay recovers exactly the fsynced commits.

- **Topology**: docker-compose.lazyfs.yml (merged over the stock docker-compose.yml by
  run-poweroff.sh ONLY — the stock project never touches FUSE) swaps the DB node for the
  lazyfs rig (db-lazyfs.Dockerfile: the SAME product binaries copied out of
  `relate-bolt:latest`, on an ubuntu:24.04 userspace that mounts lazyfs) running
  privileged with `/dev/fuse`, its store on the lazyfs mount (`/mnt/lazyfs/graph`). The
  durability mode and checkpoint interval travel on the stock compose env channel
  (`RELATE_DURABILITY_MODE`, `RELATE_CHECKPOINT_INTERVAL_SECS`) and the harness
  gets the same values as `--lane` / `--checkpoint-interval-secs`, so the setup guard
  asserts the container's mode, store path, and interval against the lazyfs rig's row
  (db.clj) — under `--nemesis poweroff`, `--lane` names only the DB node's mode and the
  check stays the full contract. The whole project is namespaced (`JEPSEN_SUFFIX`,
  default `poweroff`: project `relate-jepsen-poweroff`, DB container, data volume)
  — the feature's lanes run concurrently on one shared docker daemon, and the base
  project's default names would make two lanes' compose projects destroy each other's
  DB node mid-test. In
  full durability mode the WAL fsync happens BEFORE the commit ack, so even a power-off
  between fsync and ack loses nothing acked — the lane proves exactly that boundary.
- **The control** — why snapshot mode and not Memory VFS: Memory VFS cannot sit on
  lazyfs at all (the store never touches the mount, so the lane's fault machinery — the
  one thing a control must exercise — would be bypassed, and a plain `docker kill`
  without lazyfs would lose everything trivially). The honest control is
  durability mode `snapshot` with a 3600s checkpoint interval: the same image, the
  same lazyfs mount, the same nemesis, only the durability policy changed — acked writes
  live un-fsynced in lazyfs's memory, and the power-off must lose them. A green control
  means the lane cannot detect loss and certifies nothing; run-poweroff.sh fails on that
  as hard as on a full-mode loss.
- **The lane is Linux-CI-shaped**: it requires a FUSE-capable docker host (GitHub ubuntu
  runners and Docker Desktop's LinuxKit VM expose `/dev/fuse`; a plain macOS compose
  host does not). run-poweroff.sh probes `/dev/fuse` before anything privileged runs and
  skips loudly — exit 77 with the reason on stderr — where it is absent, then runs a
  full host-semantics probe (fsynced bytes reach the backing root, un-fsynced bytes
  never do, clear-cache + SIGKILL drops exactly the un-fsynced ones) inside the DB image
  before every invocation.

A pass certifies narrowly: *under whole-machine power-off (clear-cache + SIGKILL of
serve and its storage layer together), for this workload, fault schedule, and duration,
every acknowledged write had already been fsynced and survived, and no fabricated rows
appeared.*

## The conflict-granularity measurement

`clojure -M:granularity [bolt-uri user password]` answers, by measurement, whether two
concurrent explicit transactions SETting **different properties** of the same node
conflict or commit independently (ADR-0015 §4 reads as node-identity granularity). The
scenario is deterministic — no races: A begins and reads the probe, B SETs property b
and commits, A SETs property a and commits; the measurement is A's commit outcome, with
a same-property control proving the rig observes conflicts at all.

Measured (2026-11-01, product image built at the branch tip): **CONFLICT — node identity
granularity.** A's second COMMIT fails with `Neo.TransientError.Transaction.Outdated` in
every iteration, exactly like the same-property control. Evidence:
[`runs/granularity-measurement.log`](runs/granularity-measurement.log). (The log's `?`
characters are em-dashes lost to the container locale.)

This is the premise the bank lane's SI≡serializable argument and the write-write
conflict mapping rest on: any two transactions writing the same node conflict regardless
of property.

## Running it

Prerequisites: Docker Desktop (or any docker + compose v2), and the product image built
from the repo root:

```sh
make docker            # builds relate-bolt:<version> and :latest
```

The image must carry the ADR-0470 durability-mode wiring — the harness's setup probe
(db.clj) runs a throwaway container with an invalid `RELATE_DURABILITY_MODE` and
fails loudly unless the binary refuses it, so a stale image can never silently degrade a
lane.

From this directory, the one-shot wrappers (build the control image once, start the DB
node with the lane's compose env, run the test, tee the log under `runs/`):

```sh
./run.sh               # set lane, bolt: 5 clients, 70s, kill9 (default)
./run-http.sh          # http surface: 5 clients, 70s, kill9
NEMESIS=pause ./run.sh            # pause/unpause + flock pin, 70s
NEMESIS=sigterm-kill9 ./run.sh    # clean-to-crash cycles, 110s
NEMESIS=mid-checkpoint ./run.sh   # 5s checkpoints + timed kills, 110s
LANE=everysecond ./run.sh
LANE=snapshot CHECKPOINT_INTERVAL_SECS=2 ./run.sh
LANE=memory-vfs ./run.sh          # the negative control
WORKLOAD=counter ./run.sh         # Elle lanes: 5 clients, 330s, no faults
WORKLOAD=bank ./run.sh
WORKLOAD=bank NEMESIS=kill TIME_LIMIT=70 ./run.sh   # bank under crashes
./run.sh compare                  # 300 ops vs the Neo4j reference (on demand)
OPS=1000 SEED=657 ./run.sh compare    # bigger sweep, reproducible seed
```

Shared env knobs: `WORKLOAD`, `NEMESIS`, `LANE`, `TIME_LIMIT`, `CONCURRENCY`,
`NEMESIS_INTERVAL`, `CHECKPOINT_INTERVAL_SECS`; Elle/compare knobs: `COUNTERS`,
`ACCOUNTS`, `MAX_TRANSFER`, `OPS`, `SEED` (a fresh timestamp by default, recorded in the
test name — pass `SEED` to replay a recorded run exactly); the image via
`RELATE_IMAGE`. Extra argv passes through to the harness CLI.

The whole ordered durability matrix — the Memory VFS control FIRST (its evidence must be
on record before any full-mode green is counted), then the relaxed lanes, then the
full-mode green — aborting on the first red lane:

```sh
./lanes.sh
```

lanes.sh builds and pins `relate-bolt:jepsen-lanes` from the repo root (a private
tag, so parallel harness work on a shared docker daemon never races the `make docker`
tags) and exports it as `RELATE_IMAGE`; a one-off `./run.sh` defaults to the stock
`relate-bolt:latest`.

The lazyfs power-off lane (FUSE-capable host required — see above):

```sh
./run-poweroff.sh      # probe + 5 full-mode power-off runs (green)
                       # + snapshot control (must lose acked writes)
```

Env knobs and exit codes are documented in run-poweroff.sh's header (`FULL_RUNS`,
`TIME_LIMIT`, `CONCURRENCY`, `NEMESIS_INTERVAL`, `CHECKPOINT_INTERVAL_SECS`,
`CONTROL_MODE`, `CONTROL_CHECKPOINT_INTERVAL_SECS`, `SKIP_PROBE`, `JEPSEN_SUFFIX`; exit
`77` = skipped, no FUSE on this host). By hand, the lane is `--nemesis poweroff --lane
<mode> --checkpoint-interval-secs <n> --test-count 5` on the same CLI, with the
names `lib/isolation.sh` derives for `JEPSEN_SUFFIX=poweroff` and the DB node created
under the matching `RELATE_DURABILITY_MODE` / `RELATE_CHECKPOINT_INTERVAL_SECS`
(see the namespacing note in docker-compose.lazyfs.yml). Rebuild
the DB image — `docker build -f db-lazyfs.Dockerfile -t
relate-jepsen-db-lazyfs:latest .` — whenever `make docker` rebuilds the product
image: the rig copies the product binaries out of `relate-bolt:latest` at build
time.

Or by hand, any lane (`docker compose build control` first; the compare lane needs the
reference service too — `--profile compare up -d db neo4j`, `clojure -M:run test
--workload compare --ops 300 --seed 657`, `--profile compare down`):

```sh
docker compose up -d db
docker compose run --rm control \
    clojure -M:run test --surface http --concurrency 5 --time-limit 70
docker compose down
```

The checker contracts themselves are unit-tested without docker: `docker compose run
--rm --no-deps control clojure -M:test`.

Exit codes are jepsen's: `0` all checks passed, `1` a check failed, `2` unknown, `254`
bad arguments. First run downloads the Clojure/Maven deps into the m2 volume (a few
minutes); later runs are cached.

CI runs the set lane as a dedicated workflow (LABS-734):
`.github/workflows/jepsen.yml`, not in the per-PR matrix. Main is certified nightly at 04:50 UTC
(LABS-734's cadence; weekly later is a one-line cron change), and any ref can
be certified on demand: `gh workflow run jepsen.yml --ref <branch-or-tag>` (`time_limit` input, default
90; a `ref` input certifies any ref). It builds the product image on the runner
(`make docker`, the docker-smoke buildx recipe), runs the checker-contract
suite, then `./run.sh` — failing on every nonzero jepsen verdict, with the run
log and store dir uploaded as the `jepsen-run` artifact on failure. PRs keep
the `jepsen-harness-sanity` job in `ci.yml`: a `docker compose config`
validation of the topology.

## Run evidence and store layout

`./report.sh [RUNS_DIR]` renders the newest timestamped run log as a markdown report
(verdict, headline counts, the harness's results map). The Jepsen workflow appends it to
the run's step summary — on the workflow-run page, green or red — and attaches the log as
the `jepsen-run-log` artifact; a failed run also uploads the store as `jepsen-run`.

Output lands here in the repo (the control container bind-mounts it):

- `runs/<timestamp>-<lane>.log` — the full run log (analysis, verdict); timestamped logs
  are gitignored. The committed evidence, each green run the second of a CONSECUTIVE
  pair wherever the data volume persists — the non-virgin volume the first run left
  behind is the path a wipe bug hides in (a virgin volume is green even when the wipe is
  a silent no-op):
  - v1 (set, bolt): [`runs/first-green.log`](runs/first-green.log) then
    [`runs/second-consecutive-green.log`](runs/second-consecutive-green.log), plus
    [`runs/bolt-post-refactor-green.log`](runs/bolt-post-refactor-green.log) — a green
    run re-taken after the surface-dispatch refactor (the default bolt invocation,
    `./run.sh` with no `--surface` flag) so the pre-existing lane is evidenced on the
    post-refactor code too.
  - the http surface: [`runs/http-first-green.log`](runs/http-first-green.log) then
    [`runs/http-second-consecutive-green.log`](runs/http-second-consecutive-green.log).
  - the nemesis widening (LABS-651):
    [`runs/nemesis-pause-green.log`](runs/nemesis-pause-green.log),
    [`runs/nemesis-sigterm-kill9-green.log`](runs/nemesis-sigterm-kill9-green.log.gz),
    [`runs/nemesis-mid-checkpoint-green.log`](runs/nemesis-mid-checkpoint-green.log.gz).
  - the durability matrix (LABS-653), in evidence order — the negative control's loss
    first, the full-mode green last:
    [`runs/lane-1-memory-vfs-control.log`](runs/lane-1-memory-vfs-control.log),
    [`runs/lane-2-everysecond.log`](runs/lane-2-everysecond.log),
    [`runs/lane-3-snapshot.log`](runs/lane-3-snapshot.log),
    [`runs/lane-4-full.log`](runs/lane-4-full.log).
  - the Elle lanes (LABS-652):
    [`runs/elle-counter-green.log`](runs/elle-counter-green.log.gz),
    [`runs/elle-bank-green.log`](runs/elle-bank-green.log.gz) (5 clients, 330s, no faults,
    both gates green, strict-serializable discovery green),
    [`runs/elle-counter-kill9.log`](runs/elle-counter-kill9.log.gz) (the same lane under
    the kill nemesis — 254 uncertain `:info` ops, gates still green), and
    [`runs/granularity-measurement.log`](runs/granularity-measurement.log).
  - the compare lane (LABS-657):
    [`runs/compare-657-found-divergences.log`](runs/compare-657-found-divergences.log.gz)
    with [`runs/compare-657-divergences.json`](runs/compare-657-divergences.json).
  - the power-off lane (LABS-654): [`runs/poweroff-green.log`](runs/poweroff-green.log.gz)
    (5 consecutive hermetic full-mode power-off runs, zero acked writes lost, every
    power-off's clear-cache fault delivered and verified) and
    [`runs/poweroff-control-lost-acked.log`](runs/poweroff-control-lost-acked.log.gz) (the
    snapshot control under the same rig and nemesis: all 275 acked writes lost, faults
    verified — the lane can detect loss).

  Every committed log over 1,000 lines rides gzipped as `<name>.log.gz`
  (`gzip -9`) so no tracked file crosses the 1k-line rule; the run output
  itself is unchanged. Read one with
  `gunzip -c testing/jepsen/runs/<name>.log.gz` — the portable spelling
  (macOS `zcat` only looks for `.Z`).
- `store/<test-name>/<timestamp>/` — jepsen history, serialized test, and
  `db-container.log` (the DB node's serve stdout + stderr MERGED — docker logs routes
  the container's stderr to the CLI's stderr, and serve prints everything that matters
  there; a stdout-only capture silently loses the banner, storage-lock errors, and the
  shutdown line). Test names: `relate-set-<nemesis>[-http]-<lane>` for the set
  family (surface and lane disambiguated — two concurrent lanes must never share a store
  directory), `relate-counter[-kill9]` / `relate-bank[-kill9]` for the Elle
  lanes, `relate-compare-<seed>` for the compare lane (carrying `divergences.json`).
  Gitignored.

## Layout

| Path | What |
| --- | --- |
| `deps.edn` | jepsen 0.3.15, elle 0.2.8, neo4j-java-driver 5.28.5, data.json 2.5.1; `:run`, `:test`, and `:granularity` aliases (all `-Djava.awt.headless=true` — requiring elle loads rhizome.viz, whose top-level AWT def is guarded by exactly that property) |
| `docker-compose.yml` | control + DB node (stock product image, lane env, /data volume); `neo4j` reference behind the `compare` profile; every identifier env-parameterized for lane isolation |
| `docker-compose.lazyfs.yml` | power-off lane override: DB node on the lazyfs rig (privileged, /dev/fuse) |
| `control.Dockerfile` | control image: JVM 21 + Clojure CLI + gnuplot/graphviz/docker CLI |
| `db-lazyfs.Dockerfile` | the power-off lane's DB image: pinned lazyfs build + the stock product binaries |
| `lazyfs/lazyfs-mount.sh` | the one lazyfs mount helper (mount, wait for /proc/mounts + FIFO) |
| `lazyfs/lazyfs-entrypoint.sh` | the DB node's entrypoint: mount lazyfs, exec the stock supervisor |
| `lazyfs/lazyfs-probe.sh` | the host semantics probe (fsynced survives, un-fsynced drops) |
| `run.sh` | one-shot run, every workload/nemesis/lane combination (env-selected) |
| `run-http.sh` | one-shot green run, http lane (isolated compose identifiers) |
| `run-poweroff.sh` | the power-off lane: FUSE guard, probe, full runs, control |
| `lib/isolation.sh` | the one JEPSEN_SUFFIX → compose-names derivation every wrapper sources |
| `lanes.sh` | the ordered four-lane durability matrix (control first, full green last) |
| `src/jepsen/relate.clj` | test definition, workload/surface/lane dispatch, generator, CLI |
| `src/jepsen/relate/schedule.clj` | the nemesis schedule table — the ONE home every layer consults |
| `src/jepsen/relate/client.clj` | shared Java-driver transport: identity pin, explicit-txn runner, error taxonomy |
| `src/jepsen/relate/http_client.clj` | raw HTTP Query API v2 client (java.net.http + data.json, discovery identity gate) |
| `src/jepsen/relate/workload.clj` | the shared set + register workload statements and the shared concurrent-phase generator shape |
| `src/jepsen/relate/set.clj` | the v1 set workload (generator, handlers, checkers) |
| `src/jepsen/relate/counter.clj` | the Elle counter lane |
| `src/jepsen/relate/bank.clj` | the Elle bank lane |
| `src/jepsen/relate/elle.clj` | the curated anomaly set as jepsen checkers |
| `src/jepsen/relate/granularity.clj` | the conflict-granularity measurement program |
| `src/jepsen/relate/nemesis.clj` | the fault nemeses (kill/SIGTERM/pause/poweroff) with per-fault verification, the storage-lock probe, and the checkpoint-LSN sampler |
| `src/jepsen/relate/db.clj` | DB lifecycle: wipe volume, lane config guard, ADR-0470 wiring probe, checkpoint-env guard, restart, identity gate, workload setup; the per-surface dispatch table (`surface-impl`) |
| `src/jepsen/relate/checker.clj` | set final-state, loss bounds, memory-vfs control, client-hygiene, schedule/surface-aware fault-evidence checkers |
| `src/jepsen/relate/util.clj` | the shared deadline loop (poll-until, retry-until), the one final-read shape, and quiet close |
| `src/jepsen/relate/docker.clj` | docker CLI wrapper (fails loudly; inspect helpers, merged-stream logs) |
| `src/jepsen/relate/compare.clj` | compare workload: db lifecycle, client, checker, generator, test |
| `src/jepsen/relate/compare/gen.clj` | the graph/op generator (data first; params from the queries' consumed slots) |
| `src/jepsen/relate/compare/render.clj` | the one canonical data→Cypher renderer + the canonical state-read statements |
| `src/jepsen/relate/compare/shrink.clj` | shrink weight + per-kind shrink candidates |
| `src/jepsen/relate/compare/diff.clj` | execution, canonicalization, comparison, shrinking |
| `test/jepsen/relate/checker_test.clj` | the checker contracts on synthetic histories (`clojure -M:test`) |
| `test/jepsen/relate/compare_test.clj` | the compare engine's canonicalization/shrink/error-split contracts and the nemesis wire-down evidence (run by the same `clojure -M:test`) |

## Notes and knobs

- The harness talks to the DB as `bolt://db:7687` / `http://db:7474` on the compose
  network; nothing is published to the host. `--surface`, `--bolt-uri`, `--http-uri`,
  `--db-container`, `--db-image`, `--db-host`, `--data-volume`,
  `--auth-user/--auth-password`, `--nemesis`, `--nemesis-interval`, `--fault-fifo`,
  `--workload`, `--counters`, `--accounts`, `--max-transfer`, `--lane`,
  `--checkpoint-interval-secs`, `--neo4j-uri`, `--neo4j-user/--neo4j-password`,
  `--neo4j-container`, `--ops`, `--seed`, `--concurrency`, `--time-limit`,
  `--test-count` all exist as flags (see `clojure -M:run test --help`), defaulting to
  what compose sets up.
- Each run is hermetic: `setup!` verifies the lane config and the image wiring, kills
  the DB container, wipes the data volume, restarts, gates on the surface's liveness
  probe with its identity gate, then runs the workload's setup (the register node for
  http; the counter keys / bank accounts with a count verification for the Elle lanes —
  a silent no-op CREATE dies there). The mid-checkpoint schedule's setup also asserts
  the container actually carries `RELATE_CHECKPOINT_INTERVAL_SECS=5` — a silent miss
  would degenerate it into a kill9 rerun at the default 300s interval: green, and
  certifying nothing about mid-rotate crashes. The wipe is trusted, not
  exit-code-trusted: the volume must already exist (a `docker run -v` on a missing name
  silently auto-creates a fresh empty volume — the compose/harness name mismatch that
  made early runs non-hermetic) and /data must verifiably be empty afterwards, or setup
  fails loudly. Compose pins the data volume's name explicitly (`name:
  ${DATA_VOLUME:-relate-jepsen-data}`) to the exact name `--data-volume` defaults
  to; the m2 dependency-cache volume stays shared by design.
- **Concurrent lanes isolate by suffix or by env.** Two runs sharing the fixed compose
  names kill each other's DB mid-run (observed: a sibling's `compose up` recreated the
  DB container under another lane's test — its run lost 202 acked writes to a store it
  no longer owned). `JEPSEN_SUFFIX=<name> ./run.sh` derives a private
  `relate-jepsen-<name>` project and every DB-side container with it — the DB
  container, the control container, the data volume, and (compare lane only) the neo4j
  reference container (lanes.sh sets `relaxed-lanes`); unset, every name is the
  single-run spelling. All three wrappers source the ONE derivation,
  `lib/isolation.sh`: run.sh with no default suffix, run-http.sh with default
  `http-lane`, run-poweroff.sh with default `poweroff` (the compose identifiers are
  env-parameterized:
  `COMPOSE_PROJECT_NAME`, `DB_CONTAINER`, `RELATE_JEPSEN_CONTROL_CONTAINER`,
  `DATA_VOLUME`, `NEO4J_CONTAINER`, `RELATE_IMAGE`). The m2 dependency cache is shared deliberately across the suffixed stock
  lanes; the power-off rig pins its own, and a suffixed project builds its
  own control image.
- Two containers cannot write one store (`storage locked`, ADR-0097): n=1 is the only
  writable topology, which is exactly the durability story under test — and the pause
  lane pins it from the frozen side every cycle.
- Appending to and building the product image from source requires `.dockerignore`'s
  allowlist to carry `extensions/` (mcp depends on `extensions/relate-wire`,
  ADR-0447) and the one doc file the CLI embeds
  (`docs/api/extensions/examples/mcp-client.sh`) — both are re-included there;
  from-source `make docker` dies at `cargo chef prepare` / `cargo build` without them.

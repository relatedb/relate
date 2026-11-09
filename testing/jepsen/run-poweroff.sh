#!/usr/bin/env bash
# The lazyfs power-off lane (LABS-654): the fsync-claim test. The DB
# node's store sits on a lazyfs FUSE mount inside the DB container
# (un-fsynced bytes live only in lazyfs's memory), and the nemesis's
# power-off is the clear-cache fault + docker kill. Two halves, both
# mandatory:
#
#   1. FULL MODE (durability mode full): the WAL fsync happens BEFORE
#      the commit ack, so zero acked writes may be lost — FULL_RUNS
#      consecutive hermetic runs (default 5), all green.
#   2. CONTROL (durability mode snapshot, checkpoint interval 3600s):
#      the same rig, same nemesis, only the durability policy changed —
#      it MUST lose acked writes. A control that comes back green means
#      the lane cannot detect loss and certifies nothing; the script
#      fails on that as hard as on a full-mode loss.
#
# The lane is Linux-CI-shaped: it needs a FUSE-capable docker host
# (GitHub ubuntu runners and Docker Desktop's LinuxKit VM expose
# /dev/fuse; a plain macOS compose host does not). run-poweroff.sh probes
# the host BEFORE anything privileged runs and skips loudly — exit 77,
# reason on stderr — where /dev/fuse is absent. The stock compose project
# (run.sh) never touches FUSE.
#
# Prerequisite: the product image, `make docker` at the repo root — the
# lane's DB image copies the product binaries out of relate-bolt:latest
# (rebuild the DB image whenever the product image is rebuilt).
#
# Env knobs (defaults in parentheses):
#   FULL_RUNS (5)                    hermetic full-mode runs; all must pass
#   TIME_LIMIT (70)                  seconds per run
#   CONCURRENCY (5)                  concurrent clients
#   NEMESIS_INTERVAL (5)             seconds up/down in the power-off cycle
#   CHECKPOINT_INTERVAL_SECS (300)   the full half's timed checkpoint
#                                    period (the engine default; the
#                                    full claim holds at any period)
#   CONTROL_MODE (snapshot)          the control's durability mode
#                                    (snapshot or everysecond — the
#                                    lazyfs rig's non-syncing rows)
#   CONTROL_CHECKPOINT_INTERVAL_SECS (3600)  keeps checkpoints out of the
#                                    control — snapshot durability must
#                                    come from nowhere in one 70s run
#   SKIP_PROBE ()                    set to 1 to skip the host semantics
#                                    probe (CI debugging only)
#   JEPSEN_SUFFIX (poweroff)         the lane's compose namespace
#                                    (lib/isolation.sh)
#
# Each half reaches the DB node through ONE channel — compose env,
# RELATE_DURABILITY_MODE + RELATE_CHECKPOINT_INTERVAL_SECS
# (docker-compose.yml's db service; the lazyfs override only moves the
# store to /mnt/lazyfs/graph) — and the harness gets the SAME values as
# --lane / --checkpoint-interval-secs, so db.clj's setup guard asserts
# the container really runs them (the :lazyfs rig's row).
#
# Exit status: 0 both halves held; 1 anything failed; 77 skipped — no
# FUSE on this host, nothing ran.
set -euo pipefail
cd "$(dirname "$0")"

DB_IMAGE=relate-jepsen-db-lazyfs:latest
PRODUCT_IMAGE=relate-bolt:latest
# This lane's OWN compose namespace: the jepsen-certification feature
# runs several lanes concurrently on one shared docker daemon, and the
# base project's default container/volume names collide — two compose
# projects fighting over `relate-jepsen-db` destroy each other's DB
# node mid-test. The one derivation every lane wrapper sources gives
# this lane its private project, DB container, control container, and
# data volume (suffix `poweroff` unless the caller picks another).
JEPSEN_SUFFIX="${JEPSEN_SUFFIX:-poweroff}"
. ./lib/isolation.sh
FULL_RUNS="${FULL_RUNS:-5}"
TIME_LIMIT="${TIME_LIMIT:-70}"
CONCURRENCY="${CONCURRENCY:-5}"
NEMESIS_INTERVAL="${NEMESIS_INTERVAL:-5}"
CHECKPOINT_INTERVAL_SECS="${CHECKPOINT_INTERVAL_SECS:-300}"
CONTROL_MODE="${CONTROL_MODE:-snapshot}"
CONTROL_CHECKPOINT_INTERVAL_SECS="${CONTROL_CHECKPOINT_INTERVAL_SECS:-3600}"
# The lazyfs compose override — merged over the stock topology, which
# stays FUSE-free for every other consumer.
COMPOSE=(compose -f docker-compose.yml -f docker-compose.lazyfs.yml)

# --- 1. FUSE availability: refuse to run where /dev/fuse is absent -------
# The cheap guard. --device /dev/fuse fails on hosts without the node;
# the mknod fallback is the recipe's (a privileged container can mknod
# when the kernel has fuse but the runtime did not pass the device).
if ! docker run --rm --privileged --device /dev/fuse ubuntu:24.04 \
        sh -c 'test -c /dev/fuse' >/dev/null 2>&1 \
   && ! docker run --rm --privileged ubuntu:24.04 \
        sh -c 'mknod /dev/fuse c 10 229 2>/dev/null; test -c /dev/fuse' \
        >/dev/null 2>&1; then
    echo "SKIP: no /dev/fuse reachable in docker containers on this host —" \
         "the lazyfs power-off lane requires a FUSE-capable host (GitHub" \
         "ubuntu runners and Docker Desktop expose it; plain macOS compose" \
         "hosts do not). Nothing ran." >&2
    exit 77
fi

# --- 2. Images: product first, then the lazyfs DB rig --------------------
if ! docker image inspect "$PRODUCT_IMAGE" >/dev/null 2>&1; then
    echo "error: $PRODUCT_IMAGE is missing — run 'make docker' at the repo" \
         "root first (the lane's DB image copies the product binaries out" \
         "of it)" >&2
    exit 1
fi
build_args=(build -f db-lazyfs.Dockerfile -t "$DB_IMAGE")
token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
if [ -n "$token" ] && [ "$token" != "GITHUB_TOKEN" ] && [ "$token" != "GH_TOKEN" ]; then
    export GITHUB_TOKEN="$token"
    build_args+=(--secret id=GITHUB_TOKEN,env=GITHUB_TOKEN)
fi
if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    build_args+=(--build-arg GITHUB_ACTIONS=true)
fi
docker "${build_args[@]}" .

# --- 3. Host semantics probe: the lane's first-run probe, every run ------
# Certifies THIS host's fault semantics (fsynced bytes reach the backing
# root; un-fsynced bytes never do; clear-cache + SIGKILL drops exactly
# the un-fsynced ones) — not just the device node's existence.
if [ "${SKIP_PROBE:-0}" != "1" ]; then
    docker run --rm --privileged --device /dev/fuse \
        --entrypoint /usr/local/bin/lazyfs-probe.sh "$DB_IMAGE"
fi

# The one harness invocation both halves run: the power-off nemesis on
# the lane the exported RELATE_DURABILITY_MODE /
# RELATE_CHECKPOINT_INTERVAL_SECS name, transcript tee'd to the log
# given as $1, any further arguments appended as extra harness flags.
# Runs under `set +e` so a red jepsen run reaches its caller's verdict;
# jepsen's own exit status (PIPESTATUS, not tee's) lands in HALF_STATUS
# — a variable, not stdout, so the transcript still streams to the
# terminal. Each caller keeps its own verdict parsing.
run_half() {
    local log="$1"
    shift
    set +e
    docker "${COMPOSE[@]}" run --rm control \
        clojure -M:run test \
        --nemesis poweroff \
        --lane "$RELATE_DURABILITY_MODE" \
        --checkpoint-interval-secs "$RELATE_CHECKPOINT_INTERVAL_SECS" \
        --db-container "$DB_CONTAINER" \
        --data-volume "$DATA_VOLUME" \
        --concurrency "$CONCURRENCY" \
        --time-limit "$TIME_LIMIT" \
        --nemesis-interval "$NEMESIS_INTERVAL" \
        "$@" \
        2>&1 | tee "$log"
    HALF_STATUS="${PIPESTATUS[0]}"
    set -e
}

# --- 4. Full mode: FULL_RUNS hermetic power-off runs, all green ----------
export RELATE_DURABILITY_MODE=full
export RELATE_CHECKPOINT_INTERVAL_SECS="$CHECKPOINT_INTERVAL_SECS"
docker "${COMPOSE[@]}" build control
docker "${COMPOSE[@]}" up -d --force-recreate db

mkdir -p runs
FULL_LOG="runs/$(date -u +%Y%m%dT%H%M%SZ)-poweroff-full.log"
run_half "$FULL_LOG" --test-count "$FULL_RUNS"
FULL_STATUS="$HALF_STATUS"
if [ "$FULL_STATUS" -ne 0 ]; then
    echo "error: full mode lost the claim — run log: $FULL_LOG" \
         "(jepsen exit $FULL_STATUS)" >&2
    docker "${COMPOSE[@]}" down
    exit 1
fi
echo "full mode: $FULL_RUNS/$FULL_RUNS runs green — zero acked writes lost" \
     "under power-off ($FULL_LOG)"

# --- 5. Control: the non-syncing mode MUST lose acked writes -------------
# Same rig, same nemesis, only the durability policy changed — a
# controlled experiment, not a different test. Snapshot mode + a 3600s
# checkpoint interval means no fsync before ack and no checkpoint within
# one run: every acked write lives only in lazyfs's memory when the
# power-off lands.
export RELATE_DURABILITY_MODE="$CONTROL_MODE"
export RELATE_CHECKPOINT_INTERVAL_SECS="$CONTROL_CHECKPOINT_INTERVAL_SECS"
docker "${COMPOSE[@]}" up -d --force-recreate db

CONTROL_LOG="runs/$(date -u +%Y%m%dT%H%M%SZ)-poweroff-control.log"
run_half "$CONTROL_LOG"
CONTROL_STATUS="$HALF_STATUS"

docker "${COMPOSE[@]}" down

# The control's expected shape: jepsen exit 1 (the set checker failed),
# acked writes lost, and the faults themselves all fired — a control
# that fails because the faults never happened is as worthless as a
# green one.
LOST="$(sed -n 's/.*:lost-acked-writes {:count \([0-9][0-9]*\).*/\1/p' \
        "$CONTROL_LOG" | head -1)"
FAULTS_OK="$(grep -c ':fault-evidence {:valid? true' "$CONTROL_LOG" || true)"

if [ "$CONTROL_STATUS" -eq 0 ] || [ -z "${LOST:-}" ] || [ "$LOST" -eq 0 ]; then
    echo "error: the control came back GREEN ($CONTROL_MODE lost no acked" \
         "writes) — the lane cannot detect loss, so its full-mode green" \
         "certifies nothing. Run log: $CONTROL_LOG" >&2
    exit 1
fi
if [ "$FAULTS_OK" -lt 1 ]; then
    echo "error: the control lost writes ($LOST) but its faults did not all" \
         "fire — wrong failure shape, not the expected one. Run log:" \
         "$CONTROL_LOG" >&2
    exit 1
fi

echo "control: $CONTROL_MODE lost $LOST acked writes under the same" \
     "power-off, faults verified — the lane detects loss ($CONTROL_LOG)"
echo "lane green: full $FULL_LOG / control $CONTROL_LOG"

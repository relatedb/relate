#!/usr/bin/env bash
# The bolt stage's smoke leg (LABS-623): the whole server lifecycle and
# driver-smoke sequence, extracted from the inline .sonic/ci.yml case arm
# so shell tooling and the script-test harness can reach it.
#
# `relate serve` is the one product binary (LABS-381, ADR-0368): the
# smoke leg drives it instead of the retired standalone server. The
# load-csv and TLS example steps were dropped with the retired binary —
# serve carries neither `--load-csv-dir` nor TLS, so those documented
# capabilities have no product binary until a follow-up ADR adds the
# flags (see docs/api/bolt/examples/load_csv.md and tls.md).
#
# The structure is two blocks (LABS-649): first the whole server topology
# declared and instantiated — every fact exported under the one name its
# consumers read, all five spawns in one place — then the consumers in
# order. Dependencies between smokes and servers are textual order inside
# those two blocks, nothing interleaved.
#
# Env: CARGO_TARGET_DIR (the stage's shared target dir — the built
# `debug/relate` binary comes from there). The gosonic container
# only receives explicitly mapped env, so the workflow-level
# DO_NOT_TRACK does not reach the serve spawns; the opt-out is carried
# here (ADR-0079 layer 1). Run from the repository root, like every
# other stage script.
set -euo pipefail

: "${CARGO_TARGET_DIR:?bolt-smoke.sh needs the CARGO_TARGET_DIR of the stage}"
export DO_NOT_TRACK=1

cargo build -p relate-cli
# Pinned like the sibling smokes pin their drivers (pom 5.28.5,
# go.mod/go.sum), so the lane is deterministic regardless of the image's
# wheel cache. rust-ext accelerates the python driver's packstream codec
# (on some platforms its wheel also carries the rust Bolt socket; the
# darwin wheel is packstream-only — so nothing here relies on it to
# bypass the python driver's identity gate, the shim below does that).
python3 -m pip install neo4j==5.28.8 neo4j-rust-ext==5.28.8.0

# Readiness, not a startup guess (LABS-622), spelled once: the port poll
# proves the server accepts connections, then kill -0 stays as the crash
# check on the spawn that should be serving it. The spawn registry is
# keyed by port — the one identifier a server already has, so a pid can
# never be paired with the wrong port.
wait_ready() { # $1 = the port whose spawn must be listening and alive
	bash scripts/wait-for-port.sh "$RELATE_BOLT_HOST" "$1"
	kill -0 "${pids[$1]}"
}

# A sparse indexed array keyed by the (numeric) port — no declare -A, so
# the script also runs under macOS's bash 3.2.
pids=()

# One serve spawn, one spelling: the shared invocation prefix, the
# port-keyed registry entry, and the readiness wait live here, so each
# server is a single line, the port is stated once per spawn, and a
# spawn without its wait is structurally impossible. Extra serve flags
# pass through.
spawn_serve() { # $1 = the port to serve Bolt on; rest = extra serve flags
	"$CARGO_TARGET_DIR/debug/relate" serve --host "$RELATE_BOLT_HOST" \
	  --bolt "$1" --database :memory: "${@:2}" &
	pids[$1]=$!
	wait_ready "$1"
}

trap 'for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; done' EXIT

# The server topology, declared once under the exported names every
# consumer reads — one vocabulary, one source, drift structurally
# impossible. Two identity regimes, by consumer: the honest
# `Relate/<version>` agent (ADR-0293) is served on DEFAULT and
# PRESET_AUTH for the Go smoke — the one driver that does not gate on
# it, so the LABS-597 agent pin keeps running through a real driver.
# The current Python and Java drivers both validate the HELLO `server`
# agent and reject anything not starting `Neo4j/`, so every server those
# smokes (and the bolt doc examples) connect to runs behind the
# `--bolt-identity` compat shim. PORT_WEBSOCKET stays script-only (its
# consumer takes the value inline below).
export RELATE_BOLT_HOST=127.0.0.1
export RELATE_BOLT_PORT_DEFAULT=7687
export RELATE_BOLT_PORT_PRESET_AUTH=7689
export RELATE_BOLT_PORT_IDENTITY=7691
export RELATE_BOLT_PORT_AUTH_IDENTITY=7692
export RELATE_BOLT_IDENTITY=Neo4j/5.26.0
# ADR-0593: the python smoke's `system_ontologies` legs and the bolt
# namespace doc example (below) need the `ontology` opt-in — the
# namespace is invisible while it is off. Exported for every spawned
# server; the other smokes never touch the gated surface.
export RELATE_PREVIEW=ontology
PORT_WEBSOCKET=7690

spawn_serve "$RELATE_BOLT_PORT_DEFAULT" --auth none
spawn_serve "$RELATE_BOLT_PORT_PRESET_AUTH" --auth relate:ci-token
spawn_serve "$RELATE_BOLT_PORT_IDENTITY" --auth none \
  --bolt-identity "$RELATE_BOLT_IDENTITY"
spawn_serve "$RELATE_BOLT_PORT_AUTH_IDENTITY" --auth relate:ci-token \
  --bolt-identity "$RELATE_BOLT_IDENTITY"
spawn_serve "$PORT_WEBSOCKET" --auth none

# The consumers, in order. The python smoke and doc examples run behind
# the identity shim (the current python driver rejects the honest
# agent); the go smoke pins the honest agent on the default-identity
# servers (LABS-597); the java smoke pins both sides — the rejection on
# DEFAULT and the full suite behind the shim.
python3 surfaces/relate-bolt/tests/python_driver_smoke.py
# LABS-598's pin: the unmodified neo4j-go-driver config (Bolt 5
# handshake extras included) against the Bolt 4.4 path.
(cd surfaces/relate-bolt/tests/go_driver_smoke && go run .)
# LABS-649's pin: the unmodified official Java driver — explicit
# transactions and the Outdated conflict mapping the Jepsen harness
# depends on.
(cd surfaces/relate-bolt/tests/java_driver_smoke && mvn -q -B compile exec:java)
python3 docs/api/bolt/examples/namespace.py
python3 docs/api/bolt/examples/query_type.py
python3 docs/api/bolt/examples/temporal_params.py
python3 docs/api/bolt/examples/logon.py
npm ci --prefix docs/api/bolt/examples/websocket
(
  cd docs/api/bolt/examples/websocket
  RELATE_BOLT_PORT="$PORT_WEBSOCKET" node index.mjs
)

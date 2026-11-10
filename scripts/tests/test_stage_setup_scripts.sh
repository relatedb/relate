#!/bin/sh
# Tests for the shared CI stage-setup scripts (LABS-622, LABS-623):
#
# - wait-for-port.sh: succeeds once a listener accepts, fails loudly on
#   timeout, and rejects a missing argument
# - install-wasm-toolchain.sh / baseline-docker-setup.sh /
#   bolt-smoke.sh: bash -n syntax (running them needs the CI
#   environment: CARGO_HOME toolchain installs, a host Docker daemon,
#   the built serve binary)
# - .sonic/ci.yml and scripts/bolt-smoke.sh: every setup keeps exactly
#   its owning call sites — the stage helpers are called by their
#   stages, the smoke-leg sequence by its one dispatch, the port polls
#   and driver smokes by bolt-smoke.sh alone; the banned inline
#   spellings stay at zero
#
# - the lib-shard count has one derivation: the leg axis's lib-N cells
#   equal the bolt stage's partition denominator
#
# Assertions use the shared TAP harness (scripts/tests/lib/tap.sh).

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
WAIT_FOR_PORT="$ROOT/scripts/wait-for-port.sh"

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox relate-stage-setup-test

# --- wait-for-port.sh -------------------------------------------------------

# A listener that is up: the poll returns as soon as the port accepts.
# python3 serves the listener (the CI images and macOS both carry it).
python3 -c '
import socket, sys, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
s.listen(1)
print(s.getsockname()[1], flush=True)
time.sleep(float(sys.argv[1]))
' 10 >"$sandbox/port" 2>/dev/null &
listener_pid=$!
# The listener prints its port once it is bound; wait for that first so
# the poll below never races the bind itself. A listener that never
# reports (python3 missing, slow bind) aborts under its own name —
# never as a wait-for-port failure.
port=""
i=0
while [ -z "$port" ] && [ "$i" -lt 50 ]; do
  sleep 0.1
  port=$(sed -n '1p' "$sandbox/port" 2>/dev/null)
  i=$((i + 1))
done
if [ -z "$port" ]; then
  tap_not_ok "python3 listener bound a port"
  tap_done
  exit 1
fi
tap_ok "python3 listener bound a port"

tap_run_success \
  "wait-for-port succeeds once the port accepts" \
  bash "$WAIT_FOR_PORT" 127.0.0.1 "$port" 10
tap_run_failure \
  "wait-for-port fails loudly on timeout (closed port)" \
  bash "$WAIT_FOR_PORT" 127.0.0.1 1 1
tap_run_failure \
  "wait-for-port rejects a missing port argument" \
  bash "$WAIT_FOR_PORT" 127.0.0.1
kill "$listener_pid" 2>/dev/null
wait "$listener_pid" 2>/dev/null

# --- wrapper scripts: syntax ------------------------------------------------

for script in install-wasm-toolchain.sh baseline-docker-setup.sh bolt-smoke.sh check-sonic-socket-policy.sh; do
  tap_run_success "$script passes bash -n" bash -n "$ROOT/scripts/$script"
done

# --- .sonic/ci.yml & scripts/bolt-smoke.sh: ownership stays pinned ----------

# One counting idiom, one vocabulary (specs are
# file:pattern:expected-count; the decode is lossless, so patterns may
# carry colons — want is the text after the LAST colon): want-0 entries
# are the banned inline
# spellings the dedupes removed; the rest pin every setup to exactly its
# owning call sites — the stage helpers are called by their stages, the
# smoke-leg sequence by its one dispatch (LABS-623), the port poll and
# driver smokes by bolt-smoke.sh alone. grep -c exits 1 on a zero
# count; the count is data, not an error.
for spec in \
  '.sonic/ci.yml:bash scripts/install-wasm-bindgen:0' \
  '.sonic/ci.yml:bash scripts/install-binaryen:0' \
  '.sonic/ci.yml:bash scripts/install-docker-cli:0' \
  '.sonic/ci.yml:bash scripts/baseline-env:0' \
  '.sonic/ci.yml:sleep 1:0' \
  '.sonic/ci.yml:ln -sfn:0' \
  '.sonic/ci.yml:sh scripts/tests/test_install_sh.sh:0' \
  '.sonic/ci.yml:docs/api/c/examples:0' \
  'scripts/ffi-compile-example.sh:_examples=(return_one:0' \
  '.sonic/ci.yml:RELATE_BIN:2' \
  '.sonic/ci.yml:RELATE_BIN: /cargo-target/loadtest-smoke/debug/relate:1' \
  '.sonic/ci.yml:cargo build -p relate-cli --bin relate:1' \
  '.sonic/ci.yml:bash scripts/ffi-compile-example.sh:1' \
  '.sonic/ci.yml:make test-scripts:1' \
  '.sonic/ci.yml:install-wasm-toolchain.sh:1' \
  '.sonic/ci.yml:baseline-docker-setup.sh:1' \
  '.sonic/ci.yml:\*baseline-docker-setup:4' \
  '.sonic/ci.yml:GITHUB_WORKSPACE:1' \
  '.sonic/ci.yml:\*baseline-env-forwarding:4' \
  '.sonic/ci.yml:^ *LD_LIBRARY_PATH:0' \
  '.sonic/ci.yml:source scripts/ffi-lib.sh:1' \
  '.sonic/ci.yml:bash /workspace/scripts:0' \
  '.sonic/ci.yml:source scripts/install-wasm-toolchain.sh:1' \
  '.github/workflows/ci.yml:^ *crates::0' \
  '.github/workflows/ci.yml:LIB_SHARDS:0' \
  '.sonic/ci.yml:bash scripts/bolt-smoke.sh:1' \
  '.sonic/ci.yml:wait-for-port.sh:0' \
  '.sonic/ci.yml:python_driver_smoke.py:0' \
  '.sonic/ci.yml:go_driver_smoke:0' \
  '.sonic/ci.yml:java_driver_smoke:0' \
  'scripts/bolt-smoke.sh:wait-for-port.sh:1' \
  'scripts/bolt-smoke.sh:wait_ready:2' \
  'scripts/bolt-smoke.sh:spawn_serve:6' \
  'scripts/bolt-smoke.sh:python_driver_smoke.py:1' \
  'scripts/bolt-smoke.sh:go_driver_smoke:1' \
  'scripts/bolt-smoke.sh:java_driver_smoke:1'; do
  file=${spec%%:*}
  rest=${spec#*:}
  want=${rest##*:}
  pattern=${rest%":$want"}
  count=$(grep -c "$pattern" "$ROOT/$file")
  [ "$count" -eq "$want" ]
  tap_check "$file: $pattern spelled $count (want $want)" $?
done

# --- .sonic/ci.yml: one merge key per environment block ---------------------

# The YAML merge-key spec leaves a mapping with two `<<` keys undefined
# — the sequence form is the sanctioned multi-source merge. Guard the
# shape textually: every environment block carries at most one `<<:`
# line (a block ends at the next outer-indent key).
double_merges=$(awk '
  function flush() { if (cnt > 1) bad++; cnt = 0 }
  /^ *environment: *$/ { flush(); block = 1; cnt = 0; next }
  /^ {4}[^ ]/ && block { flush(); block = 0 }
  /^ *<</ && block { cnt++ }
  END { flush(); printf "%d", bad + 0 }
' "$ROOT/.sonic/ci.yml")
[ "$double_merges" -eq 0 ]
tap_check "environment blocks with a double merge key: $double_merges" $?

# --- .sonic/ci.yml: the lib-shard count has one derivation ------------------

# The leg axis's lib-N cells equal the bolt stage's partition
# denominator (LABS-631) — the dead LIB_SHARDS env and its comment are
# gone with it.
lib_cells=$(grep -cE '^ *- name: lib-[0-9]+$' "$ROOT/.sonic/ci.yml")
denominator=$(grep -oE 'count:.\{shard\}/[0-9]+' "$ROOT/.sonic/ci.yml" | head -n 1 | grep -oE '[0-9]+$')
[ "$lib_cells" -eq "$denominator" ]
tap_check "lib-shard count: $lib_cells axis cells == partition denominator $denominator" $?

# --- the two _topology policy modules: identical policy text ----------------

# The two-copy layout is sanctioned (each tree self-contained; neither
# imports across the docs/tests boundary) but the identity is checked,
# never aspirational: the policy region — the _TOPOLOGY_SOURCE absence
# rule, required_env, and the bolt_uri join — is identical in both
# files modulo blank lines (the strip aligns the tests copy's
# routing_uri cut with the examples copy's EOF; layout, not policy).
# The tests-tree copy additionally carries routing_uri (the compat
# suite's ROUTE probe), which the examples never need, so the
# comparison runs from _TOPOLOGY_SOURCE up to (excluding) routing_uri.
topology_policy() { # $1 = a _topology.py path; prints its policy region
	sed -n '/^_TOPOLOGY_SOURCE = (/,$p' "$1" | sed '/^def routing_uri/,$d' | sed '/^$/d'
}
topology_policy "$ROOT/surfaces/relate-bolt/tests/_topology.py" >"$sandbox/policy-tests"
topology_policy "$ROOT/docs/api/bolt/examples/_topology.py" >"$sandbox/policy-examples"
cmp -s "$sandbox/policy-tests" "$sandbox/policy-examples"
tap_check "the two _topology.py policy texts are identical" $?

# --- github-download.sh: Actions never falls back to an anonymous GET ----

# A missing token, and the Sonic literal-name forward, both refuse to
# curl. These cases exit before any network call.
tap_run_failure \
  "github-download refuses an anonymous GET in GitHub Actions" \
  env -u GH_TOKEN GITHUB_ACTIONS=true GITHUB_TOKEN= \
  sh "$ROOT/scripts/github-download.sh" --repo mozilla/sccache --tag v0.19.0 --asset sccache.tar.gz -o "$sandbox/out"
tap_run_failure \
  "github-download treats a literal GITHUB_TOKEN forward as unset" \
  env -u GH_TOKEN GITHUB_ACTIONS=true GITHUB_TOKEN=GITHUB_TOKEN \
  sh "$ROOT/scripts/github-download.sh" --repo mozilla/sccache --tag v0.19.0 --asset sccache.tar.gz -o "$sandbox/out"
tap_run_failure \
  "github-download rejects a raw URL that is not raw.githubusercontent.com" \
  env -u GH_TOKEN -u GITHUB_TOKEN -u GITHUB_ACTIONS \
  sh "$ROOT/scripts/github-download.sh" --raw https://example.com/file -o "$sandbox/out"

# --- socket policy ----------------------------------------------------------

# The committed config is the allowlist: the socket stages are the
# reviewed set, and GITHUB_TOKEN stays on the image build alone.
tap_run_success \
  "the committed Sonic config passes the socket policy" \
  bash "$ROOT/scripts/check-sonic-socket-policy.sh"

cp "$ROOT/.sonic/ci.yml" "$sandbox/extra-socket.yml"
printf '\n  policy-probe:\n    docker_socket: true\n' >> "$sandbox/extra-socket.yml"
bash "$ROOT/scripts/check-sonic-socket-policy.sh" "$sandbox/extra-socket.yml" >"$sandbox/extra-socket.out" 2>&1
extra_status=$?
[ "$extra_status" -ne 0 ]
tap_check "an unlisted docker_socket stage is refused" $?
grep -q "stage policy-probe sets docker_socket" "$sandbox/extra-socket.out"
tap_check "the refusal names the unlisted stage" $?

cp "$ROOT/.sonic/ci.yml" "$sandbox/token-on-socket.yml"
python3 - "$sandbox/token-on-socket.yml" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
old = "  build-sonic-polyglot:\n    runner: docker-cli\n    docker_socket: true\n"
new = old + "    optional_secrets:\n      - GITHUB_TOKEN\n"
file = path.read_text()
if old not in file:
    raise SystemExit("build-sonic-polyglot block not found")
path.write_text(file.replace(old, new, 1))
PY
bash "$ROOT/scripts/check-sonic-socket-policy.sh" "$sandbox/token-on-socket.yml" >"$sandbox/token-on-socket.out" 2>&1
token_status=$?
[ "$token_status" -ne 0 ]
tap_check "GITHUB_TOKEN on a socket stage other than the image build is refused" $?
grep -q "stage build-sonic-polyglot mounts the Docker socket and forwards GITHUB_TOKEN" "$sandbox/token-on-socket.out"
tap_check "the refusal names the stage that forwards the token" $?

cp "$ROOT/.sonic/ci.yml" "$sandbox/raw-socket.yml"
printf '\n  policy-probe-volume:\n    volumes:\n      - /var/run/docker.sock:/var/run/docker.sock\n' >> "$sandbox/raw-socket.yml"
bash "$ROOT/scripts/check-sonic-socket-policy.sh" "$sandbox/raw-socket.yml" >"$sandbox/raw-socket.out" 2>&1
raw_status=$?
[ "$raw_status" -ne 0 ]
tap_check "a volumes entry that mounts the Docker socket is refused" $?
grep -q "stage policy-probe-volume mounts the Docker socket through volumes" "$sandbox/raw-socket.out"
tap_check "the refusal names the stage that bind-mounts the socket" $?

tap_done

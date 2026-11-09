#!/bin/bash
# Northwind push/pull end-to-end (ADR-0401, LABS-36): the manual counterpart
# of the graph-examples differential — pull the northwind-50 dump out of a
# dockerized Neo4j EE, push it into a fresh 2026 EE target, re-push for
# idempotence, roundtrip both directions, then run the delta scenario
# (local additions + a property change + a date-typed property in one
# push) and prove the dates kept their type. The optional Aura legs run
# when an env file is present (gitignored; the same credentials file pull
# and push take — LABS-9's one connection string).
#
# Usage:   scripts/push-pull-e2e.sh
# Env:     AURA_ENV   — path to a credentials file for the real-Aura legs
#                       (default: <repo>/aura-test.env, gitignored; the
#                       Aura phases are skipped when it is absent)
#          WORK_DIR   — scratch dir (default /tmp/nw-e2e; a store found
#                       there is reused, skipping phases 0-3 — it must be
#                       unedited: the roundtrip assertions compare node and
#                       rel counts, and a store carrying duplicate keyed
#                       rows collapses them remotely (the keyed MERGE
#                       dedupes by design, ADR-0401 §4), so a reused store
#                       with hand-added duplicates fails the equality)
# Requires: docker, curl, sha256sum/shasum, python3, a local git checkout
#           of this repo with the CLI buildable.
set -u
REPO_ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$REPO_ROOT" || exit 1
W="${WORK_DIR:-/tmp/nw-e2e}"; CLI="$REPO_ROOT/target/debug/relate"
AURA_ENV="${AURA_ENV:-$REPO_ROOT/aura-test.env}"
KEYS="--merge-key Order:orderID --merge-key Customer:customerID --merge-key Product:productID --merge-key Supplier:supplierID --merge-key Category:categoryID"
mkdir -p "$W"; : > "$W/summary.txt"
log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$W/summary.txt"; }
fail() { log "FAILED: $*"; echo "FINAL: FAILED ($*)" >> "$W/summary.txt"; exit 1; }
count() { $CLI query --database "$2" "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "${3:-nodes}"; }
ready() { # $1 port $2 password
  for _ in $(seq 1 60); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://localhost:$1/db/neo4j/query/v2" \
      -u "neo4j:$2" -H 'content-type: application/json' -d '{"statement":"RETURN 1"}' 2>/dev/null)
    { [ "$code" = "200" ] || [ "$code" = "202" ]; } && return 0
    sleep 5
  done
  return 1
}

if [ -d "$W/nw-store" ]; then
  cargo build -p relate-cli 2>&1 | tail -1
  NODES=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-store")
  RELS=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-store" rels)
  log "nw-store already pulled ($NODES nodes, $RELS rels); reusing it (skipping phases 0-3)"
else
log "phase 0: build the CLI"
cargo build -p relate-cli 2>&1 | tail -1 || fail "cli build"
[ -x "$CLI" ] || fail "no $CLI"

log "phase 1: graph-examples differential, northwind (pull vs docker EE)"
RELATE_GRAPH_EXAMPLES=1 RELATE_GRAPH_EXAMPLES_DATASETS=northwind \
  RELATE_BIN="$CLI" cargo test -p relate-graph-examples --test differential >"$W/suite.log" 2>&1
grep -E 'test result|FAILED|northwind' "$W/suite.log" | tee -a "$W/summary.txt"
grep -q 'test result: ok' "$W/suite.log" || fail "suite"

log "phase 2: manual docker EE source with the northwind dump"
DUMP="$W/northwind-50.dump"
repo="$(cd "$(dirname "$0")/../../.." && pwd)"
sh "$repo/scripts/github-download.sh" \
  --raw https://raw.githubusercontent.com/neo4j-graph-examples/northwind/main/data/northwind-50.dump \
  -o "$DUMP" || fail "dump download"
echo "3aef3fdf2f9e6a7e6e813d7fcd4e46a8ea7e75573ee7d4fb84f6884ec1f66b9f  $DUMP" | shasum -a 256 -c - >/dev/null 2>&1 || fail "dump sha mismatch"
docker rm -f nw-src >/dev/null 2>&1; docker volume rm nw-src-vol >/dev/null 2>&1
docker volume create nw-src-vol >/dev/null || fail "volume"
docker run -i --rm -v nw-src-vol:/data -e NEO4J_ACCEPT_LICENSE_AGREEMENT=yes \
  neo4j:2026.09-enterprise neo4j-admin database load --from-stdin northwind < "$DUMP" >/dev/null 2>"$W/admin-load.err" || { tail -5 "$W/admin-load.err"; fail "admin load"; }
docker run -d --name nw-src -p 7479:7474 -v nw-src-vol:/data \
  -e NEO4J_ACCEPT_LICENSE_AGREEMENT=yes \
  -e NEO4J_AUTH=neo4j/referencepassword neo4j:2026.09-enterprise >/dev/null || fail "src server"
ready 7479 referencepassword || fail "src server never ready"
docker exec nw-src cypher-shell -a localhost -u neo4j -p referencepassword \
  "CREATE DATABASE northwind IF NOT EXISTS" >/dev/null 2>&1 || fail "create database"
printf 'NEO4J_URI=http://localhost:7479\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=referencepassword\nNEO4J_DATABASE=northwind\n' > "$W/src.env" && chmod 600 "$W/src.env"

log "phase 3: pull northwind into a local store"
rm -rf "$W/nw-store"
$CLI pull --from "$W/src.env" \
  --query 'MATCH (n) RETURN n AS e; MATCH (a)-[r]->(b) RETURN a AS x, r AS e, b AS y' \
  --database "$W/nw-store" >>"$W/summary.txt" 2>&1 || { tail -5 "$W/summary.txt"; fail "pull from docker"; }
NODES=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-store")
RELS=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-store" rels)
log "pulled northwind: $NODES nodes, $RELS rels"
[ -n "$NODES" ] && [ "$NODES" -gt 500 ] || fail "implausible node count ($NODES)"
fi

log "phase 4: push into a fresh docker target, then roundtrip"
docker rm -f nw-dst >/dev/null 2>&1
docker run -d --name nw-dst -p 7480:7474 -e NEO4J_ACCEPT_LICENSE_AGREEMENT=yes -e NEO4J_AUTH=neo4j/targetpassword neo4j:2026.09-enterprise >/dev/null || fail "dst server"
ready 7480 targetpassword || fail "dst server never ready"
printf 'NEO4J_URI=http://localhost:7480\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=targetpassword\nNEO4J_DATABASE=neo4j\n' > "$W/dst.env" && chmod 600 "$W/dst.env"
$CLI push --to "$W/dst.env" --database "$W/nw-store" $KEYS --dry-run >>"$W/summary.txt" 2>&1 || fail "docker dry-run"
$CLI push --to "$W/dst.env" --database "$W/nw-store" $KEYS >>"$W/summary.txt" 2>&1 || { tail -8 "$W/summary.txt"; fail "docker push"; }
log "docker push done; re-push for idempotence"
$CLI push --to "$W/dst.env" --database "$W/nw-store" $KEYS --json >"$W/repush-docker.json" 2>&1 || fail "docker re-push"
CREATED=$(python3 -c "import json;print(sum(e['created'] for e in json.load(open('$W/repush-docker.json'))['entities']))")
log "docker re-push created: $CREATED (must be 0)"
[ "$CREATED" = "0" ] || fail "re-push created $CREATED, not idempotent"
rm -rf "$W/nw-roundtrip"
$CLI pull --from "$W/dst.env" \
  --query 'MATCH (n) RETURN n AS e; MATCH (a)-[r]->(b) RETURN a AS x, r AS e, b AS y' \
  --database "$W/nw-roundtrip" >>"$W/summary.txt" 2>&1 || fail "roundtrip pull from docker"
RN=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-roundtrip")
RR=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-roundtrip" rels)
log "docker roundtrip: $RN nodes, $RR rels (source: $NODES / $RELS)"
[ "$RN" = "$NODES" ] && [ "$RR" = "$RELS" ] || fail "docker roundtrip mismatch: $RN/$RR vs $NODES/$RELS"

if [ -f "$AURA_ENV" ]; then
log "phase 5: push into real Aura, re-push, pull back"
$CLI push --to "$AURA_ENV" --database "$W/nw-store" $KEYS --dry-run >>"$W/summary.txt" 2>&1 || { tail -5 "$W/summary.txt"; fail "aura dry-run"; }
$CLI push --to "$AURA_ENV" --database "$W/nw-store" $KEYS >>"$W/summary.txt" 2>&1 || { tail -8 "$W/summary.txt"; fail "aura push"; }
log "aura push done; re-push for idempotence"
$CLI push --to "$AURA_ENV" --database "$W/nw-store" $KEYS --json >"$W/repush-aura.json" 2>&1 || fail "aura re-push"
AC=$(python3 -c "import json;print(sum(e['created'] for e in json.load(open('$W/repush-aura.json'))['entities']))")
log "aura re-push created: $AC (must be 0)"
[ "$AC" = "0" ] || fail "aura re-push created $AC, not idempotent"
rm -rf "$W/nw-aura-roundtrip"
$CLI pull --from "$AURA_ENV" \
  --query 'MATCH (n) RETURN n AS e; MATCH (a)-[r]->(b) RETURN a AS x, r AS e, b AS y' \
  --database "$W/nw-aura-roundtrip" >>"$W/summary.txt" 2>&1 || { tail -8 "$W/summary.txt"; fail "aura pull back"; }
AN=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-aura-roundtrip")
AR=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-aura-roundtrip" rels)
# The Aura remote accumulates across runs — push never deletes — so the
# pull-back is a superset whenever earlier runs left entities behind.
# Equality is guaranteed against a fresh remote (the docker legs prove
# it with a fresh container every run); against Aura assert containment.
log "aura roundtrip: $AN nodes, $AR rels (source $NODES / $RELS must be contained)"
[ "$AN" -ge "$NODES" ] && [ "$AR" -ge "$RELS" ] || fail "aura roundtrip lost source rows: $AN/$AR < $NODES/$RELS"
else
log "phase 5: skipping (no $AURA_ENV — the Aura legs are optional)"
fi

log "phase 6: the delta scenario — local additions + edits, then one push"
# Timestamped keys keep the phase re-runnable: the additions are new to
# any remote state, so the delta push must create all of them.
DTAG=$(date +%H%M%S)
DC1="DELT-$DTAG-A"; DC2="DELT-$DTAG-B"; DO1="9999-$DTAG-A"; DO2="9999-$DTAG-B"
$CLI query --database "$W/nw-store" "
CREATE (:Customer {customerID: '$DC1', companyName: 'Delta One', contactName: 'D. One'});
CREATE (:Customer {customerID: '$DC2', companyName: 'Delta Two', contactName: 'D. Two'});
CREATE (:Order {orderID: '$DO1', customerID: '$DC1', orderDate: '2026-10-24'});
CREATE (:Order {orderID: '$DO2', customerID: '$DC2', orderDate: '2026-10-24'});
MATCH (c:Customer {customerID: '$DC1'}), (o:Order {orderID: '$DO1'}) CREATE (c)-[:PURCHASED]->(o);
MATCH (c:Customer {customerID: '$DC2'}), (o:Order {orderID: '$DO2'}) CREATE (c)-[:PURCHASED]->(o);
MATCH (o:Order {orderID: '10248'}) SET o.pushedAt = date('2026-10-24');
MATCH (c:Customer {customerID: 'ALFKI'}) SET c.city = 'Delta City';
" >>"$W/summary.txt" 2>&1 || fail "local delta edits"
DN=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-store")
DR=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-store" rels)
log "local delta state: $DN nodes, $DR rels"
push_delta() { # $1 env file, $2 label for the log
  $CLI push --to "$1" --database "$W/nw-store" $KEYS --json >"$W/delta-$2.json" 2>/tmp/delta-$2.err || { cat /tmp/delta-$2.err; fail "$2 delta push"; }
  CREATED=$(python3 -c "import json;d=json.load(open('$W/delta-$2.json'));c=sum(e['created'] for e in d['entities']);print(c)")
  UPD=$(python3 -c "import json;d=json.load(open('$W/delta-$2.json'));print(sum(e['updated'] for e in d['entities']))")
  log "$2 delta push: created $CREATED (must be 6 — 4 additions + 2 new rels), updated $UPD"
  [ "$CREATED" = "6" ] || fail "$2 delta push created $CREATED, expected the 6 addition rows"
}
push_delta "$W/dst.env" docker
[ -f "$AURA_ENV" ] && push_delta "$AURA_ENV" aura
rm -rf "$W/nw-delta"
PULL_FROM="$W/dst.env"; [ -f "$AURA_ENV" ] && PULL_FROM="$AURA_ENV"
$CLI pull --from "$PULL_FROM" \
  --query 'MATCH (n) RETURN n AS e; MATCH (a)-[r]->(b) RETURN a AS x, r AS e, b AS y' \
  --database "$W/nw-delta" >>"$W/summary.txt" 2>&1 || fail "delta pull back"
XN=$(count 'MATCH (n) RETURN count(n) AS nodes' "$W/nw-delta")
XR=$(count 'MATCH ()-[r]->() RETURN count(r) AS rels' "$W/nw-delta" rels)
log "delta roundtrip: $XN nodes, $XR rels (source $DN / $DR must be contained)"
[ "$XN" -ge "$DN" ] && [ "$XR" -ge "$DR" ] || fail "delta roundtrip lost source rows: $XN/$XR < $DN/$DR"
NEWC=$(count "MATCH (c:Customer {customerID: '$DC1'}) RETURN count(c) AS n" "$W/nw-delta" n)
CHANGED=$(count "MATCH (c:Customer {customerID: 'ALFKI'}) WHERE c.city = 'Delta City' RETURN count(c) AS n" "$W/nw-delta" n)
log "delta landed: DELT1=$NEWC (must be 1), ALFKI city changed=$CHANGED (must be 1)"
[ "$NEWC" = "1" ] || fail "the delta's new customer did not land"
[ "$CHANGED" = "1" ] || fail "the delta's property change did not land"
SURVIVED=$($CLI query --database "$W/nw-delta" "
MATCH (o:Order {orderID: '10248'})
RETURN o.pushedAt = date('2026-10-24') AS survivedAsDate" | python3 -c 'import json,sys; print(json.load(sys.stdin)["survivedAsDate"])')
log "the date-typed property survived as a date: $SURVIVED (must be True)"
[ "$SURVIVED" = "True" ] || fail "the date property lost its type in the roundtrip"

log "phase 7: cleanup"
docker rm -f nw-src nw-dst >/dev/null 2>&1; docker volume rm nw-src-vol >/dev/null 2>&1
log "ALL PHASES GREEN"
echo "FINAL: SUCCESS" >> "$W/summary.txt"

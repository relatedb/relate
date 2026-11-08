# Behavioral tests for the CI test-shard partition
#
# Locks the coverage half of the test lane's contract (.sonic/ci.yml's
# `test` stage): every workspace member is either in EXACTLY ONE shard
# crate list or on the exemption table below, naming the lane that owns
# its tests. A member that slips out of every shard (the three extension
# children, then relate-ontology and relate-markdown, both
# 2026-11-02) fails CI here instead of silently losing PR coverage —
# the LABS-311 rule: the hand-typed shard lists are parallel state over
# the workspace manifest's one membership fact, so this guard reads the
# manifest and checks the lists against it.
#
# Local run:
#   bats .github/scripts/tests/test-shard-coverage.bats
#
# Requirements:
#   bats-core >= 1.5.0, bash, awk

bats_require_minimum_version 1.5.0

REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../../.." && pwd)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"
SONIC="${REPO_ROOT}/.sonic/ci.yml"

# The exemption table: one member per line, `package -> lane(s) owning
# its tests`. Reviewed state, not silent state — a stale entry (a member
# that moved into a shard, or a name that is no longer a member) fails
# the stale-entry test below, so the table cannot drift lazy.
EXEMPTED="
relate-agentbench -> the A/B dispatch job (dispatch-only, LABS-481)
relate-auth -> exercised through the http/mcp suites that consume it (os-smoke surface smokes)
relate-bolt -> bolt driver smoke + language-clients lanes
relate-codec-lab -> no CI lane (hand-run snapshot-codec measurement harness, ADR-0537; ships no tests)
relate-cookbook -> corpus lane (native recipe gate)
relate-datasets -> loadtest lanes (fraud/identity datasets)
relate-graph-examples -> graph-examples nightly
relate-http -> os-smoke surface smokes (query_api)
relate-mcp -> os-smoke surface smokes (stdio_tools)
relate-python -> python package lanes (maturin build + wheel smokes)
relate-query-latency -> query-latency ratchet lane
relate-serve -> the serve-bundle lane in feature-bundles (accept loop through the CLI serve tests)
relate-wasm -> WASM lane (browser build + site parity)
xtask -> lint stage (cargo xtask check-boundaries)
"

# One manifest list (members / exclude) as bare path entries, one per
# line — the manifest is the one membership fact this file consults.
read_manifest_list() { # $1 = key
  awk -v key="$1" '
    $0 ~ "^" key " *= *" {
      line = $0
      sub("^" key " *= *", "", line)
      sub(/^\[/, "", line)
      sub(/\][[:space:]]*$/, "", line)
      gsub(/[ ",]/, "\n", line)
      print line
    }
  ' "$CARGO_TOML" | sed '/^$/d'
}

# Workspace member package names: the members globs expanded against
# the tree, minus the excludes, each directory read by its own
# Cargo.toml `name` (the package name the shard lists spell).
member_packages() {
  local g d rel
  while IFS= read -r g; do
    for d in "${REPO_ROOT}"/$g/; do
      [ -d "$d" ] || continue
      rel="${d#"${REPO_ROOT}"/}"; rel="${rel%/}"
      grep -qxF "$rel" <(read_manifest_list exclude) && continue
      awk -F'"' '/^name *=/{ print $2; exit }' "${d}Cargo.toml"
    done
  done < <(read_manifest_list members) | sort -u
}

# One shard's crate list from .sonic/ci.yml — single-line and
# multi-line arrays alike (the balancing table is one crate per line).
shard_crates() { # $1 = shard name
  awk -v want="$1" '
    index($0, want ") crates=(") {
      sub(/^.*crates=\(/, "")
      collecting = 1
    }
    collecting {
      if (sub(/\)[[:space:]]*;;.*/, "")) collecting = 0
      gsub(/[[:space:]]/, "\n")
      print
    }
  ' "$SONIC" | sed '/^$/d'
}

exempted_names() {
  printf '%s\n' "$EXEMPTED" | sed -n 's/^\([^ ][^ ]*\) ->.*/\1/p'
}

setup() {
  member_packages > "$BATS_TEST_TMPDIR/members"
  {
    shard_crates engine
    shard_crates storage
    shard_crates planner
  } > "$BATS_TEST_TMPDIR/sharded"
  exempted_names > "$BATS_TEST_TMPDIR/exempted"
}

@test "the shard lists cover or exempt every workspace member" {
  local uncovered=0 m
  while IFS= read -r m; do
    if ! grep -qxF "$m" "$BATS_TEST_TMPDIR/sharded" \
       && ! grep -qxF "$m" "$BATS_TEST_TMPDIR/exempted"; then
      echo "member with no shard and no exemption: $m" >&2
      uncovered=1
    fi
  done < "$BATS_TEST_TMPDIR/members"
  [ "$uncovered" -eq 0 ]
}

@test "each member sits in exactly one shard" {
  local doubled
  doubled="$(sort "$BATS_TEST_TMPDIR/sharded" | uniq -d)"
  [ -z "$doubled" ] || { echo "sharded twice: $doubled" >&2; return 1; }
}

@test "no shard lists a non-member (typo guard)" {
  local bad=0 c
  while IFS= read -r c; do
    grep -qxF "$c" "$BATS_TEST_TMPDIR/members" \
      || { echo "shard lists non-member: $c" >&2; bad=1; }
  done < "$BATS_TEST_TMPDIR/sharded"
  [ "$bad" -eq 0 ]
}

@test "the exemption table carries no stale entries" {
  local bad=0 e
  while IFS= read -r e; do
    grep -qxF "$e" "$BATS_TEST_TMPDIR/members" \
      || { echo "exemption names a non-member: $e" >&2; bad=1; }
    grep -qxF "$e" "$BATS_TEST_TMPDIR/sharded" \
      && { echo "exemption is already sharded: $e" >&2; bad=1; }
  done < "$BATS_TEST_TMPDIR/exempted"
  [ "$bad" -eq 0 ]
}

# Behavioral tests for release-notes-read.sh — the agent's one shell
# lens on git history and the notes files (the exfil fix): every
# argument is validated against a strict charset before any command
# runs, and a rejected argument is never echoed — an injected value
# expanded by the shell dies in the rejection, unnamed, so neither git's
# error output nor the job log can carry the model API key.
#
# Local run:
#   bats .github/scripts/tests/release-notes-read.bats
#
# Requirements:
#   bats-core >= 1.5.0  (brew install bats-core)

bats_require_minimum_version 1.5.0

TESTS_DIR="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)"
SCRIPT="${TESTS_DIR}/../release-notes-read.sh"

setup() {
  TEST_TMP="$(mktemp -d)"
  export TEST_TMP
  (
    cd "${TEST_TMP}"
    git init -q repo
    cd repo
    git config user.email "read@test"
    git config user.name "read test"
    printf 'changelog body\n' > CHANGELOG.md
    printf 'readme body\n' > README.md
    git add CHANGELOG.md README.md
    git commit -q -m "initial"
    SHA="$(git rev-parse HEAD)"
    printf '%s\n' "$SHA" > "${TEST_TMP}/sha"
    git tag -a v0.1.0 -m "v0.1.0"
    printf 'second body\n' >> CHANGELOG.md
    git commit -qam "second (#2)"
    git tag -a v0.2.0 -m "v0.2.0"
  )
  mkdir -p "${TEST_TMP}/repo/target/release-notes"
  printf 'one\ntwo\nthree\n' > "${TEST_TMP}/repo/target/release-notes/notes.md"
}

teardown() {
  rm -rf "${TEST_TMP}"
}

run_read() {  # args: subcommand arg
  (
    cd "${TEST_TMP}/repo"
    bash "${SCRIPT}" "$@"
  )
}

@test "show reads a tagged path" {
  run run_read show v0.2.0:CHANGELOG.md
  [ "$status" -eq 0 ]
  grep -qF 'second body' <<< "$output"
}

@test "dig reads a commit over the fixed grounding paths" {
  SHA="$(cat "${TEST_TMP}/sha")"
  run run_read dig "$SHA"
  [ "$status" -eq 0 ]
  grep -qF 'readme body' <<< "$output"
}

@test "log reads a range and log30 reads at a tag" {
  run run_read log v0.1.0..v0.2.0
  [ "$status" -eq 0 ]
  grep -qF 'second (#2)' <<< "$output"

  run run_read log30 v0.2.0
  [ "$status" -eq 0 ]
  grep -qF 'initial' <<< "$output"
}

@test "lines counts a notes file" {
  run run_read lines target/release-notes/notes.md
  [ "$status" -eq 0 ]
  grep -qF '3' <<< "$output"
}

@test "an argument with shell metacharacters is refused, never echoed" {
  # The shapes an injected expansion produces: $, quotes, spaces,
  # command separators, a secret-looking value. The refusal names the
  # rule, never the argument.
  for bad in '$FIREWORKS_API_KEY' 'v1; rm -rf /' 'a b c' \
             'sk-live-abc123secret' '"$(whoami)"' 'v1$(id)u2' \
             'v0.1.0..v0.2.0;id' '/etc/passwd'; do
    run run_read show "$bad"
    [ "$status" -eq 2 ]
    ! grep -qF "$bad" <<< "$output"
  done
}

@test "lines reads nothing outside target/release-notes" {
  run run_read lines /etc/passwd
  [ "$status" -eq 2 ]
  ! grep -qF '/etc/passwd' <<< "$output"
}

@test "a secret-shaped value in the PATH position is refused, never echoed" {
  # A hex secret is charset- and SHA-shaped; in the PATH slot it is not
  # an allowlisted grounding path, and as a bare REF it does not
  # resolve - git never runs, nothing echoes.
  HEXISH="abc123def456abc123def456abc123de"
  for bad in "v0.2.0:${HEXISH}" "${HEXISH}"; do
    run run_read show "$bad"
    [ "$status" -eq 2 ]
    ! grep -qF "$HEXISH" <<< "$output"
  done
}

@test "a nonexistent ref or path is refused without git's error text" {
  run run_read show v9.9.9:CHANGELOG.md
  [ "$status" -eq 2 ]
  ! grep -qF 'v9.9.9' <<< "$output"

  run run_read show v0.2.0:docs/api/never/spec.md
  [ "$status" -eq 2 ]
}

@test "lines takes the two artifacts by exact name, existing only" {
  run run_read lines target/release-notes/../../CHANGELOG.md
  [ "$status" -eq 2 ]
  ! grep -qF 'CHANGELOG.md' <<< "$output"

  run run_read lines target/release-notes/notes.md.bak
  [ "$status" -eq 2 ]

  run run_read lines target/release-notes/absent.md
  [ "$status" -eq 2 ]
  ! grep -qF 'No such file' <<< "$output"
}

@test "an unknown subcommand is usage, not execution" {
  run run_read fetch v0.2.0:CHANGELOG.md
  [ "$status" -eq 2 ]
  grep -qF 'usage:' <<< "$output"
}

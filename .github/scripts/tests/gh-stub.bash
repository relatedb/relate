# Hermetic gh stub writer for the .github/scripts bats suites.
#
# bats-loadable: `load gh-stub` from a .bats file in this directory.
# write_gh_stub writes a gh stub executable that never touches the
# network; the caller supplies the per-subcommand success handlers as
# case arms on stdin (a quoted heredoc keeps them verbatim):
#
#   write_gh_stub "${STUBS_DIR}/gh" <<'ARMS'
#   "release list")
#     cat "${GH_STUB_RELEASES:?GH_STUB_RELEASES not set}"
#     ;;
#   ARMS
#
# The writer owns the harness around the arms, so every suite's stub
# agrees on it: each invocation appends its argv (one line per call) to
# $GH_STUB_LOG — assert calls with `grep -c '^release list '
# "${GH_STUB_LOG}"`; every arm gets a GH_STUB_FAIL=<subcommand> guard
# that fails that one subcommand loudly (the subcommand name is read
# off the arm's pattern); and an invocation matching no arm fails with
# "unexpected invocation". The caller exports GH_STUB_LOG and any
# GH_STUB_* inputs its arms read (e.g. GH_STUB_RELEASES) before running
# the script under test with the stub's directory on PATH.

write_gh_stub() {
  local dest="$1" line
  {
    printf '%s\n' \
      '#!/usr/bin/env bash' \
      'set -euo pipefail' \
      'printf '\''%s\n'\'' "$*" >> "${GH_STUB_LOG:?GH_STUB_LOG not set}"' \
      'case "${1:-} ${2:-}" in'
    while IFS= read -r line; do
      printf '%s\n' "$line"
      # An arm pattern line ("release list", "pr list") introduces an arm:
      # inject the GH_STUB_FAIL guard for that subcommand right after it.
      # The guard stays keyed on the bare subcommand word (list, view, …)
      # — the pre-existing contract the suites set GH_STUB_FAIL against.
      if [[ "$line" =~ ^[[:space:]]*\"(release|pr)\ ([a-z]+)\"\)$ ]]; then
        printf '    [ "${GH_STUB_FAIL:-}" != "%s" ] || { echo "gh stub: simulated %s failure" >&2; exit 1; }\n' \
          "${BASH_REMATCH[2]}" "${BASH_REMATCH[2]}"
      fi
    done
    printf '%s\n' \
      '  *)' \
      '    echo "gh stub: unexpected invocation: $*" >&2' \
      '    exit 1' \
      '    ;;' \
      'esac'
  } > "$dest"
  chmod +x "$dest"
}

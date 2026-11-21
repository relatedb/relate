#!/usr/bin/env bash
# Shared helpers for repo scripts (disk telemetry, CI plumbing, file
# checksums). Source this file from a script that has resolved the repo
# root, e.g.:
#   REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
#   source "$REPO/scripts/lib/common.sh"
# The bodies are POSIX sh, so /bin/sh harnesses source it with `.` too
# (scripts/tests/test_install_sh.sh).
# Do not paste copies of these helpers into callers.

# Free megabytes on the / filesystem (POSIX df, megabyte units).
free_mb() {
  df -Pm / | awk 'NR == 2 { print $4 }'
}

# Append one preformatted message line to $GITHUB_STEP_SUMMARY when the
# variable is set (local runs have no summary file). The caller owns the
# message text and format; this helper owns the guard + append mechanics.
step_summary_append() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf '%s\n' "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
}

# Bare-hex SHA256 of a file: sha256sum where it exists (Linux, CI
# runners), shasum -a 256 elsewhere (macOS hosts), and a python fallback
# on a host that ships neither coreutils. The shell tools mark an output
# line whose file name carries a backslash or newline with a leading '\'
# before the digest — and the release runners' Windows legs pass
# $RUNNER_TEMP paths (C:\Users\..., backslashes) in through the
# digest-pinned installs — so the file is hashed through stdin, where
# they print a bare '-' as the file name and the marker cannot appear
# (the v1.1.0 windows FFI release job failed its cargo-auditable digest
# pin on exactly that marker). The stdin form's trailing '  -' is cut at
# the first space in the shell — no awk fork, no pipeline — so the
# tool's status is the function's: a failed read yields no output and
# status 1 under any shell, never a pipeline's trailing-command 0.
# The body is a subshell (`( … )`, POSIX): its scratch `digest` is
# local to the call, so a sourcing caller that owns a `digest` variable
# is not clobbered, and `return 1` still ends the call with status 1.
# distribution/install.sh keeps its own inline resolution — it is a
# piped curl|sh installer and cannot source repo files.
sha256_of() (
  if command -v sha256sum >/dev/null 2>&1; then
    digest=$(sha256sum < "$1") || return 1
  elif command -v shasum >/dev/null 2>&1; then
    digest=$(shasum -a 256 < "$1") || return 1
  else
    # No shell digest tool: the one python fallback, so an installer
    # never forks this policy to carry its own. The interpreter reads
    # the path from argv, so a name spelling cannot confuse it either.
    py=python3
    command -v python3 >/dev/null 2>&1 || py=python
    digest=$(
      "$py" -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"
    ) || return 1
  fi
  printf '%s\n' "${digest%% *}"
)

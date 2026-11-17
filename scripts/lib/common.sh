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
# runners), shasum -a 256 elsewhere (macOS hosts). Both tools mark an
# output line whose file name carries a backslash or newline with a
# leading '\' before the digest — and the release runners' Windows legs
# pass $RUNNER_TEMP paths (C:\Users\..., backslashes) in through the
# digest-pinned installs — so the file is hashed through stdin, where
# the tools print no file name at all and the marker cannot appear (the
# v1.1.0 windows FFI release job failed its cargo-auditable digest pin
# on exactly that marker). The tool's status is the function's: a
# failed read yields no output and status 1 under any shell — never the
# pipeline's awk-0 — so the helper does not lean on the caller's
# pipefail.
# distribution/install.sh keeps its own inline resolution — it is a
# piped curl|sh installer and cannot source repo files.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    digest=$(sha256sum < "$1") || return 1
  else
    digest=$(shasum -a 256 < "$1") || return 1
  fi
  printf '%s\n' "$digest" | awk '{print $1}'
}

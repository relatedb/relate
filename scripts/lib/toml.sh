#!/usr/bin/env bash
# Shared TOML vocabulary for repo scripts — the readers and the writers.
# Source this file from a script that has resolved the repo root, e.g.:
#   REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
#   source "$REPO/scripts/lib/toml.sh"
# Do not paste the awk programs into callers.
#
# Writer contract: the temp file is the helper's own business (mktemp
# beside the target, removed on failure), the target's mode is preserved
# across the rename, and a write that matched nothing exits non-zero with
# the target untouched — failure is a real exit status, never a silent
# no-op rewrite.

# Print the `version = "..."` value inside one exact TOML section header.
# Usage: toml_section_version <file> <section-header, e.g. [workspace.package]>
toml_section_version() {
  awk -v section="$2" '
    /^\[/ { in_sec = ($0 == section) }
    in_sec && /^version = "/ {
      v = $0; sub(/^version = "/, "", v); sub(/".*$/, "", v)
      print v; exit
    }
  ' "$1"
}

# Print the version of one package entry in a Cargo.lock file. Returns
# non-zero when the package is absent — failure is a real exit status, not
# an empty string callers must sniff for.
# Usage: toml_lock_package_version <Cargo.lock> <package-name>
toml_lock_package_version() {
  awk -v pkg="$2" '
    $0 == "name = \"" pkg "\"" { in_pkg = 1; next }
    in_pkg && /^version = / { gsub(/"/, "", $3); print $3; found = 1; exit }
    /^\[\[package\]\]/ { in_pkg = 0 }
    END { if (!found) exit 1 }
  ' "$1"
}

# chmod the temp to the target's mode and rename it over the target
# (GNU stat -c, BSD stat -f).
_toml_replace() {
  local mode
  mode="$(stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1")"
  chmod "$mode" "$2"
  mv "$2" "$1"
}

# The writer skeleton, once: mktemp beside the target, run the awk program
# (with its -v args) over the target into the temp, remove the temp and
# return 1 when the program reports no match, otherwise _toml_replace.
# Usage: _toml_rewrite <file> <awk-program> [awk -v args...]
_toml_rewrite() {
  local file="$1" program="$2" tmp
  shift 2
  tmp="$(mktemp "$file.tmp.XXXXXX")"
  if ! awk "$@" "$program" "$file" > "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  _toml_replace "$file" "$tmp"
}

# Rewrite the `version = "..."` line inside one exact TOML section header.
# Exits non-zero with the target untouched when the section carries no
# version line — a silent pass-through would hide a grammar drift.
# Usage: toml_section_set_version <file> <section-header> <new-version>
toml_section_set_version() {
  _toml_rewrite "$1" '
    /^\[/ { in_sec = ($0 == section) }
    in_sec && /^version = "/ { sub(/"[^"]*"/, "\"" v "\""); hit = 1 }
    { print }
    END { if (!hit) exit 1 }
  ' -v section="$2" -v v="$3"
}

# Rewrite the version of one package entry in a Cargo.lock file. Exits
# non-zero with the target untouched when the package is absent — the
# reader's contract, mirrored.
# Usage: toml_lock_set_version <Cargo.lock> <package-name> <new-version>
toml_lock_set_version() {
  _toml_rewrite "$1" '
    /^\[\[package\]\]/ { in_pkg = 0 }
    $0 == "name = \"" pkg "\"" { in_pkg = 1 }
    in_pkg && !found && /^version = / { sub(/"[^"]*"/, "\"" v "\""); found = 1; in_pkg = 0 }
    { print }
    END { if (!found) exit 1 }
  ' -v pkg="$2" -v v="$3"
}

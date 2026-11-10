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

# The workspace-dependency pin vocabulary (ADR-0560): every relate* entry
# in [workspace.dependencies] carries a version pin so the publishable
# crates can reach crates.io, with one sanctioned exemption — the shared
# test-support crate stays path-only so the dev-dependencies on it strip
# at package time. The entry predicate and the exemption live here, once,
# shared by the reader and the writer below; both fail loudly on a relate*
# entry head whose inline table wraps across lines (a grammar this
# vocabulary does not rewrite), never silently half-reading it.
TOML_WORKSPACE_DEP_PATH_ONLY="relate-test-support"
_toml_ws_dep_re='^relate[a-z-]* = \{'

# Print `name version` for every `relate*` entry in a
# [workspace.dependencies] section; a pinless entry prints `name -`.
# Exits non-zero on a wrapped (multi-line) entry.
# Usage: toml_workspace_dep_versions <Cargo.toml>
toml_workspace_dep_versions() {
  awk -v re="$_toml_ws_dep_re" '
    /^\[/ { in_sec = ($0 == "[workspace.dependencies]") }
    in_sec && $0 ~ re {
      if ($0 !~ /\}/) { bad = 1; next }
      name = $1
      if ($0 ~ /version = "/) {
        line = $0
        # The extraction regex is built from the shared entry predicate
        # (re), never re-spelled: one grammar, one spelling.
        sub(re "[^}]*version = \"", "", line)
        sub(/".*$/, "", line)
        print name, line
      } else {
        print name, "-"
      }
    }
    END { if (bad) exit 1 }
  ' "$1"
}

# Rewrite the version pin of every pinned `relate*` entry in a
# [workspace.dependencies] section. Exits non-zero with the target
# untouched when no pin was rewritten, when a non-exempt entry carries no
# pin, or when an entry's inline table wraps — a silent pass-through would
# hide a grammar drift.
# Usage: toml_workspace_deps_set_version <Cargo.toml> <new-version>
toml_workspace_deps_set_version() {
  _toml_rewrite "$1" '
    /^\[/ { in_sec = ($0 == "[workspace.dependencies]") }
    in_sec && $0 ~ re {
      if ($0 !~ /\}/) { drift = 1 }
      else if ($1 == exempt) {
        # Exempt means path-only, not ignored: a pin on the exempt entry
        # is drift — it would survive the sweep stale and make crates.io
        # reject the publish (ADR-0560).
        if ($0 ~ /version = "/) { drift = 1 }
      }
      else if (sub(/version = "[^"]*"/, "version = \"" v "\"")) { hit = 1 }
      else { drift = 1 }
    }
    { print }
    END { if (drift || !hit) exit 1 }
  ' -v re="$_toml_ws_dep_re" -v exempt="$TOML_WORKSPACE_DEP_PATH_ONLY" -v v="$2"
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

#!/bin/sh
# Harness machinery for scripts/tests/test_release_bump.sh and
# scripts/tests/test_release_advance.sh: the cargo and npm stubs, the
# fixture-repo factory, and the run helper. Sourced by the test after
# tap_sandbox, the way install_sh_harness.sh is — this file holds no
# cases and no assertions.
#
# Expects from the caller: $ROOT (repo root), $sandbox (tap_sandbox's),
# $stubbin (created here under the sandbox).

stubbin="$sandbox/stub-bin"
mkdir -p "$stubbin"

# Stub cargo: release-bump.sh calls only `cargo metadata --format-version 1`
# to refresh Cargo.lock; the stub rewrites the relate package's lock version
# from the bumped workspace manifest, the one effect the bump relies on. It
# sources the fixture's own scripts/lib/toml.sh — the TOML grammar is read
# and written through the one shared vocabulary, never re-spelled here.
cat >"$stubbin/cargo" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "metadata" ]; then
	# run_bump cd's into the fixture root, so the lib resolves relatively.
	. scripts/lib/toml.sh
	v="$(toml_section_version Cargo.toml "[workspace.package]")"
	toml_lock_set_version Cargo.lock relate "$v"
	echo '{}'
	exit 0
fi
echo "stub cargo: unexpected args: $*" >&2
exit 2
EOF
chmod +x "$stubbin/cargo"

# Stub npm: the bump calls `npm install --package-lock-only --prefix
# clients/javascript` to regenerate the lockfile from the bumped manifest;
# the stub rewrites the lockfile's root versions the same way, from the
# manifest the bump just edited. The optionalDependencies file: links pass
# through untouched, as the real npm regen leaves them.
cat >"$stubbin/npm" <<'EOF'
#!/bin/sh
prefix=""
while [ $# -gt 0 ]; do
	if [ "$1" = "--prefix" ]; then
		prefix="$2"
		break
	fi
	shift
done
[ -n "$prefix" ] || {
	echo "stub npm: no --prefix" >&2
	exit 2
}
# The manifest version read goes through the shared typed vocabulary
# (scripts/lib is copied into every fixture; cwd is the fixture root
# inside run_bump), never a layout-sniffing sed.
v=$(python3 scripts/lib/npm_json.py version "$prefix/package.json")
python3 - "$prefix/package-lock.json" "$v" <<'PY'
import json
import sys

path, version = sys.argv[1], sys.argv[2]
with open(path) as fh:
    data = json.load(fh)
data["version"] = version
root = data["packages"][""]
root["version"] = version
# file: links mirror the linked platform manifests' versions into
# packages["platforms/<dir>"] entries (what the real regen writes); the
# stub reads each platform manifest's bumped version the same way.
import glob, os
manifest_dir = os.path.dirname(path)
for manifest in sorted(glob.glob(os.path.join(manifest_dir, "platforms", "*", "package.json"))):
    with open(manifest) as fh:
        plat = json.load(fh)
    entry = os.path.join("platforms", os.path.basename(os.path.dirname(manifest)))
    data["packages"].setdefault(entry, {})
    data["packages"][entry]["version"] = plat["version"]
with open(path, "w") as fh:
    json.dump(data, fh, indent=2)
    fh.write("\n")
PY
EOF
chmod +x "$stubbin/npm"

# build_fixture <dir> <changelog-file>: a minimal repo the bump can run in.
build_fixture() {
	local fx="$1" changelog_src="$2" p doc
	mkdir -p "$fx/scripts" "$fx/surfaces/relate-python/python/relate" \
		"$fx/clients/java" "$fx/clients/dotnet/src/Relate.Net" \
		"$fx/clients/javascript/platforms" \
		"$fx/docs/reference" "$fx/docs/api/cli/examples" "$fx/docs/api/java/examples"
	cp "$ROOT/scripts/release-bump.sh" "$fx/scripts/"
	cp "$ROOT/scripts/changelog_rotate.py" "$fx/scripts/"
	cp "$ROOT/scripts/changes_collect.py" "$fx/scripts/"
	cp -R "$ROOT/scripts/lib" "$fx/scripts/lib"
	cp "$ROOT/scripts/version_tokens.py" "$fx/scripts/"
	# The copy carries the source tree's bytecode caches when earlier suites
	# ran Python there; the fixture's `git add -A` would commit them and the
	# bump's own python3 calls would then dirty tracked files no surface owns.
	find "$fx/scripts" -name __pycache__ -type d -prune -exec rm -rf {} +

	cat >"$fx/Cargo.toml" <<'EOF'
[workspace.package]
version = "0.16.1"

[workspace]
members = []

# Mirrors the real workspace: relate* entries carry a version pin the
# sweep rewrites (ADR-0560); the path-only entry models
# relate-test-support, which must survive the sweep untouched.
[workspace.dependencies]
relate-model = { path = "relate/relate-model", version = "0.16.1" }
relate-test-support = { path = "testing/relate-test-support" }
EOF
	cat >"$fx/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "relate"
version = "0.16.1"
EOF
	cat >"$fx/surfaces/relate-python/pyproject.toml" <<'EOF'
[project]
name = "relate-db"
version = "0.16.1"
EOF
	cat >"$fx/surfaces/relate-python/python/relate/__init__.py" <<'EOF'
def _fallback_version():
    return "0.16.1"


__version__ = _fallback_version()


def other_fallback():
    return "0.16.1"
EOF
	cat >"$fx/clients/java/pom.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>org.relate</groupId>
  <artifactId>relate</artifactId>
  <version>0.16.1</version>
</project>
EOF
	cat >"$fx/clients/dotnet/src/Relate.Net/Relate.Net.csproj" <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <Version>0.16.1</Version>
    <Description>A client.</Description>
  </PropertyGroup>
</Project>
EOF
	# The five platform packages are spelled once, as one list; the two
	# documents derive every row from it, emitted by the json.dump the npm
	# stub above already uses — no hand-escaped quotes to get wrong.
	local plats="linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64"
	for p in $plats; do
		mkdir -p "$fx/clients/javascript/platforms/$p"
		cat >"$fx/clients/javascript/platforms/$p/package.json" <<EOF
{
  "name": "@relate/$p",
  "version": "0.16.1"
}
EOF
	done
	python3 - "$fx/clients/javascript" $plats <<'PY'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
deps = {f"@relate/relate-{p}": f"file:platforms/{p}" for p in sys.argv[2:]}
(root / "package.json").write_text(
    json.dumps(
        {"name": "@relate/relate", "version": "0.16.1", "optionalDependencies": deps},
        indent=2,
    )
    + "\n"
)
(root / "package-lock.json").write_text(
    json.dumps(
        {
            "version": "0.16.1",
            "packages": {
                "": {"version": "0.16.1", "optionalDependencies": deps},
                **{f"platforms/{p}": {"version": "0.16.1"} for p in sys.argv[2:]},
            },
        },
        indent=2,
    )
    + "\n"
)
PY
	for doc in README.md docs/limitations.md docs/reference/rust-library.md \
		docs/reference/cli-flags.md docs/api/cli/examples/agents.md \
		docs/api/java/examples/RelateE2E.java; do
		echo "Release 0.16.1 notes." >"$fx/$doc"
	done
	cp "$changelog_src" "$fx/CHANGELOG.md"

	# The fragment directory every checkout of the repository carries
	# (ADR-0627): its README, so the collector's pool is empty but the
	# directory exists. The seed_fragments helper below fills the pool.
	mkdir -p "$fx/.changes"
	printf '%s\n' "# Changelog fragments" >"$fx/.changes/README.md"

	git -C "$fx" init -q
	git -C "$fx" add -A
	git -C "$fx" -c user.email=test@example.com -c user.name=test \
		commit -qm fixture
}

# seed_fragments <dir>: commit one valid fragment pool into a fixture —
# an Added entry and a Notes entry, the shapes a real cut consumes. The
# files are committed (the rollback's git checkout restores them from
# HEAD), never left untracked.
seed_fragments() {
	local fx="$1"
	mkdir -p "$fx/.changes"
	cat >"$fx/.changes/2-flag.md" <<'EOF'
### Added

- A flag.
EOF
	cat >"$fx/.changes/3-notes.md" <<'EOF'
### Notes

- An internal note.
EOF
	git -C "$fx" add .changes
	git -C "$fx" -c user.email=test@example.com -c user.name=test \
		commit -qm fragments
}

run_bump() { # $1 = fixture dir, $2 = NEXT_VERSION, the rest = release-bump arguments
	local dir="$1"
	local ver="$2"
	shift 2
	(cd "$dir" && PATH="$stubbin:$PATH" NEXT_VERSION="$ver" \
		bash scripts/release-bump.sh "$@") 2>"$dir/stderr.log"
}

# The canonical after-cut changelog: a release section exists, an empty
# [Unreleased] sits above it, and the compare refs point at the cut.
# One fixture factory for every suite that needs the shape — the bump
# suite's steady-state case and the advance suite's differential both
# start from these exact bytes.
after_cut_changelog() { # $1 = output file
	cat >"$1" <<'EOF'
# Changelog

## [Unreleased]

## [0.1.0] — 2026-11-08

### Initial release

First release of Relate.

[Unreleased]: https://github.com/relatedb/relate/compare/v0.1.0...HEAD

[0.1.0]: https://github.com/relatedb/relate/releases/tag/v0.1.0
EOF
}

#!/usr/bin/env python3
"""Render the Homebrew formula for a release and write it into the project tap.

Usage:
  homebrew_formula.py <manifest> <tag> --tap-dir <dir> [--dry-run]

`<manifest>` is the release asset manifest (assets.json: asset name to sha256
and size) the release ships. The formula pins, for every platform Homebrew
serves, the release download URL and the manifest's sha256 for that asset:
macOS arm64 and amd64, Linux arm64 and amd64. Every one of those four assets
must carry a manifest entry, or the render fails naming the missing asset —
a formula that cannot verify what it installs never reaches the tap.

`--tap-dir` is a checkout of the tap repository (`relatedb/homebrew-tap`); the
formula lands at `Formula/relate.rb`. `--dry-run` prints the formula and
writes nothing, so the release proves the render for the new tag without
pushing to the tap.

Run: python3 scripts/homebrew_formula.py release/assets.json v1.2.0 --tap-dir tap
     python3 scripts/homebrew_formula.py release/assets.json v1.2.0 --tap-dir tap --dry-run
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from string import Template

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from release_assets import asset_sha256, load_json_object, release_download_url
from release_version import release_version

# The tap's formula path, relative to the tap checkout root.
FORMULA_PATH = Path("Formula") / "relate.rb"

# (os block, cpu block, release asset), in formula order. Each asset is a
# bare executable attached to the release.
PLATFORMS = (
    ("macos", "arm", "relate-darwin-arm64"),
    ("macos", "intel", "relate-darwin-amd64"),
    ("linux", "arm", "relate-linux-arm64"),
    ("linux", "intel", "relate-linux-amd64"),
)

# The formula is generated: the version comes from the tag in the download
# URL, so no `version` line can drift from it. The install writes the
# provenance marker (ADR-0659) beside the real binary, which relate reads to
# defer its upgrade to `brew upgrade relate`. The real binary and the marker
# live in libexec, reachable on PATH through the bin symlink: a data file in
# bin fails `brew audit`. `$` is the only Template metacharacter, so Ruby's
# `#{}` interpolation is literal.
FORMULA = Template(
    """class Relate < Formula
  desc "Embedded openCypher graph database in a single binary"
  homepage "https://relatedb.ai/"
  license "Apache-2.0"

$platforms

  def install
    libexec.install cached_download => "relate"
    chmod 0755, libexec/"relate"
    (libexec/".relate-package-managed").write <<~JSON
      {"manager":"homebrew","upgrade":"brew upgrade relate"}
    JSON
    bin.install_symlink libexec/"relate"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/relate --version")
  end
end
"""
)


class FormulaError(Exception):
    """The manifest cannot produce a formula that installs every platform."""


def _platform_blocks(manifest: dict[str, object], tag: str) -> str:
    """The `on_macos`/`on_linux` blocks, one `url`+`sha256` pair per CPU."""
    lines: list[str] = []
    for os_name in dict.fromkeys(os_name for os_name, _, _ in PLATFORMS):
        lines.append(f"  on_{os_name} do")
        for _, cpu, asset in (row for row in PLATFORMS if row[0] == os_name):
            lines.append(f"    on_{cpu} do")
            lines.append(f'      url "{release_download_url(tag, asset)}"')
            lines.append(f'      sha256 "{asset_sha256(manifest, asset, FormulaError)}"')
            lines.append("    end")
        lines.append("  end")
        lines.append("")
    return "\n".join(lines[:-1])


def render(manifest: dict[str, object], tag: str) -> str:
    """The complete formula for one release tag and manifest."""
    release_version(tag, FormulaError)  # Homebrew scans the version from the tag in each URL
    return FORMULA.substitute(platforms=_platform_blocks(manifest, tag))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Render the Homebrew formula for a release into the project tap."
    )
    parser.add_argument("manifest", type=Path, help="the release asset manifest (assets.json)")
    parser.add_argument("tag", help="the release tag, e.g. v1.2.0")
    parser.add_argument(
        "--tap-dir",
        type=Path,
        required=True,
        help="the tap checkout to write Formula/relate.rb into",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="print the formula and write nothing",
    )
    args = parser.parse_args(argv)

    try:
        formula = render(load_json_object(args.manifest, "manifest", FormulaError), args.tag)
    except FormulaError as err:
        print(err, file=sys.stderr)
        return 1

    target = args.tap_dir / FORMULA_PATH
    summary = f"{target} (relate {release_version(args.tag, FormulaError)})"
    if args.dry_run:
        print(f"would write {summary}")
        print(formula, end="")
        return 0
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(formula)
    print(f"wrote {summary}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

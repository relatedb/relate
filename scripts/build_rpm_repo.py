#!/usr/bin/env python3
"""Build the RPM package and the signed YUM/DNF repository tree for one
relate release binary (ADR-0659).

Usage:
  python3 scripts/build_rpm_repo.py --binary PATH --version X.Y.Z \\
      --glibc-floor GLIBC_X.Y --output DIR [--base-url URL]

The package keeps the real binary out of `PATH` at `/usr/lib/relate/relate`
and symlinks `/usr/bin/relate` to it, so the provenance marker
`/usr/lib/relate/.relate-package-managed` lands beside the canonicalized
`current_exe` — the file an upgrade would overwrite (ADR-0659, "Install
provenance"). The marker names the manager (`rpm`) and the command the user
runs to upgrade the package.

The output tree is the flat repository layout dnf reads:

  <output>/relate.repo                       the repo file the user adds
  <output>/repo-signing-key.asc              the key its gpgkey= points at
  <output>/<arch>/relate-<version>-1.<arch>.rpm
  <output>/<arch>/repodata/                  createrepo_c metadata
  <output>/<arch>/repodata/repomd.xml.asc    signature over repomd.xml

`createrepo_c` writes the metadata and scripts/sign-repo-metadata.sh signs
`repomd.xml` with the REPO_GPG_KEY secret, verifying the signature against
the committed public key (distribution/repo-signing-key.asc). The packages
carry no signature of their own: repo_gpgcheck authenticates the index, and
the index's checksums carry the package bytes (ADR-0659, "Signing-key
policy").

The package is built for the host's rpmbuild architecture; a multi-arch
repository runs this once per architecture on a builder of that
architecture. Requires `rpmbuild`, `createrepo_c` and `gpg` on PATH.
"""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from release_version import release_tag

ROOT = Path(__file__).resolve().parents[1]
SIGN_SCRIPT = ROOT / "scripts" / "sign-repo-metadata.sh"
PUBLIC_KEY = ROOT / "distribution" / "repo-signing-key.asc"

# The canonical repository the release publishes (ADR-0659, "Repository
# hosting"): relate-web serves the generated tree at this path.
DEFAULT_BASE_URL = "https://relatedb.ai/rpm"

# The command dnf users run to upgrade the package. The marker carries it
# verbatim so `relate upgrade` can point at it (ADR-0659).
UPGRADE_COMMAND = "sudo dnf upgrade relate"

GLIBC_FLOOR_RE = re.compile(r"^GLIBC_(\d+\.\d+)$")

REQUIRED_TOOLS = ("rpm", "rpmbuild", "createrepo_c")


def _require_tools() -> None:
    missing = [tool for tool in REQUIRED_TOOLS if shutil.which(tool) is None]
    if missing:
        raise ValueError(f"missing required tools on PATH: {', '.join(missing)}")


def parse_glibc_floor(value: str) -> str:
    """`GLIBC_2.35` -> `2.35`, the version rpm's glibc dependency compares."""
    match = GLIBC_FLOOR_RE.match(value)
    if match is None:
        raise ValueError(
            f"glibc floor '{value}' is not GLIBC_<major>.<minor>, e.g. GLIBC_2.35"
        )
    return match.group(1)


def marker_json() -> str:
    """The provenance marker's content, written beside the real binary."""
    return f'{{"manager": "rpm", "upgrade": "{UPGRADE_COMMAND}"}}'


def render_spec(version: str, glibc_floor: str, binary: Path) -> str:
    """The RPM spec: the binary under /usr/lib/relate, the marker beside it,
    and a /usr/bin symlink onto PATH."""
    return f"""\
Name:           relate
Version:        {version}
Release:        1
Summary:        Embedded openCypher graph database CLI
License:        Apache-2.0
URL:            https://relatedb.ai
# The oldest glibc the release binary's symbol versions run on (ADR-0659,
# "Consequences"); a distribution older than this must refuse the package.
Requires:       glibc >= {glibc_floor}

%description
Relate is an embedded in-memory property-graph engine with a single-binary
command-line shell. This package is managed by your RPM package manager:
upgrade it with `{UPGRADE_COMMAND}`, not `relate upgrade`.

%install
mkdir -p %{{buildroot}}/usr/lib/relate %{{buildroot}}/usr/bin
install -m 0755 {shlex.quote(str(binary))} %{{buildroot}}/usr/lib/relate/relate
cat > %{{buildroot}}/usr/lib/relate/.relate-package-managed <<'MARKER'
{marker_json()}
MARKER
chmod 0644 %{{buildroot}}/usr/lib/relate/.relate-package-managed
ln -s /usr/lib/relate/relate %{{buildroot}}/usr/bin/relate

%files
%dir /usr/lib/relate
/usr/lib/relate/relate
/usr/lib/relate/.relate-package-managed
/usr/bin/relate
"""


def render_repo_file(base_url: str) -> str:
    """The dnf repository file: metadata signature verified, package
    signatures not carried."""
    return f"""\
[relate]
name=Relate
baseurl={base_url}/$basearch
enabled=1
# The index (repomd.xml) is signed with the relate repository key and
# verified here; the packages carry no signature, their integrity rides
# the signed index's checksums.
repo_gpgcheck=1
gpgcheck=0
gpgkey={base_url}/repo-signing-key.asc
"""


def _run(argv: list[str], **kwargs: object) -> None:
    subprocess.run(argv, check=True, **kwargs)  # noqa: S603 — fixed argv


def build(binary: Path, version: str, glibc_floor: str, output: Path, base_url: str) -> Path:
    """Build the rpm and its signed repository tree; return the repo file."""
    if not binary.is_file():
        raise ValueError(f"release binary '{binary}' does not exist")
    glibc = parse_glibc_floor(glibc_floor)
    release_tag(version, ValueError)  # the version the package records is bare X.Y.Z
    if not os.environ.get("REPO_GPG_KEY"):
        raise ValueError(
            "REPO_GPG_KEY is empty — the repository metadata cannot be signed"
        )
    if output.is_file():
        raise ValueError(f"output path '{output}' is a file, not a directory")
    if output.exists() and any(output.iterdir()):
        raise ValueError(f"output directory '{output}' is not empty")

    _require_tools()
    arch = subprocess.run(
        ["rpm", "--eval", "%{_arch}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if not arch:
        raise ValueError("rpm --eval %{_arch} reported no architecture")

    with tempfile.TemporaryDirectory(prefix="relate-rpm-") as topdir:
        topdir_path = Path(topdir)
        (topdir_path / "SPECS").mkdir()
        spec = topdir_path / "SPECS" / "relate.spec"
        spec.write_text(render_spec(version, glibc, binary.resolve()))
        _run(
            [
                "rpmbuild",
                "-bb",
                "--define",
                f"_topdir {topdir_path}",
                str(spec),
            ],
            stdout=subprocess.DEVNULL,
        )
        rpms = sorted((topdir_path / "RPMS" / arch).glob("*.rpm"))
        if len(rpms) != 1:
            raise ValueError(f"rpmbuild produced {len(rpms)} packages, expected 1")

        repo_dir = output / arch
        repo_dir.mkdir(parents=True)
        shutil.copy2(rpms[0], repo_dir / rpms[0].name)

    _run(["createrepo_c", str(repo_dir)], stdout=subprocess.DEVNULL)

    repomd = repo_dir / "repodata" / "repomd.xml"
    _run([str(SIGN_SCRIPT), str(repomd), str(repomd) + ".asc"])

    shutil.copy2(PUBLIC_KEY, output / "repo-signing-key.asc")
    repo_file = output / "relate.repo"
    repo_file.write_text(render_repo_file(base_url))
    print(f"wrote {output / 'relate.repo'} and {repo_dir / rpms[0].name}")
    return repo_file


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Build the RPM package and its signed YUM/DNF repository tree."
    )
    parser.add_argument("--binary", type=Path, required=True, help="the release binary")
    parser.add_argument("--version", required=True, help="release version, X.Y.Z")
    parser.add_argument(
        "--glibc-floor",
        required=True,
        help="the binary's glibc floor as GLIBC_<major>.<minor>, e.g. GLIBC_2.35",
    )
    parser.add_argument("--output", type=Path, required=True, help="repository tree directory")
    parser.add_argument(
        "--base-url",
        default=DEFAULT_BASE_URL,
        help=f"the repository's public root (default: {DEFAULT_BASE_URL})",
    )
    args = parser.parse_args(argv)
    try:
        build(args.binary, args.version, args.glibc_floor, args.output, args.base_url)
    except (ValueError, subprocess.CalledProcessError) as err:
        print(f"build_rpm_repo: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

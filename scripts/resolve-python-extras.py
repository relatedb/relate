#!/usr/bin/env python3
"""Pin relate-python's optional extras for the nightly advisory scan
(.github/workflows/dependency-advisories.yml, ADR-0702).

The Python bindings' native code is Rust (cargo-deny scans it); their only
third-party Python dependencies are the optional extras, declared in
surfaces/relate-python/pyproject.toml as version floors with no lock file.
This resolves those extras the way a fresh `pip install relate-db[...]`
would today, and writes the result as a pinned requirements file that
osv-scanner reads with `-L requirements.txt:OUTPUT`.

The resolution runs `pip install --dry-run --report` in a throwaway venv:
nothing is installed, and `--only-binary=:all:` keeps pip from building
(and so executing) any sdist. It resolves for the running interpreter, so
the pins are what that Python version would get.

Usage: resolve-python-extras.py OUTPUT

Requires Python 3.11+ (the stdlib TOML reader) and network access to PyPI.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import tomllib
import venv
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PYPROJECT = ROOT / "surfaces" / "relate-python" / "pyproject.toml"


def extras_requirements(pyproject: Path) -> list[str]:
    """Every requirement listed under any optional-dependency extra."""
    try:
        extras = tomllib.loads(pyproject.read_text())["project"]["optional-dependencies"]
    except (OSError, tomllib.TOMLDecodeError, KeyError) as exc:
        sys.exit(f"resolve-python-extras: no [project.optional-dependencies] in {pyproject}: {exc}")
    reqs = sorted({req for group in extras.values() for req in group})
    if not reqs:
        sys.exit(f"resolve-python-extras: {pyproject} declares no optional dependencies")
    return reqs


def resolve(reqs: list[str]) -> list[tuple[str, str]]:
    """(name, version) for every distribution pip would install for reqs."""
    with tempfile.TemporaryDirectory() as tmp:
        env_dir = Path(tmp) / "venv"
        venv.create(env_dir, with_pip=True)
        python = env_dir / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
        report = Path(tmp) / "report.json"
        subprocess.run(
            [
                str(python), "-m", "pip", "install",
                "--dry-run", "--ignore-installed", "--only-binary=:all:",
                "--quiet", "--disable-pip-version-check",
                "--report", str(report),
                *reqs,
            ],
            check=True,
        )
        installs = json.loads(report.read_text())["install"]
    return sorted(
        ((item["metadata"]["name"], item["metadata"]["version"]) for item in installs),
        key=lambda pin: pin[0].lower(),
    )


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] in ("-h", "--help"):
        print("usage: resolve-python-extras.py OUTPUT", file=sys.stderr)
        return 2
    output = Path(sys.argv[1])
    reqs = extras_requirements(PYPROJECT)
    pins = resolve(reqs)
    lines = [
        f"# {PYPROJECT.relative_to(ROOT)} extras ({', '.join(reqs)}),",
        f"# resolved for Python {sys.version_info.major}.{sys.version_info.minor}",
        *(f"{name}=={version}" for name, version in pins),
    ]
    output.write_text("\n".join(lines) + "\n")
    print(f"resolve-python-extras: pinned {len(pins)} distributions to {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

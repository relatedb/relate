#!/usr/bin/env python3
"""Install one package-manager channel's published package and assert it runs.

Usage:
  install_smoke.py [--channel <name>] --tag <vX.Y.Z>

`.github/workflows/install-smoke.yml` dispatches one job per channel with the
release tag to check: each manager publishes a tag after the release is cut,
at its own pace, so the lane takes the tag as an input rather than riding the
release. `--channel` checks one channel; without it every channel runs in
table order, because one failing channel never hides the others' results.

Each command is the channel's documented install command (the README's
per-manager block, ADR-0659) with the manager's non-interactive flag added
(`-y`, `--accept-*-agreements`): the documentation is for people at a
terminal, the lane runs unattended. The installed `relate` must then print
`Relate v<version>` for the tag's version and answer one query. One line per
channel:

  install-smoke: <channel> ok <tag>
  install-smoke: <channel> FAILED: <why>     (the command output follows)

Exit 0 when every checked channel passed, 1 when one failed.

Run: python3 scripts/install_smoke.py --channel homebrew --tag v1.2.0
     python3 scripts/install_smoke.py --tag v1.2.0
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))

from release_version import release_version

# channel -> the documented install command, in ADR-0659's channel order.
# Every command must leave `relate` on PATH.
CHANNELS: dict[str, str] = {
    "homebrew": "brew install relatedb/tap/relate",
    "apt": (
        "sudo install -d -m 0755 /etc/apt/keyrings\n"
        "curl -fsSL https://relatedb.ai/apt/repo-signing-key.asc"
        " | sudo tee /etc/apt/keyrings/relate.asc >/dev/null\n"
        'echo "deb [signed-by=/etc/apt/keyrings/relate.asc]'
        ' https://relatedb.ai/apt stable main"'
        " | sudo tee /etc/apt/sources.list.d/relate.list >/dev/null\n"
        "sudo apt-get update\n"
        "sudo apt-get install -y relate"
    ),
    "rpm": (
        "sudo curl -fsSL https://relatedb.ai/rpm/relate.repo"
        " -o /etc/yum.repos.d/relate.repo && sudo dnf install -y relate"
    ),
    "winget": (
        "winget install --id Relatedb.Relate -e"
        " --accept-source-agreements --accept-package-agreements"
    ),
    "scoop": (
        "scoop bucket add relatedb https://github.com/relatedb/scoop-bucket"
        " && scoop install relate"
    ),
    "chocolatey": "choco install relate -y",
}

# The one query every channel answers: a constant, so a broken engine shows
# up as a wrong answer rather than as a missing result.
QUERY = "RETURN 1 AS ok"
QUERY_ANSWER = "ok\n1"
VERSION_PREFIX = "Relate v"


def run(command: str) -> subprocess.CompletedProcess[str]:
    """Run one command through the platform shell, output captured."""
    return subprocess.run(
        command, shell=True, text=True, capture_output=True, errors="replace"
    )


def _report(channel: str, reason: str, *results: subprocess.CompletedProcess[str]) -> bool:
    """One channel's failure line, the offending commands' output under it."""
    print(f"install-smoke: {channel} FAILED: {reason}", file=sys.stderr)
    for result in results:
        for stream in (result.stdout, result.stderr):
            for line in stream.splitlines():
                print(f"    {line}", file=sys.stderr)
    return False


def check(channel: str, command: str, version: str) -> bool:
    """Install one channel's package and assert the binary it left runs."""
    install = run(command)
    if install.returncode != 0:
        return _report(channel, f"install command exited {install.returncode}", install)
    reported = run("relate --version")
    if reported.returncode != 0 or f"{VERSION_PREFIX}{version}" not in reported.stdout:
        return _report(channel, f"--version did not print '{VERSION_PREFIX}{version}'", reported)
    answered = run(f'relate --format csv "{QUERY}"')
    if answered.returncode != 0 or answered.stdout.strip() != QUERY_ANSWER:
        return _report(channel, f"'{QUERY}' was not answered {QUERY_ANSWER!r}", answered)
    print(f"install-smoke: {channel} ok v{version}")
    return True


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--channel",
        choices=list(CHANNELS),
        help="check one channel; default: every channel",
    )
    parser.add_argument(
        "--tag",
        required=True,
        type=lambda value: release_version(value, argparse.ArgumentTypeError),
        help="release tag to check (vX.Y.Z)",
    )
    args = parser.parse_args(argv)
    channels = [args.channel] if args.channel else list(CHANNELS)
    passed = [check(channel, CHANNELS[channel], args.tag) for channel in channels]
    return 0 if all(passed) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

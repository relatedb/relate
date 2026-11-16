"""The Dockerfile chef stage's pinned build-tool versions are the
installer scripts' versions.

Two lanes install each build tool from its GitHub release: the Docker
image build (the Dockerfile chef stage, digest pinned inline per tool)
and the CI host lanes (scripts/install-sccache.sh,
scripts/install-cargo-auditable.sh). Both lanes sha-verify against
their own pin, so a version that drifts between them is silent — the
image and the release workflows would embed different tools with
nothing flagging the skew. This guard ties each Dockerfile ARG to its
installer script's VERSION; a bump that touches one side fails here
until the other follows.
"""

import re
import unittest
from pathlib import Path

from script_loader import ROOT

DOCKERFILE = ROOT / "Dockerfile"


def dockerfile_arg(name: str) -> str:
    """The chef stage's ``ARG <name>=<value>``; exactly one, or a loud error."""
    matches = re.findall(rf"^ARG {name}=(\S+)$", DOCKERFILE.read_text(encoding="utf-8"), re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"Dockerfile: expected exactly one ARG {name}=, found {len(matches)}")
    return matches[0]


def installer_version(script: str) -> str:
    """The installer script's ``VERSION=`` constant, leading ``v`` stripped."""
    text = (ROOT / "scripts" / script).read_text(encoding="utf-8")
    matches = re.findall(r'^VERSION="?([^"\n]+)"?$', text, re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"{script}: expected exactly one VERSION=, found {len(matches)}")
    return matches[0].removeprefix("v")


class ToolPinTests(unittest.TestCase):
    """Each pinned tool's version is spelled once across the two lanes."""

    def test_sccache_version_matches_install_sccache_sh(self) -> None:
        self.assertEqual(
            dockerfile_arg("SCCACHE_VERSION"),
            installer_version("install-sccache.sh"),
        )

    def test_cargo_auditable_version_matches_install_cargo_auditable_sh(self) -> None:
        self.assertEqual(
            dockerfile_arg("CARGO_AUDITABLE_VERSION"),
            installer_version("install-cargo-auditable.sh"),
        )


if __name__ == "__main__":
    unittest.main()

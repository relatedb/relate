#!/usr/bin/env python3
"""Tests for scripts/install_smoke.py — the per-channel install smoke lane.

A fixture run drives the lane with its commands replaced: the channels that
install run `true`, the one deliberately broken channel runs `false`, and a
fake `relate` on PATH answers `--version` and the query — so the broken
channel must fail alone and the rest must still report ok. The remaining
cases pin the two assertions the lane owes every channel (the version line
and the query) and the workflow's one-job-per-channel binding.

Run: python3 scripts/tests/test_install_smoke.py
"""

from __future__ import annotations

import contextlib
import io
import os
import re
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from script_loader import ROOT, load

SCRIPT = ROOT / "scripts" / "install_smoke.py"
WORKFLOW = ROOT / ".github" / "workflows" / "install-smoke.yml"
TAG = "v1.2.0"
VERSION = "1.2.0"

smoke = load("install_smoke", SCRIPT)

# Every channel of the lane, its command swapped for the fixture run: one
# channel is deliberately broken, the rest install nothing.
FIXTURE_CHANNELS = {
    "homebrew": "true",
    "apt": "true",
    "rpm": "true",
    "winget": "true",
    "scoop": "false",
    "chocolatey": "true",
}

# The installed binary the fixture commands "install": FAKE_VERSION moves the
# version it reports, FAKE_QUERY_EXIT the exit status of the query.
FAKE_RELATE = """#!/bin/sh
case "$*" in
  *--version*) echo "Relate v${FAKE_VERSION:-1.2.0}" ;;
  *) echo ok; echo 1; exit "${FAKE_QUERY_EXIT:-0}" ;;
esac
"""


def run_lane(
    argv: list[str],
    channels: dict[str, str] | None = None,
    env: dict[str, str] | None = None,
) -> tuple[int, str, str]:
    """Run the lane with a fake `relate` first on PATH; (code, out, err)."""
    with tempfile.TemporaryDirectory() as tmp:
        bin_dir = Path(tmp) / "bin"
        bin_dir.mkdir()
        relate = bin_dir / "relate"
        relate.write_text(FAKE_RELATE)
        relate.chmod(relate.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        environ = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}
        environ.update(env or {})
        out, err = io.StringIO(), io.StringIO()
        with (
            mock.patch.object(smoke, "CHANNELS", channels or dict(smoke.CHANNELS)),
            mock.patch.dict(os.environ, environ),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            code = smoke.main(argv)
    return code, out.getvalue(), err.getvalue()


class FixtureRun(unittest.TestCase):
    def test_a_broken_channel_fails_alone(self) -> None:
        code, out, err = run_lane(["--tag", TAG], FIXTURE_CHANNELS)
        self.assertEqual(code, 1)
        self.assertIn("install-smoke: scoop FAILED: install command exited 1", err)
        self.assertNotIn("install-smoke: scoop ok", out)
        for channel in ("homebrew", "apt", "rpm", "winget", "chocolatey"):
            self.assertIn(f"install-smoke: {channel} ok {TAG}", out)

    def test_every_channel_passing_prints_one_ok_line_in_table_order(self) -> None:
        code, out, _ = run_lane(["--tag", TAG], {c: "true" for c in FIXTURE_CHANNELS})
        self.assertEqual(code, 0)
        self.assertEqual(
            out.splitlines(),
            [f"install-smoke: {channel} ok {TAG}" for channel in FIXTURE_CHANNELS],
        )


class Assertions(unittest.TestCase):
    def test_a_version_other_than_the_tag_fails_the_channel(self) -> None:
        code, _, err = run_lane(
            ["--channel", "homebrew", "--tag", TAG],
            {"homebrew": "true"},
            env={"FAKE_VERSION": "0.9.0"},
        )
        self.assertEqual(code, 1)
        self.assertIn("--version did not print 'Relate v1.2.0'", err)

    def test_a_query_that_does_not_answer_fails_the_channel(self) -> None:
        code, _, err = run_lane(
            ["--channel", "homebrew", "--tag", TAG],
            {"homebrew": "true"},
            env={"FAKE_QUERY_EXIT": "1"},
        )
        self.assertEqual(code, 1)
        self.assertIn("'RETURN 1 AS ok' was not answered 'ok\\n1'", err)

    def test_an_unknown_channel_is_a_usage_error(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                smoke.main(["--channel", "nix", "--tag", TAG])
        self.assertEqual(raised.exception.code, 2)


class WorkflowBinding(unittest.TestCase):
    def test_the_workflow_runs_one_job_per_channel(self) -> None:
        channels = re.findall(r"--channel (\w+)", WORKFLOW.read_text())
        self.assertEqual(sorted(channels), sorted(smoke.CHANNELS))


if __name__ == "__main__":
    unittest.main()

"""Shared helpers for the stellar night-shift tool tests: the fixture
ledger builders (f, led), a UTC datetime inside (or outside) the
night-shift window (utc), and a temp git repo with write/commit helpers
(RepoFixture). The pipeline modules the modules under test import
(facts, incremental) live in the stellar-review plugin (ADR-0623):
STELLAR_ROOT names the installed opencode-stellar-review package root,
and the test modules put its tools directory on sys.path before
importing this one, so facts resolves from there."""

import os
import shutil
import sys
import tempfile
import unittest

_STELLAR_TOOLS = os.path.join(
    os.environ.get("STELLAR_ROOT", ""), "skills", "stellar-review", "tools"
)
sys.path.insert(0, _STELLAR_TOOLS)
import facts  # noqa: E402


def f(fid, path, sev="medium", status="open", q=None):
    """One open ledger finding, the shape nightshift.open_findings reads."""
    return {"id": fid, "path": path, "severity": sev, "status": status,
            "checklist_id": "std-1", "quote_hash": q or fid, "summary": "s", "remedy": "r"}


def led(*fs):
    """One ledger document from its findings."""
    return {"findings": {x["id"]: x for x in fs}}


def utc(h, month=1):
    """A UTC datetime inside the night-shift window (or not, per h)."""
    from datetime import datetime, timezone
    return datetime(2026, month, 15, h, 0, tzinfo=timezone.utc)


class RepoFixture(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        facts.git(self.dir, "init", "-q", "-b", "main")
        facts.git(self.dir, "config", "user.email", "t@t.t")
        facts.git(self.dir, "config", "user.name", "t")

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def write(self, path, content):
        full = os.path.join(self.dir, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as fh:
            fh.write(content)

    def commit(self, msg):
        facts.git(self.dir, "add", "-A")
        facts.git(self.dir, "commit", "-qm", msg)
        return facts.git(self.dir, "rev-parse", "HEAD").strip()

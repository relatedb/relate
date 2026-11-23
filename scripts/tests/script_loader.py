"""Shared helpers for the scripts/tests/ suites, owning two policies.

The first policy is loading a subject from a path: the subjects under
scripts/ are hyphenated (`ci-timings-report.py`, `baseline-fingerprint.py`),
so they are not importable by name, and `load` is the one importlib loader
(the two suites previously carried verbatim copies). The second is capturing
a subject's run: `capture_stderr` calls a subject's main(argv) with stderr
captured — the one capture policy a test uses instead of hand-rolling a
StringIO sequence. Both are imported as a plain module: scripts/tests/ is
sys.path[0] when the suite runs.
"""

import contextlib
import importlib.util
import io
import sys
from pathlib import Path
from types import ModuleType
from typing import Callable

ROOT = Path(__file__).resolve().parents[2]


def capture_stderr(main: Callable[[list[str]], int], argv: list[str]) -> tuple[int, str]:
    """Call a subject's main(argv) with stderr captured — the code path a
    workflow step runs, where a refusal names its offenders. Returns the
    exit code and the captured stderr."""
    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        code = main(argv)
    return code, err.getvalue()


def load(name: str, path: Path) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    # Register before exec — the documented way to run a module from a
    # path. Classes resolve their own module through sys.modules:
    # dataclasses, typing.get_type_hints, and pickling all look there, and
    # without the registration a dataclass in the loaded module crashes
    # on Python 3.12 (its _is_type probe resolves *string* annotations
    # through sys.modules[cls.__module__], which was None).
    prior = sys.modules.get(name)
    sys.modules[name] = mod
    try:
        spec.loader.exec_module(mod)
    except BaseException:
        # Remove exactly what this call installed: restore a prior
        # entry, delete only when there was none.
        if prior is None:
            del sys.modules[name]
        else:
            sys.modules[name] = prior
        raise
    return mod

#!/usr/bin/env python3
"""The one atomic sidecar-write policy for the Python release scripts.

Writing a file through a sibling temp and an atomic rename keeps the target
always either untouched or complete: an interrupt mid-write never truncates
it. The temp carries the `<name>.tmp.XXXXXX` sidecar shape scripts/lib/
toml.sh's writers own and the release bump's rollback sweeps, so a killed
run leaves nothing a later sweep would miss.

`scripts/lib/pom.py` and `scripts/changelog_rotate.py` both rewrite a
release file in place and previously carried their own copy of this policy
— and had already drifted (the mode preserved with `os.chmod(stat.S_IMODE(
...))` in one, `shutil.copymode` in the other). One helper, one shape.

Not for the shell side: scripts/lib/toml.sh is the shell spelling of the
same policy and stays the one home for shell callers.
"""

from __future__ import annotations

import os
import stat
import tempfile
from pathlib import Path


def atomic_write(path: Path, text: str) -> None:
    """Write `text` over `path` through a sibling temp and an atomic rename.

    The temp is `path.name + ".tmp.XXXXXX"` beside the target, and the
    rename preserves the target's mode (mkstemp creates 0600) — the sidecar
    shape and mode policy the shell side (toml.sh) and the release bump's
    rollback share. On any failure the temp is removed and the target is
    left as it was.
    """
    fd, tmp_name = tempfile.mkstemp(prefix=f"{path.name}.tmp.", dir=path.parent)
    tmp = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(text)
        os.chmod(tmp, stat.S_IMODE(path.stat().st_mode))
        os.replace(tmp, path)
    finally:
        # A no-op once the rename landed; otherwise the interrupted write
        # leaves nothing behind.
        tmp.unlink(missing_ok=True)

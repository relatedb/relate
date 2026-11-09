"""Drive the hidden Archie surface (ADR-0543) in a PTY.

Two launches of the real binary on a terminal: truecolor (banner art with
24-bit half blocks, the tinted glyph prompt, `.archie s`), then NO_COLOR
(plain shade characters, no colour escapes, the classic prompt). Prints `OK`.
"""

from __future__ import annotations

import os
import sys

import pty_driver

BANNER = b"End statements with ;  (.quit to exit)"
HALF_BLOCK = "▀".encode()
SHADE = "▓".encode()
HL_FG = b"\x1b[38;2;76;153;164m"


def session(binary: str, env: dict[str, str], keys: bytes, until: pty_driver.Pred) -> bytes:
    ready = lambda b: b"> " in b or "∩".encode() in b
    with pty_driver.session(binary, env, ready=ready) as (fd, out):
        os.write(fd, keys)
        return out + pty_driver.read_until(fd, until, 10.0)


def main() -> int:
    binary = sys.argv[1]
    base = {"LANG": "en_US.UTF-8", "RELATE_BANNER": "1"}

    colour = session(
        binary,
        {**base, "COLORTERM": "truecolor", "RELATE_PROMPT": "archie"},
        b".archie s\r",
        lambda b: b.count(HALF_BLOCK) > 10,
    )
    for needle, what in [
        (BANNER, "banner text"),
        (HL_FG, "24-bit highlight colour"),
        ("∩".encode(), "glyph prompt"),
        (HALF_BLOCK, "half blocks"),
    ]:
        if needle not in colour:
            pty_driver.fail(f"truecolor session lacks {what}", colour)

    plain = session(
        binary,
        {**base, "NO_COLOR": "1", "RELATE_PROMPT": "archie"},
        b".archie\r",
        lambda b: b.count(SHADE) > 20,
    )
    if BANNER not in plain or SHADE not in plain:
        pty_driver.fail("plain session lacks banner or shades", plain)
    if b"\x1b[38;2;" in plain or "∩".encode() in plain:
        pty_driver.fail("NO_COLOR session coloured or used the glyph", plain)
    if b"cypher> " not in plain:
        pty_driver.fail("NO_COLOR session lost the classic prompt", plain)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

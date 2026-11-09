"""Drive the hidden text eggs (`.fortune`, `.cowsay`, `.matrix`) in a PTY.

One launch of the real binary on a terminal, NO_COLOR so the output is plain
text. argv: binary. `.fortune` is asserted by shape (a non-empty plain line
between the echoed command and the next prompt), so the test need not know
where the shipped fortunes file lives. Prints `OK`.
"""

from __future__ import annotations

import os
import re
import sys

import pty_driver

ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")


def fortune_line(data: bytes) -> bytes | None:
    """The first plain output line that is neither the echoed command nor a prompt."""
    for raw in ANSI.sub(b"", data).splitlines():
        line = raw.strip()
        if line and b".fortune" not in line and not line.endswith(b">"):
            return line
    return None


def main() -> int:
    binary = sys.argv[1]
    env = {"LANG": "en_US.UTF-8", "NO_COLOR": "1", "COLUMNS": "40", "LINES": "12"}
    with pty_driver.session(binary, env) as (fd, _):
        os.write(fd, b".fortune\r")
        pty_driver.expect(
            fd,
            lambda b: fortune_line(b) is not None and pty_driver.prompt_ready(b),
            ".fortune printed no fortune followed by a prompt",
        )

        os.write(fd, b".cowsay hello archie\r")
        pty_driver.expect(
            fd,
            lambda b: "▓".encode() in b and b"< hello archie >" in b,
            ".cowsay bubble missing",
        )

        os.write(fd, b".matrix decrypted\r")
        got = pty_driver.expect(
            fd,
            lambda b: b"decrypted" in b and b"\x1b[2K" in b,
            ".matrix did not decrypt",
            15.0,
        )
        if b"\x1b[38;5;" in got:
            pty_driver.fail(".matrix coloured under NO_COLOR", got)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

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
        got = pty_driver.read_until(
            fd, lambda b: fortune_line(b) is not None and b.rstrip().endswith(b">"), 10.0
        )
        if fortune_line(got) is None:
            pty_driver.fail(".fortune printed no fortune", got)

        os.write(fd, b".cowsay hello archie\r")
        got = pty_driver.read_until(fd, lambda b: "▓".encode() in b, 10.0)
        if b"< hello archie >" not in got:
            pty_driver.fail(".cowsay bubble missing", got)

        os.write(fd, b".matrix decrypted\r")
        got = pty_driver.read_until(fd, lambda b: b"decrypted" in b and b"\x1b[2K" in b, 15.0)
        if b"decrypted" not in got or b"\x1b[38;5;" in got:
            pty_driver.fail(".matrix did not decrypt, or coloured under NO_COLOR", got)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Drive the hidden terminal games (ADR-0547) in a PTY.

For each of `.dash`, `.dash demo`, `.life`, `.snake`, `.snake demo`, `.tetris`: the game enters
the alternate screen and hides the cursor, `q` leaves it and the cursor and
the shell answer a query again. Ctrl-C also leaves cleanly, and a window
that is too small is refused before the screen is touched. Prints `OK`.
"""

from __future__ import annotations

import os
import sys
from collections.abc import Callable

import pty_driver

ENTER = b"\x1b[?1049h"
LEAVE = b"\x1b[?1049l"
HIDE = b"\x1b[?25l"
SHOW = b"\x1b[?25h"


def session(binary: str, size: tuple[int, int], steps: Callable[[int], None]) -> None:
    with pty_driver.session(binary, {"LANG": "en_US.UTF-8"}, size=size) as (fd, _):
        steps(fd)


def play_and_leave(command: bytes, leave_key: bytes) -> Callable[[int], None]:
    def steps(fd: int) -> None:
        os.write(fd, command + b"\r")
        out = pty_driver.read_until(fd, lambda b: HIDE in b and b"\x1b[H" in b, 10.0)
        if ENTER not in out or HIDE not in out:
            pty_driver.fail(f"{command!r} did not take the screen", out)
        os.write(fd, leave_key)
        out = pty_driver.read_until(fd, lambda b: SHOW in b and LEAVE in b, 10.0)
        if LEAVE not in out or SHOW not in out:
            pty_driver.fail(f"{command!r} left the terminal as it was", out)
        os.write(fd, b"RETURN 41 + 1 AS answer;\r")
        out = pty_driver.read_until(fd, lambda b: b"42" in b, 10.0)
        if b"42" not in out:
            pty_driver.fail(f"shell dead after {command!r}", out)

    return steps


def too_small(fd: int) -> None:
    os.write(fd, b".tetris\r")
    out = pty_driver.read_until(fd, lambda b: b"too small" in b, 10.0)
    if b"too small" not in out or ENTER in out:
        pty_driver.fail("small window not refused cleanly", out)


def main() -> int:
    binary = sys.argv[1]
    big = (100, 40)
    for command in [b".dash", b".dash demo", b".life", b".snake", b".snake demo", b".tetris"]:
        session(binary, big, play_and_leave(command, b"q"))
    session(binary, big, play_and_leave(b".tetris", b"\x03"))
    session(binary, (40, 10), too_small)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

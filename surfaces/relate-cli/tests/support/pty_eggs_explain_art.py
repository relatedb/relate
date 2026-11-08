"""Drive `.explain-art` (ADR-0543) in a PTY: a read query draws the flame
(truecolor backgrounds, then NO_COLOR shades), a write is refused and runs
nothing. Prints `OK`."""

from __future__ import annotations

import os
import sys

import pty_driver

HEADER = b"plan flame: width ="


def session(binary: str, env: dict[str, str], lines: list[bytes], until) -> bytes:
    pid, fd = pty_driver.fork_exec([binary], extra_env=env)
    try:
        out = pty_driver.read_until(fd, lambda b: b"> " in b, 15.0)
        for line in lines:
            os.write(fd, line)
        out += pty_driver.read_until(fd, until, 10.0)
        os.write(fd, b".quit\r")
        pty_driver.drain(fd, 2.0)
        if pty_driver.expect_clean_exit(pid, 5.0) != 0:
            raise SystemExit("FAIL: shell did not exit cleanly")
        return out
    finally:
        pty_driver.cleanup(pid, fd)


def main() -> int:
    binary = sys.argv[1]
    base = {"LANG": "en_US.UTF-8", "RELATE_BANNER": "0"}
    query = b".explain-art MATCH (n) RETURN n;\r"

    colour = session(
        binary,
        {**base, "COLORTERM": "truecolor"},
        [query],
        lambda b: b"ProduceResults" in b and b.count(b"\x1b[0m") >= 2,
    )
    if HEADER not in colour or b"\x1b[48;2;" not in colour:
        print(f"FAIL: truecolor flame missing\n{colour!r}", file=sys.stderr)
        return 1

    plain = session(
        binary,
        {**base, "NO_COLOR": "1"},
        [query],
        lambda b: b"ProduceResults" in b,
    )
    if HEADER not in plain or b"\x1b[48;2;" in plain:
        print(f"FAIL: NO_COLOR flame wrong\n{plain!r}", file=sys.stderr)
        return 1

    refused = session(
        binary,
        base,
        [b".explain-art CREATE (:Leak);\r", b"MATCH (n:Leak) RETURN count(n) AS c;\r"],
        lambda b: b"refusing a write" in b and b"0" in b.split(b"refusing a write", 1)[1],
    )
    if HEADER in refused:
        print(f"FAIL: write drew a flame\n{refused!r}", file=sys.stderr)
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

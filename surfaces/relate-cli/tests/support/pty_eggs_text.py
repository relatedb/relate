"""Drive the hidden text eggs (`.fortune`, `.cowsay`, `.matrix`) in a PTY.

One launch of the real binary on a terminal, NO_COLOR so the output is plain
text. argv: binary, then the path of the shipped fortunes file. Prints `OK`.
"""

from __future__ import annotations

import os
import sys

import pty_driver


def main() -> int:
    binary, fortunes_path = sys.argv[1], sys.argv[2]
    fortunes = [
        l.strip()
        for l in open(fortunes_path, encoding="utf-8")
        if l.strip() and not l.startswith("#")
    ]
    env = {"LANG": "en_US.UTF-8", "NO_COLOR": "1", "COLUMNS": "40", "LINES": "12"}
    pid, fd = pty_driver.fork_exec([binary], extra_env=env)
    try:
        out = pty_driver.read_until(fd, lambda b: b"> " in b, 15.0)

        os.write(fd, b".fortune\r")
        got = pty_driver.read_until(fd, lambda b: any(f.encode() in b for f in fortunes), 10.0)
        if not any(f.encode() in got for f in fortunes):
            print(f"FAIL: .fortune printed no fortune\n{got!r}", file=sys.stderr)
            return 1

        os.write(fd, b".cowsay hello archie\r")
        got = pty_driver.read_until(fd, lambda b: "▓".encode() in b, 10.0)
        if b"< hello archie >" not in got:
            print(f"FAIL: .cowsay bubble missing\n{got!r}", file=sys.stderr)
            return 1

        os.write(fd, b".matrix decrypted\r")
        got = pty_driver.read_until(fd, lambda b: b"decrypted" in b and b"\x1b[2K" in b, 15.0)
        if b"decrypted" not in got or b"\x1b[38;5;" in got:
            print(f"FAIL: .matrix did not decrypt, or coloured under NO_COLOR\n{got!r}", file=sys.stderr)
            return 1

        os.write(fd, b".quit\r")
        pty_driver.drain(fd, 2.0)
        if pty_driver.expect_clean_exit(pid, 5.0) != 0:
            print("FAIL: shell did not exit cleanly", file=sys.stderr)
            return 1
    finally:
        pty_driver.cleanup(pid, fd)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

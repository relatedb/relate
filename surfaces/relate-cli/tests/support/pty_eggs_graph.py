"""Drive the hidden `.lolwut` and `.graph` eggs (ADR-0543) in a PTY.

One UTF-8 session: `.lolwut` prints braille art and the version line, then a
small graph is created and `.graph` draws it with labels and the
"showing N of M" footer. Prints `OK`.
"""

from __future__ import annotations

import os
import sys

import pty_driver


def has_braille(data: bytes) -> bool:
    return any(0x2801 <= ord(c) <= 0x28FF for c in data.decode("utf-8", "replace"))


def main() -> int:
    binary = sys.argv[1]
    env = {"LANG": "en_US.UTF-8", "RELATE_BANNER": "0", "NO_COLOR": "1"}
    pid, fd = pty_driver.fork_exec([binary], extra_env=env)
    try:
        out = pty_driver.read_until(fd, lambda b: b"> " in b, 15.0)
        os.write(fd, b".lolwut\r")
        lol = pty_driver.read_until(fd, lambda b: b"Relate ver. " in b, 10.0)
        pty_driver.drain(fd, 0.5)
        if not has_braille(lol):
            print(f"FAIL: .lolwut drew no braille\n{lol!r}", file=sys.stderr)
            return 1
        os.write(fd, b"CREATE (:P {name:'Ada'})-[:KNOWS]->(:P {name:'Grace'});\r")
        pty_driver.read_until(fd, lambda b: b"> " in b, 10.0)
        pty_driver.drain(fd, 0.5)
        os.write(fd, b".graph\r")
        graph = pty_driver.read_until(fd, lambda b: b"relationships" in b, 10.0)
        for needle, what in [
            (b"Ada", "node label"),
            (b"Grace", "node label"),
            (b"showing 2 of 2 nodes, 1 of 1 relationships", "footer"),
        ]:
            if needle not in graph:
                print(f"FAIL: .graph lacks {what}\n{graph!r}", file=sys.stderr)
                return 1
        if not has_braille(graph):
            print(f"FAIL: .graph drew no braille\n{graph!r}", file=sys.stderr)
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

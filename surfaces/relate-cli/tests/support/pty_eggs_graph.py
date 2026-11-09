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
    with pty_driver.session(binary, env) as (fd, _):
        os.write(fd, b".lolwut\r")
        lol = pty_driver.read_until(fd, lambda b: b"Relate ver. " in b, 10.0)
        pty_driver.drain(fd, 0.5)
        if not has_braille(lol):
            pty_driver.fail(".lolwut drew no braille", lol)
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
                pty_driver.fail(f".graph lacks {what}", graph)
        if not has_braille(graph):
            pty_driver.fail(".graph drew no braille", graph)
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())

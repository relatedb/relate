#!/usr/bin/env python3
"""Docker image smoke test for the relate-bolt container (ADR-0128,
unified `relate serve` runtime per ADR-0249): one process serving
Bolt (7687), the HTTP Query API v2 (7474), and MCP (7475) on one store
with one shared credential.

Build the image first, then run this against a docker daemon:

  make docker && python3 surfaces/relate-bolt/tests/docker_smoke.py

Skips with a note (exit 0) when docker or the image is absent, so a local
`cargo test`-adjacent run without the image never fails; CI (the nightly
`docker-smoke` job in .github/workflows/background-cadences.yml) builds
the image first.

Needs Python 3.11+ and the official driver (`pip install neo4j`).
RELATE_DOCKER_IMAGE overrides the image tag (default
`relate-bolt:<workspace version>`, matching `make docker`).
"""

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]


def default_image():
    with open(REPO_ROOT / "Cargo.toml", "rb") as f:
        version = tomllib.load(f)["workspace"]["package"]["version"]
    return f"relate-bolt:{version}"


IMAGE = os.environ.get("RELATE_DOCKER_IMAGE", default_image())


def docker(*args, check=True):
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, check=check
    ).stdout.strip()


def docker_available():
    if shutil.which("docker") is None:
        print("docker smoke: skipped (no docker binary)")
        return False
    if subprocess.run(["docker", "info"], capture_output=True).returncode != 0:
        print("docker smoke: skipped (no docker daemon)")
        return False
    if not docker("images", "-q", IMAGE):
        print(f"docker smoke: skipped (no image {IMAGE}; build it: make docker)")
        return False
    return True


class Container:
    """A detached container that is always removed; ports are published to
    ephemeral host ports (`-p 0:N`) and read back with `docker port`, so
    parallel jobs never collide."""

    def __init__(self, name, *run_args):
        self.name = name
        docker("rm", "-f", name, check=False)
        # DO_NOT_TRACK=1 opts the container's relate children (the
        # exec'd serve, fleet children, seed CLI steps) out of telemetry
        # (ADR-0079 §6): this smoke harness spawns test containers, not
        # product usage, and a container inherits no host env — `docker
        # run -e` is the only channel in.
        self.cid = docker(
            "run", "-d", "--name", name, "-e", "DO_NOT_TRACK=1", *run_args
        )

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.remove()

    def port(self, container_port, timeout=10):
        # `docker port` can race the daemon's mapping registration on a
        # loaded host (observed as a flake: exit 1 right after start).
        deadline = time.monotonic() + timeout
        while True:
            out = docker("port", self.name, f"{container_port}/tcp", check=False)
            if out:
                # one line per stack (0.0.0.0:N / [::]:N) — same N
                return int(out.rsplit(":", 1)[1])
            if time.monotonic() > deadline:
                raise AssertionError(f"{self.name}: no published port for {container_port}")
            time.sleep(0.2)

    def logs(self):
        return subprocess.run(
            ["docker", "logs", self.name],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        ).stdout

    def wait_for_log(self, needle, occurrences=1, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.logs().count(needle) >= occurrences:
                return
            time.sleep(0.2)
        raise AssertionError(f"{self.name}: {needle!r} not in logs:\n{self.logs()}")

    def exit_code(self, timeout=30):
        # `docker wait` blocks until the container exits and prints the exit
        # code from the exit event itself — atomic. The previous inspect
        # polling read ExitCode and Running in two separate inspect calls,
        # which can straddle the exit transition: ExitCode is still its
        # default 0 while the container runs, so a fleet whose children all
        # failed was intermittently misreported as exiting 0.
        try:
            out = subprocess.run(
                ["docker", "wait", self.name],
                capture_output=True,
                text=True,
                check=True,
                timeout=timeout,
            ).stdout.strip()
        except subprocess.TimeoutExpired:
            raise AssertionError(f"{self.name}: did not exit")
        return int(out)

    def remove(self):
        docker("rm", "-f", self.name, check=False)


_VOLUME_SEQ = 0


def volume():
    """A docker named volume for `/data`. Named, not a bind-mounted
    tempdir: the container writes as UID 1000, and on real Linux the host
    user cannot delete that tree afterwards (CI broke with PermissionError
    in TemporaryDirectory cleanup); the daemon removes named volumes as
    root on any platform."""
    global _VOLUME_SEQ
    _VOLUME_SEQ += 1
    name = f"cl-smoke-{os.getpid()}-{_VOLUME_SEQ}"
    docker("volume", "create", name)

    class _Vol:
        def __enter__(self):
            return name

        def __exit__(self, *exc):
            docker("volume", "rm", "-f", name, check=False)

    return _Vol()


def wait_port(port, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                return
        except OSError:
            time.sleep(0.2)
    raise AssertionError(f"port {port} never opened")


def token_from(logs):
    m = re.search(r"generated token for user relate: ([0-9a-f]+)", logs)
    assert m, f"no generated token in logs:\n{logs}"
    return m.group(1)


def count(driver, label):
    records, _, _ = driver.execute_query(f"MATCH (n:{label}) RETURN count(n) AS c")
    return records[0]["c"]


def bolt_driver(c, auth=None):
    """The container's Bolt driver on its published 7687 port. The port is
    read per call — an ephemeral host port moves on `docker restart` —
    and waited on before connecting; `auth` is the driver's credential
    (None = no auth, the driver default)."""
    from neo4j import GraphDatabase

    port = c.port(7687)
    wait_port(port)
    return GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=auth)


def bolt_count(c, token, label):
    """The label's node count over Bolt with the generated-token
    credential."""
    with bolt_driver(c, ("relate", token)) as driver:
        return count(driver, label)


def http_post(port, path, body, token=None):
    """POST JSON to the HTTP or MCP surface; returns (status, body)."""
    import base64
    import urllib.error
    import urllib.request

    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=body.encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if token is not None:
        blob = base64.b64encode(f"relate:{token}".encode()).decode()
        req.add_header("Authorization", f"Basic {blob}")
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return resp.status, resp.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def boot_seed_restart():
    """Env-fallback seed, generated-token auth via `docker logs`, restart
    keeps the database and the token (seed skipped, token reused) — and the
    one token unlocks all three surfaces (Bolt, HTTP, MCP; ADR-0249)."""
    with volume() as data, Container(
        "cl-smoke-boot",
        "-p", "0:7687", "-p", "0:7474", "-p", "0:7475",
        "-v", f"{data}:/data",
        "-e", "RELATE_QUERY=UNWIND range(1,100) AS i CREATE (:Person {id: i})",
        IMAGE,
    ) as c:
        c.wait_for_log("Bolt enabled on")
        token = token_from(c.logs())
        assert bolt_count(c, token, "Person") == 100

        # The same generated token unlocks the HTTP Query API and MCP.
        http_port, mcp_port = c.port(7474), c.port(7475)
        status, body = http_post(
            http_port, "/db/neo4j/query/v2",
            '{"statement": "MATCH (n:Person) RETURN count(n) AS c"}', token,
        )
        assert status == 200 and json.loads(body)["data"]["values"] == [[100]], (status, body)
        status, _ = http_post(http_port, "/db/neo4j/query/v2", '{"statement": "RETURN 1"}')
        assert status == 401, status
        status, body = http_post(
            mcp_port, "/",
            '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}', token,
        )
        assert status == 200 and {"write_cypher", "read_cypher", "get_schema"} <= {
            t["name"] for t in json.loads(body)["result"]["tools"]
        }, (status, body)
        status, _ = http_post(mcp_port, "/", '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}')
        assert status == 401, status

        docker("restart", c.name)
        c.wait_for_log("Bolt enabled on", occurrences=2)
        logs = c.logs()
        assert "exists, skipping seed" in logs, logs
        assert "token reused from /data/graph.auth-token" in logs, logs
        assert bolt_count(c, token, "Person") == 100  # the ephemeral port moved on restart
        print("boot/seed/restart: seeded, one token unlocks bolt+http+mcp, token persisted and reused")


def preset_auth_and_negatives():
    with volume() as data, Container(
        "cl-smoke-auth",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=relate:ci-token",
        IMAGE,
    ) as c:
        c.wait_for_log("Bolt enabled on")
        assert "auth: basic (from RELATE_AUTH)" in c.logs(), c.logs()
        with bolt_driver(c, ("relate", "ci-token")) as driver:
            records, _, _ = driver.execute_query("RETURN 1 AS n")
            assert records[0]["n"] == 1
        # The negative probe asserts on the failure itself — the success
        # sentinel rides the `else` arm, never inside the `except` whose
        # `except Exception` would catch the AssertionError and mask the
        # authored message.
        with bolt_driver(c, ("relate", "wrong")) as driver:
            try:
                driver.execute_query("RETURN 1 AS n")
            except Exception as exc:
                assert "Unauthorized" in str(exc), f"expected Unauthorized, got {exc!r}"
            else:
                raise AssertionError("wrong credentials must not connect")
        print("preset auth: env credential accepted, wrong credentials rejected")


def auth_args_are_redacted_in_logs():
    """`--auth` as container args (a documented path) must not leak the
    credential into `docker logs` via the startup banner — logs travel
    further than `docker inspect`. Also pins the legacy
    `--allow-plaintext` spelling: accepted and dropped with a note
    (ADR-0249), never forwarded to `serve`."""
    with volume() as data, Container(
        "cl-smoke-redact",
        "-v", f"{data}:/data",
        # Custom args replace the CMD; the entrypoint appends the image's
        # --host/--database/port defaults (all interfaces).
        IMAGE, "--auth", "relate:should-not-appear", "--allow-plaintext",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        logs = c.logs()
        assert "should-not-appear" not in logs, logs
        # Rust's {:?} on the argv Vec renders ["--auth", "<redacted>"]
        assert '"<redacted>"' in logs, logs
        assert "auth: basic (from --auth)" in logs, logs
        print("auth args: redacted in docker logs, still authenticated")


def missing_uri_arg_stays_consistent():
    """Args replacing the CMD but omitting --uri/--database: the image's
    default is /data/graph — the entrypoint must pass the seeded store
    explicitly or `serve` would serve a different store than the one
    seeded. Also pins the legacy `--bind`/`--allow-plaintext` aliases
    (ADR-0249)."""
    with volume() as data, Container(
        "cl-smoke-nouri",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=none",
        "-e", "RELATE_QUERY=UNWIND range(1,7) AS i CREATE (:N {id: i})",
        IMAGE, "--bind", "0.0.0.0:7687", "--allow-plaintext",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        assert "uri=/data/graph" in c.logs(), c.logs()
        with bolt_driver(c) as driver:
            assert count(driver, "N") == 7
        print("no --uri in args: server serves the seeded /data/graph")


def image_defaults_resolve_without_cmd_flags():
    """Args replacing the CMD without --host/--database/ports: the
    entrypoint resolves the image defaults (all-interfaces, /data/graph,
    7687/7474/7475) explicitly — serve's own loopback/no-defaults posture
    would be an unreachable server over an unseeded volume. The loud
    ADR-0248 plaintext WARNING is the documented container posture."""
    with volume() as data, Container(
        "cl-smoke-defaults",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_QUERY=UNWIND range(1,3) AS i CREATE (:N {id: i})",
        IMAGE, "--auth", "none",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        logs = c.logs()
        assert "uri=/data/graph" in logs, logs
        assert "Bolt enabled on 0.0.0.0:7687." in logs, logs
        assert "HTTP enabled on 0.0.0.0:7474." in logs, logs
        assert "http://0.0.0.0:7475" in logs, logs
        assert "WARNING" in logs and "PLAINTEXT" in logs, logs
        with bolt_driver(c) as driver:
            assert count(driver, "N") == 3
        print("image defaults: all three surfaces on all interfaces, seeded volume served")


def boolean_flag_order_is_safe():
    """A valueless bool flag before --uri/--auth must not swallow the next
    token: the entrypoint seeds the URI the server serves, and --auth
    stays redacted in logs (the default CMD ordering masked this)."""
    with volume() as data, Container(
        "cl-smoke-boolorder",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_QUERY=UNWIND range(1,5) AS i CREATE (:N {id: i})",
        IMAGE, "--bind", "0.0.0.0:7687", "--allow-plaintext",
        "--uri", "/data/custom.rl", "--auth", "relate:order-secret",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        logs = c.logs()
        assert "uri=/data/custom.rl" in logs, logs
        assert "order-secret" not in logs, logs
        assert '"<redacted>"' in logs, logs
        with bolt_driver(c, ("relate", "order-secret")) as driver:
            assert count(driver, "N") == 5
        print("bool flag order: seeded the served URI, credential redacted")


def file_uri_seeds_once():
    """A documented file:// URI must seed the on-disk database exactly
    once — checking existence on the raw URI string never matches and
    would re-seed on every restart."""
    with volume() as data, Container(
        "cl-smoke-fileuri",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=none",
        "-e", "RELATE_QUERY=UNWIND range(1,4) AS i CREATE (:N {id: i})",
        IMAGE, "--bind", "0.0.0.0:7687", "--allow-plaintext", "--uri", "file:///data/graph.rl",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        with bolt_driver(c) as driver:
            assert count(driver, "N") == 4
        docker("restart", c.name)
        c.wait_for_log("Bolt enabled on", occurrences=2)
        assert "exists, skipping seed" in c.logs(), c.logs()
        print("file:// uri: seeded once, skipped on restart")


def bad_inputs_exit_loudly():
    # cfg stays a host dir: it only carries seed.json into the container
    # (read-only use — the container never writes /config), so the host can
    # clean it up unlike the UID-1000-owned /data tree. It must be
    # world-readable: TemporaryDirectory is 0o700, which UID 1000 cannot
    # traverse on real Linux (the file then looks absent and the container
    # silently env-falls-back instead of failing — Docker Desktop hid it).
    with tempfile.TemporaryDirectory() as cfg, volume() as data:
        os.chmod(cfg, 0o755)
        seed = Path(cfg, "seed.json")
        seed.write_text('{"steps": [{"query": "RETURN 1", "bogus": 1}]}')
        os.chmod(seed, 0o644)
        with Container("cl-smoke-badcfg", "-v", f"{cfg}:/config", "-v", f"{data}:/data", IMAGE) as c:
            assert c.exit_code() != 0
            assert "unknown field `bogus`" in c.logs(), c.logs()
    with volume() as data, Container(
        "cl-smoke-badforce", "-v", f"{data}:/data", "-e", "RELATE_FORCE_SEED=maybe", IMAGE
    ) as c:
        assert c.exit_code() != 0
        assert "invalid RELATE_FORCE_SEED=maybe" in c.logs(), c.logs()
    print("bad inputs: invalid seed.json and FORCE_SEED exit loudly, naming the cause")


def loopback_bind_warns():
    # New spelling: --host.
    with volume() as data, Container(
        "cl-smoke-loopback",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=none",
        IMAGE, "--host", "127.0.0.1",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        assert "reachable only inside the container" in c.logs(), c.logs()
    # The fleet path warns too — here via the legacy --bind alias.
    with volume() as data, Container(
        "cl-smoke-loopfleet",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=none",
        "-e", "INSTANCES=2",
        IMAGE, "--bind", "127.0.0.1:7687",
    ) as c:
        c.wait_for_log("instance 2 listening")
        assert "reachable only inside the container" in c.logs(), c.logs()
    print("loopback host: warned (single + fleet, new and legacy spellings)")


def bolt_identity_passes_through_serve():
    """`--bolt-identity` as a container arg passes through the entrypoint
    to `serve`'s Bolt surface: the HELLO SUCCESS `server` metadata is the
    verbatim value (ADR-0293 via the ADR-0249 arg pass-through) — the
    cypher-shell admission shape."""
    with volume() as data, Container(
        "cl-smoke-identity",
        "-p", "0:7687",
        "-v", f"{data}:/data",
        "-e", "RELATE_AUTH=none",
        IMAGE, "--bolt-identity", "Neo4j/5.26.0",
    ) as c:
        c.wait_for_log("Bolt enabled on")
        with bolt_driver(c) as driver:
            agent = driver.get_server_info().agent
            assert agent == "Neo4j/5.26.0", agent
        print("bolt identity: --bolt-identity verbatim in the container HELLO metadata")


def fleet_stop_and_isolation():
    """INSTANCES=2: both ports serve, data is per-instance, and `docker
    stop` (SIGTERM) is handled — children killed, exit 143, no SIGKILL
    timeout."""
    from neo4j import GraphDatabase

    with volume() as data, Container(
        "cl-smoke-fleet",
        "-p", "0:7687", "-p", "0:7688",
        "-v", f"{data}:/data",
        "-e", "INSTANCES=2",
        "-e", "RELATE_AUTH=relate:fleet",
        "-e", "RELATE_QUERY=UNWIND range(1,10) AS i CREATE (:T {id: i})",
        IMAGE,
    ) as c:
        c.wait_for_log("instance 2 listening")
        # Every surface's port steps per instance; unset http/mcp bases
        # derive from the bolt base so the ranges never overlap (ADR-0249).
        assert "bolt 7688, http 7690, mcp 7692" in c.logs(), c.logs()
        ports = [c.port(7687), c.port(7688)]
        for i, port in enumerate(ports, start=1):
            wait_port(port)
            driver = GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=("relate", "fleet"))
            assert count(driver, "T") == 10
            driver.execute_query("CREATE (:Marker {instance: $i})", i=i)
            driver.close()
        for i, port in enumerate(ports, start=1):
            driver = GraphDatabase.driver(f"bolt://127.0.0.1:{port}", auth=("relate", "fleet"))
            records, _, _ = driver.execute_query("MATCH (m:Marker) RETURN m.instance AS i")
            assert [r["i"] for r in records] == [i], f"instance {i} sees foreign markers"
            driver.close()

        start = time.monotonic()
        docker("stop", c.name)
        stopped_in = time.monotonic() - start
        assert c.exit_code() == 143, f"expected 143 (SIGTERM handled), got {c.exit_code()}"
        assert stopped_in < 5, f"docker stop took {stopped_in:.1f}s — SIGTERM not handled?"
        assert "signal received, stopping the fleet" in c.logs(), c.logs()
        print("fleet: per-instance data isolated, docker stop -> 143 without SIGKILL")


def fleet_exits_when_all_children_die():
    """Every child fails at startup (a flag `serve` rejects): the fleet
    must exit non-zero promptly — a supervisor that only waits on signals
    would hang forever with a dead fleet looking like a running
    container."""
    with volume() as data, Container(
        "cl-smoke-deadfleet",
        "-v", f"{data}:/data",
        "-e", "INSTANCES=2",
        "-e", "RELATE_AUTH=none",
        IMAGE, "--bogus-flag", "bogus",
    ) as c:
        code = c.exit_code(timeout=60)
        assert code not in (0, 130, 143), f"expected a child failure code, got {code}:\n{c.logs()}"
        assert "all instances exited" in c.logs(), c.logs()
        print("dead fleet: exits non-zero instead of hanging")


def main():
    if not docker_available():
        return
    print(f"docker smoke: image {IMAGE}")
    boot_seed_restart()
    preset_auth_and_negatives()
    auth_args_are_redacted_in_logs()
    boolean_flag_order_is_safe()
    missing_uri_arg_stays_consistent()
    image_defaults_resolve_without_cmd_flags()
    file_uri_seeds_once()
    bad_inputs_exit_loudly()
    loopback_bind_warns()
    bolt_identity_passes_through_serve()
    fleet_stop_and_isolation()
    fleet_exits_when_all_children_die()
    print("ok")


if __name__ == "__main__":
    sys.exit(main())

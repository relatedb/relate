#!/usr/bin/env python3
"""Stamp the ADR-0071 loadtest environment class onto a report.

The class is the Docker recipe plus runner label, not hostname or CPU
model. Report `schema_version` stays 6; `environment` is sidecar metadata.

The class model itself (`CLASS`, `CLASS_FIELDS`, `environment_from`,
`environment_mismatches`) and the runner-label policy (`runner_label`)
live in the importable sibling `scripts/loadtest_env.py`, shared with
`scripts/compare-loadtest.py`; this script is a hyphenated CLI, so it
loads the sibling with `importlib.util.spec_from_file_location`
instead of a plain import.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

_ENV = Path(__file__).resolve().parent / "loadtest_env.py"
_spec = importlib.util.spec_from_file_location("loadtest_env", _ENV)
_env = importlib.util.module_from_spec(_spec)
assert _spec.loader is not None
_spec.loader.exec_module(_env)
environment_from = _env.environment_from
runner_label = _env.runner_label


def pinned_reference(image: str, inspected: str) -> str:
    """`name@digest` for `image`, from a docker inspect's RepoDigest/Id.

    The caller's own digest, if it carries one (the env pins do), is
    replaced by the inspected one rather than appended — re-pinning an
    already-pinned reference is a no-op, and `name@a@b` is not a reference.
    """
    digest = inspected.split("@", 1)[1] if "@" in inspected else inspected
    return f"{image.split('@', 1)[0]}@{digest}"


def docker_image_pin(image: str) -> str:
    """Return name@sha256:... (or name@Id when RepoDigests is empty)."""
    fmt = (
        '{{if .RepoDigests}}{{index .RepoDigests 0}}'
        '{{else}}{{.Id}}{{end}}'
    )
    result = subprocess.run(
        ["docker", "inspect", "--format", fmt, image],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"baseline-fingerprint: docker inspect failed for {image!r}: {result.stderr.strip()}"
        )
    pin = result.stdout.strip()
    if not pin:
        raise SystemExit(f"baseline-fingerprint: empty inspect for {image!r}")
    return pinned_reference(image, pin)


def container_uname_m(run_image: str) -> str:
    result = subprocess.run(
        ["docker", "run", "--rm", run_image, "uname", "-m"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"baseline-fingerprint: uname -m failed in {run_image!r}: {result.stderr.strip()}"
        )
    return result.stdout.strip()


def config_int(report: dict[str, Any], key: str) -> int | None:
    raw = (report.get("config") or {}).get(key)
    if raw is None:
        # memory-controls keeps the same knobs on `controls` (warmup is
        # `warmup_ops` there). Other reports keep `config`.
        controls = report.get("controls") or {}
        raw = controls.get("warmup_ops" if key == "warmup" else key)
    if raw is None:
        return None
    if isinstance(raw, list):
        # The bulk harness keeps `config.ops` as the list of op ids it
        # measured, not a count, so there is no scalar ops knob to stamp
        # (the class model already allows an absent one).
        return None
    return int(raw)


def _required_image(given: str | None, key: str, flag: str) -> str:
    """The run's image reference: the flag, then the environment.

    No fabricated default (REL8-2081): a floating tag here re-resolves on
    every pull and drifts the ADR-0071 class on a Hub repush, the failure
    the pins in scripts/baseline-env.sh exist to stop — that home exports
    both names, so every baseline lane arrives with them set. A caller with
    neither is a loud stop, never a guess.
    """
    value = given or os.environ.get(key)
    if not value:
        raise SystemExit(
            f"baseline-fingerprint: {key} is not set — the pins live in "
            f"scripts/baseline-env.sh (run through a baseline lane), or "
            f"pass {flag}"
        )
    return value


def _arg_or_env(arg: Any, env: str, default: Any, cast: Any) -> Any:
    """The flag when given, else the environment variable, else the default."""
    if arg is not None:
        return arg
    return cast(os.environ.get(env, str(default)))


def discover_environment(report: dict[str, Any], args: argparse.Namespace) -> dict[str, Any]:
    build_image = args.build_image
    run_image = args.run_image
    cpus = _arg_or_env(args.cpus, "BASELINE_CPUS", 2, int)
    period = _arg_or_env(args.cpu_period_us, "BASELINE_CPU_PERIOD_US", 10000, int)
    quota = _arg_or_env(args.cpu_quota_us, "BASELINE_CPU_QUOTA_US", period * cpus, int)
    memory = _arg_or_env(args.memory or None, "BASELINE_MEMORY", "2g", str)
    arch = args.arch or container_uname_m(run_image)
    build_pin = args.build_image_pin or docker_image_pin(build_image)
    run_pin = args.run_image_pin or docker_image_pin(run_image)
    return environment_from(
        arch=arch,
        os_name=args.os,
        build_image=build_pin,
        run_image=run_pin,
        cpus=cpus,
        cpu_period_us=period,
        cpu_quota_us=quota,
        memory=memory,
        ops=config_int(report, "ops"),
        warmup=config_int(report, "warmup"),
        workers=config_int(report, "workers"),
        runner=args.runner or runner_label(),
    )


def stamp(path: Path, env: dict[str, Any]) -> None:
    report = json.loads(path.read_text())
    report["environment"] = env
    path.write_text(json.dumps(report, indent=2) + "\n")


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("stamp", help="Write environment onto a loadtest report")
    sp.add_argument("report", type=Path)
    sp.add_argument("--arch")
    sp.add_argument("--os", default="linux")
    sp.add_argument("--build-image")
    sp.add_argument("--run-image")
    sp.add_argument("--build-image-pin")
    sp.add_argument("--run-image-pin")
    sp.add_argument("--cpus", type=int)
    sp.add_argument("--cpu-period-us", type=int)
    sp.add_argument("--cpu-quota-us", type=int)
    sp.add_argument("--memory")
    sp.add_argument("--runner")

    args = p.parse_args()
    if args.cmd == "stamp":
        # An image is demanded only when the stamp actually needs it: the
        # build reference when its pin is absent, the run reference when its
        # pin is absent OR no --arch was given (the arch is read out of the
        # run image). Both pins plus --arch need neither reference.
        if not args.build_image_pin:
            args.build_image = _required_image(
                args.build_image, "BASELINE_BUILD_IMAGE", "--build-image"
            )
        if not args.run_image_pin or not args.arch:
            args.run_image = _required_image(args.run_image, "BASELINE_RUN_IMAGE", "--run-image")
        report = json.loads(args.report.read_text())
        env = discover_environment(report, args)
        stamp(args.report, env)
        print(f"baseline-fingerprint: stamped {args.report}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())

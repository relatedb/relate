#!/usr/bin/env python3
"""The spec-harness regression gate and baseline writer (ADR-0661).

Reads the cypher-test-harness `reports/summary.json` produced from the
PR's own Python-wheel build, and one of two documents beside it:

- `--baseline PATH`: the machine-written baseline the lane's main-merge
  run keeps in the harness repository (`baselines/engine-main.json`).
  The gate answers one question — does this PR fail a scenario that
  main does not already fail? — and exits 1 when it does.
- `--emit-baseline PATH` (with `--engine-sha` and `--harness-sha`): the
  writer side. The same tool owns both directions of the contract so
  the id derivation cannot drift between the writer and the gate: one
  spelling of the fact, read twice.

The contracts live in modules beside this script, each imported once:
`spec_harness_common.py` (the ADR-0482 summary contract and the
fail-closed document readers), `spec_harness_ids.py` (the run-side
scenario-id model ADR-0661 §1 pins), and `spec_harness_baseline.py`
(the baseline document, the comparison, and the emit). This file owns
the CLI: argument contracts, the two mode flows, and the rendering —
counts and categories only, both already public vocabulary in the
coverage comment. Scenario ids and corpus paths never render anywhere
in this repository; the diagnosis of a red gate is a diff inside the
harness repository.

Exit codes: 0 — green, or baseline written; 1 — red, new failing
scenarios; 2 — no verdict possible (a summary or baseline the shape
contract does not recognize, a missing baseline, a revision mismatch):
fail-closed, never a silent pass (ADR-0661 §5).
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path

_SCRIPTS_DIR = str(Path(__file__).resolve().parent)
if _SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, _SCRIPTS_DIR)

from spec_harness_baseline import (  # noqa: E402
    Baseline,
    Verdict,
    emit_baseline,
    gate_verdict,
    previous_delta,
)
from spec_harness_common import (  # noqa: E402
    SHA1_HEX,
    MissingDocument,
    ShapeError,
    append_step_summary,
    category_counts,
    parse_summary,
    read_document,
    short_sha,
)
from spec_harness_ids import (  # noqa: E402
    ScenarioIds,
)


def _sha40(text: str) -> str:
    """The `--engine-sha` / `--harness-sha` argparse type: a full 40-hex
    sha, or the parser's own usage error (exit 2, the one path every bad
    flag takes)."""
    if not SHA1_HEX.fullmatch(text):
        raise argparse.ArgumentTypeError(f"{text!r} is not a 40-hex sha")
    return text


def render_verdict(verdict: Verdict) -> str:
    """The gate's markdown — the table the lane appends to the coverage
    comment. Counts and categories only; the ids stay in the harness
    repository (ADR-0661 §4)."""
    lines = [f"## Spec-harness gate — {verdict.light}", ""]
    if verdict.red:
        lines.append(
            f"- **{verdict.new_count} new failing scenarios vs main** "
            f"({verdict.counts_phrase})"
        )
        if verdict.new_failed:
            breakdown = ", ".join(
                f"{name} ×{count}"
                for name, count in category_counts(
                    verdict.new_failed.values()
                )
            )
            lines.append(
                f"- new assertion failures by category: {breakdown}"
            )
    else:
        lines.append("- no new failing scenarios vs main")
    lines.append(
        f"- baseline: engine `{short_sha(verdict.engine_sha)}` / harness "
        f"`{short_sha(verdict.harness_sha)}` — {verdict.carried} carried "
        "as not passing"
        + (f", {verdict.fixed_phrase}" if verdict.fixed_phrase else "")
    )
    if verdict.red:
        lines.append(
            "- scenario ids never render in this repository (ADR-0661 "
            "§4): diff `baselines/engine-main.json` in the harness "
            "repository against this run's `reports/summary.json`"
        )
    return "\n".join(lines)


def _status_line(verdict: Verdict) -> str:
    """The stderr status line — stdout is the comment-bound table, so
    the verdict travels on stderr like the coverage report's."""
    if not verdict.red:
        return (
            f"spec-harness-gate: {verdict.light} no new failing scenarios "
            + (f"({verdict.fixed_phrase})" if verdict.fixed_phrase else "")
        )
    return (
        f"spec-harness-gate: {verdict.light} {verdict.new_count} new "
        f"failing scenarios ({verdict.counts_phrase})"
    )


def _ids_from(
    raw: object,
) -> tuple[tuple[ScenarioIds, object] | None, list[str]]:
    """The gate's parse of the summary: the full ADR-0482 §3 contract
    (counts, rollups, the @fails exclusion — one shared validation) and
    then the per-scenario `features` walk the gate adds on top. The two
    parsed products return as one pair — both present or none — so the
    callers guard a single None: the writer reads the Summary's resolved
    rollups, the gate reads the ids."""
    summary, diags = parse_summary(raw)
    if summary is None:
        return None, diags
    assert isinstance(raw, dict)  # parse_summary validated the object
    ids, diags = ScenarioIds.from_summary(raw)
    if ids is None:
        return None, diags
    return (ids, summary), []


def _report_diags(diags: list[str]) -> int:
    """The one diagnostic funnel: every parse failure — the
    tuple-returning validators and the raising reads alike — prints
    through here and takes exit 2, the prefix, stream and code spelled
    once for all call sites."""
    for d in diags:
        print(f"spec-harness-gate: {d}", file=sys.stderr)
    return 2


@dataclass(frozen=True)
class _WriterRequest:
    """The validated writer-mode inputs: the non-None invariants the
    argparse checks already proved, as a typed boundary instead of
    Namespace attributes the callee must trust."""
    summary_path: Path
    emit_path: Path
    engine_sha: str
    harness_sha: str
    previous_path: Path | None


@dataclass(frozen=True)
class _GateRequest:
    """The validated gate-mode inputs — the writer request's mirror:
    what the PR flow needs, typed, with the optional pieces optional."""
    summary_path: Path
    baseline_path: Path
    harness_sha: str | None
    require_revision: bool


def _writer_mode(request: _WriterRequest) -> int:
    """The merge-run flow: measure nothing, write what the run measured.
    The previous baseline resolves FIRST — its corruption exits 2
    before anything is written, so a failed run never leaves a new
    baseline or a 'wrote' line behind; its *absence* is the first write
    (the bootstrap line, green, rendered by the delta's own spelling)
    (ADR-0661 §2/§5). This CLI owns both output lines: the document
    module returns them, never prints."""
    parsed, diags = _ids_from(
        read_document(request.summary_path, "summary")
    )
    if parsed is None:
        return _report_diags(diags)
    ids, summary = parsed
    delta: str | None = None
    if request.previous_path is not None:
        previous: Baseline | None = None
        try:
            previous_raw = read_document(
                request.previous_path, "previous baseline"
            )
        except MissingDocument:
            pass  # the bootstrap's first write: the delta's None arm
        else:
            previous, diags = Baseline.from_json(previous_raw)
            if previous is None:
                return _report_diags(diags)
        delta = previous_delta(previous, ids, request.harness_sha)
    print(
        emit_baseline(
            ids, summary,
            engine_sha=request.engine_sha,
            harness_sha=request.harness_sha,
            path=request.emit_path,
        )
    )
    if delta is not None:
        print(delta)
    return 0


def _gate_mode(request: _GateRequest) -> int:
    """The PR flow: read the baseline, assert the like-for-like pin when
    it applies, compare, and render the verdict (exit 1 on red)."""
    baseline_raw = read_document(
        request.baseline_path,
        "baseline",
        missing=(
            f"the baseline is missing: {request.baseline_path} — the "
            "harness repository carries no gate baseline; bootstrap "
            "per ADR-0661 §5 (dispatch the lane to write the first "
            "one) before the ruleset requires the check"
        ),
    )
    parsed, diags = _ids_from(
        read_document(request.summary_path, "summary")
    )
    if parsed is None:
        return _report_diags(diags)
    ids, _summary = parsed
    baseline, diags = Baseline.from_json(baseline_raw)
    if baseline is None:
        return _report_diags(diags)
    if request.require_revision and (
        request.harness_sha != baseline.harness_sha
    ):
        print(
            "spec-harness-gate: the harness checkout is not the "
            "baseline's recorded revision — the like-for-like pin "
            "did not apply (ADR-0661 §3)",
            file=sys.stderr,
        )
        return 2
    verdict = gate_verdict(ids, baseline)
    table = render_verdict(verdict)
    print(table)
    append_step_summary(table)
    print(_status_line(verdict), file=sys.stderr)
    return 1 if verdict.red else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "summary", type=Path, help="path to reports/summary.json"
    )
    # The two modes are one required choice: argparse owns the
    # exactly-one-of invariant, not a hand-rolled cross-check.
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--baseline",
        type=Path,
        default=None,
        metavar="PATH",
        help="gate mode: the baseline to compare against "
        "(harness repository baselines/engine-main.json)",
    )
    mode.add_argument(
        "--emit-baseline",
        type=Path,
        default=None,
        metavar="PATH",
        help="writer mode: write the baseline measured by this run here",
    )
    ap.add_argument(
        "--engine-sha",
        type=_sha40,
        default=None,
        metavar="SHA",
        help="writer mode: the engine commit the run measured "
        "(recorded in the baseline; required with --emit-baseline)",
    )
    ap.add_argument(
        "--harness-sha",
        type=_sha40,
        default=None,
        metavar="SHA",
        help="the harness revision the run executed (recorded in the "
        "emitted baseline; asserted under --require-baseline-revision)",
    )
    ap.add_argument(
        "--previous-baseline",
        type=Path,
        default=None,
        metavar="PATH",
        help="writer mode: the superseded baseline, for the watchdog's "
        "delta line (ADR-0661 §6)",
    )
    ap.add_argument(
        "--require-baseline-revision",
        action="store_true",
        help="gate mode: fail unless --harness-sha is the revision the "
        "baseline records — the pinned-checkout assertion (ADR-0661 §3)",
    )
    args = ap.parse_args()
    # Presence is spelled as identity, not truthiness: a degenerate
    # empty-path argument is a present flag, and the shas are validated
    # 40-hex or absent — `is None` is the question every check below
    # asks. Every mode-exclusive flag is validated by the same
    # mechanism: the group owns baseline-vs-emit, these checks own the
    # flags that belong to one mode only, so nothing is silently
    # accepted and ignored.
    if args.baseline is not None and (
        args.engine_sha is not None or args.previous_baseline is not None
    ):
        ap.error(
            "--baseline is gate mode: --engine-sha and "
            "--previous-baseline belong to --emit-baseline"
        )
    if args.emit_baseline is not None and args.require_baseline_revision:
        ap.error(
            "--require-baseline-revision belongs to gate mode "
            "(--baseline)"
        )
    if args.emit_baseline is not None and (
        args.engine_sha is None or args.harness_sha is None
    ):
        ap.error("--emit-baseline requires --engine-sha and --harness-sha")
    if args.require_baseline_revision and args.harness_sha is None:
        ap.error(
            "--require-baseline-revision needs --baseline and --harness-sha"
        )

    try:
        if args.emit_baseline is not None:
            return _writer_mode(
                _WriterRequest(
                    summary_path=args.summary,
                    emit_path=args.emit_baseline,
                    engine_sha=args.engine_sha,
                    harness_sha=args.harness_sha,
                    previous_path=args.previous_baseline,
                )
            )
        return _gate_mode(
            _GateRequest(
                summary_path=args.summary,
                baseline_path=args.baseline,
                harness_sha=args.harness_sha,
                require_revision=args.require_baseline_revision,
            )
        )
    except ShapeError as err:
        return _report_diags([str(err)])


if __name__ == "__main__":
    raise SystemExit(main())

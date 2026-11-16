#!/usr/bin/env bash
# opencode-review-prep.sh — scripted inputs for the per-kind review
# pipelines (ADR-0603: the rust and security lanes).
#
#   opencode-review-prep.sh model  — fetch the PR endpoints, compute the
#                                    facts and the merge-based diff
#                                    artifact, and set SCRIPTED_NOOP when
#                                    the kind's no-op gate holds (the
#                                    model step never runs on a
#                                    kind-irrelevant diff)
#   opencode-review-prep.sh post   — the same recompute, after the model
#                                    step: the post step re-derives
#                                    facts and the no-op gate from git,
#                                    never from the .stellar documents
#                                    the model had the whole run to
#                                    rewrite
#
# env (both modes): BASE_SHA HEAD_SHA GH_TOKEN REVIEW_KIND
#
# Facts and the diff artifact are the stellar pipeline's facts.py — the
# one spelling of changed-file facts (path, status, docs classification)
# and of the merge-based PR diff — written to .stellar/facts.json and
# .stellar/diff.patch. The endpoint fetch and merge-base resolution are
# lib/review-endpoints.sh's, shared with stellar-prep.sh.
set -euo pipefail

MODE="${1:?usage: opencode-review-prep.sh model|post}"

. "$(dirname "$0")/lib/review-endpoints.sh"

: "${BASE_SHA:?opencode-review-prep.sh: BASE_SHA required}"
: "${HEAD_SHA:?opencode-review-prep.sh: HEAD_SHA required}"
: "${GH_TOKEN:?opencode-review-prep.sh: GH_TOKEN required}"
: "${REVIEW_KIND:?opencode-review-prep.sh: REVIEW_KIND required}"

T=.agents/skills/stellar-review/tools
P=.github/scripts/opencode-review-post.py

# The PR's real diff is merge-base...head, never base-tip..head (a
# behind-main branch would fold main's newer commits in, reversed). The
# model reads the artifact; the fetch is the only authenticated
# operation here and it authenticates per command.
mkdir -p .stellar
AUTH=$(review_auth "$GH_TOKEN")
MB=$(fetch_review_base "$AUTH" "$BASE_SHA" "$HEAD_SHA")
BASE="${MB:-$BASE_SHA}"
echo "review base: $BASE (merge-base of $BASE_SHA and $HEAD_SHA)"

if [ "$MODE" = "model" ]; then
  # The kind's no-op gate is opencode-review-post.py's — the one
  # spelling; the post step recomputes the same decision, so only the
  # env skip that gates the model step lives here. The gate consumes
  # the facts straight from facts.py's stdout (--stdout-json) — the
  # decision never round-trips through the .stellar documents, which
  # become model-writable the moment the model step starts.
  if [ "$(python3 "$T/facts.py" --base "$BASE" --head "$HEAD_SHA" --out .stellar/facts.json --stdout-json \
          | python3 "$P" gate --kind "$REVIEW_KIND" --facts -)" = "true" ]; then
    echo 'SCRIPTED_NOOP=true' >> "${GITHUB_ENV:-/dev/null}"
  fi
elif [ "$MODE" = "post" ]; then
  # Post mode: the same pipe, and the boolean is this script's last
  # stdout line — the run block captures it (the model step has run by
  # now, so nothing here may trust .stellar on disk: a surviving
  # model-spawned process could otherwise rewrite facts.json between
  # the trusted write and a later gate read and flip the lane to a
  # no-op pass). The file write still happens — the poster reads it
  # after the verdict machinery below; only the gate decision rides
  # the pipe.
  python3 "$T/facts.py" --base "$BASE" --head "$HEAD_SHA" --out .stellar/facts.json --stdout-json \
    | python3 "$P" gate --kind "$REVIEW_KIND" --facts -
fi

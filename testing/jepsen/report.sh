#!/usr/bin/env bash
# Render the newest Jepsen run log under runs/ as a markdown report: the
# verdict, the headline counts, and the harness's own results map. The
# Jepsen workflow appends it to the run's step summary, so the result is
# readable on the workflow-run page without downloading an artifact.
#
# Usage: report.sh [RUNS_DIR]   (default: runs/ beside this script)
# Prints markdown on stdout. Exit 0 whenever it rendered a report — the
# verdict is the harness's exit status, never this script's; exit 1 only
# when there is no run log to report on.
set -euo pipefail

RUNS_DIR="${1:-$(dirname "$0")/runs}"
# run.sh names its log <UTC timestamp>Z-<workload>.log; the committed
# evidence logs beside it never match, so a run that died before writing
# a log is reported as "no run log", not as an old result.
LOG="$(ls -t "$RUNS_DIR"/[0-9]*T[0-9]*Z-*.log 2>/dev/null | head -1 || true)"
if [ -z "$LOG" ]; then
  echo "jepsen-report: no run log under $RUNS_DIR" >&2
  exit 1
fi

# The harness prints its results map after "jepsen.core {:stats" and
# closes it with a top-level ":valid? <v>}" line.
RESULTS="$(awk '
  /jepsen\.core \{:stats/ { on = 1; sub(/^.*jepsen\.core /, "") }
  on { print }
  on && /^ :valid\? [^ ]*\}$/ { exit }
' "$LOG")"

# The top-level verdict is the last ` :valid? ` line at column one.
VERDICT="$(printf '%s\n' "$RESULTS" | sed -n 's/^ :valid? \([^ }]*\)}*$/\1/p' | tail -1)"
case "$VERDICT" in
  true)    ICON="PASS" ;;
  false)   ICON="FAIL" ;;
  unknown) ICON="UNKNOWN" ;;
  *)       ICON="NO VERDICT"; VERDICT="not printed (run aborted before analysis)" ;;
esac

# Name of the test from the store path the harness wrote.
TEST="$(sed -n 's#.*jepsen\.store Wrote /src/testing/jepsen/store/\([^/]*\)/.*#\1#p' "$LOG" | tail -1)"

field() { printf '%s\n' "$RESULTS" | sed -n "s/.*:$1 \([0-9][0-9]*\).*/\1/p" | head -1; }

echo "## Jepsen: $ICON"
echo
echo "| | |"
echo "|---|---|"
echo "| Test | \`${TEST:-unknown}\` |"
echo "| Verdict (\`:valid?\`) | \`$VERDICT\` |"
for f in attempted-count acknowledged-count recovered-count kills-fired restarts-recovered; do
  v="$(field "$f")"
  [ -z "$v" ] || echo "| $f | $v |"
done
lost="$(printf '%s\n' "$RESULTS" | sed -n 's/.*:lost-acked-writes {:count \([0-9]*\).*/\1/p' | head -1)"
[ -z "$lost" ] || echo "| lost acknowledged writes | $lost |"
echo "| Run log | \`$(basename "$LOG")\` |"
echo
if [ -n "$RESULTS" ]; then
  echo "<details><summary>Harness results</summary>"
  echo
  echo '```clojure'
  printf '%s\n' "$RESULTS"
  echo '```'
  echo
  echo "</details>"
else
  echo "The log has no results map; tail of the run log:"
  echo
  echo '```'
  tail -n 40 "$LOG"
  echo '```'
fi

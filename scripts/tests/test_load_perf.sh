#!/bin/sh
# The load-perf measurement script's parse-and-report suite. The engine and
# time(1) are stubs — what is under test is everything else: the GNU and BSD
# time extractions (including GNU's h:mm:ss wall form and BSD's bytes-vs-kB
# RSS), the execute_ms pickup from the engine's own timings line, the
# report table, the medians, the per-run state reset, and the failure rules
# (a query that fails, a run that cannot be read back, a host whose time(1)
# is neither GNU nor BSD). A real 1M-round belongs to the lane, not a
# test: the numbers it would produce are host properties, not assertions.
#
# Assertions use the shared TAP harness (scripts/tests/lib/tap.sh); every
# status assertion goes through tap_check, the one spelling.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
SCRIPT=$ROOT/scripts/load-perf.sh

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox load-perf-test

# The engine stub: --version answers the report header, anything else is a
# `query` run that prints exactly one timings line — the shape `--jsonl
# --timings` produces for a write statement.
mkdir "$sandbox/bin"
cat >"$sandbox/bin/relate" <<'STUB'
#!/bin/sh
if [ "$1" = "--version" ]; then
	echo "Relate v9.9.9-stub"
	exit 0
fi
printf '{"timings":{"elapsed_ms":1234.9,"execute_ms":1234.5,"parse_ms":1.0},"planCache":"miss"}\n'
STUB
chmod +x "$sandbox/bin/relate"

# The time(1) stubs speak one format each, so the probe picks the right
# branch per test: the GNU stub implements `-v -o FILE`, the BSD stub
# rejects -v (making the probe fall through) and folds `-l` stats into
# stderr — the real ones' exact shapes:
#   GNU wall "1:07.03" -> 67.03 s; user 6.51; sys 0.33; RSS 3261696 kB
#   BSD wall 6.97; user 6.51; sys 0.33; RSS 3188637696 bytes -> 3113904 kB
cat >"$sandbox/bin/time-gnu" <<'STUB'
#!/bin/sh
# usage: time-gnu -v -o FILE cmd...
[ "$1" = "-v" ] && [ "$2" = "-o" ] || exit 64
out=$3
shift 3
"$@" || exit $?
cat >"$out" <<'EOF'
	Command being timed: "stub"
	Elapsed (wall clock) time (h:mm:ss or m:ss): 1:07.03
	User time (seconds): 6.51
	System time (seconds): 0.33
	Percent of CPU this job got: 100.0
	Maximum resident set size (kbytes): 3261696
EOF
STUB
cat >"$sandbox/bin/time-bsd" <<'STUB'
#!/bin/sh
# usage: time-bsd -l cmd...  (rejects -v, prints stats on stderr)
[ "$1" = "-l" ] || exit 64
shift
"$@" || exit $?
cat >&2 <<'EOF'
        6.97 real         6.51 user         0.33 sys
          3188637696  maximum resident set size
EOF
STUB
# The silent stub passes the GNU probe (the probe runs `true`) but writes
# an empty stats file for every real round — the fabrication guard's
# regression: a missing field must fail the run, never print 0.00.
cat >"$sandbox/bin/time-silent" <<'STUB'
#!/bin/sh
[ "$1" = "-v" ] && [ "$2" = "-o" ] || exit 64
out=$3
shift 3
"$@" || exit $?
if [ "$1" = true ]; then
	printf 'Maximum resident set size (kbytes): 1\n' >"$out"
else
	: >"$out"
fi
STUB
# The no-time stub refuses everything — a host whose time(1) is neither
# format must fail by name before any round runs.
cat >"$sandbox/bin/time-none" <<'STUB'
#!/bin/sh
echo "usage: time-none … (not GNU, not BSD)" >&2
exit 64
STUB
chmod +x "$sandbox/bin/time-gnu" "$sandbox/bin/time-bsd" \
	"$sandbox/bin/time-silent" "$sandbox/bin/time-none"

run_with() { # $1 = time stub, $2.. = script args
	stub=$1
	shift
	LOAD_PERF_RANGE=100 LOAD_PERF_TIME="$sandbox/bin/$stub" \
		sh "$SCRIPT" "$@"
}

# The rounds channel rides the one runner: a subshell exports the value so
# run_with — the only function that builds the script command — carries it.
run_rounds() { # $1 = LOAD_PERF_ROUNDS value, rest = run_with args
	(
		export LOAD_PERF_ROUNDS="$1"
		shift
		run_with "$@"
	)
}

# The rounds knob through its documented channel: the env name, with the
# positional form as the fallback.
run_rounds 2 time-gnu "$sandbox/bin/relate" 9 "$sandbox/out-env-rounds" >/dev/null 2>&1
[ "$(wc -l <"$sandbox/out-env-rounds/report.txt")" -eq 2 ]; \
	tap_check "LOAD_PERF_ROUNDS sets the round count (env over positional)" $?

# A round count that cannot name a positive integer is refused before any
# round runs — it would skip the loop and the medians would divide nothing.
run_rounds 0 time-gnu "$sandbox/bin/relate" 1 "$sandbox/out-rounds0" >/dev/null 2>&1
[ $? -eq 2 ]; tap_check "a zero round count is refused by name" $?
run_rounds 00 time-gnu "$sandbox/bin/relate" 1 "$sandbox/out-rounds00" >/dev/null 2>&1
[ $? -eq 2 ]; tap_check "a zero-padded round count is refused by name" $?
run_rounds x time-gnu "$sandbox/bin/relate" 1 "$sandbox/out-roundsx" >/dev/null 2>&1
[ $? -eq 2 ]; tap_check "a non-numeric round count is refused by name" $?

tap_run_success "the GNU run completes" run_with time-gnu \
	"$sandbox/bin/relate" 3 "$sandbox/out-gnu"
tap_run_success "the BSD run completes" run_with time-bsd \
	"$sandbox/bin/relate" 3 "$sandbox/out-bsd"

report=$sandbox/out-gnu/REPORT.md
[ -s "$report" ]; tap_check "the GNU report exists" $?

# execute_ms is the engine's own number — 1234.5 ms prints as 1.23 s.
grep -q '| 1 | 1.23 s |' "$report"; tap_check "execute comes from the timings line" $?
# GNU's 1:07.03 wall form must land as 67.03, not a raw string.
grep -q '| 67.03s |' "$report"; tap_check "the GNU h:mm:ss wall converts to seconds" $?
grep -q '| 6.51s | 0.33s | 3185 MB |' "$report"; \
	tap_check "GNU user/sys/RSS parse and normalize to MB" $?
grep -q 'round 3 execute_ms=1234.5' "$sandbox/out-gnu/report.txt"; \
	tap_check "report.txt carries three machine-readable rounds" $?
grep -q 'Medians: 1.23 s execute · 67.03s wall' "$report"; \
	tap_check "medians read the collected rounds" $?
grep -q 'Relate v9.9.9-stub' "$report"; \
	tap_check "the report header names the binary's version" $?

# The BSD side: the real line shapes (value-first seconds, bytes RSS) and
# the bytes→kB→MB normalization 3188637696 B -> 3113904 kB -> 3041 MB.
grep -q '| 6.97s | 6.51s | 0.33s | 3041 MB |' "$sandbox/out-bsd/REPORT.md"; \
	tap_check "the BSD extraction and byte-RSS normalization parse" $?

# A second run into the same out-dir resets as one unit: the medians must
# describe this run's three rounds, not six rows across two runs.
run_with time-gnu "$sandbox/bin/relate" 3 "$sandbox/out-gnu" >/dev/null 2>&1
[ "$(wc -l <"$sandbox/out-gnu/report.txt")" -eq 3 ]; \
	tap_check "a second run into the same out-dir resets the rounds" $?

# Failure rule 1: a failed query fails the script.
cat >"$sandbox/bin/relate-fail" <<'STUB'
#!/bin/sh
[ "$1" = "--version" ] && { echo "Relate v9.9.9-stub"; exit 0; }
echo "EngineError: stub refuses" >&2
exit 2
STUB
chmod +x "$sandbox/bin/relate-fail"
tap_run_failure "a failed query fails the script" run_with time-gnu \
	"$sandbox/bin/relate-fail" 1 "$sandbox/out-fail"
# A failed run publishes neither artifact: no half table, no half record.
[ ! -f "$sandbox/out-fail/REPORT.md" ] && [ ! -f "$sandbox/out-fail/report.txt" ]; \
	tap_check "a failed run publishes neither report artifact" $?

# Failure rule 2: a query that answers without a timings line is an
# unreadable run — the report never stands in for a run it cannot read.
cat >"$sandbox/bin/relate-mute" <<'STUB'
#!/bin/sh
[ "$1" = "--version" ] && { echo "Relate v9.9.9-stub"; exit 0; }
printf '[]\n'
STUB
chmod +x "$sandbox/bin/relate-mute"
tap_run_failure "a run without a timings line fails the script" run_with time-gnu \
	"$sandbox/bin/relate-mute" 1 "$sandbox/out-mute"

# Failure rule 3 (the fabrication guard): a time(1) whose per-round output
# never matched must fail as unreadable — a 0.00s row is a measurement the
# host never took.
tap_run_failure "a round with no matching time fields fails the script" run_with time-silent \
	"$sandbox/bin/relate" 1 "$sandbox/out-silent"

# Failure rule 4: a time(1) that is neither GNU nor BSD fails by name
# before the first round, instead of misreporting a 127 as a query failure.
msg=$(run_with time-none "$sandbox/bin/relate" 1 "$sandbox/out-none" 2>&1)
st=$?
[ "$st" -ne 0 ]; tap_check "an unusable time(1) fails the script" $?
printf '%s\n' "$msg" | grep -q 'neither GNU nor BSD time(1)'; \
	tap_check "the failure names the timing tool" $?

tap_done

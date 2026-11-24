#!/bin/sh
# The REL8-211 load ground truth, repeatable on any host: N rounds of the
# 1M-node + 1M-relationship `UNWIND ... CREATE` statement, each on a fresh
# store, recording execute (the engine's own timings line), wall, user, sys
# and peak RSS per round, with a load stamp before and after every run.
#
# Reporting only — the sqlite-gate lane's rule: no number fails this
# script, only a run it cannot read back does (a failed query, a missing
# timings line, a time(1) output whose fields never matched, a load stamp
# that never came back). Extractions print nothing when their label never
# matched, so a missing field reaches the unreadable-run gate as an empty
# string, never as a fabricated 0. Comparisons are the reader's job, on
# the job summary or the PR, because absolute numbers are host-class
# properties.
#
# usage: scripts/load-perf.sh <relate-binary> [rounds] [out-dir]
# env:
#   LOAD_PERF_RANGE   items per round (default 1000000 — the ticket's 1M;
#                     the test suite shrinks it)
#   LOAD_PERF_ROUNDS  best-of-N rounds (default 3; the positional form
#                     below is the fallback, not the other channel)
#   LOAD_PERF_TIME    time(1) to wrap each round (default /usr/bin/time;
#                     GNU -v on CI Linux, BSD -l on a dev Mac, and the
#                     test suite injects stubs through here)
set -u

BIN=${1:?usage: load-perf.sh <relate-binary> [rounds] [out-dir]}
ROUNDS=${LOAD_PERF_ROUNDS:-${2:-3}}
OUT=${3:-target/load-perf}
RANGE=${LOAD_PERF_RANGE:-1000000}
TIME_BIN=${LOAD_PERF_TIME:-/usr/bin/time}

# The one vocabulary of a round's fields: the extractors set these names,
# the unreadable-run gate checks them, the machine-readable record writes
# them, and the medians read them back — a new metric is this list plus
# its extractor, never a fifth edit site.
METRICS="execute_ms wall user sys rss_kb"

# The one refusal shape for every start-up refusal: named message, stderr,
# status — one spelling of the policy.
die() { # $1 = exit status, $2.. = message
	echo "load-perf: $*" >&2
	exit "$1"
}

[ -x "$BIN" ] || die 2 "no executable binary at $BIN"

# A round count that is not a positive integer silently skips the loop and
# the medians would divide an empty record. One test covers every bad
# spelling — empty, non-numeric, 0, zero-padded — because test(1) fails
# closed on a non-integer operand (its own diagnostic is dropped; this
# message replaces it).
if ! [ "$ROUNDS" -gt 0 ] 2>/dev/null; then
	die 2 "rounds must be a positive integer (got '$ROUNDS')"
fi

# The statement is the ticket's ground truth verbatim except the range,
# which the test suite shrinks — the shape (self-loop rel, three props,
# the string concat) is the workload whose cost is per-item create/install.
STMT="UNWIND range(1,$RANGE) as id create (p:Person {name:\"Tomas \"+id, id:id, age:id%100})-[:KNOWS]->(p);"

mkdir -p "$OUT" || exit 1
DB=$OUT/store

# The run's artifact names, spelled once: the reset, the writers, and the
# publish all read these, so no artifact can exist that the reset misses.
REPORT=$OUT/.REPORT.md
REPORT_PUBLISHED=$OUT/REPORT.md
RECORD=$OUT/.report.txt
RECORD_PUBLISHED=$OUT/report.txt

# One run's state resets as one unit: rows or per-round files from a longer
# earlier run in the same out-dir must never ride into this run's medians
# or its CI artifact. The per-round out-/time- globs derive from the one
# literal run_round writes.
rm -f "$REPORT" "$REPORT_PUBLISHED" "$RECORD" "$RECORD_PUBLISHED" \
	"$OUT"/out-*.jsonl "$OUT"/time-*.txt

# Both formats are probed the same way — the candidate flags run against
# `true`, everything they emit lands in one file under $OUT, one grep
# reads the label — so a host's three states (GNU, BSD, no usable time(1))
# are decided on observed output and the third fails by name here, not as
# a 127 at round 1 misreported as a query failure.
"$TIME_BIN" -v -o "$OUT/time-probe" true >/dev/null 2>&1
if grep -q 'Maximum resident set size' "$OUT/time-probe" 2>/dev/null; then
	TIME_FMT=gnu
else
	"$TIME_BIN" -l true >"$OUT/time-probe" 2>&1
	if grep -q 'maximum resident set size' "$OUT/time-probe" 2>/dev/null; then
		TIME_FMT=bsd
	else
		die 3 "$TIME_BIN is neither GNU nor BSD time(1) — set LOAD_PERF_TIME"
	fi
fi
rm -f "$OUT/time-probe"

# h:mm:ss.cc / m:ss.cc / ss.cc -> seconds (GNU's Elapsed form): every
# field folds left-to-right, the last included — 1:07.03 is m:ss, 67.03.
# Empty in, empty out: the unreadable-run gate must see absence, not 0.00.
to_seconds() {
	awk -v t="$1" 'BEGIN {
		if (t == "") { print ""; exit }
		n = split(t, f, ":")
		s = 0
		for (i = 1; i <= n; i++) s = s * 60 + f[i]
		printf "%.2f", s
	}'
}

# One formatter per unit, shared by the row writer and the medians footer:
# a precision or divisor change is one edit, and empty stays empty.
fmt_seconds() { # $1 = milliseconds -> "N.NN s"
	awk -v m="$1" 'BEGIN { if (m == "") { print ""; exit } printf "%.2f s", m/1000 }'
}
fmt_mb() { # $1 = kbytes -> "N MB"
	awk -v k="$1" 'BEGIN { if (k == "") { print ""; exit } printf "%.0f MB", k/1024 }'
}

# Bytes -> kbytes (BSD's RSS is bytes; GNU's is already kbytes), with the
# same empty-in/empty-out contract as every converter above.
to_kbytes() {
	awk -v b="$1" 'BEGIN { if (b == "") { print ""; exit } printf "%.0f", b/1024 }'
}

# The measured argv, spelled once: the GNU and BSD rows must run the same
# command, and the time(1) spelling is the only thing that may differ. The
# case arm owns the whole per-format dispatch — the wrapper spelling AND
# its extractor — so the run/extract pair is checked by structure, not by
# a naming convention a third format could silently break.
run_round() { # uses the loop's round; sets run_status
	set -- "$BIN" query "$STMT" -d "$DB" --jsonl --timings
	case $TIME_FMT in
	gnu)
		"$TIME_BIN" -v -o "$OUT/time-$round.txt" "$@" >"$OUT/out-$round.jsonl"
		run_status=$?
		extract_gnu
		;;
	bsd)
		# BSD time folds its stats into the command's stderr, so they
		# land in time-$round.txt beside the engine's own messages;
		# the engine's stdout (the timings line) stays in out-$round.
		"$TIME_BIN" -l "$@" >"$OUT/out-$round.jsonl" 2>"$OUT/time-$round.txt"
		run_status=$?
		extract_bsd
		;;
	esac
}

# One label→field extraction per format, the label a fixed string: last
# match wins, no match prints nothing. The empty-on-no-match policy lives
# here once, not per field.
gnu_field() { # $1 = label text; prints the last field of its (last) line
	awk -v lbl="$1" 'index($0, lbl) {v=$NF} END{print v}' "$OUT/time-$round.txt"
}
bsd_field() { # $1 = label text; prints the first field of its (last) line
	awk -v lbl="$1" 'index($0, lbl) {v=$1} END{print v}' "$OUT/time-$round.txt"
}

extract_gnu() { # sets wall, user, sys, rss_kb (empty when a label never matched)
	wall=$(to_seconds "$(gnu_field 'Elapsed (wall clock)')")
	user=$(gnu_field 'User time (seconds)')
	sys=$(gnu_field 'System time (seconds)')
	rss_kb=$(gnu_field 'Maximum resident set size')
}

# The BSD summary line is positional ("6.97 real 6.51 user 0.33 sys") —
# selected on its field shape so a command's own stderr cannot counterfeit
# it, then parsed once for the three seconds fields.
extract_bsd() { # sets wall, user, sys, rss_kb (empty when a label never matched)
	cpu=$(awk 'NF==6 && $2=="real" && $4=="user" && $6=="sys"{l=$0} END{print l}' "$OUT/time-$round.txt")
	set -- $(printf '%s\n' "$cpu" | awk 'NF>=6{printf "%.2f %.2f %.2f", $1, $3, $5}')
	wall=${1:-}
	user=${2:-}
	sys=${3:-}
	rss_kb=$(to_kbytes "$(bsd_field 'maximum resident set size')")
}

# The load stamp is part of the record, not decoration: a row whose stamps
# moved during the run is contaminated, whatever its numbers say. Like
# every other extraction, print-on-match only — a host whose uptime never
# spoke the label yields an empty stamp and the gate refuses it by name.
load_stamp() { uptime 2>/dev/null | sed -n 's/.*load averages*: //p'; }

VERSION=$("$BIN" --version 2>/dev/null | head -n 1)
VERSION=${VERSION:-unknown}

# Both artifacts of the run are assembled beside their published names and
# moved into place only once complete — a failed round publishes neither a
# half table nor a half record.
{
	echo "# Load perf — REL8-211 ground truth"
	echo
	echo "binary: \`$BIN\` ($VERSION) · rounds: $ROUNDS · range: $RANGE · time: $TIME_FMT · $(date -u +%Y-%m-%dT%H:%M:%SZ)"
	echo
	echo "| round | execute | wall | user | sys | peak RSS | load before → after |"
	echo "| --- | --- | --- | --- | --- | --- | --- |"
} >"$REPORT"

round=0
while [ "$round" -lt "$ROUNDS" ]; do
	round=$((round + 1))
	rm -rf "$DB"
	PRE=$(load_stamp)
	run_round
	POST=$(load_stamp)

	# A failed query is the other thing this script fails on: the report
	# never stands in for a run that did not happen. Its store stays on
	# disk for diagnosis; the next run's reset removes it.
	if [ "$run_status" -ne 0 ]; then
		echo "load-perf: round $round exited $run_status — see $OUT/out-$round.jsonl" >&2
		exit "$run_status"
	fi

	# execute_ms: the engine's own number, the same one the ticket's local
	# measurements quote, so CI rows and local rows are the same metric.
	execute_ms=$(grep -o '"execute_ms":[0-9.]*' "$OUT/out-$round.jsonl" 2>/dev/null | head -n 1 | cut -d: -f2)

	# One gate over the one vocabulary, load stamps included: any field
	# that never matched — an empty string by construction above — is a
	# row that did not happen.
	missing=
	for m in $METRICS; do
		eval "v=\$$m"
		[ -n "$v" ] || missing="$missing $m"
	done
	[ -n "$PRE" ] || missing="$missing pre_stamp"
	[ -n "$POST" ] || missing="$missing post_stamp"
	if [ -n "$missing" ]; then
		echo "load-perf: round $round unreadable (missing:$missing) — see $OUT" >&2
		exit 1
	fi

	echo "| $round | $(fmt_seconds "$execute_ms") | ${wall}s | ${user}s | ${sys}s | $(fmt_mb "$rss_kb") | $PRE → $POST |" >>"$REPORT"

	# Machine-readable line over the same vocabulary the gate checked:
	# medians below and any future baseline freeze read these, never the
	# markdown table.
	{
		printf 'round %s' "$round"
		for m in $METRICS; do
			eval "v=\$$m"
			printf ' %s=%s' "$m" "$v"
		done
		echo
	} >>"$RECORD"
done
rm -rf "$DB"

# Median per metric (odd N is the middle row; even N the lower middle —
# best-of-three is the intended shape and this stays correct for it).
median_of() { # $1 = metric name as spelled in the record
	grep -o " $1=[0-9.]*" "$RECORD" | cut -d= -f2 | sort -n \
		| awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'
}

{
	echo
	echo "Medians: $(fmt_seconds "$(median_of execute_ms)") execute · $(median_of wall)s wall · $(median_of user)s user · $(fmt_mb "$(median_of rss_kb)") RSS"
	echo
	echo "Reading: rows are comparable run-to-run on one host class only (the load stamps above say whether a row was quiet). Reporting lane — no gate, no baseline; the workload and its rationale are REL8-211."
} >>"$REPORT"

mv "$REPORT" "$REPORT_PUBLISHED"
mv "$RECORD" "$RECORD_PUBLISHED"
cat "$REPORT_PUBLISHED"

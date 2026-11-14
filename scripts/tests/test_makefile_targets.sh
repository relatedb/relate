#!/bin/sh
# The Makefile target inventory is spelled once: the ANNOTATED variable
# in the Makefile derives every `## <name>:` help annotation into a
# name list, .PHONY consumes it, and this guard reads the same ANNOTATED
# and the same .PHONY back out of make's own database (make -p) — the
# name grammar and the phony guarantee are each asserted from one place,
# and every drift direction breaks this gate:
#
# - a target defined without an annotation silently drops out of .PHONY
#   (a same-named file would shadow its task)
# - an annotation naming no target is help-text rot, and its name
#   pollutes .PHONY with a prerequisite nothing provides
# - a .PHONY that is not exactly the annotated inventory (either
#   direction — a reverted literal list included) is parallel state
# - a scripts/tests suite runner that is not a test-scripts prerequisite
#   escapes the one owner the CI lanes invoke
#
# Every derivation is a checked step whose diagnostic is kept: a broken
# make or an empty inventory fails loudly, never compares as silence.
#
# Assertions use the shared TAP harness (scripts/tests/lib/tap.sh).

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
MAKEFILE="$ROOT/Makefile"

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox makefile-targets-test

list_them() { tr '\n' ' ' <"$1" | sed 's/^ *//; s/ *$//'; }

# One read-normalize-sort derivation over make's database: $1 is the sed
# key that picks the line, the shared tail turns it into a sorted name
# list. Every db-sourced inventory below goes through this.
db_names() { # $1 = sed extraction expression
	sed -n "$1" "$sandbox/db" | tr -s ' \n' '\n' | sed '/^$/d' | sort -u
}

# The target-name grammar and the target-line shape, each spelled once
# and shared by the definition grep and the suite-runner awk below: a
# name, then a colon, then whitespace or end of line. The shape itself
# excludes `:=` assignments (spaced or not) and `.`-special targets, so
# no compensating filter is needed.
NAME_GRAMMAR='[a-zA-Z0-9_][a-zA-Z0-9_-]*'
TARGET_LINE="^${NAME_GRAMMAR}:([[:space:]]|$)"

# One policy per helper: must_be_non_empty is the extraction guard (an
# inventory that came back empty is a failure in its own right, never a
# silent empty set); must_be_empty is the difference guard (offenders
# ride the not-ok line).
must_be_non_empty() { # $1 = inventory file, $2 = description
	if [ -s "$1" ]; then
		tap_ok "$2"
	else
		tap_not_ok "$2 (empty: extraction failed?)"
	fi
}

must_be_empty() { # $1 = difference file, $2 = description
	if [ -s "$1" ]; then
		tap_not_ok "$2 (offenders: $(list_them "$1"))"
	else
		tap_ok "$2"
	fi
}

# One gated comparison: a difference only means something when BOTH of
# its inventories extracted — an empty side compares as an empty
# difference, a vacuous pass printed beside the extraction failure above.
# An unextracted side skips the comparison instead (the extraction
# already failed the run), so the ok line never stands in for a
# comparison that never happened. $1 = comm mode, $2/$3 = inventories,
# $4 = description; the difference lands in $sandbox/diff.
gated_diff() {
	if [ -s "$2" ] && [ -s "$3" ]; then
		comm "$1" "$2" "$3" | tr -d '\t' | sed '/^$/d' >"$sandbox/diff"
		must_be_empty "$sandbox/diff" "$4"
	else
		tap_skip "$4" "an inventory is empty (extraction failed above)"
	fi
}

# --- the three inventories, each a checked step -----------------------------

# Target definitions: literal target lines only — the shape carries the
# invariant. A missing Makefile or broken grep rides its own stderr
# into the log and leaves an empty inventory for the non-empty guard
# below.
grep -E "$TARGET_LINE" "$MAKEFILE" \
	| cut -d: -f1 | sort -u >"$sandbox/targets"

# make's own database: the one place ANNOTATED and .PHONY are both
# spelled. A parse failure is a named failure with its first stderr
# line on the not-ok; the database file still exists (possibly empty),
# so the inventory guards below fail loudly too.
if make -C "$ROOT" --no-print-directory -p -n >"$sandbox/db" 2>"$sandbox/make-err"; then
	tap_ok "make printed its database"
else
	tap_not_ok "make printed its database ($(head -n 1 "$sandbox/make-err" 2>/dev/null))"
fi

# The annotated inventory, read back from the database's ANNOTATED
# variable — the same derivation .PHONY consumes, never re-derived here.
db_names 's/^ANNOTATED :*= //p' >"$sandbox/annotated"

# What make itself believes is phony.
db_names 's/^\.PHONY: //p' >"$sandbox/phony"

# The one owner's prerequisite list: the test-scripts aggregate's
# members, straight from make's own rule database.
db_names 's/^test-scripts: //p' >"$sandbox/owned"

# --- the guarantees ----------------------------------------------------------

must_be_non_empty "$sandbox/targets" "the target inventory is non-empty"
must_be_non_empty "$sandbox/annotated" "the annotated inventory is non-empty"
must_be_non_empty "$sandbox/phony" "the phony inventory is non-empty"

gated_diff -23 "$sandbox/targets" "$sandbox/annotated" \
	"every target carries a ## annotation"

gated_diff -13 "$sandbox/targets" "$sandbox/annotated" \
	"no annotation names a missing target"

# The symmetric difference: names in the annotation inventory but not
# phony, and phony but not annotated, in one file (comm -3 marks the
# second column with a tab).
gated_diff -3 "$sandbox/annotated" "$sandbox/phony" \
	".PHONY is exactly the annotated inventory (both directions)"

# --- every suite runner hangs off the one owner ------------------------------

# Targets whose recipes run a scripts/tests suite (a tab-indented recipe
# line naming scripts/tests/). Continuation lines of prerequisite lists
# are tab-indented too but never name scripts/tests/, so they cannot
# masquerade as suite runners.
#
# The recipe context is the last rule-start seen, and an unindented line
# that is not one (an assignment, a conditional, a comment) ends it — so
# a tab-indented scripts/tests/ line under an unrecognized construct is
# never credited to an unrelated target. Such a line is reported instead
# of dropped: an unattributed suite runner must not vanish from the
# inventory below and pass the ownership comparison by absence.
#
# The scan is a checked step of its own, like the make -p step above: a
# pipeline would report only sort's status, so a failed awk (a syntax
# error, an unreadable Makefile) would read as "no offenders" — its
# status is asserted here and its stderr stays stderr, never the
# offenders file.
if awk -v line="$TARGET_LINE" -v offenders="$sandbox/unattributed" '
	/^\t/ {
		if (/scripts\/tests\//) {
			if (name == "") { print $0 >offenders; next }
			print name
		}
		next
	}
	$0 ~ line { name = $1; sub(/:.*$/, "", name); next }
	/^[[:space:]]*$/ { next }
	{ name = "" }
' "$MAKEFILE" >"$sandbox/suite-runners-raw" 2>"$sandbox/scan-err"; then
	tap_ok "the suite-runner scan ran"
else
	tap_not_ok "the suite-runner scan ran ($(head -n 1 "$sandbox/scan-err" 2>/dev/null))"
fi
sort -u "$sandbox/suite-runners-raw" >"$sandbox/suite-runners"

# The same difference guard as every comparison above: the unattributed
# lines are the offenders, and an empty file is the pass.
must_be_empty "$sandbox/unattributed" \
	"every scripts/tests recipe line sits under a named target"

must_be_non_empty "$sandbox/suite-runners" "the suite-runner inventory is non-empty"
must_be_non_empty "$sandbox/owned" "the test-scripts member inventory is non-empty"

gated_diff -23 "$sandbox/suite-runners" "$sandbox/owned" \
	"every scripts/tests suite runner is a test-scripts prerequisite"

tap_done

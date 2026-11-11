# Behavioral tests for testing/jepsen/report.sh: the markdown report the
# Jepsen workflow appends to its step summary.
#
# Local run:  bats .github/scripts/tests/jepsen-report.bats

SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../../.." && pwd)/testing/jepsen/report.sh"
FIX="$(cd "$(dirname "$BATS_TEST_FILENAME")" && pwd)/fixtures/jepsen-report"

@test "a valid run reports PASS with the headline counts" {
  run "$SCRIPT" "$FIX/green"
  [ "$status" -eq 0 ]
  [[ "$output" == *"## Jepsen: PASS"* ]]
  [[ "$output" == *'`relate-set-kill9`'* ]]
  [[ "$output" == *"| acknowledged-count | 204 |"* ]]
  [[ "$output" == *"| lost acknowledged writes | 0 |"* ]]
}

@test "an invalid run reports FAIL and the lost-write count" {
  run "$SCRIPT" "$FIX/red"
  [ "$status" -eq 0 ]
  [[ "$output" == *"## Jepsen: FAIL"* ]]
  [[ "$output" == *"| lost acknowledged writes | 3 |"* ]]
}

@test "only timestamp-named run logs are reported, never committed evidence" {
  run "$SCRIPT" "$FIX/green"
  [[ "$output" == *'`20260101T000000Z-set.log`'* ]]
  [[ "$output" != *"second-consecutive-green"* ]]
}

@test "no run log exits 1 so the workflow warns instead of reporting stale evidence" {
  run "$SCRIPT" "$FIX/none"
  [ "$status" -eq 1 ]
  [[ "$output" == *"no run log"* ]]
}

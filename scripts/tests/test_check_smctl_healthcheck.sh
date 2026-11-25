#!/bin/sh
# Hermetic suite for the DigiCert credentials preflight gate
# (scripts/check-smctl-healthcheck.sh): the 2FA pass, the
# connected-but-1FA refusal (the v1.3.0 release's failure mode), the
# not-connected refusal, an empty report, the credentials-redaction
# contract, and the argument contract. Fixtures are canned
# `smctl healthcheck` reports in the shape of DigiCert's documented
# output, so the suite needs no smctl and runs on any host.
# Output and exit contract: scripts/tests/lib/tap.sh.

set -u

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
GATE="$ROOT/scripts/check-smctl-healthcheck.sh"

. "$ROOT/scripts/tests/lib/tap.sh"

tap_sandbox check-smctl-healthcheck

run_gate() { # rest = gate arguments — the canonical capture (tap_run_capture) parameterized over this suite's gate
	tap_run_capture "$sandbox/out" "$sandbox/err" -- bash "$GATE" "$@"
}

# --- pass: the report shows both factors -------------------------------

two_fa="$sandbox/two-fa"
cat >"$two_fa" <<'REPORT'
--------- User credentials ---------
Status: Connected

Username: ci-relate
Accounts: RelatedB
Authentication: 2FA
Environment: Prod
Credentials:
        Host: clientauth.one.digicert.com
        API key: 012345fe67a1234f56a7d8c911_055xxxxxxxxxxxxxxxxxxxxxxxxxx
        Client certificate file path: D:\a\_temp\relate-windows-signing\client-auth.p12
        Client certificate password: 1+cJxxxxxxmt
REPORT
run_gate "$two_fa"
tap_check "the gate passes a 2FA report" "$tap_status"
assert_same "the pass line names the accepted factors" \
	"$(cat "$sandbox/out")" \
	"check-smctl-healthcheck: DigiCert authentication is 2FA (client certificate accepted)"

# --- connected, but the client certificate was not accepted ------------

one_fa="$sandbox/one-fa"
cat >"$one_fa" <<'REPORT'
--------- User credentials ------
Status: Connected

Username: ci-relate
Accounts: RelatedB
Authentication: 1FA
Environment: Prod
Credentials:
        Host: clientauth.one.digicert.com
        API key: 012345fe67a1234f56a7d8c911_055xxxxxxxxxxxxxxxxxxxxxxxxxx
        Client certificate file path: D:\a\_temp\relate-windows-signing\client-auth.p12
        Client certificate password: hunter2EXAMPLE
Your client certificate path or password is incorrect. You will not be able to complete specific actions (such as sign, generate keypairs and approve releases) until these credentials are corrected.
REPORT
run_gate "$one_fa"
tap_check_refusal "the gate fails a connected 1FA report" "$tap_status"
assert_same "the refusal names the certificate secrets, the report riding stderr with the credentials redacted" \
	"$(cat "$sandbox/err")" \
	"--------- User credentials ------
Status: Connected

Username: ci-relate
Accounts: RelatedB
Authentication: 1FA
Environment: Prod
Credentials:
        Host: clientauth.one.digicert.com
        API key: [redacted by check-smctl-healthcheck]
        Client certificate file path: D:\a\_temp\relate-windows-signing\client-auth.p12
        Client certificate password: [redacted by check-smctl-healthcheck]
Your client certificate path or password is incorrect. You will not be able to complete specific actions (such as sign, generate keypairs and approve releases) until these credentials are corrected.
check-smctl-healthcheck: $one_fa: smctl connected 1FA only — the client authentication certificate was not accepted (path or password wrong, or the certificate is not the one paired with the API key's user). Fix WINDOWS_SIGN_CLIENT_CERT_FILE_B64 / WINDOWS_SIGN_CLIENT_CERT_PASSWORD; DigiCert's healthcheck errors page has the walkthrough."

# --- not connected (the docs' spelling) ---------------------------------

offline="$sandbox/offline"
cat >"$offline" <<'REPORT'
--------- User credentials ------
Status: Not connected
REPORT
run_gate "$offline"
tap_check_refusal "the gate fails a not-connected report" "$tap_status"
assert_same "the refusal names the host and API key" \
	"$(cat "$sandbox/err")" \
	"$(cat "$offline")
check-smctl-healthcheck: $offline: smctl did not connect — check WINDOWS_SIGN_SM_HOST and WINDOWS_SIGN_API_KEY (DigiCert's healthcheck errors page lists the hosts per region)."

# --- connection failed (what smctl 1.68 prints for a rejected API key) ---

wrong_token="$sandbox/wrong-token"
cat >"$wrong_token" <<'REPORT'
--------- Network ---------
Host: https://clientauth.one.digicert.com
Status: Reachable

--------- User credentials ---------
Status: Connection failed

Credentials:
	Host: https://clientauth.one.digicert.com
	API key: 012345fe67a1234f56a7d8c911_055xxxxxxxxxxxxxxxxxxxxxxxxxx
2 errors occurred:
	* Error (401): {
    "error" : {
      "status" : "wrong_token",
      "message" : "Invalid JWT/S token."
    }
  }
REPORT
run_gate "$wrong_token"
tap_check_refusal "the gate fails a connection-failed report" "$tap_status"
assert_same "the refusal names the host and API key, the API-key line redacted" \
	"$(cat "$sandbox/err")" \
	"$(cat "$wrong_token" | sed 's/^	API key:.*/	API key: [redacted by check-smctl-healthcheck]/')
check-smctl-healthcheck: $wrong_token: smctl did not connect — check WINDOWS_SIGN_SM_HOST and WINDOWS_SIGN_API_KEY (DigiCert's healthcheck errors page lists the hosts per region)."

# --- no loadable credentials (the real v1.3.0-credentials report) -------

no_creds="$sandbox/no-creds"
cat >"$no_creds" <<'REPORT'
--------- Network ---------
Host: https://clientauth.one.digicert.com
Status: Reachable

--------- User credentials ---------
Status: No credentials found.

--------- Signing tools ---------
Apksigner:
	Mapped: No
REPORT
run_gate "$no_creds"
tap_check_refusal "the gate fails a no-credentials report" "$tap_status"
assert_same "the refusal names the certificate-load remedy" \
	"$(cat "$sandbox/err")" \
	"$(cat "$no_creds")
check-smctl-healthcheck: $no_creds: smctl found no loadable credentials — the client authentication certificate did not load (wrong password, or the base64 secret does not decode to the .p12) or the SM_* environment is incomplete. Fix WINDOWS_SIGN_CLIENT_CERT_FILE_B64 / WINDOWS_SIGN_CLIENT_CERT_PASSWORD."

# --- unrecognised report (empty, crashed, or a format change) -----------

unrecognised_empty="$sandbox/unrecognised-empty"
: >"$unrecognised_empty"
run_gate "$unrecognised_empty"
tap_check_refusal "the gate fails an empty report as unrecognised" "$tap_status"
assert_same "the empty report is refused without a guessed remedy" \
	"$(cat "$sandbox/err")" \
	"check-smctl-healthcheck: $unrecognised_empty: no Status or Authentication line — this classifier does not recognise the report (an smctl output-format change, or a crashed run); inspect the report above and update the classifier's markers."

unrecognised_crash="$sandbox/unrecognised-crash"
cat >"$unrecognised_crash" <<'REPORT'
panic: runtime error: index out of range [7] with length 0

goroutine 1 [running]:
main.main()
	/build/smctl/main.go:42 +0x1f
REPORT
run_gate "$unrecognised_crash"
tap_check_refusal "the gate fails a crash report as unrecognised" "$tap_status"
assert_same "the crash report is refused without a guessed remedy" \
	"$(cat "$sandbox/err")" \
	"$(cat "$unrecognised_crash")
check-smctl-healthcheck: $unrecognised_crash: no Status or Authentication line — this classifier does not recognise the report (an smctl output-format change, or a crashed run); inspect the report above and update the classifier's markers."

# --- usage --------------------------------------------------------------

run_gate
assert_same "the gate refuses to run without a report (exit 2)" "$tap_status" 2
assert_same "the usage line names the argument" \
	"$(cat "$sandbox/err")" "usage: $GATE REPORT"

run_gate "$sandbox/no-such-report"
assert_same "the gate refuses a missing report file (exit 2)" "$tap_status" 2

run_gate "$two_fa" extra
assert_same "the gate refuses a second argument (exit 2)" "$tap_status" 2

tap_done

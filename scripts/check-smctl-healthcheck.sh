#!/usr/bin/env bash
# Gate DigiCert Software Trust Manager credentials before a release signs.
#
# `smctl healthcheck` authenticates the DigiCert ONE API user from the
# SM_* environment exactly the way `smctl sign` does, then reports which
# authentication factors it accepted. Signing needs both — the API key
# and the client authentication certificate — and DigiCert's API answers
# a 1FA-authenticated sign call with a bare 403 ("User is not
# multi-factor authenticated. Missing Client Authentication
# Certificate.") that names neither the failing credential nor the
# remedy. This classifier turns the healthcheck report into that
# diagnosis before the sign step runs.
#
# Every documented report state is its own verdict: `Authentication: 2FA`
# passes; a connected-but-1FA report fails with the client-certificate
# remedy; a not-connected report (either spelling the binary uses — the
# docs' "Not connected", or smctl 1.68's "Connection failed" with 401
# wrong_token errors for a rejected API key) fails with the host/API-key
# remedy; a "Status: No credentials found" report fails with the
# cert-load remedy (the certificate would not load with the configured
# password, or the SM_* environment is incomplete); anything else — an
# empty report, a crashed run, or a future change to smctl's output
# format — fails as unrecognised rather than guessing a remedy. The
# verdict is derived once and every refusal shares one report echo and
# one exit: the arms differ only by their message. The echoed report
# keeps the diagnosis (status, factors accepted, host, the
# client-certificate path smctl resolved) with each credential label's
# value redacted — the label prefix and its indentation stay, smctl
# masks those values itself, and the redaction keeps a future unmasking
# from leaking them into a run log; an empty report prints nothing.
#
# Usage: check-smctl-healthcheck.sh REPORT
#   REPORT — a file capturing `smctl healthcheck` stdout+stderr
set -euo pipefail

finish() { # $1 = exit status, $2 = message — a pass prints to stdout, a refusal to stderr
  if [ "$1" -eq 0 ]; then
    printf '%s\n' "$2"
  else
    printf '%s\n' "$2" >&2
  fi
  exit "$1"
}

if [[ $# -ne 1 ]]; then
  finish 2 "usage: $0 REPORT"
fi
report=$1

if [[ ! -f "$report" ]]; then
  finish 2 "check-smctl-healthcheck: $report: no such file"
fi

if grep -qF 'Authentication: 2FA' "$report"; then
  verdict=two-fa
elif grep -qF 'Status: Connected' "$report"; then
  verdict=one-fa
elif grep -qE 'Status: (Not connected|Connection failed)' "$report"; then
  # Both spellings the binary uses for an unauthenticated connection:
  # the docs say "Not connected", smctl 1.68 prints "Connection failed"
  # (with 401 wrong_token errors) for a rejected API key.
  verdict=offline
elif grep -qF 'Status: No credentials found' "$report"; then
  verdict=no-creds
else
  verdict=unrecognised
fi

case "$verdict" in
  two-fa)
    sm_status=0
    sm_message="check-smctl-healthcheck: DigiCert authentication is 2FA (client certificate accepted)"
    ;;
  one-fa)
    sm_status=1
    sm_message="check-smctl-healthcheck: $report: smctl connected 1FA only — the client authentication certificate was not accepted (path or password wrong, or the certificate is not the one paired with the API key's user). Fix WINDOWS_SIGN_CLIENT_CERT_FILE_B64 / WINDOWS_SIGN_CLIENT_CERT_PASSWORD; DigiCert's healthcheck errors page has the walkthrough."
    ;;
  offline)
    sm_status=1
    sm_message="check-smctl-healthcheck: $report: smctl did not connect — check WINDOWS_SIGN_SM_HOST and WINDOWS_SIGN_API_KEY (DigiCert's healthcheck errors page lists the hosts per region)."
    ;;
  no-creds)
    sm_status=1
    sm_message="check-smctl-healthcheck: $report: smctl found no loadable credentials — the client authentication certificate did not load (wrong password, or the base64 secret does not decode to the .p12) or the SM_* environment is incomplete. Fix WINDOWS_SIGN_CLIENT_CERT_FILE_B64 / WINDOWS_SIGN_CLIENT_CERT_PASSWORD."
    ;;
  unrecognised)
    sm_status=1
    sm_message="check-smctl-healthcheck: $report: no Status or Authentication line — this classifier does not recognise the report (an smctl output-format change, or a crashed run); inspect the report above and update the classifier's markers."
    ;;
esac

if [[ "$sm_status" -ne 0 ]]; then
  # Redact the value, keep the line: the label prefix and its indentation
  # stay, so a diagnostic line that mentions a credential label keeps its
  # context while the value never reaches the log.
  sed -E 's/^([[:space:]]*(API key|Client certificate password):).*/\1 [redacted by check-smctl-healthcheck]/' \
    "$report" >&2
fi

finish "$sm_status" "$sm_message"

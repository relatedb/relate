### Notes

- Release tooling: the Windows signing leg preflights its DigiCert
  credentials with `smctl healthcheck` before signing and classifies the
  report (`scripts/check-smctl-healthcheck.sh`), so a failed client
  certificate surfaces as the secret to fix instead of DigiCert's bare
  sign-time 403; the echoed report redacts the API-key and
  client-certificate-password lines.

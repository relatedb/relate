### Notes

- Release tooling fix: the Chocolatey install script computes the
  binary's SHA-256 through the .NET crypto classes instead of
  `Get-FileHash` — Chocolatey's PowerShell host does not reliably load
  the Utility module (an explicit import did not survive it either),
  and the v1.3.0 install died at the SHA256SUMS gate on it.

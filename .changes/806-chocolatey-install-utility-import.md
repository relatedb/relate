### Notes

- Release tooling fix: the Chocolatey install script imports
  `Microsoft.PowerShell.Utility` explicitly — Chocolatey's PowerShell
  host does not auto-load it on every host version, and the installer's
  SHA256SUMS check died with "Get-FileHash is not recognized" on the
  v1.3.0 publish.

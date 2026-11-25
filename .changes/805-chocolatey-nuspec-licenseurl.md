### Notes

- Release tooling fix: the Chocolatey package's nuspec declares the
  license through `licenseUrl` (the SPDX URL) instead of the
  `<license>` element, which `choco pack` refuses (CHCU0002) — the
  v1.3.0 Chocolatey publish failed on it.

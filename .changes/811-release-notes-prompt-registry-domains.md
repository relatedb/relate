### Notes

- Release tooling fix: the release-notes prompt separates package-manager
  command words from registry domain spellings — the output guard audits
  every bare domain on a line carrying `npm`, `pip`, `cargo`, `brew` or
  kin, and the registries' own domains are not allowlisted, so prose
  pairing them refused as an unaudited destination; registries get their
  domain-free names instead.

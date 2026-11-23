### Added

- The CLI installs and upgrades through six package managers: Homebrew
  (`brew install relatedb/tap/relate`), APT (`apt-get install relate`,
  amd64 and arm64 from the signed repository at relatedb.ai/apt), RPM
  (`dnf install relate` from the signed repository at relatedb.ai/rpm),
  winget, Chocolatey and Scoop — every channel's package built from the
  release's own signed, checksummed artifacts. On a package-managed
  install, `relate upgrade` names the package manager's own upgrade
  command and exits non-zero instead of replacing the binary, and the
  curl-pipe installers refuse to overwrite a package-managed install.
  The release gate now derives the Linux binary's glibc floor from its
  ELF symbol versions (refusing anything above the documented 2.35 cap)
  and pins the x86-64-baseline ISA note, so a .deb/.rpm install always
  starts on the declared-supported distributions.

# ADR-0586: Windows installs through a PowerShell installer

Status: Proposed
Claim: f5a09629ddce507f33267ba12357939863928aec

## Context

ADR-0068 §3 limited the one-line installer to macOS and Linux and left
Windows with a manual `.exe` download. That path verifies neither the SSHSIG
manifest signature (ADR-0115) nor the signed version header (ADR-0457). A
browser download raises SmartScreen for the unsigned binary, and the user
puts it on `PATH` by hand. `install.sh` died with `unsupported OS` under Git
Bash, MSYS2 and Cygwin, where coding agents on Windows run shell commands.
`relate upgrade` refused Windows before its `--check` branch, and its
refusal linked a README anchor that does not exist. Windows ships what the
integrity chain needs: Windows PowerShell 5.1, and `ssh-keygen` as part of
the inbox OpenSSH client (`-Y verify` needs OpenSSH 8.1 or later).

## Decision

1. **`install.ps1`.** `distribution/install.ps1` is the Windows installer:
   `irm https://relatedb.ai/install.ps1 | iex`, or the
   `powershell -ExecutionPolicy ByPass -c "…"` form from `cmd` and Git Bash.
   It runs in Windows PowerShell 5.1 and PowerShell 7 and ships as a release
   asset beside `install.sh`, reaching the website the same way (ADR-0546).
   Run from a pipe, each installer says once that it ran before it could be
   read, and points at the README's inspect-then-run steps.
2. **The `install.sh` contract, unchanged.** The release resolves through
   `releases.json` (ADR-0571). Assets come over the direct route with no API
   request and no token, through the same override seams (ADR-0585). Both
   seams take the `https://` gate before any request, in both installers:
   `releases.json` only selects a release, but a plaintext fetch would let
   an on-path attacker steer which signed release installs. The manifest is verified against the pinned key with the
   inbox `ssh-keygen` before the binary downloads (ADR-0115), its version
   header is bound to the tag under the same legacy ceiling (ADR-0457), and
   the binary's SHA-256 is checked last. Every failure installs nothing; a
   missing `ssh-keygen` fails closed naming the `Add-WindowsCapability`
   command. The manifest reaches `ssh-keygen` as its exact bytes, never
   through a PowerShell text pipeline (5.1 re-encodes and rewrites line
   endings).
3. **Install location.** `%LOCALAPPDATA%\Programs\relate\relate.exe`, no
   elevation. Each installer validates the variable its install directory
   comes from (`LOCALAPPDATA` here, `HOME` for `install.sh`): unset, empty or
   relative values are refused before any request. The directory is appended to the user `PATH` (keeping the
   registry value's expandable form) and the change is broadcast to new
   shells. The binary is staged beside its final path and moved in. A running
   `.exe` can be renamed but not overwritten, so an existing one is retired
   to a unique name first, and restored if the move fails.
4. **Architecture.** amd64 only, the one Windows asset; Windows 11 on ARM64
   runs it under x64 emulation, and earlier ARM64 Windows is refused.
5. **`install.sh` under Windows shells.** `MINGW*`, `MSYS*` and `CYGWIN*`
   exit non-zero naming the PowerShell one-liner and download nothing, so
   every Windows shell gets one install location and one `PATH` update.
6. **`relate upgrade` on Windows.** Self-replace stays out of scope
   (ADR-0069 §6). The refusal and a Windows build's other reinstall pointers
   name the PowerShell one-liner. `--check` downloads and replaces nothing,
   so it runs on every platform; where the replace is unsupported, its
   advice line carries the refusal instead of advising `relate upgrade`.
7. **Verification.** An offline harness runs the installer under both
   PowerShell versions on the Windows CI lane with real signatures; the
   `install.sh` harness pins its constants to the `install.sh` and CLI
   copies on every host; the release pipeline installs the published release
   through `irm | iex` under both shells and runs it with `HOME` unset.

## Alternatives considered

- **`install.sh` installs the `.exe` under Git Bash.** Rejected: it lands in
  `~/.local/bin`, which native shells lack on `PATH`, so Windows would have
  two install locations. The one-liner runs from the same Git Bash prompt.
- **winget or Scoop as the only Windows path.** Rejected for now: both rely
  on third-party manifest repositories and review queues, and neither checks
  the release signature. They stay follow-ups pointing at the same asset.
- **Verify in .NET instead of `ssh-keygen`.** Rejected: .NET Framework, which
  Windows PowerShell 5.1 runs on, has no Ed25519 or SSHSIG support, and a
  bundled verifier is downloaded code that runs before anything is verified.
- **Authenticode signing instead.** Rejected as a substitute: it answers
  SmartScreen, not the integrity chain. `irm` downloads carry no mark of the
  web, so the installer raises no prompt; signing stays a follow-up.
- **Windows self-upgrade now.** Deferred: renaming the running `.exe` and
  writing the new one revisits ADR-0069 §6 and needs its own decision.
  Rerunning the installer is the upgrade until then.

## Consequences

- Windows gets a one-liner with the same fail-closed checks as macOS and
  Linux. The release asset contract gains `install.ps1`; relate-web must
  serve it beside `install.sh` (a change in that repository).
- Installer constants now have three spellings (CLI, `install.sh`,
  `install.ps1`); drift guards hold them together, so a key rotation or URL
  move changes all three.
- A host without the OpenSSH client, or with one older than 8.1, cannot
  install until it adds one; the installer says how.
- `iex` runs in the caller's session: the script defines its functions and
  constants there, fails with `throw` rather than `exit`, and keeps
  preference changes in its own scope. Windows PowerShell sessions that pin
  a protocol list without TLS 1.2 get TLS 1.2 added.

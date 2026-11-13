# ADR-0587: Windows MSVC builds link the C runtime statically

Status: Proposed
Claim: a7635b8322bff2b691509f3beb9d4a2afc5ab511

## Context

Rust's `x86_64-pc-windows-msvc` target links the C runtime dynamically by
default. The released `relate-windows-amd64.exe` therefore imports
`VCRUNTIME140.dll` and the Universal CRT forwarders, so it does not start on
a clean Windows install, in Windows Sandbox, or on Server Core without the
Visual C++ redistributable. The failure is a missing-DLL dialog, not an
error from relate. The FFI DLL has the same dependency, and it is loaded by
hosts — Go, C, Java — that may not bring the runtime with them. The Python
and Node native modules link the same way, so they depend on whatever the
host process or the system provides.
The workspace had no Windows target entry in its Cargo configuration.

## Decision

1. **Static CRT for every Windows MSVC target.** The workspace Cargo
   configuration sets `+crt-static` for `x86_64-pc-windows-msvc` and for
   `aarch64-pc-windows-msvc` (no ARM64 build yet; it takes the same
   setting when one is added). The setting is workspace-wide: the CLI
   executable, the FFI DLL, and the Python and Node native modules all
   carry their own copy of the C runtime and need no redistributable.
2. **Each artifact owns its runtime.** No public surface hands a CRT object
   — a `FILE*`, a CRT heap pointer meant for the caller's `free` — across
   its boundary. The C ABI already frees what it allocates through its own
   functions. A future surface that needs to share a CRT object across a
   module boundary needs its own decision.
3. **No environment override on Windows builds.** An environment
   `RUSTFLAGS` replaces Cargo's target table, so no Windows CI or release
   build sets one.
4. **Release check.** The release workflow reads the built CLI's import
   table and fails if it names a dynamic C runtime (`vcruntime`, `msvcp`,
   `ucrtbase`, `api-ms-win-crt`). It then runs the binary under PowerShell.

## Alternatives considered

- **Static CRT for the CLI only, through `RUSTFLAGS` in the release
  build.** Rejected: CI and local builds would link differently from the
  release, and the FFI DLL has the same dependency for its non-Python,
  non-Node hosts.
- **Bundle or chain-install the Visual C++ redistributable.** Rejected: it
  needs elevation, turns a single-file binary into a setup step, and
  contradicts the installer's no-elevation, per-user scope.
- **Leave it dynamic and document the prerequisite.** Rejected: the failure
  happens before relate runs, so users never see relate's own message
  explaining it.

## Consequences

- Windows binaries start on any supported Windows with no runtime
  prerequisite. They grow by roughly the size of the static runtime.
- C runtime security fixes reach users through relate releases, not through
  Windows Update's redistributable servicing.
- Every Windows module carries its own CRT heap, so rule 2 is load-bearing
  for any future surface.
- Build scripts and proc-macros on a Windows host build with the same
  setting, which the MSVC target supports.

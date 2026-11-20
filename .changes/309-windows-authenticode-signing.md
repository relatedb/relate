### Added

- The Windows release binary (`relate-windows-amd64.exe`) is signed with an
  Authenticode signature carrying an RFC 3161 timestamp at release time, so
  SmartScreen and enterprise download policies see a signed publisher
  instead of an unknown one (ADR-0594).

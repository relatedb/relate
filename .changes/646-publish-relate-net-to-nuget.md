### Added

- The .NET client is published to nuget.org as `Relate.Net`: every release builds
  one package carrying all five supported RIDs, and the release pipeline pushes it
  with a short-lived OIDC-issued key instead of a stored credential (ADR-0652).
  Install it with `dotnet add package Relate.Net`.

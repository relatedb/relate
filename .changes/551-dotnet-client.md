### Added

- New .NET client `Relate.Net` under `clients/dotnet` (ADR-0625): an
  embedded C#, F# and Visual Basic API over the typed C ABI v2, with
  disposable databases, connections and transactions, typed parameters,
  graph and temporal values, result metadata and structured errors. CI
  builds the native library from the same checkout and runs the
  three-language tests on Linux, macOS and Windows.

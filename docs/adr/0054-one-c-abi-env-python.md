# ADR 0054: One C ABI, one env prefix, one Python package

## Status

Accepted

## Context

ADR-0053 left two C libraries (`cypherlite_*` vs `memorydb_*`), `MEMORYDB_*`
env fallbacks, two TCK feature trees, and two Python stacks (PyO3 `cypherlite`
vs ctypes `memorydb`). Those leftovers block a single product identity.

## Decision

- **C ABI.** `cypherlite-ffi` is the only published C library. The Bolt-client
  handle model (database + many connections + tabular results) is absorbed
  into this crate under `cypherlite_*` names. Existing Swift functions
  (`cypherlite_open`, `cypherlite_execute`, …) stay. `cypherlite-bolt-ffi` and
  `memorydb_*` symbols are deleted. Language clients load `libcypherlite_ffi`
  via `CYPHERLITE_LIB`.
- **Env.** Product settings are `CYPHERLITE_*` only. `MEMORYDB_*` is not read.
  CIP ratchet env (`CIP_EVAL_MEMORYDB_*`) and `cip-eval-memorydb.json` keep
  their filenames.
- **TCK corpus.** Feature files live only under `testdata/tck`. Both harnesses
  (`cypherlite-tck` strict and `cypherlite-tck-opencypher` 100%) run that tree.
  The two *runners* stay; skip/error-class contracts are not merged.
- **Python.** The PyO3 package `cypherlite` is the only Python product.
  `clients/python` (ctypes) is deleted. Wheels publish to PyPI on tagged
  releases via trusted publishing when configured.

Clients implementing embedded Relate are outside this rename.

## Alternatives considered

- Keep `memorydb_*` as aliases forever — rejected; two names forever.
- Retarget clients onto the Swift JSON handle only — rejected; clients need
  multiple connections per database.
- Hard-cut env without an ADR — rejected; ADR-0053 promised a window.

## Consequences

- Swift XCFramework is unchanged at the symbol level used today.
- JS/Java/Go clients must be rewritten onto `cypherlite_*` tabular APIs.
- Operators must set `CYPHERLITE_*` instead of `MEMORYDB_*`.

## Later references

- PyPI distribution name is `neo4j-cypherlite` because `cypherlite` is already
  published by another project. The import module remains `cypherlite`.
- Clients implementing embedded Relate embed `cypherlite::Database`.
- ADR-0066: the CIP ratchet env vars are now `CIP_EVAL_*` (was
  `CIP_EVAL_MEMORYDB_*`) and the baseline is `baselines/cip-eval.json` (was
  `baselines/cip-eval-memorydb.json`). The MemoryDB name is retired from live
  surfaces; the `cypherlite_*` ABI commitment in this ADR is unaffected.
- ADR-0502: the typed C ABI v2 removes the JSON handle API (`cypherlite_open`,
  `cypherlite_new`, `cypherlite_execute` and the rest of that family), the
  `cypherlite_connect` and `cypherlite_query` aliases, and the thread-local
  `cypherlite_last_error`. The one `cypherlite_*` library and its name prefix
  in this ADR are unaffected.

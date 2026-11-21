### Fixed

- Under a configured logical graph size limit, concurrent writes no longer fail intermittently with `capacity exceeded`: a commit group now charges a dense id-array growth reservation once per group instead of once per group-mate crossing the same growth boundary. Single-writer admission is unchanged, and writes that genuinely exceed the headroom are still refused.

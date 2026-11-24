### Fixed

- A cancelled statement now stops inside the contracted chain and hub-star
  kernels instead of running to the end of their expansion and row
  conversion: a CLI `--timeout` or Bolt `--query-timeout` bounds cancel-to-
  return on those kernels the way it already did on the per-row walker.

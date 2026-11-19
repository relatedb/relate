### Changed

- Dense id-span stores reserve the smaller of a 1.5-growth ladder rung
  and the old power-of-two doubling, and the memory floor charges
  exactly that reservation — the worst-case slot slack in a restored
  store drops from below 2× to below 1.5× (ADR-0617).

### Notes

- Release tooling fix: a tagless re-cut no longer wedges the changelog.
  The fragment collector merges a pool section into an `[Unreleased]`
  heading a prior cut already parked entries under (idempotently — an
  entry the section already carries verbatim is skipped), and the
  rotation merges a populated `[Unreleased]` into the existing version
  section instead of skipping — the state that refused the v1.3.0
  re-dispatch ("contributes a 'Notes' section [Unreleased] already
  carries") now completes: all six pending entries landed in the
  v1.3.0 section with `[Unreleased]` emptied.

### Notes

- Release tooling fix: a repo-signing-key rotation landed without
  re-signing the committed fixture, turning every PR's
  `review-scripts-test` lane red since. New
  `repo-signing-fixture-refresh.yml` re-signs the fixture from the
  `REPO_GPG_KEY` secret and opens a PR; the lane now also runs on the
  key and fixture paths so a rotation cannot slip past it again
  (RELEASING.md "Repository signing key" carries the rotation runbook).

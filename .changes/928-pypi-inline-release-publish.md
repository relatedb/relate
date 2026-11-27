### Notes

- Release tooling fix: a release cut's PyPI publish failed by
  construction — `release.yml` chained `publish-registries.yml`, and
  PyPI's Trusted Publishing does not support reusable workflows, so no
  registration could accept the chained token (the v1.0.0, v1.3.0 and
  v1.4.0 cuts each failed the leg and recovered by manual dispatch).
  PyPI now publishes through the inline `publish-pypi` job in
  `release.yml` (ADR-0710): a cut with `registries=true` lands
  `relate-db` on PyPI green from its own run. `publish-registries.yml`
  keeps its PyPI leg as the direct-dispatch recovery arm.

### Notes

- Release infrastructure: the repository signing keypair was rotated —
  the first key's private half was never stored, so the APT/RPM
  repository lanes failed closed on their first real run. The new
  public half is committed; the private half lives as the
  `REPO_GPG_KEY` secret with an owner custody copy.

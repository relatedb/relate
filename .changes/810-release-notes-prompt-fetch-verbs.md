### Notes

- Release tooling fix: the release-notes prompt forbids `curl`, `wget`
  and `git clone` in unfenced prose — the output guard reads any line
  carrying a fetch verb as execution-shaped and refuses it without an
  audited destination; the mechanism is named in prose and the command
  rides in a fenced example.

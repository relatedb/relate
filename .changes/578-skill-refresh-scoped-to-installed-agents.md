### Changed

- `relate upgrade` no longer installs the agent skill into every detected
  agent when only some have it: the post-upgrade refresh updates only the
  agents whose bundle is installed and leaves the rest untouched (no
  prompt, no install, no hint, `--yes` included). The skill reaches new
  agents only when nothing is installed anywhere (the first-install
  prompt, or `--yes`) or through an explicit `relate skill install`
  (ADR-0632, REL8-1875).

### Fixed

- The `relate upgrade --yes` help line states what the flag does — install
  the skill when none is installed, without asking, while installed
  bundles update regardless — instead of the ambiguous
  "Auto-accept the post-upgrade skill refresh".

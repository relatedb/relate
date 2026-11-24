### Notes

- The baseline images' digest pins have one home (`scripts/baseline-env.sh`,
  read by name via `--print-image build|run`): the ceremony no longer carries its own
  floating defaults (which is how the memory lanes drifted off the pinned
  class while the make-driven lanes stayed on it), the Dockerfile's ARG
  defaults are gone so a standalone build fails loudly, and the fingerprint
  stamp no longer fabricates a floating image default. The run image is
  re-pinned to the dated tag `debian:bookworm-20261123-slim@sha256:e67eeead…`.

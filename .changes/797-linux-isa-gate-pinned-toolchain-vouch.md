### Notes

- Release tooling fix: the x86-64 baseline ISA gate accepts the pinned
  build container's note-less baseline link under an explicit
  `--pinned-toolchain` vouch — the container's rustc+gcc link emits no
  "x86 ISA needed" ELF property note at baseline, so the gate's
  note-must-exist arm had refused every release build — and the lane also
  pins `-C target-cpu=x86-64` explicitly. A note listing
  x86-64-v2/v3/v4 still fails the release.

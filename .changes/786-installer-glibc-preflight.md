### Fixed

- The Linux installer refuses a system the release binary cannot run:
  `install.sh` checks the running glibc (`getconf`, `ldd --version` as
  the fallback) against the documented 2.35 floor and dies before
  downloading anything — naming the floor, the distro floor (Debian 12+,
  Ubuntu 22.04+, Fedora 36+), and the build-from-source and Python-wheel
  alternatives — instead of installing a binary the dynamic loader
  refuses at exec. A musl Linux, or any version the preflight cannot
  determine, is refused the same way (REL8-1841).

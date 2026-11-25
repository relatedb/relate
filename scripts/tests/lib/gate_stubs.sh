# Fixture generators for gate suites that fake a binutils tool on PATH.
# Sourced after scripts/tests/lib/tap.sh by the readelf-driven gate suites
# only (test_check_linux_isa.sh, test_check_glibc_floor.sh); tap.sh stays
# the generic TAP harness.

tap_stub_readelf() { # $1 = stub dir, $2 = the readelf flag the gate passes, $3 = fixture suffix; installs a fake readelf that cats "<binary>.<suffix>" for that flag and exits 1 otherwise (also when the fixture is absent — the readelf-failed state)
	case "$2" in
	-*) ;;
	*) echo "tap_stub_readelf: flag must start with '-': $2" >&2; return 2 ;;
	esac
	case "$2" in
	*[!-A-Za-z0-9]*) echo "tap_stub_readelf: flag must be [-A-Za-z0-9]: $2" >&2; return 2 ;;
	esac
	mkdir -p "$1"
	cat >"$1/readelf" <<STUB
#!/bin/sh
# Fake readelf: the fixture beside the named binary holds its report.
for last; do :; done
case "\$1" in
  "$2") cat "\$last.$3" ;;
  *) exit 1 ;;
esac
STUB
	chmod +x "$1/readelf"
}

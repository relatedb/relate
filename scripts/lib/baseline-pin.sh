# The pin grammar's one home (REL8-2081): `repo:tag@sha256:<64 hex>`, repo a
# single official-library segment. Sourced by the drift check, its suite and
# the pins guard, so "is this a pin" and "which part is this" have one answer
# — including the single-reference rule: a docker reference never contains
# whitespace, so a multi-line value can never pass as one pin.
#
#   pin_valid <ref>            → 0 when the reference is a pin
#   digest_valid <digest>      → 0 when the bare digest is sha256:<64 hex>
#   pin_part <ref> name|tag|digest|repo  (refused unless <ref> is a pin)
#   read_pin <pins-home-script> build|run → the validated pin on stdout
pin_valid() {
	case "$1" in
		""|*[[:space:]]*) return 1 ;;
	esac
	printf '%s\n' "${1%@*}" | grep -Eq '^[^/:@]+:[^:@]+$' || return 1
	digest_valid "${1##*@}"
}

digest_valid() { # $1 = a bare digest
	printf '%s\n' "$1" | grep -Eq '^sha256:[0-9a-f]{64}$'
}

pin_part() { # $1 = the reference, $2 = the part; the parts exist only for a pin
	pin_valid "$1" || return 1
	rest="${1%@*}" # repo:tag
	case "$2" in
		name) printf '%s\n' "$rest" ;;
		repo) printf '%s\n' "${rest%%:*}" ;;
		tag) printf '%s\n' "${rest#*:}" ;;
		digest) printf '%s\n' "${1##*@}" ;;
		*)
			printf 'pin_part: unknown part %s\n' "$2" >&2
			return 2
			;;
	esac
}

read_pin() { # $1 = the pins-home script, $2 = build|run; prints the validated pin
	ref=$("$1" --print-image "$2") || return 1
	pin_valid "$ref" || {
		printf 'baseline-pin: %s is not name:tag@sha256:<64 hex>: %s\n' "$2" "$ref" >&2
		return 1
	}
	printf '%s\n' "$ref"
}

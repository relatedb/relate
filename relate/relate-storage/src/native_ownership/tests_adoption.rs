//! ADR-0623's engine-sidecar adoption pins: the rename-fallback
//! decision over the exclusive-rename primitive — extracted from
//! `tests.rs` (the no-1k-line rule, LABS-311), a sibling of the reset
//! arcs. The pin needs no fixture from `tests.rs` — only the seam it
//! pins, reached through the ancestor path.

use super::super::exclusive_rename_unsupported;

#[test]
fn only_the_unsupported_class_falls_back_to_a_plain_adoption_rename() {
    // The fallback decision, pinned: the syscall wrapper itself cannot
    // be made to answer "unsupported" on a platform that has it, so
    // the decision is factored (`exclusive_rename_unsupported`) and
    // pinned here against the real errno decodes of the pinned
    // toolchain. Exactly the unsupported class falls back to the plain
    // rename: the helper's explicit `Unsupported` construction on
    // targets without the syscall, `ENOSYS` (decoded `Unsupported`) on
    // kernels without one, and the `ENOTSUP`/`EOPNOTSUPP` a filesystem
    // answers this one call with (matched by errno — std decodes them
    // `Uncategorized`). A genuinely occupied destination (`EEXIST`,
    // decoded `AlreadyExists` — the strict primitive's answer for a
    // raced-in `.relate/`), an argument error (`EINVAL`), and a
    // missing source (`ENOENT`) all stay failures: the adoption fails
    // closed rather than replacing or guessing. The non-unix arm pins
    // its own contract beside it: the platform's exclusive rename
    // always exists, so even the `Unsupported` construction stays
    // strict there — no fallback is reachable on a platform that
    // cannot report the class.
    #[cfg(unix)]
    let unsupported = |err: std::io::Error| {
        assert!(exclusive_rename_unsupported(&err), "{err:?} must fall back");
    };
    let strict = |err: std::io::Error| {
        assert!(
            !exclusive_rename_unsupported(&err),
            "{err:?} must stay strict"
        );
    };
    #[cfg(unix)]
    {
        unsupported(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "exclusive rename is unsupported on this target",
        ));
        unsupported(std::io::Error::from_raw_os_error(libc::ENOSYS));
        unsupported(std::io::Error::from_raw_os_error(libc::ENOTSUP));
        // EOPNOTSUPP equals ENOTSUP on Linux and differs on macOS; pin
        // the constant this platform's filesystems actually answer
        // with, whichever it is. Asserted as two values — never an
        // or-pattern over the constants, which Linux's equal
        // definitions make an unreachable pattern.
        unsupported(std::io::Error::from_raw_os_error(libc::EOPNOTSUPP));
        strict(std::io::Error::from_raw_os_error(libc::EEXIST));
        strict(std::io::Error::from_raw_os_error(libc::EINVAL));
        strict(std::io::Error::from_raw_os_error(libc::ENOENT));
    }
    #[cfg(not(unix))]
    {
        strict(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "exclusive rename is unsupported on this target",
        ));
    }
}

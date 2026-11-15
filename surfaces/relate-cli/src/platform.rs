//! The one os/arch vocabulary the CLI names a host by: the release
//! channel's asset names (`relate-<os>-<arch>`, ADR-0250 §2, ADR-0431 §1)
//! and the agent-feedback report's `os` / `os_arch` properties
//! (ADR-0588) speak the same words, so the Rust-to-word tables live here
//! once. Each consumer layers its own restriction on top — the channel
//! publishes macOS and Linux only; the report also names Windows.

/// The vocabulary word for a `std::env::consts::OS` value, or `None` for
/// an os the vocabulary does not name.
pub(crate) fn os_word(os: &str) -> Option<&'static str> {
    match os {
        "linux" => Some("linux"),
        "macos" => Some("darwin"),
        "windows" => Some("windows"),
        _ => None,
    }
}

/// The vocabulary word for a `std::env::consts::ARCH` value, or `None`
/// for an architecture the vocabulary does not name.
pub(crate) fn arch_word(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("arm64"),
        "x86_64" => Some("amd64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_name_the_supported_hosts_and_refuse_the_rest() {
        assert_eq!(os_word("linux"), Some("linux"));
        assert_eq!(os_word("macos"), Some("darwin"));
        assert_eq!(os_word("windows"), Some("windows"));
        assert_eq!(os_word("freebsd"), None);
        assert_eq!(arch_word("aarch64"), Some("arm64"));
        assert_eq!(arch_word("x86_64"), Some("amd64"));
        assert_eq!(arch_word("riscv64"), None);
    }
}

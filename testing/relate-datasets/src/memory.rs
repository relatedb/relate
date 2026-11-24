//! The dataset harnesses' memory-reporting policy: the require-sampling
//! guard and the byte formatting for CLI lines.
//!
//! The platform samplers themselves live in the workspace's canonical
//! `relate_test_support::sampling` module — one sampler family for every
//! harness that reports process RSS (dataset load tests, memory shapes,
//! checkpoints, bulk load tests, and the query-latency suite); this module
//! keeps the harness policy on top of them. Sampling is platform-gated:
//! Linux reads `/proc/<pid>/status`, macOS asks `task_info`. Everywhere else
//! the samplers return `None`; every runner that samples its own process
//! calls [`require_sampling_available`] before sampling, so an unsupported
//! platform fails with one explicit error instead of a fabricated zero-byte
//! report. The memory-shape runner is the exception: it samples the
//! `relate-bolt` child's pid through the canonical `process_status`, so the
//! two memory loadtest lanes call [`require_loadtest_preflight`] — the
//! child-sampling capability and the binary they spawn — instead of this
//! process-sampling check.

use std::path::Path;

use relate_test_support::sampling::{process_peak_rss_bytes, process_rss_bytes, process_status};

/// Refuse to run a memory-reporting harness without RSS sampling.
///
/// The one guard every runner calls before its first sample. Returns an error
/// naming the missing platform sampler instead of letting a report fill with
/// zero-byte measurements.
pub fn require_sampling_available() -> relate::Result<()> {
    if process_rss_bytes().is_some() && process_peak_rss_bytes().is_some() {
        return Ok(());
    }
    Err(relate::Error::semantic(
        "memory reporting needs process RSS sampling (Linux /proc/<pid>/status or macOS task_info); RSS sampling is unavailable on this platform",
    ))
}

/// The preflight both memory loadtest lanes open with: the Linux `/proc`
/// capability their child-pid RSS sampling needs, then the product binary
/// the lane spawns. One home for both checks — `lane` and `bin_noun` name
/// the lane and its binary in the messages (`memory-shape` spawns
/// `relate serve`, `memory-controls` spawns `relate`), so neither lane's
/// diagnostics are a second spelling.
pub(crate) fn require_loadtest_preflight(
    lane: &str,
    bin_noun: &str,
    bin: &Path,
) -> Result<(), String> {
    if process_status(std::process::id()).is_none() {
        return Err(format!(
            "{lane} RSS sampling needs Linux /proc/<pid>/status (VmRSS and VmHWM)"
        ));
    }
    if !bin.is_file() {
        return Err(format!(
            "{bin_noun} binary not found at {} — build it before this loadtest",
            bin.display()
        ));
    }
    Ok(())
}

/// `used / limit` for CLI lines. `limit` `None` is unlimited; `used` `None` is
/// unmeasured (the runner could not query the engine's logical bytes, so the
/// report honestly omits them instead of printing a fabricated zero).
pub fn format_logical_cap(used: Option<u64>, limit: Option<u64>) -> String {
    match (used, limit) {
        (Some(u), Some(l)) => format!("{} / {}", format_bytes(u), format_bytes(l)),
        (Some(u), None) => format!("{} / unlimited", format_bytes(u)),
        (None, _) => "not measured".into(),
    }
}

/// Human-readable byte count for CLI / docs examples.
pub fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let x = n as f64;
    if x >= K * K * K {
        format!("{:.2} GiB", x / (K * K * K))
    } else if x >= K * K {
        format!("{:.2} MiB", x / (K * K))
    } else if x >= K {
        format!("{:.1} KiB", x / K)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_test_support::sampling::SAMPLING_SUPPORTED;

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.00 MiB");
    }

    #[test]
    fn format_logical_cap_shows_limit_or_unlimited() {
        assert_eq!(
            format_logical_cap(Some(10 << 20), Some(10 << 20)),
            "10.00 MiB / 10.00 MiB"
        );
        assert_eq!(format_logical_cap(Some(100), None), "100 B / unlimited");
        assert_eq!(format_logical_cap(None, Some(10 << 20)), "not measured");
    }

    #[test]
    fn rss_sampling_guard_matches_platform_support() {
        // The support set is the sampling module's own fact (SAMPLING_SUPPORTED):
        // the guard passes where sampling is supported and refuses with its
        // documented error where it is not.
        assert_eq!(require_sampling_available().is_ok(), SAMPLING_SUPPORTED);
    }
}

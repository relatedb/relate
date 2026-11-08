//! Memory control verifications and workspace memory calibration.
//!
//! A control verification checks that a limit holds. The calibration measures
//! resident memory per live logical workspace byte.

pub mod calibration;
pub mod controls;
pub mod harness;
pub mod scenario;
pub mod workload;

/// 1024-based, matching the memory-shape loadtest. GB once the value
/// reaches 1024 MB. Smaller sizes stay in MB.
pub fn format_mem(bytes: u64) -> String {
    format_mem_signed(bytes as f64)
}

/// [`format_mem`] for a signed byte count. A negative value keeps its
/// sign and still picks MB or GB from the magnitude.
pub fn format_mem_signed(bytes: f64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let negative = bytes.is_sign_negative();
    let mb = bytes.abs() / MB;
    let body = if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.2} MB")
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::{format_mem, format_mem_signed};

    #[test]
    fn format_mem_picks_mb_or_gb() {
        assert_eq!(format_mem(3_448_463), "3.29 MB");
        assert_eq!(format_mem(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_mem_signed(-105_925.0), "-0.10 MB");
        assert_eq!(format_mem_signed(1024.0 * 1024.0 * 1024.0), "1.00 GB");
    }
}

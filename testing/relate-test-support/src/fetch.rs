//! The pinned, fail-closed data-set fetch the testing suites share
//! (ADR-0243): the `data.sha256` pin-table parse, the
//! streamed sha256, the verified download, and the per-data-set
//! lock-and-stamp ensure a workload wraps around its own extraction.
//!
//! Nothing is vendored: every data set is downloaded on first use,
//! checked against its committed pin, and re-verified on every run. The
//! cache layout is this module's — a workload only names its files:
//!
//! ```text
//! <root>/archives/<archive>   the archive as downloaded
//! <root>/<name>               the extracted data set (file or directory)
//! <root>/.<name>.ok           stamp holding the archive's sha256
//! <root>/.<name>.lock         per-data-set ensure lock (fs4, exclusive)
//! ```
//!
//! A workload keeps what only it knows: its cache root, its archive and
//! pin-table paths, its extraction, and the text of its offline refusal —
//! which names the command its user runs.
//!
//! A stamp is only believed when the target it describes exists in its
//! [`Target`] kind (a file, or a directory tree) and `.<name>.ok` holds that
//! data set's pinned sha256, trimmed. That is the whole of the fast path's
//! evidence: the stamp records which archive was verified and extracted, not
//! the extracted bytes, so a target rewritten behind its stamp is not
//! re-hashed on the next run — a different pin, or a missing target, sends
//! [`ensure`] back through the lock, the archive verify and the extractor.

mod download;
mod ensure;
mod pins;

pub use download::{download_verified, sha256_file};
pub use ensure::{ensure, Dataset, MissingArchive, Target};
pub use pins::{is_sha256_hex, parse_pins, pin_sha256, Pin};

use std::path::PathBuf;

/// Why a pinned fetch is not available. Every fallible step names the
/// reason a user can act on, typed so callers match on it instead of
/// sniffing the message.
#[derive(Debug)]
pub enum FetchError {
    /// The pin table is malformed: a line that is not
    /// `<sha256> <name>`, a pin that is not 64 lowercase hex characters,
    /// or a table naming nothing.
    PinTable {
        /// The pin table, as the refusal names it.
        path: PathBuf,
        /// The offending line, where the table has one.
        line: Option<usize>,
        detail: String,
    },
    /// A downloaded archive's sha256 is not its pin: it was discarded.
    PinMismatch {
        /// The URL the archive was downloaded from.
        url: String,
        expected: String,
        got: String,
    },
    /// The pin table names no pin for a file the data set needs.
    PinMissing {
        /// The pin table, as the refusal names it.
        path: PathBuf,
        /// The file the table does not name.
        name: String,
    },
    /// The response body is larger than the caller's per-download cap.
    TooLarge { url: String, limit: u64 },
    /// The downloader is https only: the URL, or a redirect hop's, is
    /// plain http.
    HttpsOnly { url: String },
    /// The HTTP fetch failed.
    Network { url: String, detail: String },
    /// A filesystem step failed.
    Io {
        what: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PinTable { path, line, detail } => match line {
                Some(line) => write!(f, "{}:{line}: {detail}", path.display()),
                None => write!(f, "{}: {detail}", path.display()),
            },
            Self::PinMismatch { url, expected, got } => write!(
                f,
                "{url}: sha256 {got} does not match the pinned {expected}; the archive was \
                 discarded"
            ),
            Self::PinMissing { path, name } => {
                write!(f, "{}: the pin table does not name {name}", path.display())
            }
            Self::TooLarge { url, limit } => write!(
                f,
                "fetching {url}: the response is larger than the {limit} byte limit; the \
                 download was discarded"
            ),
            Self::HttpsOnly { url } => write!(f, "fetching {url}: downloads are https only"),
            Self::Network { url, detail } => write!(f, "fetching {url}: {detail}"),
            Self::Io { what, source } => write!(f, "{what}: {source}"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The suites whose errors are human-readable messages (query-latency's
/// `Result<T, String>`) convert a fetch refusal into its text, so the
/// shared kit fits the suite's own error convention.
impl From<FetchError> for String {
    fn from(err: FetchError) -> Self {
        err.to_string()
    }
}

/// The failure of an I/O step, naming what it was doing.
fn io_error(what: String, source: std::io::Error) -> FetchError {
    FetchError::Io { what, source }
}

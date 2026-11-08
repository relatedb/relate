//! The verified download: sha256 streaming, the https-only agent, and the
//! `.part` file that only a verified body is renamed out of.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{io_error, FetchError};
use crate::hex;

/// A whole archive at a slow link is still one request: the download's
/// global timeout, from the first connect to the last body byte.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// sha256 of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String, FetchError> {
    let mut f = File::open(path).map_err(|e| io_error(format!("opening {}", path.display()), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| io_error(format!("reading {}", path.display()), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// The one HTTP agent the downloader uses: https only — ureq applies that
/// pin to every hop of a redirect chain, so a redirect to plaintext is
/// refused exactly like a plaintext URL — and a whole-archive timeout.
/// Built per download, as the use-once client was before it.
fn download_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build(),
    )
}

/// Download `url` to `dest` through a temporary [`PartFile`], hashing as it
/// streams; the file is renamed into place only when its sha256 is `want`. The
/// downloader is https only (every redirect hop included) and refuses a body
/// larger than `max_bytes` — the per-call cap the caller sets from the archive
/// it expects — naming the URL and the limit. Either refusal discards the
/// download.
pub fn download_verified(
    url: &str,
    dest: &Path,
    want: &str,
    max_bytes: u64,
) -> Result<(), FetchError> {
    let parent = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| io_error(format!("creating {}", parent.display()), e))?;
    let response = download_agent().get(url).call().map_err(|e| match e {
        ureq::Error::RequireHttpsOnly(url) => FetchError::HttpsOnly { url },
        other => FetchError::Network {
            url: url.to_string(),
            detail: other.to_string(),
        },
    })?;
    eprintln!("downloading {url}");
    stream_verified(
        response.into_body().into_reader(),
        dest,
        want,
        max_bytes,
        url,
    )
}

/// A download's `.part` file: the body streams into it, and only the verified
/// file is renamed onto the archive. The guard removes whatever it still holds
/// when it drops, so an abandoned or refused download never leaves its bytes
/// behind.
struct PartFile {
    path: PathBuf,
    /// Set once the file was renamed onto the archive.
    committed: bool,
}

impl PartFile {
    /// The part file one download of `dest` writes: the name carries a process
    /// id AND a per-download sequence number — callers in one process share a
    /// process id, and two of them fetching the same archive concurrently must
    /// not write the same `.part` (one thread's rename removes the file out
    /// from under the other).
    fn beside(dest: &Path) -> Self {
        static PART_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = PART_SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            path: dest.with_extension(format!("part.{}.{}", std::process::id(), seq)),
            committed: false,
        }
    }

    /// Create the file the body streams into.
    fn create(&self) -> Result<File, FetchError> {
        File::create(&self.path)
            .map_err(|e| io_error(format!("creating {}", self.path.display()), e))
    }

    /// Rename the finished, verified part onto `dest`.
    fn commit(mut self, dest: &Path) -> Result<(), FetchError> {
        std::fs::rename(&self.path, dest)
            .map_err(|e| io_error(format!("renaming into {}", dest.display()), e))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.committed {
            crate::remove_or_report("fetch", &self.path, || std::fs::remove_file(&self.path));
        }
    }
}

/// Stream `source` into `dest`, hashing as it goes: the body lands in a
/// sibling [`PartFile`] that is renamed onto `dest` only when its sha256 is
/// `want`, and a body over `max_bytes` or a hash mismatch discards it. `url`
/// labels the refusals.
fn stream_verified(
    mut source: impl Read,
    dest: &Path,
    want: &str,
    max_bytes: u64,
    url: &str,
) -> Result<(), FetchError> {
    let part = PartFile::beside(dest);
    let mut out = part.create()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = source
            .read(&mut buf)
            .map_err(|e| io_error(format!("reading {url}"), e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            return Err(FetchError::TooLarge {
                url: url.to_string(),
                limit: max_bytes,
            });
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| io_error(format!("writing {}", part.path.display()), e))?;
    }
    out.sync_all()
        .map_err(|e| io_error(format!("syncing {}", part.path.display()), e))?;
    let got = hex(&hasher.finalize());
    if got != want {
        return Err(FetchError::PinMismatch {
            url: url.to_string(),
            expected: want.to_string(),
            got,
        });
    }
    part.commit(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one 64-lowercase-hex pin every fabricated archive is pinned to;
    /// tests only need a spelling, never a digest they compute themselves.
    const PIN: &str = "967dd4b0733367be1fb2e59e017b0eba941ce1c252fe8e304af42bde45ff3f65";

    #[test]
    fn sha256_file_streams_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The downloader's https-only pin: the agent refuses a plain http URL,
    /// and ureq applies `https_only` to every redirect hop, so a redirect
    /// to plaintext is the same refusal.
    #[test]
    fn the_downloader_is_https_only() {
        assert!(download_agent().config().https_only());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("archive.gz");
        let url = "http://127.0.0.1:1/archive.gz";
        let err = download_verified(url, &dest, PIN, 1 << 20).unwrap_err();
        match &err {
            FetchError::HttpsOnly { url: got } => assert_eq!(got, url),
            other => panic!("expected an https-only refusal, got {other:?}"),
        }
        assert!(err.to_string().contains(url), "{err}");
        assert!(!dest.exists(), "a refused download never lands");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no part file is left behind"
        );
    }

    /// A body over the per-call cap fails closed with a typed too-large
    /// refusal naming the URL and the limit, and nothing is renamed into
    /// place — the part file's guard removes what the stream wrote.
    #[test]
    fn a_body_over_the_limit_is_refused_naming_url_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("archive.gz");
        let url = "https://example.org/archive.gz";
        let body = vec![0u8; 11];
        let err = stream_verified(std::io::Cursor::new(body), &dest, PIN, 10, url).unwrap_err();
        match &err {
            FetchError::TooLarge { url: got, limit } => {
                assert_eq!(got, url);
                assert_eq!(*limit, 10);
            }
            other => panic!("expected a too-large refusal, got {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains(url) && text.contains("10"), "{text}");
        assert!(!dest.exists(), "an over-limit body never lands");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the part file is discarded"
        );
    }

    /// A body exactly at the limit is accepted, and a mismatched hash
    /// discards the download instead of renaming it into place.
    #[test]
    fn the_limit_is_inclusive_and_a_mismatch_discards() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("archive.gz");
        std::fs::write(dir.path().join("body"), b"abc").unwrap();
        let got = sha256_file(&dir.path().join("body")).unwrap();
        stream_verified(std::io::Cursor::new(b"abc".to_vec()), &dest, &got, 3, "u").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "the archive and the body fixture, and no part file"
        );
        let mismatch = dir.path().join("mismatch");
        let err = stream_verified(
            std::io::Cursor::new(b"abcd".to_vec()),
            &mismatch,
            &got,
            4,
            "u",
        )
        .unwrap_err();
        assert!(matches!(err, FetchError::PinMismatch { .. }), "{err:?}");
        assert!(!mismatch.exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "a mismatched download leaves no part file"
        );
    }
}

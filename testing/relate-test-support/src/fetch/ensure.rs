//! The per-data-set lock-and-stamp ensure and the cache layout it owns.

use std::fs::File;
use std::path::{Path, PathBuf};

use super::{download_verified, io_error, sha256_file, FetchError};

/// What a data set's extraction produces: one file, or a directory tree.
/// The stamp only certifies a target of this kind, and a re-extraction
/// removes the previous one before the extractor runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The extraction produces one file.
    File,
    /// The extraction produces a directory tree.
    Directory,
}

impl Target {
    /// Whether `path` is this kind of target.
    fn present(self, path: &Path) -> bool {
        match self {
            Self::File => path.is_file(),
            Self::Directory => path.is_dir(),
        }
    }

    /// The io call that removes this kind of target.
    fn remove(self, path: &Path) -> std::io::Result<()> {
        match self {
            Self::File => std::fs::remove_file(path),
            Self::Directory => std::fs::remove_dir_all(path),
        }
    }
}

/// One data set the [`ensure`] caches. Named fields, so the archive's
/// name, its URL and its pin — all strings — cannot be transposed at a
/// call site.
#[derive(Debug, Clone, Copy)]
pub struct Dataset<'a> {
    /// The extracted data set's name under the cache root; the stamp and
    /// the lock are named from it.
    pub name: &'a str,
    /// What the extraction produces.
    pub target: Target,
    /// The archive's file name under `<root>/archives`.
    pub archive: &'a str,
    /// Where the archive is downloaded from.
    pub url: &'a str,
    /// The archive's pinned sha256.
    pub sha256: &'a str,
    /// The largest archive body the download accepts.
    pub max_bytes: u64,
}

/// What [`ensure`] may do when the data set's archive is neither cached nor
/// its pin: download it, or refuse with the caller's own error — only the
/// workload knows the command its user runs to fetch its data set.
#[derive(Debug)]
pub enum MissingArchive<F> {
    /// Download the archive from the data set's URL, verified against its pin.
    Download,
    /// Refuse with this error.
    Refuse(F),
}

/// Make sure `dataset` is extracted under `root` and return its path.
///
/// The fast path is the stamp beside the target — `.<name>.ok` holding the
/// archive's sha256, with the target present in its [`Target`] kind — and
/// those two facts are all it certifies (the module doc says what that leaves
/// unverified). Otherwise one [`DatasetLock`] serializes the writers (the same
/// file, so a fetch process and a test in another process agree), the cached
/// archive is verified against its pin — or downloaded when `missing` allows
/// it — the stale target and stamp are removed, `extract` writes the target
/// from the verified archive, and the stamp is written last. Every stamp read
/// other than "no stamp" propagates: an unreadable stamp cannot be treated as
/// "not stamped" without re-extracting, under the extractor's hands, a data
/// set the stamp cannot vouch for.
///
/// The offline refusal is the caller's, carried by
/// [`MissingArchive::Refuse`]: it names the command only the workload knows.
pub fn ensure<F>(
    root: &Path,
    dataset: &Dataset<'_>,
    missing: MissingArchive<F>,
    extract: impl FnOnce(&Path) -> Result<(), F>,
) -> Result<PathBuf, F>
where
    F: From<FetchError>,
{
    let extracted = root.join(dataset.name);
    let stamp = root.join(format!(".{}.ok", dataset.name));
    let stamped = || -> Result<bool, FetchError> {
        if !dataset.target.present(&extracted) {
            return Ok(false);
        }
        match std::fs::read_to_string(&stamp) {
            Ok(text) => Ok(text.trim() == dataset.sha256),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_error(format!("reading {}", stamp.display()), e)),
        }
    };
    if stamped()? {
        return Ok(extracted);
    }
    std::fs::create_dir_all(root)
        .map_err(|e| io_error(format!("creating {}", root.display()), e))?;
    // One writer per data set: the first through does the work, the rest see
    // the stamp under the lock and take the fast path.
    // Released on drop, on every return path below.
    let _lock = DatasetLock::acquire(&root.join(format!(".{}.lock", dataset.name)))?;
    if stamped()? {
        return Ok(extracted);
    }
    let archive = root.join("archives").join(dataset.archive);
    let cached = archive.is_file() && sha256_file(&archive)? == dataset.sha256;
    if !cached {
        match missing {
            MissingArchive::Download => {
                download_verified(dataset.url, &archive, dataset.sha256, dataset.max_bytes)?
            }
            MissingArchive::Refuse(error) => return Err(error),
        }
    }
    remove_if_present(&extracted, dataset.target)?;
    remove_if_present(&stamp, Target::File)?;
    extract(&archive)?;
    std::fs::write(&stamp, format!("{}\n", dataset.sha256))
        .map_err(|e| io_error(format!("writing {}", stamp.display()), e))?;
    Ok(extracted)
}

/// One data set's exclusive ensure lock, held from the first stamp re-check to
/// the last stamp write: an exclusive advisory lock over the whole
/// download+extract+stamp serializes the writers, and the guard releases it
/// when it drops — on the error paths too, so a refused fetch never wedges the
/// data set for the next run.
struct DatasetLock(File);

impl DatasetLock {
    /// Take the exclusive advisory lock `path` names — the same file for every
    /// writer, so processes agree — creating it when the cache is fresh.
    fn acquire(path: &Path) -> Result<Self, FetchError> {
        let file = File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| io_error(format!("opening {}", path.display()), e))?;
        fs4::FileExt::lock_exclusive(&file)
            .map_err(|e| io_error(format!("locking {}", path.display()), e))?;
        Ok(Self(file))
    }
}

impl Drop for DatasetLock {
    fn drop(&mut self) {
        // Closing the file releases the advisory lock; the explicit unlock
        // keeps the release on the guard that took it.
        let _ = fs4::FileExt::unlock(&self.0);
    }
}

/// Remove `path` if it exists, as `target`'s kind. Fail closed: a missing
/// path is the expected case, every other error (a permissions failure, a
/// directory where a file was expected) propagates — swallowing one would
/// let a stale extraction or stamp certify what it no longer describes.
fn remove_if_present(path: &Path, target: Target) -> Result<(), FetchError> {
    match target.remove(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(format!("removing {}", path.display()), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// `Dataset` over it is cached and verified without a download.
    fn cache_archive(root: &Path, archive: &str, body: &[u8]) -> String {
        let path = root.join("archives").join(archive);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        sha256_file(&path).unwrap()
    }

    /// The extractor the ensure tests share: copy the verified archive
    /// into the target — the simplest extraction that produces one.
    fn copy_target(archive: &Path, target: &Path) -> Result<(), TestError> {
        std::fs::copy(archive, target)
            .map(|_| ())
            .map_err(|e| io_error(format!("extracting {}", archive.display()), e).into())
    }

    /// The ensure tests' own error type: [`ensure`] is generic over the
    /// caller's error, and these fixtures never refuse a cached archive, so
    /// their refusal names no command.
    #[derive(Debug)]
    enum TestError {
        /// The offline refusal no fixture returns: every fixture caches its
        /// archive before [`ensure`] runs.
        NotCached,
        /// A refusal the kit itself made — an unreadable stamp, a wrong pin.
        Kit(FetchError),
    }

    impl From<FetchError> for TestError {
        fn from(err: FetchError) -> Self {
            Self::Kit(err)
        }
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::NotCached => f.write_str("the data set is not cached"),
                Self::Kit(err) => err.fmt(f),
            }
        }
    }

    /// The ensure fixtures' missing-archive policy: the archive is cached
    /// before `ensure` runs, so the refusal is built and never returned.
    fn refusal() -> MissingArchive<TestError> {
        MissingArchive::Refuse(TestError::NotCached)
    }

    /// Every data set gets its own lock and stamp, and a cached one needs
    /// no network: two data sets in one root do not share either.
    #[test]
    fn one_lock_per_dataset() {
        let root = tempfile::tempdir().unwrap();
        for name in ["alpha", "beta"] {
            let archive = format!("{name}.gz");
            let sha256 = cache_archive(root.path(), &archive, name.as_bytes());
            let dataset = Dataset {
                name,
                target: Target::File,
                archive: &archive,
                url: "https://example.org/archive.gz",
                sha256: &sha256,
                max_bytes: 1 << 20,
            };
            let target = root.path().join(name);
            let got = ensure(root.path(), &dataset, refusal(), |a| {
                copy_target(a, &target)
            })
            .unwrap();
            assert_eq!(got, target);
        }
        for name in ["alpha", "beta"] {
            assert!(root.path().join(format!(".{name}.lock")).is_file());
            assert!(root.path().join(format!(".{name}.ok")).is_file());
        }
    }

    /// Two ensures of one data set serialize on its lock: the extractor
    /// runs once, and the second caller takes the stamped fast path.
    #[test]
    fn concurrent_ensures_extract_once() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();
        let name = "alpha";
        let archive = format!("{name}.gz");
        let sha256 = cache_archive(root_path, &archive, b"body");
        let dataset = Dataset {
            name,
            target: Target::File,
            archive: &archive,
            url: "https://example.org/archive.gz",
            sha256: &sha256,
            max_bytes: 1 << 20,
        };
        let target = root_path.join(name);
        let extractions = std::sync::atomic::AtomicUsize::new(0);
        let count = &extractions;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|scope| {
            let first_target = target.clone();
            scope.spawn(move || {
                ensure(root_path, &dataset, refusal(), move |a| {
                    count.fetch_add(1, Ordering::SeqCst);
                    entered_tx.send(()).unwrap();
                    // Hold the lock while the other caller arrives, so the
                    // two really overlap.
                    release_rx.recv().unwrap();
                    copy_target(a, &first_target)
                })
                .unwrap();
            });
            entered_rx.recv().unwrap();
            let second_target = target.clone();
            let second = scope.spawn(move || {
                ensure(root_path, &dataset, refusal(), move |a| {
                    copy_target(a, &second_target)
                })
                .unwrap()
            });
            release_tx.send(()).unwrap();
            second.join().unwrap();
        });
        assert_eq!(extractions.load(Ordering::SeqCst), 1);
        assert!(root_path.join(format!(".{name}.ok")).is_file());
    }

    /// An unreadable stamp is an error, never "not stamped": the ensure
    /// must not extract over a data set its stamp cannot vouch for.
    #[test]
    fn an_unreadable_stamp_propagates() {
        let root = tempfile::tempdir().unwrap();
        let name = "alpha";
        let archive = format!("{name}.gz");
        let sha256 = cache_archive(root.path(), &archive, b"body");
        let dataset = Dataset {
            name,
            target: Target::File,
            archive: &archive,
            url: "https://example.org/archive.gz",
            sha256: &sha256,
            max_bytes: 1 << 20,
        };
        let target = root.path().join(name);
        std::fs::write(&target, b"stale").unwrap();
        // A directory where the stamp should be: reading it fails with a
        // kind other than NotFound.
        std::fs::create_dir(root.path().join(format!(".{name}.ok"))).unwrap();
        let err = ensure(root.path(), &dataset, refusal(), |a| {
            copy_target(a, &target)
        })
        .unwrap_err();
        assert!(
            matches!(err, TestError::Kit(FetchError::Io { .. })),
            "{err:?}"
        );
        assert!(
            err.to_string().contains(&format!(".{name}.ok")),
            "the refusal names the stamp: {err}"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"stale",
            "the target is untouched"
        );
    }
}

//! Fetch the LDBC SNB `CsvBasic-StringDateFormatter` data set (ADR-0243):
//! download from datasets.ldbcouncil.org, fail closed against the pinned
//! sha256 in `workloads/graph-benchmark-ldbc/data.sha256`, and extract with pure-Rust zstd
//! and tar. Nothing is vendored. The ukrailway lane's data set (ADR-0479's
//! fourth workload) is fetched here too, at file granularity: no archive
//! and no stamp — every CSV is named with its sha256 in
//! `workloads/ukrailway/data.sha256`, downloaded atomically, and
//! re-verified on every run.
//!
//! What every data set's fetch shares — the pin-table parse, the streamed
//! sha256, the verified download and the lock-and-stamp ensure — is
//! `relate_test_support::fetch`'s (ADR-0243). This module owns the
//! workload's own pieces: its URLs and pins, its cache layout, its tar/zstd
//! extraction, and the text of its refusals.
//!
//! The cache layout matches the upstream benchmark's own fetch script, so
//! a data set that script already fetched under a `.cache/ldbc/` root is
//! reused by pointing `RELATE_QUERY_LATENCY_DATA` at it:
//!
//! ```text
//! <data>/archives/<name>.tar.zst   the archive as downloaded
//! <data>/<name>/                   the extracted data set
//! <data>/.<name>.ok                stamp holding the archive's sha256
//! <data>/.<name>.lock              per-dataset ensure lock (exclusive)
//! ```

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

use relate_test_support::fetch;

use crate::workload::{pinned_sha256, ScaleFactor};
use crate::{read, work_root, Result};

/// Where the archives come from.
pub const URL_BASE: &str = "https://datasets.ldbcouncil.org/snb-interactive-v1";

/// The pinned dogofbrian/ukrailway commit the CSVs are fetched at — the
/// ONE spelling of the pin: the URL base ([`ukrailway_url_base`])
/// formats from it and the ukrailway lane's graph line names its short
/// form, so a new pin is one edit, never a second copy to keep in step.
pub(crate) const PINNED_COMMIT: &str = "ce82229e547f46ef4dd0c680f42756e17bde8922";

/// The largest archive body the download accepts: the sf1 archive is the
/// biggest the suite fetches, and this ceiling is a runaway bound, not a
/// size the data reaches.
const MAX_ARCHIVE_BYTES: u64 = 1 << 30;

/// The largest single ukrailway CSV the download accepts: the pin table
/// names eight tables, so the ceiling is a runaway bound, not a size the
/// data reaches.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// Where the ukrailway CSVs come from (ADR-0479's deferred fourth lane):
/// the `data/` directory of dogofbrian/ukrailway at [`PINNED_COMMIT`] —
/// the workload's `data.sha256` names every file and its sha256, so the
/// pin and the checksums are one committed table.
fn ukrailway_url_base() -> String {
    format!("https://raw.githubusercontent.com/dogofbrian/ukrailway/{PINNED_COMMIT}/data")
}

/// The data cache: `RELATE_QUERY_LATENCY_DATA`, else
/// `<work root>/data`.
pub fn data_root() -> PathBuf {
    std::env::var_os("RELATE_QUERY_LATENCY_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| work_root().join("data"))
}

/// Make sure `sf`'s data set is extracted under `root` and return its
/// directory. With `download` false a missing or unverified archive is an
/// error instead of a download.
pub fn ensure_dataset(root: &Path, sf: ScaleFactor, download: bool) -> Result<PathBuf> {
    let name = sf.dataset_name();
    let archive = format!("{name}.tar.zst");
    let url = format!("{URL_BASE}/{archive}");
    let sha256 = pinned_sha256(sf)?;
    let dataset = fetch::Dataset {
        name: &name,
        target: fetch::Target::Directory,
        archive: &archive,
        url: &url,
        sha256: &sha256,
        max_bytes: MAX_ARCHIVE_BYTES,
    };
    // The offline refusal names the cache root and the command that fetches a
    // verified copy, which only this workload knows: it rides the policy.
    let missing = if download {
        fetch::MissingArchive::Download
    } else {
        fetch::MissingArchive::Refuse(format!(
            "{name} is not cached under {} (run `query-latency fetch --sf {sf}`)",
            root.display()
        ))
    };
    fetch::ensure(root, &dataset, missing, |archive| {
        eprintln!("query-latency: extracting {name}");
        extract_tar_zst(archive, root).and_then(|()| {
            if root.join(&name).is_dir() {
                Ok(())
            } else {
                Err(format!("{} did not create {name}/", archive.display()))
            }
        })
    })
}

/// Make sure the ukrailway data set is under `root` and return its
/// directory: every file the pin table names, fetched on demand and
/// verified against its sha256. Unlike the graph-benchmark-ldbc archives there is nothing
/// to extract, so there is no stamp either — each file is downloaded
/// atomically (verified before its rename into place) and re-verified on
/// every run: 18 MB of hashing, not 241 MB of archive. A mismatched file
/// with `download` false is an error naming both hashes; with `download`
/// true it is fetched again (the graph-benchmark-ldbc archive's discard-and-redownload
/// rule, at file granularity).
pub(crate) fn ensure_ukrailway(root: &Path, pin_table: &Path, download: bool) -> Result<PathBuf> {
    let pins = fetch::parse_pins(&read(pin_table)?, pin_table)?;
    let dir = root.join("ukrailway");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    for pin in &pins {
        let dest = dir.join(&pin.name);
        if dest.is_file() {
            let got = fetch::sha256_file(&dest)?;
            if got == pin.sha256 {
                continue;
            }
            if !download {
                return Err(format!(
                    "{}: sha256 {got} does not match the pinned {}; delete the file or re-run \
                     with downloads enabled",
                    dest.display(),
                    pin.sha256
                ));
            }
        } else if !download {
            return Err(format!(
                "{} is not cached under {} (run `query-latency corpus ukrailway` to \
                 fetch it)",
                pin.name,
                root.display()
            ));
        }
        fetch::download_verified(
            &format!("{}/{}", ukrailway_url_base(), pin.name),
            &dest,
            &pin.sha256,
            MAX_FILE_BYTES,
        )?;
    }
    Ok(dir)
}

/// Extract a `.tar.zst` archive under `root`. The archive is untrusted
/// input: only regular files and directories are extracted, and an entry
/// whose path is absolute or climbs out of `root` (`..`) fails the
/// extraction.
pub fn extract_tar_zst(archive: &Path, root: &Path) -> Result<()> {
    let f = File::open(archive).map_err(|e| format!("opening {}: {e}", archive.display()))?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(BufReader::new(f))
        .map_err(|e| format!("{}: not a zstd stream: {e}", archive.display()))?;
    extract_tar(decoder, root).map_err(|e| format!("{}: {e}", archive.display()))
}

/// Extract a tar stream under `root` with the restrictions of
/// [`extract_tar_zst`].
pub fn extract_tar(reader: impl Read, root: &Path) -> Result<()> {
    std::fs::create_dir_all(root).map_err(|e| format!("creating {}: {e}", root.display()))?;
    let mut tar = tar::Archive::new(reader);
    let entries = tar.entries().map_err(|e| format!("reading tar: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("reading tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("tar entry path: {e}"))?
            .into_owned();
        if !path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "tar entry {} leaves the extraction root",
                path.display()
            ));
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(format!(
                "tar entry {} is neither a file nor a directory ({kind:?})",
                path.display()
            ));
        }
        let inside = entry
            .unpack_in(root)
            .map_err(|e| format!("extracting {}: {e}", path.display()))?;
        if !inside {
            return Err(format!(
                "tar entry {} leaves the extraction root",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_with(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, kind, body) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*kind);
            h.set_size(body.len() as u64);
            h.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
            // `set_path` refuses `..`; write the raw name so the test can
            // build the hostile archive the extractor must reject.
            let name = &mut h.as_old_mut().name;
            name[..path.len()].copy_from_slice(path.as_bytes());
            h.set_cksum();
            b.append(&h, *body).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn extracts_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let tar = tar_with(&[
            ("ds/", tar::EntryType::Directory, b""),
            ("ds/static/", tar::EntryType::Directory, b""),
            (
                "ds/static/tag_0_0.csv",
                tar::EntryType::Regular,
                b"id|name\n1|a\n",
            ),
        ]);
        extract_tar(&tar[..], dir.path()).unwrap();
        let got = std::fs::read_to_string(dir.path().join("ds/static/tag_0_0.csv")).unwrap();
        assert_eq!(got, "id|name\n1|a\n");
    }

    #[test]
    fn refuses_paths_that_climb_out() {
        let dir = tempfile::tempdir().unwrap();
        let tar = tar_with(&[("../evil.csv", tar::EntryType::Regular, b"x")]);
        let err = extract_tar(&tar[..], &dir.path().join("root")).unwrap_err();
        assert!(err.contains("leaves the extraction root"), "{err}");
        assert!(!dir.path().join("evil.csv").exists());
    }

    #[test]
    fn refuses_links() {
        let dir = tempfile::tempdir().unwrap();
        let tar = tar_with(&[("link", tar::EntryType::Symlink, b"")]);
        let err = extract_tar(&tar[..], dir.path()).unwrap_err();
        assert!(err.contains("neither a file nor a directory"), "{err}");
    }

    #[test]
    fn a_stamped_dataset_needs_no_archive_or_network() {
        let root = tempfile::tempdir().unwrap();
        let sf = ScaleFactor::Sf0_1;
        std::fs::create_dir_all(root.path().join(sf.dataset_name())).unwrap();
        std::fs::write(
            root.path().join(format!(".{}.ok", sf.dataset_name())),
            pinned_sha256(sf).unwrap(),
        )
        .unwrap();
        let dir = ensure_dataset(root.path(), sf, false).unwrap();
        assert_eq!(dir, root.path().join(sf.dataset_name()));
    }

    #[test]
    fn a_wrong_stamp_without_network_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let sf = ScaleFactor::Sf0_1;
        std::fs::create_dir_all(root.path().join(sf.dataset_name())).unwrap();
        std::fs::write(
            root.path().join(format!(".{}.ok", sf.dataset_name())),
            "0000",
        )
        .unwrap();
        let err = ensure_dataset(root.path(), sf, false).unwrap_err();
        assert!(err.contains("is not cached"), "{err}");
    }

    /// The pin and its URL base are one fact: the base spells the
    /// pinned commit exactly once, through [`PINNED_COMMIT`], and the
    /// pin is a full commit hash — the shape every short form (the
    /// ukrailway lane's graph line among them) is derived from.
    #[test]
    fn the_ukrailway_url_base_spells_the_pinned_commit() {
        assert_eq!(PINNED_COMMIT.len(), 40, "a full commit hash");
        assert_eq!(
            ukrailway_url_base(),
            "https://raw.githubusercontent.com/dogofbrian/ukrailway/\
             ce82229e547f46ef4dd0c680f42756e17bde8922/data"
        );
    }

    /// The committed pin table parses, and a fully cached, verified data
    /// directory is the ensure's fast path with no network — the same
    /// cached-without-network shape the graph-benchmark-ldbc data set's stamp gives.
    /// (Fabricated files hashed against their own pins, so the test
    /// never downloads.)
    #[test]
    fn a_verified_ukrailway_cache_needs_no_network() {
        let table = crate::corpus::Workload::Ukrailway.dir().join("data.sha256");
        let pins = fetch::parse_pins(&read(&table).unwrap(), &table).unwrap();
        assert_eq!(pins.len(), 8, "the eight dogofbrian/ukrailway CSVs");
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ukrailway");
        std::fs::create_dir_all(&dir).unwrap();
        for pin in &pins {
            let body = format!("body of {}", pin.name);
            std::fs::write(dir.join(&pin.name), body.as_bytes()).unwrap();
        }
        // The fabricated bodies match no pin: a mismatch offline is an
        // error naming both hashes, never a silent run.
        let err = ensure_ukrailway(root.path(), &table, false).unwrap_err();
        assert!(err.contains("does not match the pinned"), "{err}");
        // Pin the table to what is actually on disk and the ensure passes.
        let table2 = root.path().join("pins");
        let lines: String = pins
            .iter()
            .map(|pin| {
                let sha256 = fetch::sha256_file(&dir.join(&pin.name)).unwrap();
                format!("{sha256}  {}\n", pin.name)
            })
            .collect();
        std::fs::write(&table2, lines).unwrap();
        let got = ensure_ukrailway(root.path(), &table2, false).unwrap();
        assert_eq!(got, dir);
        // A missing file offline names it and the cache root.
        std::fs::remove_file(dir.join(&pins[0].name)).unwrap();
        let err = ensure_ukrailway(root.path(), &table2, false).unwrap_err();
        assert!(
            err.contains(&format!("{} is not cached", pins[0].name)),
            "{err}"
        );
    }
}

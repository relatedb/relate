//! Reading a checkout's history for `git-to-graph`
//! (ADR-0279, ADR-0376).
//!
//! One `std::process::Command` runs git with no shell and a null stdin, using
//! the argv pinned in ADR-0376 and windowed by ADR-0597's `--until`, and
//! [`parse_log`] turns its `-z` output into
//! a typed commit list. The parser is a pure function over the raw stdout bytes:
//! a malformed record is an error naming that record, never a dropped path or
//! a fabricated default.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use relate_core::temporal::{parse_iso, Temporal};
use relate_wire::read_cap::{drain_stderr, read_capped, CappedRead};

use crate::ImportError;

/// The record separator `%x1e` starts every commit record.
const RECORD_SEP: u8 = 0x1e;

/// The field separator `%x1f` between the five header fields.
const FIELD_SEP: u8 = 0x1f;

/// One commit read from `git log`, before paths are mapped onto the work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommit {
    /// Full `%H` commit id.
    pub sha: String,
    /// Mailmap-resolved author name `%aN`.
    pub author_name: String,
    /// Mailmap-resolved author email `%aE`.
    pub author_email: String,
    /// Strict ISO 8601 author date `%aI`.
    pub authored_at: String,
    /// Commit subject `%s`.
    pub subject: String,
    /// `--relative` paths the commit touched, in git's order.
    pub paths: Vec<String>,
}

impl GitCommit {
    /// The author date `%aI` as seconds since the Unix epoch with its offset
    /// applied, so two commits' author instants compare (ADR-0376).
    ///
    /// The value is parsed by the engine's temporal parser (ADR-0021); this
    /// module only enforces the `%aI` shape that parser also accepts looser
    /// ISO spellings of. A value outside that shape, or one the engine
    /// rejects, is an error naming the value, never a fabricated instant.
    pub fn authored_instant(&self) -> Result<i64, String> {
        let value = self.authored_at.as_str();
        let malformed = || format!("git returned an invalid author date: {value}");
        if !has_git_author_date_shape(value) {
            return Err(malformed());
        }
        match parse_iso("datetime", value) {
            // `%aI` can only be a zoned datetime; the shape guard above
            // already rules out every other `Temporal` variant.
            Ok(Temporal::DateTime { utc, .. }) => Ok(utc.and_utc().timestamp()),
            Ok(_) | Err(_) => Err(malformed()),
        }
    }
}

/// Whether `value` is exactly the `%aI` spelling: `YYYY-MM-DDTHH:MM:SS`
/// followed by `Z` or `±HH:MM`.
///
/// Only the shape is checked here — the fixed width, the literal separators,
/// the zone form, and no leap second — because calendar and clock ranges are
/// [`parse_iso`]'s. The engine's parser also accepts further ISO forms
/// (date-only, minute precision, space separators) that git never emits, so
/// the width check keeps those out. Two values the parser would accept stay
/// malformed here: `:60`, which it normalizes to `:59`, and a zone hour past
/// 23, which its offset parser reads as a larger offset (`+24:00` becomes a
/// whole day, `+00:60` an hour).
fn has_git_author_date_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 && bytes.len() != 25 {
        return false;
    }
    let digits = |start: usize| bytes[start..start + 2].iter().all(u8::is_ascii_digit);
    // A fixed-width two-digit field compares like a number against `max`.
    let at_most = |start: usize, max: &[u8]| bytes[start..start + 2] <= *max;
    bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && digits(5)
        && bytes[7] == b'-'
        && digits(8)
        && bytes[10] == b'T'
        && digits(11)
        && bytes[13] == b':'
        && digits(14)
        && bytes[16] == b':'
        && digits(17)
        && bytes[17..19] != [b'6', b'0']
        && match bytes[19] {
            b'Z' => bytes.len() == 20,
            b'+' | b'-' => {
                bytes.len() == 25
                    && digits(20)
                    && at_most(20, b"23")
                    && bytes[22] == b':'
                    && digits(23)
                    && at_most(23, b"59")
            }
            _ => false,
        }
}

/// Read `HEAD`'s history of `dir` with the ADR-0376 invocation.
///
/// The [`Opts`] window becomes `--max-count=N`, `--since=DATE` and
/// `--until=DATE` (ADR-0597); each bound is omitted when `None`. The
/// reader takes the options whole — the fs-to-graph `walk(root, &Opts)`
/// house shape — so the three bounds have one struct and a `since`/
/// `until` swap cannot compile. Merge commits are excluded. The child's
/// stdout is read through a cap of `log_limit` bytes (ADR-0377): the byte
/// that would exceed it fails the read and terminates the child, and no
/// partial output is parsed. A missing git, a directory git refuses, or
/// any non-zero exit is an `Err`; a non-zero exit carries git's trimmed
/// stderr.
pub fn read_history(
    dir: &Path,
    opts: &crate::Opts,
    log_limit: u64,
) -> Result<Vec<GitCommit>, ImportError> {
    let args = log_args(dir, opts);
    let mut command = Command::new("git");
    command.args(&args);
    let stdout = run_command(&mut command, log_limit)?;
    parse_log(&stdout).map_err(ImportError::Failed)
}

/// The exact `git` argv of ADR-0376, program name excluded, with the
/// optional window elements ADR-0376 and ADR-0597 append.
fn log_args(dir: &Path, opts: &crate::Opts) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "-C".into(),
        dir.as_os_str().to_owned(),
        "-c".into(),
        "log.showSignature=false".into(),
        "--no-pager".into(),
        "log".into(),
        "--no-merges".into(),
        "--no-color".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--relative".into(),
        "--name-only".into(),
        "-z".into(),
        "--format=%x1e%H%x1f%aN%x1f%aE%x1f%aI%x1f%s".into(),
    ];
    if let Some(max_commits) = opts.max_commits {
        args.push(format!("--max-count={max_commits}").into());
    }
    if let Some(since) = opts.since.as_deref() {
        args.push(format!("--since={since}").into());
    }
    if let Some(until) = opts.until.as_deref() {
        args.push(format!("--until={until}").into());
    }
    args.push("--".into());
    args.push(".".into());
    args
}

/// Run `command` with a null stdin, mapping spawn and exit failures to the
/// child's `Error:` texts.
///
/// The child's stdout is read through the wire kit's shared [`read_capped`]
/// guard rather than buffered by `Command::output`, so `log_limit` bounds
/// memory while the history is still arriving (ADR-0377). On the byte past
/// the cap the child is terminated and reaped before
/// [`ImportError::LogTooLarge`] returns.
fn run_command(command: &mut Command, log_limit: u64) -> Result<Vec<u8>, ImportError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ImportError::GitNotInstalled
            } else {
                ImportError::Failed(format!("failed to run git: {e}"))
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .expect("a child spawned with piped stdout has a stdout handle");
    let stderr = child
        .stderr
        .take()
        .expect("a child spawned with piped stderr has a stderr handle");
    // Drain stderr on its own thread: git must never block on a full stderr
    // pipe while this thread is waiting for more stdout. The drain's bound
    // and mechanism are the wire kit's one shared helper (ADR-0430 lifted
    // it beside the bounded reads so the extension wire's child reuses it).
    let stderr_reader = std::thread::spawn(move || drain_stderr(stderr, "git"));
    let stdout = match read_capped(stdout, log_limit) {
        Ok(CappedRead::Complete(stdout)) => Ok(stdout),
        Ok(CappedRead::Exceeded) => {
            let _ = child.kill();
            Err(ImportError::LogTooLarge { limit: log_limit })
        }
        Err(error) => {
            let _ = child.kill();
            Err(ImportError::Failed(format!(
                "failed to read git log output: {error}"
            )))
        }
    };
    // Both handles are reaped on every path below, including a failed
    // `wait`, so the drain thread can never outlive this call.
    let status = child.wait();
    let stderr = stderr_reader.join();
    let stdout = stdout?;
    let status = status.map_err(|e| ImportError::Failed(format!("failed to run git: {e}")))?;
    let stderr = stderr
        .map_err(|_| ImportError::Failed("failed to collect git's stderr".to_string()))?
        .map_err(|e| ImportError::Failed(format!("failed to run git: {e}")))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return Err(ImportError::Failed(format!("git exited with {status}")));
        }
        return Err(ImportError::Failed(stderr.to_string()));
    }
    Ok(stdout)
}

/// Parse the `-z` stdout of the ADR-0376 invocation.
///
/// Records are separated by `0x1e`, header fields by `0x1f`, and paths by
/// NUL. A record is
/// `\x1e<sha>\x1f<name>\x1f<email>\x1f<date>\x1f<subject>\0` — with paths
/// following a newline, each NUL-terminated — and a commit with no files has
/// no body at all. Empty stdout is an empty history; anything else that does
/// not match is an error naming the record.
pub fn parse_log(stdout: &[u8]) -> Result<Vec<GitCommit>, String> {
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    if stdout[0] != RECORD_SEP {
        return Err("malformed git log output: missing record separator".to_string());
    }
    let mut records: Vec<&[u8]> = stdout.split(|byte| *byte == RECORD_SEP).collect();
    // The leading separator yields one empty slice; a record boundary also
    // leaves the next record directly after the previous record's final NUL,
    // so the only other empty slice is the artifact after a trailing
    // separator.
    if records.first().is_some_and(|record| record.is_empty()) {
        records.remove(0);
    }
    if records.last().is_some_and(|record| record.is_empty()) {
        records.pop();
    }
    records
        .iter()
        .enumerate()
        .map(|(index, record)| parse_record(index + 1, record))
        .collect()
}

/// Parse one record, naming its one-based ordinal in every error.
fn parse_record(ordinal: usize, record: &[u8]) -> Result<GitCommit, String> {
    let header_end = record.iter().position(|byte| *byte == 0).ok_or_else(|| {
        format!("malformed git log record {ordinal}: header is not NUL-terminated")
    })?;
    let fields: Vec<&[u8]> = record[..header_end]
        .split(|byte| *byte == FIELD_SEP)
        .collect();
    if fields.len() != 5 {
        return Err(format!(
            "malformed git log record {ordinal}: expected 5 header fields, found {}",
            fields.len()
        ));
    }
    let sha = header_field(ordinal, "sha", fields[0])?;
    let author_name = header_field(ordinal, "author name", fields[1])?;
    let author_email = header_field(ordinal, "author email", fields[2])?;
    let authored_at = header_field(ordinal, "author date", fields[3])?;
    let subject = header_field(ordinal, "subject", fields[4])?;
    if sha.is_empty() {
        return Err(format!("malformed git log record {ordinal}: empty sha"));
    }
    let paths = parse_paths(ordinal, &record[header_end + 1..])?;
    Ok(GitCommit {
        sha,
        author_name,
        author_email,
        authored_at,
        subject,
        paths,
    })
}

/// Decode one header field, naming it and the record on invalid UTF-8.
fn header_field(ordinal: usize, name: &str, bytes: &[u8]) -> Result<String, String> {
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| format!("malformed git log record {ordinal}: {name} is not UTF-8"))
}

/// Parse the body after the header's NUL: empty for a fileless commit,
/// otherwise a leading newline followed by NUL-terminated paths.
fn parse_paths(ordinal: usize, body: &[u8]) -> Result<Vec<String>, String> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let Some(paths) = body.strip_prefix(b"\n") else {
        return Err(format!(
            "malformed git log record {ordinal}: path list does not start with a newline"
        ));
    };
    let Some(paths) = paths.strip_suffix(b"\0") else {
        return Err(format!(
            "malformed git log record {ordinal}: path list is not NUL-terminated"
        ));
    };
    paths
        .split(|byte| *byte == 0)
        .enumerate()
        .map(|(index, path)| {
            if path.is_empty() {
                return Err(format!(
                    "malformed git log record {ordinal}: path {} is empty",
                    index + 1
                ));
            }
            // `File.path` renders with `to_string_lossy` (ADR-0376), so the
            // parser applies the same rendering before the path reaches the
            // graph.
            Ok(String::from_utf8_lossy(path).into_owned())
        })
        .collect()
}

// The reader's tests live in `tests.rs` (the dir-module pattern the
// importer's own tests use, LABS-421) so this file stays under the repo's
// 1k-line budget.
#[cfg(test)]
mod tests;

//! `relate info --database PATH [--json]` (ADR-0165, ADR-0200): print store metadata
//! gathered by a cold read (`relate_storage::store_info_uri`) — no open,
//! no lock, no WAL replay — as a human key/value table or one JSON object.
//! Both shapes render the shared [`InfoReport`] projection; the layout ×
//! snapshot split, the unavailable policy, and the label set live there.

use crate::parse_error::ParseError;
use crate::store_info_render::InfoReport;

/// Parsed `info` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoCmd {
    /// The store to inspect: a native directory or a `.rl` bundle.
    pub database: String,
    /// `--json`: emit one JSON object instead of the key/value table.
    pub json: bool,
}

/// Parse the argument tail after the `info` verb. A usage error is an `Err`
/// string the caller prints as the `info` synopsis line plus a
/// `Run 'relate info --help' for more information.` hint, then exits 2.
pub fn parse_info_args(rest: &[String]) -> Result<InfoCmd, ParseError> {
    let mut json = false;
    let mut database = None;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => crate::parse_error::flag_once("info", "--json", &mut json)?,
            "-d" | "--database" => {
                let value = crate::parse_error::database_flag(&mut args)?;
                crate::parse_error::set_once("info", &mut database, "--database", value.clone())?;
            }
            other => {
                return Err(ParseError::unexpected("info", other));
            }
        }
    }
    let database = database.ok_or_else(|| "info requires --database".to_string())?;
    // The canonical in-memory classification (ADR-0163 §5): `:memory:` and
    // the empty URI both name the in-memory store, which has no on-disk
    // layout for a cold info read.
    if relate_storage::is_memory_uri(&database) {
        return Err(
            "info cannot use :memory: or an empty --database path (info reads an on-disk store)"
                .into(),
        );
    }
    Ok(InfoCmd { database, json })
}

/// Run `info`. Returns the process exit code (0 success, 1 runtime). The
/// database string goes to `store_info_uri` verbatim — it owns the URI
/// classification (`file://` strip, remote-scheme rejection), the same
/// rules a session open applies. The store path is user-supplied, so every
/// interpolated string in the output passes through the shared sanitizer
/// first (see `print_error`).
pub fn run(cmd: InfoCmd) -> i32 {
    let info = match relate_storage::store_info_uri(&cmd.database) {
        Ok(info) => info,
        Err(err) => {
            crate::print_error(format_args!("Failed to read store info: {err}"));
            return 1;
        }
    };
    let report = InfoReport::from_store_info(&info);
    if cmd.json {
        // `json!` builds plain values, so serialization cannot fail.
        println!("{}", report.to_json());
    } else {
        print_table(&report);
    }
    0
}

/// The human key/value table: the shared store block, then the
/// checkpoint-time counts (labeled with the checkpoint LSN so staleness is
/// explicit, ADR-0165 Decision 3) and the index/constraint catalog.
fn print_table(report: &InfoReport) {
    for (key, value) in report.store_lines() {
        println!("{key}: {value}");
    }
    for line in report.catalog_lines() {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn parse_requires_database() {
        assert!(parse_info_args(&args(&[]))
            .unwrap_err()
            .to_string()
            .contains("--database"));
        assert!(parse_info_args(&args(&["--json"]))
            .unwrap_err()
            .to_string()
            .contains("--database"));
    }

    #[test]
    fn parse_rejects_duplicated_json_flag() {
        assert!(
            parse_info_args(&args(&["--database", "./db", "--json", "--json"]))
                .unwrap_err()
                .to_string()
                .contains("only once")
        );
    }

    /// `--database` parks through the shared once-only policy (LABS-510):
    /// a repeat is a usage error, not a silent last-wins — the same rule
    /// `push` and `serve` apply to the same flag.
    #[test]
    fn parse_rejects_duplicated_database_flag() {
        assert!(parse_info_args(&args(&["--database", "./a", "-d", "./b"]))
            .unwrap_err()
            .to_string()
            .contains("info --database may be given only once"));
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert_eq!(
            parse_info_args(&args(&["--bogus", "--database", "./db"])).unwrap_err(),
            ParseError::unexpected("info", "--bogus")
        );
        assert_eq!(
            parse_info_args(&args(&["--database", "./db", "--to", "a.rl"])).unwrap_err(),
            ParseError::unexpected("info", "--to")
        );
    }

    #[test]
    fn parse_rejects_positional_database() {
        assert_eq!(
            parse_info_args(&args(&["./a"])).unwrap_err(),
            ParseError::unexpected("info", "./a")
        );
    }

    #[test]
    fn parse_rejects_memory() {
        assert!(parse_info_args(&args(&["--database", ":memory:"]))
            .unwrap_err()
            .to_string()
            .contains(":memory:"));
        assert!(
            parse_info_args(&args(&["--database", ":memory:", "--json"]))
                .unwrap_err()
                .to_string()
                .contains(":memory:")
        );
        // The canonical `is_memory_uri` also classifies the empty URI as
        // in-memory; it is rejected at parse time with the same contract.
        assert!(parse_info_args(&args(&["--database", ""]))
            .unwrap_err()
            .to_string()
            .contains(":memory:"));
    }

    #[test]
    fn parse_accepts_database_and_json_in_either_order() {
        for flag_order in [
            args(&["--database", "./db", "--json"]),
            args(&["--json", "--database", "./db"]),
        ] {
            let cmd = parse_info_args(&flag_order).unwrap();
            assert_eq!(
                cmd,
                InfoCmd {
                    database: "./db".to_string(),
                    json: true
                }
            );
        }
        let short = parse_info_args(&args(&["-d", "./db", "--json"])).unwrap();
        assert_eq!(short.database, "./db");
        assert!(short.json);
        let cmd = parse_info_args(&args(&["--database", "./db"])).unwrap();
        assert_eq!(
            cmd,
            InfoCmd {
                database: "./db".to_string(),
                json: false
            }
        );
    }
}

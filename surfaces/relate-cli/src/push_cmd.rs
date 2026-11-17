//! `relate push --to CREDS --database PATH [scope] [policies]` (ADR-0401):
//! `pull`'s mirror — push a scoped subgraph of the local store into a
//! Neo4j/Aura remote over the same HTTP Query API v2 client and the same
//! credentials file. Constraint-keyed MERGE, batched UNWIND, additive by
//! default; `--dry-run` prints the plan without touching the remote.

use std::collections::BTreeMap;

use crate::parse_error::ParseError;
use relate::Database;
use relate_formatter::{text_grid_lines, ColumnAlign};
use relate_load::{
    parse_key_spec, parse_updated_at_spec, push as push_load, EntityReport, PolicySpec, PushOpts,
    PushOutcome, PushReport, PushScope, RelMode, UnkeyedAction, DEFAULT_BATCH_SIZE,
};

/// Parsed `push` command (flags + the required database path).
#[derive(Debug, Clone)]
pub struct PushCmd {
    /// `--to`: a credentials env file path, or a Query API URI (the
    /// username/password then come from the process environment).
    pub to: String,
    pub database: String,
    /// The resolved selector (one of `--query` / `--labels` / `--types`,
    /// none = the whole store) — decided once where the exclusivity check
    /// lives, never re-derived at the call site.
    pub scope: PushScope,
    pub batch_size: usize,
    pub nodes_update: PolicySpec,
    pub rels_update: PolicySpec,
    pub updated_at: Vec<String>,
    pub rel_mode: RelMode,
    /// `--merge-key LABEL:PROP[,PROP]` overrides, keyed by label.
    pub merge_keys: BTreeMap<String, Vec<String>>,
    /// `--rel-key TYPE:PROP[,PROP]` overrides, keyed by relationship type.
    pub rel_keys: BTreeMap<String, Vec<String>>,
    pub unkeyed_labels: UnkeyedAction,
    pub dry_run: bool,
    pub json: bool,
    pub allow_plaintext: bool,
}

/// Parse the argument tail after the `push` verb. A usage error is an `Err`
/// string for `flags::usage_error` to print as the `push` synopsis line plus
/// a `Run 'relate push --help' for more information.` hint, then exit 2.
pub fn parse_push_args(rest: &[String]) -> Result<PushCmd, ParseError> {
    let mut to = None;
    let mut database = None;
    let mut query = None;
    let mut labels = Vec::new();
    let mut types = Vec::new();
    let mut batch_size = None;
    let mut nodes_update = None;
    let mut rels_update = None;
    let mut updated_at = Vec::new();
    let mut rel_mode = None;
    let mut merge_keys = BTreeMap::new();
    let mut rel_keys = BTreeMap::new();
    let mut unkeyed_labels = None;
    let mut dry_run = false;
    let mut json = false;
    let mut allow_plaintext = false;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--to" => {
                let value =
                    crate::parse_error::flag_value(&mut args, "--to", "a credentials file or URI")?;
                crate::parse_error::set_once("push", &mut to, "--to", value.clone())?;
            }
            "-d" | "--database" => {
                let value = crate::parse_error::database_flag(&mut args)?;
                crate::parse_error::set_once("push", &mut database, "--database", value.clone())?;
            }
            "--query" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--query",
                    "a Cypher statement or file path",
                )?;
                crate::parse_error::set_once("push", &mut query, "--query", value.clone())?;
            }
            "--labels" => {
                let value = crate::parse_error::flag_value(&mut args, "--labels", "a label list")?;
                append_list(&mut labels, value, "--labels")?;
            }
            "--types" => {
                let value = crate::parse_error::flag_value(&mut args, "--types", "a type list")?;
                append_list(&mut types, value, "--types")?;
            }
            "--batch-size" => {
                let value = crate::parse_error::flag_value(&mut args, "--batch-size", "a number")?;
                let size = parse_batch_size(value)?;
                crate::parse_error::set_once("push", &mut batch_size, "--batch-size", size)?;
            }
            "--nodes-update" => {
                let value =
                    crate::parse_error::flag_value(&mut args, "--nodes-update", "a policy")?;
                let spec = PolicySpec::parse(value).map_err(ParseError::from)?;
                crate::parse_error::set_once("push", &mut nodes_update, "--nodes-update", spec)?;
            }
            "--rels-update" => {
                let value = crate::parse_error::flag_value(&mut args, "--rels-update", "a policy")?;
                let spec = PolicySpec::parse(value).map_err(ParseError::from)?;
                crate::parse_error::set_once("push", &mut rels_update, "--rels-update", spec)?;
            }
            "--updated-at" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--updated-at",
                    "comma-separated property names",
                )?;
                let mut names = parse_updated_at_spec(value).map_err(ParseError::from)?;
                for name in &names {
                    if updated_at.contains(name) {
                        return Err(format!("--updated-at names {name:?} twice").into());
                    }
                }
                updated_at.append(&mut names);
            }
            "--rel-mode" => {
                let value = crate::parse_error::flag_value(&mut args, "--rel-mode", "a mode")?;
                let mode = RelMode::parse(value).map_err(ParseError::from)?;
                crate::parse_error::set_once("push", &mut rel_mode, "--rel-mode", mode)?;
            }
            "--merge-key" => {
                let value =
                    crate::parse_error::flag_value(&mut args, "--merge-key", "LABEL:PROP[,PROP]")?;
                let (label, props) =
                    parse_key_spec("--merge-key", value).map_err(ParseError::from)?;
                if merge_keys.insert(label.clone(), props).is_some() {
                    return Err(format!("--merge-key names {label:?} twice").into());
                }
            }
            "--rel-key" => {
                let value =
                    crate::parse_error::flag_value(&mut args, "--rel-key", "TYPE:PROP[,PROP]")?;
                let (rel_type, props) =
                    parse_key_spec("--rel-key", value).map_err(ParseError::from)?;
                if rel_keys.insert(rel_type.clone(), props).is_some() {
                    return Err(format!("--rel-key names {rel_type:?} twice").into());
                }
            }
            "--unkeyed-labels" => {
                let value =
                    crate::parse_error::flag_value(&mut args, "--unkeyed-labels", "an action")?;
                let action = UnkeyedAction::parse(value).map_err(ParseError::from)?;
                crate::parse_error::set_once(
                    "push",
                    &mut unkeyed_labels,
                    "--unkeyed-labels",
                    action,
                )?;
            }
            "--dry-run" => {
                crate::parse_error::flag_once("push", "--dry-run", &mut dry_run)?;
            }
            "--json" => {
                crate::parse_error::flag_once("push", "--json", &mut json)?;
            }
            "--allow-plaintext" => {
                crate::parse_error::flag_once("push", "--allow-plaintext", &mut allow_plaintext)?;
            }
            other => {
                return Err(ParseError::unexpected("push", other));
            }
        }
    }
    let to = to.ok_or_else(|| "push requires --to".to_string())?;
    let database = database.ok_or_else(|| "push requires --database".to_string())?;
    if relate_storage::is_memory_uri(&database) {
        return Err("push cannot read :memory: (--database must be an on-disk store)".into());
    }
    // The scope is exactly one selector (ADR-0401's synopsis), stated once:
    // the legal arms resolve it, the `_` arm is the exclusivity error.
    let scope = match (query, labels.is_empty(), types.is_empty()) {
        (None, true, true) => PushScope::All,
        (Some(query), true, true) => PushScope::Query(query),
        (None, false, true) => PushScope::Labels(labels),
        (None, true, false) => PushScope::Types(types),
        _ => {
            return Err(
                "push takes one of --query, --labels, or --types (none pushes the whole store)"
                    .into(),
            )
        }
    };
    Ok(PushCmd {
        to,
        database,
        scope,
        batch_size: batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
        nodes_update: nodes_update.unwrap_or_else(PolicySpec::default_spec),
        rels_update: rels_update.unwrap_or_else(PolicySpec::default_spec),
        updated_at,
        rel_mode: rel_mode.unwrap_or(RelMode::Keyed),
        merge_keys,
        rel_keys,
        unkeyed_labels: unkeyed_labels.unwrap_or(UnkeyedAction::Skip),
        dry_run,
        json,
        allow_plaintext,
    })
}

/// Append one comma-separated list value to `target`, rejecting empty
/// entries (a label list is explicit, never guessed).
fn append_list(target: &mut Vec<String>, value: &str, flag: &str) -> Result<(), ParseError> {
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(format!("{flag} has an empty name in {value:?}").into());
        }
        target.push(item.to_string());
    }
    Ok(())
}

/// `--batch-size` parses as a positive number (ADR-0401 §2).
fn parse_batch_size(value: &str) -> Result<usize, ParseError> {
    value
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("invalid --batch-size {value:?} (expected a number >= 1)").into())
}

/// Run `push`. Returns the process exit code: 0 success (including a
/// dry run), 1 runtime/remote failure (the partial report still prints),
/// 2 usage (handled at parse).
pub fn run(cmd: PushCmd) -> i32 {
    match run_inner(&cmd) {
        Ok(()) => 0,
        Err(e) => {
            crate::print_error(format_args!("Error: {e}"));
            1
        }
    }
}

fn run_inner(cmd: &PushCmd) -> Result<(), relate_load::LoadError> {
    // `--to`'s URI-vs-env-file split is owned by Credentials (the one
    // spelling; the CLI adds no second).
    let credentials = relate_load::Credentials::from_flag_value(&cmd.to, "push --to")?;
    let database =
        Database::open(&cmd.database).map_err(|e| relate_load::LoadError::Engine(e.to_string()))?;
    let opts = PushOpts {
        credentials,
        scope: cmd.scope.clone(),
        batch_size: cmd.batch_size,
        nodes_update: cmd.nodes_update.clone(),
        rels_update: cmd.rels_update.clone(),
        updated_at: cmd.updated_at.clone(),
        rel_mode: cmd.rel_mode,
        merge_keys: cmd.merge_keys.clone(),
        rel_keys: cmd.rel_keys.clone(),
        unkeyed_labels: cmd.unkeyed_labels,
        allow_plaintext: cmd.allow_plaintext,
        dry_run: cmd.dry_run,
    };
    match push_load(&opts, &database)? {
        PushOutcome::Completed(report) => {
            print_report(cmd, &report);
            Ok(())
        }
        PushOutcome::Partial { report, error } => {
            // The partial report prints exactly what landed (ADR-0401 §6),
            // then the failure; the exit code says it failed.
            print_report(cmd, &report);
            Err(error)
        }
    }
}

/// Print the run report: one JSON object with `--json`, the per-entity
/// table by default (ADR-0401 §6).
fn print_report(cmd: &PushCmd, report: &PushReport) {
    if cmd.json {
        println!("{}", report.to_json());
    } else {
        print_table(report);
    }
}

/// One column of the human table: its header, alignment, and the cell
/// it reads off an entity — the one model the header line, the cells,
/// the widths and the alignment all derive from.
struct Column {
    header: &'static str,
    align: ColumnAlign,
    cell: fn(&EntityReport) -> String,
}

/// The table's columns, in print order. Entity names, merge keys and
/// statement shapes are catalog strings echoed to the terminal, so each
/// goes through the shared formatter's `sanitize_error_text`. `skipped` sums the entity's skip
/// reasons; `warn` counts the `--updated-at` incomparables.
const COLUMNS: [Column; 9] = [
    Column {
        header: "entity",
        align: ColumnAlign::Left,
        cell: |e| relate_formatter::sanitize_error_text(&e.name),
    },
    Column {
        header: "total",
        align: ColumnAlign::Right,
        cell: |e| e.total.to_string(),
    },
    Column {
        header: "created",
        align: ColumnAlign::Right,
        cell: |e| e.created.to_string(),
    },
    Column {
        header: "updated",
        align: ColumnAlign::Right,
        cell: |e| e.updated.to_string(),
    },
    Column {
        header: "skipped",
        align: ColumnAlign::Right,
        cell: |e| {
            (e.skipped_unkeyed + e.skipped_policy + e.skipped_remote_newer + e.skipped_dangling)
                .to_string()
        },
    },
    Column {
        header: "warn",
        align: ColumnAlign::Right,
        cell: |e| e.incomparable.to_string(),
    },
    Column {
        header: "merge key",
        align: ColumnAlign::Left,
        cell: |e| relate_formatter::sanitize_error_text(&e.merge_key.join(",")),
    },
    Column {
        header: "policy",
        align: ColumnAlign::Left,
        // A counted bucket (unkeyed-skip, unkeyed-rel, dangling) writes
        // nothing and carries no policy — shown as "-".
        cell: |e| {
            e.policy
                .map_or_else(|| "-".to_string(), |p| p.as_str().to_string())
        },
    },
    Column {
        header: "batches",
        align: ColumnAlign::Right,
        cell: |e| e.batches.to_string(),
    },
];

/// The whole human report as lines: the entity grid, then either the
/// dry-run plan dump with its planned-totals line or the run summary
/// (ADR-0401 §6). Pure, so the output is assertable.
fn report_lines(report: &PushReport) -> Vec<String> {
    // The header is row zero; the grid lays every row out in display
    // columns through the shared formatter.
    let header: Vec<String> = COLUMNS.iter().map(|c| c.header.to_string()).collect();
    let rows: Vec<Vec<String>> = std::iter::once(header)
        .chain(
            report
                .entities
                .iter()
                .map(|entity| COLUMNS.iter().map(|column| (column.cell)(entity)).collect()),
        )
        .collect();
    let aligns: Vec<ColumnAlign> = COLUMNS.iter().map(|c| c.align).collect();
    let mut lines = text_grid_lines(&rows, &aligns);
    if report.dry_run {
        // The plan's statement shapes, one line per entity.
        for entity in &report.entities {
            if let Some(statement) = &entity.statement {
                lines.push(format!(
                    "{}: {}",
                    relate_formatter::sanitize_error_text(&entity.name),
                    relate_formatter::sanitize_error_text(statement)
                ));
            }
        }
        let rows: usize = report.entities.iter().map(|e| e.total).sum();
        lines.push(format!(
            "dry run: {} entities, {} rows planned; nothing was sent",
            report.entities.len(),
            rows
        ));
    } else {
        lines.push(format!(
            "pushed {} rows in {} statements ({} bytes) in {} ms",
            report.rows_sent,
            report.statements_sent,
            report.bytes_sent,
            report.elapsed.as_millis()
        ));
    }
    lines
}

/// The human report, printed.
fn print_table(report: &PushReport) {
    for line in report_lines(report) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    fn base(query: &[&str]) -> Vec<String> {
        let mut all = args(query);
        all.extend(args(&["--to", "/tmp/n.env", "--database", "./graph"]));
        all
    }

    #[test]
    fn parse_rejects_duplicate_merge_keys_and_rel_keys() {
        // A repeated --merge-key for the same label silently overwrote the
        // earlier override (input loss); it now fails closed naming it.
        let err = parse_push_args(&base(&[
            "--merge-key",
            "Movie:title",
            "--merge-key",
            "Movie:released",
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("--merge-key names \"Movie\" twice"), "{err}");
        let err = parse_push_args(&base(&[
            "--rel-key",
            "ACTED_IN:roles",
            "--rel-key",
            "ACTED_IN:year",
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("--rel-key names \"ACTED_IN\" twice"), "{err}");
        // Different names stay additive, not duplicates.
        assert!(parse_push_args(&base(&[
            "--merge-key",
            "Movie:title",
            "--merge-key",
            "Person:name"
        ]))
        .is_ok());
    }

    #[test]
    fn parse_rejects_repeated_boolean_flags() {
        // The once-only guard is uniform across the boolean flags, the
        // serve/info precedent — not a --json special case.
        for flag in ["--dry-run", "--json", "--allow-plaintext"] {
            let err = parse_push_args(&base(&[flag, flag]))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("{flag} may be given only once")),
                "{flag}: {err}"
            );
        }
    }

    #[test]
    fn parse_requires_to_and_database() {
        assert!(parse_push_args(&args(&["--database", "./graph"]))
            .unwrap_err()
            .to_string()
            .contains("--to"));
        assert!(parse_push_args(&args(&["--to", "/tmp/n.env"]))
            .unwrap_err()
            .to_string()
            .contains("--database"));
    }

    #[test]
    fn parse_rejects_memory() {
        let err = parse_push_args(&args(&["--to", "n.env", "--database", ":memory:"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains(":memory:"), "{err}");
    }

    #[test]
    fn parse_defaults() {
        let cmd = parse_push_args(&base(&[])).unwrap();
        assert_eq!(cmd.batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(cmd.nodes_update, PolicySpec::default_spec());
        assert_eq!(cmd.rel_mode, RelMode::Keyed);
        assert_eq!(cmd.unkeyed_labels, UnkeyedAction::Skip);
        assert!(!cmd.dry_run);
        assert!(!cmd.json);
        assert!(!cmd.allow_plaintext);
    }

    #[test]
    fn parse_scope_selectors_are_exclusive() {
        let err = parse_push_args(&base(&["--labels", "Movie", "--types", "ACTED_IN"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("one of --query"), "{err}");
        let err = parse_push_args(&base(&[
            "--query",
            "MATCH (n) RETURN n",
            "--labels",
            "Movie",
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("one of --query"), "{err}");
        // Each alone parses; none is the whole store.
        assert!(parse_push_args(&base(&["--labels", "Movie,Person"])).is_ok());
        assert!(parse_push_args(&base(&["--types", "ACTED_IN"])).is_ok());
        assert!(parse_push_args(&base(&["--query", "MATCH (n) RETURN n"])).is_ok());
        assert!(matches!(
            parse_push_args(&base(&[])).unwrap().scope,
            PushScope::All
        ));
    }

    #[test]
    fn parse_policies_scoped_and_global() {
        let cmd = parse_push_args(&base(&["--nodes-update", "Movie=replace,Person=skip"])).unwrap();
        assert_eq!(
            cmd.nodes_update.resolve("Movie"),
            relate_load::UpdatePolicy::Replace
        );
        assert_eq!(
            cmd.nodes_update.resolve("Person"),
            relate_load::UpdatePolicy::Skip
        );
        let err = parse_push_args(&base(&["--nodes-update", "wipe"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown update policy"), "{err}");
    }

    #[test]
    fn parse_batch_size_must_be_positive() {
        let cmd = parse_push_args(&base(&["--batch-size", "250"])).unwrap();
        assert_eq!(cmd.batch_size, 250);
        for bad in ["0", "-1", "many", ""] {
            assert!(
                parse_push_args(&base(&["--batch-size", bad])).is_err(),
                "{bad:?} parsed"
            );
        }
    }

    #[test]
    fn parse_updated_at_and_merge_key() {
        let cmd = parse_push_args(&base(&[
            "--updated-at",
            "updatedAt,updated",
            "--merge-key",
            "Movie:myId",
            "--merge-key",
            "Event:tenant,code",
        ]))
        .unwrap();
        assert_eq!(cmd.updated_at, ["updatedAt", "updated"]);
        assert_eq!(cmd.merge_keys["Movie"], ["myId".to_string()].as_slice());
        assert_eq!(cmd.merge_keys["Event"], ["tenant", "code"]);
        let err = parse_push_args(&base(&["--merge-key", "Movie"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--merge-key"), "{err}");
        assert!(err.contains("NAME:PROP"), "{err}");
    }

    #[test]
    fn parse_rel_key_overrides() {
        let cmd = parse_push_args(&base(&[
            "--rel-key",
            "ACTED_IN:role",
            "--rel-key",
            "LEASED:tenant,slot",
        ]))
        .unwrap();
        assert_eq!(cmd.rel_keys["ACTED_IN"], ["role".to_string()].as_slice());
        assert_eq!(cmd.rel_keys["LEASED"], ["tenant", "slot"]);
        let err = parse_push_args(&base(&["--rel-key", "ACTED_IN"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--rel-key"), "{err}");
        assert!(err.contains("NAME:PROP"), "{err}");
    }

    #[test]
    fn parse_rel_mode_and_unkeyed_labels() {
        assert_eq!(
            parse_push_args(&base(&["--rel-mode", "structural"]))
                .unwrap()
                .rel_mode,
            RelMode::Structural
        );
        assert_eq!(
            parse_push_args(&base(&["--rel-mode", "create"]))
                .unwrap()
                .rel_mode,
            RelMode::Create
        );
        // The default is keyed: resolve per type, else one per pair.
        assert_eq!(
            parse_push_args(&base(&[])).unwrap().rel_mode,
            RelMode::Keyed
        );
        assert!(parse_push_args(&base(&["--rel-mode", "merge"])).is_err());
        assert!(parse_push_args(&base(&["--rel-mode", "unique"])).is_err());
        assert!(parse_push_args(&base(&["--unkeyed-labels", "insert"])).is_err());
    }

    #[test]
    fn parse_rejects_unknown_flag_and_positionals() {
        assert_eq!(
            parse_push_args(&args(&["--bogus", "--database", "./g", "--to", "x.env"])).unwrap_err(),
            ParseError::unexpected("push", "--bogus")
        );
        assert_eq!(
            parse_push_args(&args(&["--to", "x.env", "--database", "./g", "./extra"])).unwrap_err(),
            ParseError::unexpected("push", "./extra")
        );
    }

    #[test]
    fn parse_dry_run_json_allow_plaintext() {
        let cmd = parse_push_args(&base(&["--dry-run", "--json", "--allow-plaintext"])).unwrap();
        assert!(cmd.dry_run);
        assert!(cmd.json);
        assert!(cmd.allow_plaintext);
        assert!(parse_push_args(&base(&["--json", "--json"])).is_err());
    }

    /// The table renders one row per entity and the summary line, in both
    /// modes.
    #[test]
    fn table_renders_entities_and_summary() {
        let report = sample_report(false);
        assert_eq!(
            report_lines(&report),
            [
                "entity  total  created  updated  skipped  warn  merge key  policy  batches",
                "Person      2        1        1        0     0  name       add           1",
                "pushed 2 rows in 1 statements (120 bytes) in 3 ms",
            ]
        );
        // The dry run adds the statement shapes and the planned totals.
        let dry = report_lines(&sample_report(true));
        assert!(
            dry[2].starts_with("Person: UNWIND $rows AS row MERGE"),
            "{dry:?}"
        );
        assert_eq!(
            dry[3],
            "dry run: 1 entities, 2 rows planned; nothing was sent"
        );
        // Wide characters stay aligned: the grid measures display columns.
        let mut wide = sample_report(false);
        wide.entities[0].name = "漢字".into();
        let lines = report_lines(&wide);
        assert!(lines[1].starts_with("漢字    "), "{lines:?}");
        let json = report.to_json();
        assert!(json["entities"][0]["entity"] == "Person");
    }

    fn sample_report(dry_run: bool) -> PushReport {
        PushReport {
            dry_run,
            entities: vec![relate_load::EntityReport {
                name: "Person".into(),
                is_rel: false,
                total: 2,
                created: 1,
                updated: 1,
                skipped_unkeyed: 0,
                skipped_policy: 0,
                skipped_remote_newer: 0,
                skipped_dangling: 0,
                incomparable: 0,
                merge_key: vec!["name".into()],
                policy: Some(relate_load::UpdatePolicy::Add),
                statement: Some("UNWIND $rows AS row MERGE (n:`Person` {`name`: row.key.`name`}) SET n += row.props".into()),
                batches: 1,
            }],
            statements_sent: 1,
            rows_sent: 2,
            bytes_sent: 120,
            elapsed: std::time::Duration::from_millis(3),
        }
    }

    #[test]
    fn json_report_carries_the_counters() {
        let json = sample_report(false).to_json();
        assert_eq!(json["dryRun"], false);
        assert_eq!(json["entities"][0]["created"], 1);
        assert_eq!(json["rowsSent"], 2);
    }
}

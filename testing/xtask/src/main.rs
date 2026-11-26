mod boundaries;
mod fixture_sync;
mod package_check;
mod row_vec;
mod wasm_pkg;
mod workspace;

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const HELP: &str = "\
Workspace maintenance tasks for Relate.

Usage: cargo xtask <COMMAND> [OPTIONS]

Commands:
  check-boundaries  Check workspace dependency edges against ADR-0103 rules R1-R5, R7
  check-package     Package the `relate` facade and build the extracted crate against the workspace

Options:
  -h, --help  Print help
";

const CHECK_BOUNDARIES_HELP: &str = "\
Check workspace dependency edges against ADR-0103 rules R1-R5 and the
extensions-group placement rules R7 (ADR-0431 §11).

Reads `cargo metadata --format-version 1 --no-deps` (using $CARGO if set,
else `cargo`), classifies every workspace member by its directory
(relate/<pkg>, surfaces/<pkg>, testing/<pkg>, extensions/<pkg>) and
checks every workspace-internal dependency edge by kind (normal, dev,
build).

Usage: cargo xtask check-boundaries [OPTIONS]

Options:
  -h, --help  Print help

Exit status:
  0  no violations
  1  one or more violations, each printed as
     `<from-pkg> -> <to-pkg> (<dep-kind>): <rule id>` or
     `<pkg> (<dir>): R5`
  2  usage error or `cargo metadata` failure

Also runs the executor `Vec<Row>` guard (ADR-0555): a non-test fn under
relate/relate-executor/src returning `Vec<Row>` or `Result<Vec<Row>>` must
be listed, with a reason, in testing/xtask/row_vec_allowlist.txt; a listed
fn that no longer exists fails too. Violations print as
`<file>::<fn>: <reason>` and exit 1.

Also runs the facade fixture-copy guard (LABS-1414): the crate-local tree
at relate/relate/tests/fixtures/pre_adr0084_default_wal/ must be
byte-identical, file for file, to the tree relate-storage owns at
relate/relate-storage/tests/fixtures/pre_adr0084_default_wal/. A file
missing, extra, or drifted on either side prints as
`<tree>/<rel-path>: <reason>` and exits 1; an unreadable tree exits 2.

Also runs the wasm-package guard (REL8-1234): the wasm bundle's npm
manifest and the files its build produces must agree — every file the
build emits (the js/ directory the build script copies wholesale, the
wasm-bindgen output, snippets/, the package README stamped from
npm/README.md) must be listed in
surfaces/relate-wasm/npm/package.json.template's `files` array, every
`files` entry must name something the build produces, and every
`exports` subpath must point at a `files` entry. A disagreement prints
as `<path>: <reason>` and exits 1; an unreadable or unparsable template
or js/ tree, or a missing npm/README.md the build copies, exits 2.
";

const CHECK_PACKAGE_HELP: &str = "\
Check that the `relate` facade's packaged crate builds against the current
workspace (the package drift window check).

`cargo package` verifies the packaged crate against the crates.io
registry, so it fails for as long as workspace members carry unpublished
changes (the published versions lag the workspace). This check packages
`relate` with `cargo package --no-verify`, extracts the .crate tarball,
appends a [patch.crates-io] table to the extracted manifest mapping every
member of the facade's transitive workspace closure (normal and build
edges, derived from the workspace manifests) to its workspace path, and
builds the extracted lib and all its examples under that patch with
--message-format=json — the same verification, against the workspace.
The verdict comes from the typed build records: every closure member
and the extracted facade itself must have produced a compiler artifact
whose manifest is the workspace one, so a patch entry cargo did not
apply (a registry fallback) fails the check instead of greening it.

The scratch directory (under the system temp dir, no default to
configure) is removed on every exit path, panic included; nothing is
written inside the repository. Uses $CARGO if set, else `cargo` on PATH,
and needs `tar` on PATH to extract the tarball.

Usage: cargo xtask check-package [OPTIONS]

Options:
  -h, --help  Print help (the only option; no positionals)

Exit status:
  0  the packaged lib and every packaged example built under the patch
  1  packaging, extraction or the patched build failed (error on stderr)
  2  usage error, or the check could not run: a `cargo metadata` failure
     or a workspace without the `relate` facade, an unresolvable
     workspace root, or an unusable scratch directory (error on stderr)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("check-boundaries") => subcommand(&args, CHECK_BOUNDARIES_HELP, check_boundaries),
        Some("check-package") => subcommand(&args, CHECK_PACKAGE_HELP, check_package),
        Some(other) => usage_error(&format!("unknown command '{other}'")),
        None => usage_error("missing command"),
    }
}

/// One dispatch shape for a command's sub-arguments: no argument runs
/// the command, `-h`/`--help` prints its help, anything else is a
/// usage error.
fn subcommand(args: &[String], help: &str, run: fn() -> ExitCode) -> ExitCode {
    match args.get(1).map(String::as_str) {
        None => run(),
        Some("-h" | "--help") => {
            print!("{help}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unexpected argument '{other}'")),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{HELP}");
    ExitCode::from(2)
}

/// What one check concluded: its violations, and the opening of the one
/// line it reports — what the check looked at, with no outcome in it.
struct Verdict {
    violations: Vec<String>,
    opening: String,
}

/// A guard's verdict from a guard module's violations: the opening names
/// the guard. The one adapter every guard row goes through, so a guard
/// module stays a pure `root -> violations` fn.
fn guard_verdict(label: &str, result: Result<Vec<String>, String>) -> Result<Verdict, String> {
    let opening = format!("check-boundaries: {label}");
    let violations = result.map_err(|e| format!("{opening}: {e}"))?;
    Ok(Verdict {
        violations,
        opening,
    })
}

/// The dependency-edge check as a `Verdict`: its opening carries the
/// counts it read.
fn dependency_edges(root: &Path) -> Result<Verdict, String> {
    // The identity the failure path needs; the counts only exist once
    // the graph is read.
    let subject = "check-boundaries: dependency edges";
    let packages = workspace::workspace_graph(root).map_err(|e| format!("{subject}: {e}"))?;
    let report = boundaries::check(&packages);
    // The bundle's protocol is plain strings, the shape the guard
    // modules already return; the typed violations render the same way
    // the check used to print them.
    let violations: Vec<String> = report.violations.iter().map(ToString::to_string).collect();
    Ok(Verdict {
        opening: format!(
            "check-boundaries: {} packages, {} edges checked",
            report.packages, report.edges
        ),
        violations,
    })
}

/// The checks `check-boundaries` runs, in one list: every entry returns
/// the same `Verdict`, so a new guard is one row here and no dispatch
/// shape sits outside the roster.
fn check_boundaries() -> ExitCode {
    let root = workspace_root();
    // All checks always run and print; exit 2 (I/O or metadata failure)
    // outranks a plain violation failure.
    [
        dependency_edges(&root),
        guard_verdict("executor Vec<Row> guard", row_vec::check(&root)),
        guard_verdict("fixture copy guard", fixture_sync::check(&root)),
        guard_verdict("wasm package guard", wasm_pkg::check(&root)),
    ]
    .into_iter()
    .map(report)
    .max()
    .map_or(ExitCode::SUCCESS, Outcome::exit_code)
}

/// What one maintenance command concluded — the binary's one verdict
/// vocabulary and its one exit table, shared by every command.
/// Variants are declared in severity order: the derived ordering ranks
/// `Clean < Findings < Error`. `Findings` — exit 1 — is the class of a
/// check that ran and found what it looks for (a boundary rule
/// violation for `check-boundaries`, a packaging or patched-build
/// failure for `check-package`); `Error` — exit 2 — is the class of a
/// check that could not run. Each command prints its findings and
/// errors where they are classified; this type carries only the
/// verdict.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Clean,
    Findings,
    Error,
}

impl Outcome {
    fn exit_code(self) -> ExitCode {
        match self {
            Outcome::Clean => ExitCode::SUCCESS,
            Outcome::Findings => ExitCode::FAILURE,
            Outcome::Error => ExitCode::from(2),
        }
    }
}

/// The workspace root, from this crate's directory at `testing/xtask`.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh scratch directory under the system temp dir, removed on
/// every exit path — success, failure and panic alike — by `TempDir`'s
/// Drop. The crate's one scratch-dir policy: every check and test that
/// needs a throwaway tree takes it from here (REL8-1234 lifted it out
/// of `package_check`, which owned the only copy).
fn scratch_dir() -> Result<tempfile::TempDir, String> {
    tempfile::TempDir::new()
        .map_err(|e| format!("failed to create a scratch directory under the system temp dir: {e}"))
}

/// A path's forward-slash rendering — the one spelling of a path that
/// goes into a message or a scan key, so a walk on Windows and one on
/// POSIX produce the same strings (row_vec's test-file test splits on
/// `/`, so the shape is load-bearing there).
fn slash_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Walk every file under `root` and hand each one to `leaf` — with its
/// path, and with its path relative to `root` — in directory order. The
/// one directory walker the guards' collectors express themselves
/// through, so no second `read_dir`/`strip_prefix` skeleton grows beside
/// it; an unreadable directory is an `Err` naming it.
fn walk_files(
    root: &Path,
    leaf: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    fn walk(
        dir: &Path,
        root: &Path,
        leaf: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if file_type.is_dir() {
                walk(&path, root, leaf)?;
            } else {
                let rel = path.strip_prefix(root).expect("walked path is under root");
                leaf(&path, rel)?;
            }
        }
        Ok(())
    }
    walk(root, root, leaf)
}

/// The one reporter, and the bundle's one protocol: every check returns
/// a `Verdict` (or an error that already names it), so the dependency
/// edge check and every guard are the same kind of thing. The outcome is
/// composed here, never pre-rendered by a check — `: clean` on stdout
/// when it found nothing, the violations then `: <n> violations` on
/// stderr when it did — so a failing check cannot report itself clean,
/// and an I/O or metadata failure exits 2.
fn report(verdict: Result<Verdict, String>) -> Outcome {
    match verdict {
        Ok(Verdict {
            violations,
            opening,
        }) if violations.is_empty() => {
            println!("{opening}: clean");
            Outcome::Clean
        }
        Ok(Verdict {
            violations,
            opening,
        }) => {
            for violation in &violations {
                eprintln!("{violation}");
            }
            eprintln!("{opening}: {} violations", violations.len());
            Outcome::Findings
        }
        Err(e) => {
            eprintln!("error: {e}");
            Outcome::Error
        }
    }
}

/// `cargo xtask check-package` (see `package_check`): package the facade
/// unverified, then build the extracted crate patched onto the workspace.
/// The pipeline prints its failure where it is classified and returns
/// the shared verdict: `Outcome::Error` (the check could not run) is the
/// exit-2 class, `Outcome::Findings` (the check ran and the packaged
/// crate failed it) is the exit-1 class — the same one table
/// `check-boundaries` reads.
fn check_package() -> ExitCode {
    match package_check::check(&workspace_root()) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        // The failure already printed where it was classified; the
        // verdict carries only the exit class.
        Err(outcome) => outcome.exit_code(),
    }
}

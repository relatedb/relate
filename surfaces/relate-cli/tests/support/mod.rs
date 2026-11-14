//! Shared harness for the CLI end-to-end integration tests: spawn the
//! `relate` binary built by Cargo with an optional custom `$HOME` (and
//! optional working directory), capturing status/stdout/stderr.

use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

mod mock_mixpanel;

#[allow(unused_imports)] // Each test binary uses a subset of the mock harness.
pub use mock_mixpanel::{FollowupRun, MockMixpanel, DELIVERY_WAIT, QUIET_GRACE};

/// The `serve` e2e harness (spawn/wait/SIGINT on one spawned server, plus
/// the pinned credential pair): shared by `e2e_serve.rs` and
/// `e2e_serve_read_only.rs`, so the spawn/port/SIGINT policy has one copy.
/// Compiled only when both served surfaces are in the build — the same
/// gate as the suites that use it.
#[cfg(all(feature = "http", feature = "mcp"))]
#[allow(dead_code)] // Each test binary compiles this module and uses a subset.
pub mod serve;

/// The `upgrade` e2e harness (the releases.json loopback and the
/// newer-release document): shared by `e2e_upgrade.rs` and
/// `e2e_upgrade_route.rs`, so the fixture has one copy. Compiled only when
/// self-upgrade is in the build — the same gate as the suites that use it.
#[cfg(feature = "upgrade")]
#[allow(dead_code)] // Each test binary compiles this module and uses a subset.
pub mod releases;

/// The client-side Bolt exchange helpers (handshake, struct send/read, the
/// SUCCESS/FAILURE metadata unpack, connect-and-HELLO): shared by
/// `e2e_serve.rs` and `e2e_serve_read_only.rs`, so the client-side Bolt
/// test policy has one copy. Compiled only when the Bolt surface is in the
/// build — the same gate as the tests that use it.
#[cfg(feature = "bolt")]
#[allow(dead_code)] // Each test binary compiles this module and uses a subset.
pub mod bolt;

/// The extension-fixture install kit (the e2e twin of the unit suites'
/// `extensions::testkit`): a package under a tempdir `$XDG_CONFIG_HOME`,
/// its `sh` or compiled child, and the `run` invocation pinned to that
/// root — shared by the run-verb suite (`e2e_run.rs`) and the
/// fixture-language suite (`e2e_extension_fixtures.rs`), so the
/// install/isolate policy has one copy. Compiled only when the
/// extension mount is in the build — the same gate as the suites that
/// use it.
#[cfg(feature = "extensions")]
#[allow(dead_code)] // Each test binary compiles this module and uses a subset.
pub mod extensions;

/// The first-party importer e2e kit (ADR-0431 §11): the `fs-to-graph` and
/// `git-to-graph` children built and installed as the packages they ship
/// as, and the `run <name>` invocations against that root — the coverage
/// the old lab e2e suites owned, moved onto the extension mount.
/// Compiled only when the extension mount is in the build — the same gate
/// as the suites that use it.
#[cfg(feature = "extensions")]
#[allow(dead_code)] // Each test binary compiles this module and uses a subset.
pub mod importers;

/// Path to the relate binary the e2e suites spawn.
///
/// ADR-0251 §5 (build once per SHA, shared everywhere): `RELATE_BIN`
/// points at a shared prebuilt binary — `scripts/prebuilt-bin.sh` stages
/// one per commit under `target/.prebuilt/<sha>/` and prints the path to
/// export — so repeated e2e runs and the smoke lanes reuse one build
/// instead of each recompiling their own. An unset or dangling value
/// falls back to the binary Cargo built for this crate.
pub fn binary_path() -> std::path::PathBuf {
    if let Some(prebuilt) = std::env::var_os("RELATE_BIN") {
        let prebuilt = std::path::PathBuf::from(prebuilt);
        if prebuilt.is_file() {
            return prebuilt;
        }
    }
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_relate"))
}

/// [`binary_path`] in the `&str` form the PTY driver argv takes. The one
/// UTF-8 expectation for the binary path lives here, so every PTY spawn site
/// passes `binary_path_str()` instead of re-spelling the expect.
#[allow(dead_code)] // Only the PTY suites stringify the binary path.
pub fn binary_path_str() -> String {
    binary_path()
        .to_str()
        .expect("the CLI binary path is UTF-8")
        .to_string()
}

mod ambient_scrub;

// Re-exported so the e2e suites keep the historical
// `support::AGENT_ENV_VARS` path; only some suites read it.
#[allow(unused_imports)]
pub use ambient_scrub::AGENT_ENV_VARS;

use ambient_scrub::scrub_ambient;

/// The telemetry axes the host must not leak into a child: every
/// config-dir base (the XDG override and the two Windows bases a real
/// Windows runner sets ambiently — without the scrub, a test that pins
/// the no-base refusal by removing `$HOME` would resolve the
/// developer's actual profile instead) plus the two opt-out controls
/// and the endpoint override.
/// [`run_cli`] removes them through [`CliRunOptions::default`];
/// [`pty_command`] removes them from the driver process whose environment the
/// PTY child inherits. One list, so the two spawn paths cannot disagree.
const TELEMETRY_AXES: [&str; 8] = [
    "XDG_CONFIG_HOME",
    "APPDATA",
    "USERPROFILE",
    "DO_NOT_TRACK",
    "RELATE_TELEMETRY",
    "RELATE_MIXPANEL_ENDPOINT",
    "RELATE_FEEDBACK_ENDPOINT",
    "RELATE_FEEDBACK_TOKEN",
];

/// A `Command` for the relate binary with every agent-harness variable,
/// `RELATE_MAX_ROWS` and `RELATE_PREVIEW` removed, so a test run
/// gives the same result under Claude Code, Codex, Cursor… and in a developer
/// shell that exported a preview opt-in, as it does in CI. The substring
/// rules' variables (`EDITOR`, `TERM_PROGRAM`, and `PATH`'s `.pi/agent`
/// entries) are neutralized too, because the piped-default assertions
/// (ADR-0107, ADR-0426) depend on detection not firing. Tests that exercise
/// agent detection or a preview gate set the variable they need explicitly.
///
/// This function is the ONE owner of that ambient scrub for the binary, which
/// is why the removals live here rather than in [`CliRunOptions::unset_env`]:
/// every `cli_command`-built spawn — [`run_cli`], the suites that spawn a
/// server or stream the child directly — goes through it. The one path that
/// does not build a `Command` for the binary is the PTY driver, which spawns
/// the binary from Python; [`pty_command`] applies the same [`scrub_ambient`]
/// to that driver's process, so the guarantee covers it too.
///
/// `RELATE_PREVIEW` is the case worth spelling out: it is a product-side
/// opt-in (ADR-0371) the harness has no stake in, yet it is scrubbed here.
/// An inherited `RELATE_PREVIEW=ontology` enables the `db.ontology.*`
/// family at open, so a gate-off assertion (`e2e_ontology_preview`'s authored
/// disabled error and its hidden `SHOW PROCEDURES` rows) would flip state
/// under a developer shell that exports it. A test that wants the opt-in
/// sets it through [`CliRunOptions::with_envs`], which applies after this
/// removal.
///
/// Telemetry is off. [`run_cli`] clears this again and leaves telemetry
/// at its product default only for tests that need it: the cold-start
/// guard and the mock-sink suite, both of which pin the endpoint
/// themselves. With the spool model (ADR-0545) an enabled run never
/// waits on the network at exit, but it still writes spool files and
/// still spawns a drain — the forced-off default keeps every other
/// test's fake `$HOME` free of both and the drain offline.
pub fn cli_command() -> Command {
    let mut cmd = Command::new(binary_path());
    scrub_ambient(&mut cmd);
    cmd.env("RELATE_TELEMETRY", "off");
    cmd
}

/// The `python3` command that runs a PTY driver script from `tests/support/`.
///
/// The PTY suites spawn the binary through `pty_driver.fork_exec`, which
/// copies the driver process's `os.environ` into the child. The ambient scrub
/// therefore has to run on this command, before that copy: [`scrub_ambient`]
/// removes the agent markers, the row cap and the preview opt-in, and
/// [`TELEMETRY_AXES`] the config-dir, telemetry and feedback axes. [`assert_pty_driver_ok`]
/// documents the PTY-only setup (scratch `HOME`, forced terminal, sinkholed
/// endpoint) the driver applies to the child, which no `cli_command` child
/// needs.
#[allow(dead_code)] // Only the PTY suites spawn through the driver.
pub fn pty_command(script: &str) -> Command {
    let path = format!("{}/tests/support/{script}", env!("CARGO_MANIFEST_DIR"));
    let mut cmd = Command::new("python3");
    cmd.arg(path);
    scrub_ambient(&mut cmd);
    for name in TELEMETRY_AXES {
        cmd.env_remove(name);
    }
    cmd
}

/// Run a PTY driver script from `tests/support/` with `args` after the
/// script. This is the one PTY harness tail: it owns the spawn, the driver's
/// exit-success assertion and the `OK` success line, and every failure
/// message names the script. Transcript assertions beyond `OK` live inside
/// the driver scripts; when a suite first needs them on the Rust side, a
/// variant that returns the transcript gets added at that point. A driver
/// that reports a failure exits non-zero with its own diagnostic, which
/// lands in the message here.
///
/// `pty_driver.fork_exec` owns the child's terminal and telemetry setup — it
/// forces `TERM`, the 80x24 window, and a scratch `HOME` with tracking off —
/// so a caller sets no `TERM` and touches no telemetry variable.
#[cfg(unix)]
#[allow(dead_code)] // Each e2e binary compiles this module and uses a subset.
pub fn assert_pty_driver_ok(script: &str, args: &[&str]) {
    let output = pty_command(script)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("python3 must be available to drive the PTY ({script}): {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "PTY driver {script} failed (status {:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
    assert!(
        stdout.contains("OK"),
        "PTY driver {script} did not print OK\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// The exact stderr line the CLI prints when it exits with an open
/// transaction (ADR-0164; `OPEN_TRANSACTION_WARNING` in `src/session.rs`).
/// Pinned byte-for-byte as an observable contract: wrapper scripts grep
/// stderr for `rolling back`.
#[allow(dead_code)] // Only the exit-contract e2e suites pin the warning.
pub const OPEN_TRANSACTION_WARNING: &str = "Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.";

/// The ADR-0402 Decision 3 statement note, byte-for-byte, from its optional
/// ADR-0199 position: `at` is the 1-based `(statement, total)` a numbered
/// batch expectation names, `None` the single-statement (or single-position)
/// form. The e2e suites' one builder, so a numbered expectation and an
/// unnumbered one cannot drift — the CLI's own renderer
/// (`zero_effect_write_note`) is crate-private, and the e2e side pins the
/// bytes independently.
///
/// One format string, like the production renderer: the position is the
/// sentence's one substitution slot, never a second numbered literal.
#[allow(dead_code)] // Only the zero-effect e2e suite pins the note.
pub fn statement_note(at: Option<(usize, usize)>) -> String {
    let at = at
        .map(|(statement, total)| format!(" {statement} of {total}"))
        .unwrap_or_default();
    format!("note: write statement{at} changed nothing; the graph is unchanged\n")
}

/// The ADR-0402 Decision 4 commit summary, byte-for-byte.
#[allow(dead_code)] // Only the zero-effect e2e suite pins the summary.
pub const COMMIT_NOTE: &str = "note: .commit changed nothing; the transaction applied no changes\n";

/// Options for [`run_cli`]. The defaults match the historical general-suite
/// runner: no `$HOME` override (the child inherits the host home), the
/// refused Mixpanel endpoint, no working-directory override, the telemetry
/// axes removed before the ordered `env` list applies, no extra env, and no
/// piped stdin.
// Every integration-test binary compiles its own copy of this module and uses
// a subset of it; allow the rest.
#[allow(dead_code)]
pub struct CliRunOptions<'a> {
    /// Arguments passed to the CLI (flags, positionals).
    pub args: &'a [&'a str],
    /// `$HOME` override (for fake agent dirs and isolated telemetry
    /// config). `None` lets the child inherit the host home.
    pub home: Option<&'a Path>,
    /// Working-directory override (for tests that create a database
    /// directory on disk).
    pub cwd: Option<&'a Path>,
    /// The child's telemetry environment, as one ordered (key, value)
    /// list — later entries win. Pre-populated with the harness defaults
    /// (`RELATE_TELEMETRY=off`, a refused `RELATE_MIXPANEL_
    /// ENDPOINT` so an enabled run's drain stays offline); the
    /// telemetry suite overwrites the endpoint and drops the off default
    /// through the builders below. This is the model: no parallel
    /// endpoint/telemetry knobs whose application order is load-bearing.
    pub env: Vec<(&'a str, &'a str)>,
    /// Variables removed from the child before [`Self::env`] is applied.
    /// Pre-populated with [`TELEMETRY_AXES`]; [`Self::without_envs`] adds a
    /// variable the harness does not scrub — a developer shell's
    /// `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`, say, which would cap or reject runs the test
    /// expects to be unaffected. The ambient agent / preview / row-cap scrub
    /// is not this list's job: [`cli_command`] owns it for every
    /// `cli_command`-built spawn, and [`pty_command`] mirrors it for the PTY
    /// driver.
    pub unset_env: Vec<&'a str>,
    /// Raw bytes piped to the child's stdin (so callers can feed invalid
    /// UTF-8). `None` closes stdin immediately.
    pub stdin: Option<&'a [u8]>,
}

impl Default for CliRunOptions<'_> {
    fn default() -> Self {
        Self {
            args: &[],
            home: None,
            cwd: None,
            env: vec![
                ("RELATE_TELEMETRY", "off"),
                ("RELATE_MIXPANEL_ENDPOINT", "http://127.0.0.1:1"),
            ],
            unset_env: TELEMETRY_AXES.to_vec(),
            stdin: None,
        }
    }
}

// Not every test binary uses every constructor; allow until they do.
#[allow(dead_code)]
impl<'a> CliRunOptions<'a> {
    /// Args-only run; every other knob keeps its [`Default`] value.
    pub fn new(args: &'a [&'a str]) -> Self {
        Self {
            args,
            ..Default::default()
        }
    }

    /// Override `$HOME` (fake agent dirs, isolated telemetry config).
    pub fn in_home(mut self, home: &'a Path) -> Self {
        self.home = Some(home);
        self
    }

    /// Override the working directory (tests that create a database
    /// directory on disk).
    pub fn in_dir(mut self, cwd: &'a Path) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Point the Mixpanel endpoint elsewhere (e.g. a local mock sink) and
    /// drop the `RELATE_TELEMETRY=off` default: a sink is only useful
    /// when events are sent, and the spool roundtrip's follow-up runs
    /// drain into it (ADR-0545 §2).
    pub fn with_endpoint(mut self, endpoint: &'a str) -> Self {
        self.env.retain(|(k, _)| *k != "RELATE_MIXPANEL_ENDPOINT");
        self.env.push(("RELATE_MIXPANEL_ENDPOINT", endpoint));
        self.with_default_telemetry()
    }

    /// Point the feedback endpoint at a loopback sink (ADR-0588) and leave
    /// `RELATE_TELEMETRY` unset: a report is only sent when telemetry is
    /// enabled, and the feedback suite's mock sink is the destination.
    pub fn with_feedback_endpoint(mut self, endpoint: &'a str) -> Self {
        self.env.retain(|(k, _)| *k != "RELATE_FEEDBACK_ENDPOINT");
        self.env.push(("RELATE_FEEDBACK_ENDPOINT", endpoint));
        self.with_default_telemetry()
    }

    /// Leave `RELATE_TELEMETRY` unset so the product layers (default
    /// on, config file, first-run notice, endpoint rejection) decide.
    /// Tests that assert a disabled end state may still opt in — the name
    /// says what the child sees: the product default, not "enabled".
    pub fn with_default_telemetry(mut self) -> Self {
        self.env.retain(|(k, _)| *k != "RELATE_TELEMETRY");
        self
    }

    /// Extra environment overrides; applied after the defaults (later
    /// entries win), so a test can re-set anything the harness scrubbed
    /// or defaulted.
    pub fn with_envs(mut self, envs: &'a [(&'a str, &'a str)]) -> Self {
        self.env.extend_from_slice(envs);
        self
    }

    /// Remove environment variables from the child before [`Self::env`] is
    /// applied, so a test can drop an ambient value the harness does not own.
    /// The agent-harness, preview and row-cap variables are already removed
    /// by [`cli_command`] for every `cli_command`-built spawn, and by
    /// [`pty_command`] for the PTY driver; this is for the rest. An explicit
    /// [`Self::with_envs`] entry for the same name still wins: removal runs
    /// first.
    pub fn without_envs(mut self, names: &'a [&'a str]) -> Self {
        self.unset_env.extend_from_slice(names);
        self
    }

    /// Pipe raw bytes to the child's stdin.
    pub fn with_stdin(mut self, stdin: &'a [u8]) -> Self {
        self.stdin = Some(stdin);
        self
    }
}

/// Run the CLI with the given options; returns (exit status, stdout,
/// stderr). The host environment cannot leak into the test: [`cli_command`]
/// scrubs the agent/preview/max-rows variables, the options' `unset_env`
/// names are removed next, and `env` is applied last — an explicit set wins
/// over a removal, and every telemetry axis comes from the options.
// Every integration-test binary compiles its own copy of this module and uses
// a subset of it; allow the rest.
#[allow(dead_code)]
pub fn run_cli(options: CliRunOptions<'_>) -> (ExitStatus, String, String) {
    let CliRunOptions {
        args,
        home,
        cwd,
        env,
        unset_env,
        stdin,
    } = options;
    let mut cmd = cli_command();
    cmd.args(args);
    // One ordered env policy: ambient values go first, then the ordered
    // `env` map (defaults pre-populated, builders' entries later) is
    // everything the child sees on these axes — applied unconditionally,
    // in order, no conditional knobs.
    for name in unset_env {
        cmd.env_remove(name);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    spawn_cli(cmd, stdin)
}

fn spawn_cli(mut cmd: Command, stdin: Option<&[u8]>) -> (ExitStatus, String, String) {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to spawn relate binary");

    {
        let mut child_stdin = child.stdin.take().expect("failed to open stdin");
        if let Some(input) = stdin {
            // A child that exits without reading (bare `relate` on a
            // pipe prints help and leaves stdin alone) closes the pipe
            // before this write lands.
            if let Err(error) = child_stdin.write_all(input) {
                if error.kind() != std::io::ErrorKind::BrokenPipe {
                    panic!("failed to write stdin: {error}");
                }
            }
        }
        // child_stdin dropped here -> EOF for the child process
    }

    let output = child.wait_with_output().expect("failed to wait for child");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Fresh temporary directory, removed on drop.
// Not every e2e binary needs a scratch dir; allow until more do.
#[allow(dead_code)]
pub fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// The telemetry spool's file-name prefix, pinned for the e2e suites —
/// the e2e twin of the CLI's own fact (`SPOOL_PREFIX` in
/// `src/telemetry/spool.rs`; e2e binaries cannot reach the bin's private
/// items). Every e2e spelling of a spool file name — scans and seeds —
/// derives from this one constant, like the other pinned strings here.
#[allow(dead_code)]
pub const SPOOL_FILE_PREFIX: &str = "telemetry-spool-";

/// The claim file-name prefix, the e2e twin of `CLAIM_PREFIX` — a
/// sweep's in-flight rename target. The pending-file scan below counts
/// both, so a drain that has claimed (renamed) files mid-test can never
/// make the spool look falsely empty.
#[allow(dead_code)]
pub const CLAIM_FILE_PREFIX: &str = "telemetry-claim-";

/// The delivery follow-ups' work query (ADR-0545 §2): a follow-up's
/// whole job is to stay alive long enough for its drain thread to POST,
/// and engine work buys that window even on a saturated runner where a
/// `RETURN 1` run may live only milliseconds. The one spelling of the
/// query, shared by every suite's follow-up closure — the env wiring
/// around it stays per-suite.
#[allow(dead_code)]
pub const DELIVERY_WORK_QUERY: &str = "UNWIND range(1, 100000) AS i RETURN count(i) AS c";

/// The `telemetry-spool-*.jsonl` files currently under `dir` — the one
/// spool-scan helper for the e2e suites, so the scan shape cannot drift
/// between them.
#[allow(dead_code)]
pub fn spool_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(SPOOL_FILE_PREFIX))
        })
        .map(|entry| entry.path())
        .collect()
}

/// The pending telemetry files currently under `dir` — spool files and
/// claims alike, the sweep's whole vocabulary: a drain renames spool
/// files to claim names while it delivers, so a scan that counts only
/// one prefix can race the drain and report a falsely empty spool.
#[allow(dead_code)]
pub fn pending_telemetry_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.starts_with(SPOOL_FILE_PREFIX) || name.starts_with(CLAIM_FILE_PREFIX)
            })
        })
        .map(|entry| entry.path())
        .collect()
}

/// How many nodes the store at `db` holds, read through the CLI's JSONL
/// count: the one helper for the suites that check a report against the
/// state it claims (ADR-0199's `committed`, ADR-0402's "changed nothing"). It
/// delegates to [`query_count`], the canonical JSONL count reader, so the read
/// and its one-row assertion exist once; the throwaway `home` is just the
/// isolated `$HOME` that run needs.
#[allow(dead_code)] // Only the suites that assert on stored state use it.
pub fn node_count(db: &Path) -> u64 {
    let home = tempdir();
    query_count(home.path(), db, &[], "MATCH (n) RETURN count(n) AS n")
}

/// Run the CLI with the given args and a custom `$HOME` (for fake agent
/// dirs). Convenience wrapper over [`run_cli`] for the common isolated-home
/// case.
// Only the info E2E suite uses it; allow until more do.
#[allow(dead_code)]
pub fn run_cli_in_home(args: &[&str], home: &Path) -> (ExitStatus, String, String) {
    run_cli(CliRunOptions {
        args,
        home: Some(home),
        ..Default::default()
    })
}

/// Seed `db` with one statement through the CLI. `extra_flags` select things
/// like the snapshot codec. The caller's tempdir must outlive the run.
#[allow(dead_code)]
pub fn seed_database(home: &Path, db: &Path, extra_flags: &[&str], statement: &str) {
    let mut args: Vec<&str> = extra_flags.to_vec();
    args.extend(["--database", db.to_str().unwrap(), statement]);
    let (status, _stdout, stderr) = run_cli_in_home(&args, home);
    assert!(status.success(), "seed failed: {stderr}");
}

/// A `.rl` bundle holding one `:Person` (`Alice`), at `home/bundle.rl`: seed
/// a store directory through the CLI, clone it into a bundle. The one
/// fixture the `--load` suites (e2e_load, e2e_run_from_url,
/// e2e_serve_read_only) share — one copy, so the fixture cannot drift
/// between them. The caller's tempdir must outlive the runs.
#[allow(dead_code)]
pub fn person_bundle(home: &Path) -> std::path::PathBuf {
    let db = home.join("seed-db");
    seed_database(home, &db, &[], "CREATE (p:Person {name: 'Alice'})");
    let bundle = home.join("bundle.rl");
    let (status, _stdout, stderr) = run_cli_in_home(
        &["clone", db.to_str().unwrap(), bundle.to_str().unwrap()],
        home,
    );
    assert!(status.success(), "clone dir->bundle failed: {stderr}");
    bundle
}

/// The staged scratch path a remote `--load` announcement names (LABS-872,
/// ADR-0500): the one machine cut both remote suites share — the path
/// immediately follows `downloaded to `, up to the comma before the
/// disposition clause. Guards the parse: what it returns is a `.rl` bundle
/// path, so a wording change that breaks the cut fails loudly here instead
/// of passing vacuously.
#[allow(dead_code)]
pub fn staged_path(stderr: &str) -> std::path::PathBuf {
    let staged = stderr
        .lines()
        .find_map(|l| l.split_once("downloaded to "))
        .map(|(_, rest)| rest.split_once(',').expect("the disposition clause").0)
        .expect("the announcement names the staged path");
    assert!(staged.ends_with(".rl"), "a staged bundle path: {staged}");
    std::path::PathBuf::from(staged)
}

/// LABS-1120: the per-statement timing terminal line of a `--jsonl`
/// stream (the `--timings` opt-in, ADR-0581) is metadata, never a data
/// row — the one typed discriminator every JSONL-reading helper and e2e
/// comparison uses: the terminal line
/// is an object carrying exactly the timing rider's two keys (`timings`
/// and `planCache`, the formatter's one rider builder), so a data row
/// that happens to carry a `timings` column stays a row.
#[allow(dead_code)]
pub fn timing_terminal_line(line: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(map)) => {
            map.len() == 2 && map.contains_key("timings") && map.contains_key("planCache")
        }
        _ => false,
    }
}

/// The 250-row fixture the agent-mode and row-cap suites stream — the
/// shape the row cap cuts to 100 under a detected harness.
// Only those two suites use it; allow in the other test binaries.
#[allow(dead_code)]
pub const ROW_CAP_QUERY: &str = "UNWIND range(1, 250) AS i RETURN i";

/// The one suite runner the agent-mode and row-cap suites share: a
/// `--jsonl` invocation over [`ROW_CAP_QUERY`] on the pure-row default
/// (ADR-0581) — those suites count data rows and the rowCap record, never
/// the terminal timing line; the opt-in stream is pinned in `e2e_jsonl`.
/// One copy, so a prelude change lands once.
// Only those two suites run the row-cap stream; allow in the other test binaries.
#[allow(dead_code)]
pub fn run_row_cap(
    args: &[&str],
    envs: &[(&str, &str)],
) -> (std::process::ExitStatus, String, String) {
    let mut all = vec!["--jsonl"];
    all.extend_from_slice(args);
    all.push(ROW_CAP_QUERY);
    run_cli(CliRunOptions::new(&all).with_envs(envs))
}

/// Run one follow-up query against a persisted database with `--jsonl` and
/// return every row, parsed. `params` are the `--param name=value` arguments
/// the query takes; pass `&[]` for a query without parameters.
///
/// The one JSONL query reader: the importer e2e suites never parse the
/// output themselves. The args never carry `--timings`, so under ADR-0581's
/// default-off the stream is pure rows by construction — every line is a
/// data row.
// Only the importer e2e suites query a persisted database; allow in the others.
#[allow(dead_code)]
pub fn query_rows(
    home: &Path,
    db: &Path,
    params: &[String],
    cypher: &str,
) -> Vec<serde_json::Value> {
    let db = db.to_str().expect("the database path is UTF-8");
    let mut args: Vec<&str> = vec!["--jsonl", "--database", db];
    for param in params {
        args.push("--param");
        args.push(param);
    }
    args.push(cypher);
    let (status, stdout, stderr) = run_cli(CliRunOptions::new(&args).in_home(home));
    assert!(status.success(), "query failed: {stderr}\nquery: {cypher}");
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("bad JSON {line:?}: {error}"))
        })
        .collect()
}

/// The first row of [`query_rows`], as JSON.
#[allow(dead_code)]
pub fn query_row(home: &Path, db: &Path, params: &[String], cypher: &str) -> serde_json::Value {
    let rows = query_rows(home, db, params, cypher);
    rows.into_iter()
        .next()
        .unwrap_or_else(|| panic!("no rows for {cypher}"))
}

/// The one row of [`query_rows`] — the read-back shape for suites that
/// expect exactly one row (a second row is a failure, never silently the
/// first).
#[allow(dead_code)]
pub fn query_single_row(
    home: &Path,
    db: &Path,
    params: &[String],
    cypher: &str,
) -> serde_json::Value {
    let rows = query_rows(home, db, params, cypher);
    assert_eq!(rows.len(), 1, "expected one row from {cypher}: {rows:?}");
    rows.into_iter().next().expect("one row")
}

/// The single `n` column of a count/sum query.
#[allow(dead_code)]
pub fn query_count(home: &Path, db: &Path, params: &[String], cypher: &str) -> u64 {
    let rows = query_rows(home, db, params, cypher);
    assert_eq!(rows.len(), 1, "expected one row from {cypher}: {rows:?}");
    rows[0]["n"]
        .as_u64()
        .unwrap_or_else(|| panic!("`n` is not an unsigned integer in {cypher}: {}", rows[0]))
}

/// Run the CLI with the given args, a custom `$HOME`, and piped raw stdin
/// bytes (so callers can feed invalid UTF-8). Uses the same isolated-home /
/// cleared-telemetry-env setup as [`run_cli_in_home`]. Returns (exit status,
/// stdout, stderr).
// Only the general, telemetry, bundle and info E2E suites pipe stdin into an
// isolated home; allow until more do.
#[allow(dead_code)]
pub fn run_cli_stdin_in_home(
    args: &[&str],
    home: &Path,
    stdin: &[u8],
) -> (ExitStatus, String, String) {
    run_cli(CliRunOptions {
        args,
        home: Some(home),
        stdin: Some(stdin),
        ..Default::default()
    })
}

/// The snapshot codec pinned in a database directory's `manifest.json`.
// Only the durability and pull E2E suites read the manifest; allow until more do.
#[allow(dead_code)]
pub fn manifest_codec(db: &Path) -> String {
    parse_manifest_codec(&manifest_bytes(db))
}

/// The store's `manifest.json` bytes — the one read every manifest
/// consumer (the codec pin, the parsed document) decodes from.
fn manifest_bytes(db: &Path) -> Vec<u8> {
    std::fs::read(db.join("manifest.json")).expect("manifest.json reads")
}

/// The store's `manifest.json`, parsed — the one read the suite's
/// marker asserts share (the codec pin and the format version alike).
#[allow(dead_code)]
pub fn manifest_json(db: &Path) -> serde_json::Value {
    serde_json::from_slice(&manifest_bytes(db)).expect("manifest.json parses")
}

/// The snapshot codec pinned in a `.rl` bundle's `manifest.json`. Opening
/// the bundle takes its lock, so the CLI process must have exited.
#[allow(dead_code)]
pub fn bundle_manifest_codec(bundle: &Path) -> String {
    let vfs = relate_storage::BundleVfs::open_or_create(bundle).expect("bundle opens");
    parse_manifest_codec(&relate_storage::read_to_vec(&vfs, "manifest.json").unwrap())
}

#[allow(dead_code)]
fn parse_manifest_codec(bytes: &[u8]) -> String {
    let manifest: serde_json::Value = serde_json::from_slice(bytes).expect("manifest.json parses");
    manifest["codec"]
        .as_str()
        .expect("manifest.json has a string codec")
        .to_string()
}

/// Every command word with its compile gate, in the help's stable order:
/// the feature ↔ command mapping as data, the one table suites share.
// Only the help and bundle E2E suites list them; allow until more do.
#[allow(dead_code)]
pub fn command_gates() -> [(bool, &'static str); 13] {
    [
        (true, "query"),
        (true, "clone"),
        (cfg!(feature = "info"), "info"),
        (cfg!(feature = "pull"), "pull"),
        (cfg!(feature = "push"), "push"),
        (cfg!(feature = "skill"), "skill"),
        (cfg!(feature = "upgrade"), "upgrade"),
        (cfg!(feature = "telemetry"), "telemetry"),
        (true, "feedback"),
        (cfg!(feature = "extensions"), "run"),
        (cfg!(feature = "serve"), "serve"),
        (cfg!(feature = "agent-context"), "agent-context"),
        (cfg!(feature = "extensions"), "extension"),
    ]
}

/// The commands compiled into this build, in the help's stable order.
// Only the help and bundle E2E suites list them; allow until more do.
#[allow(dead_code)]
pub fn compiled_commands() -> Vec<&'static str> {
    command_gates()
        .into_iter()
        .filter_map(|(compiled_in, word)| compiled_in.then_some(word))
        .collect()
}

/// The compiled-in snapshot codecs as the help rows spell them, joined
/// with `|`: the codec set and its listing order come from the one typed
/// source — `compiled()` already excludes Parquet in a build without the
/// feature, and its declaration order pins the order — so a codec change
/// fails the help-golden suites without a golden re-capture.
// Only the help and bundle E2E suites list them; allow until more do.
#[allow(dead_code)]
pub fn compiled_codecs() -> String {
    relate::SnapshotCodecKind::compiled()
        .iter()
        .map(|codec| codec.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// The `Commands:` block of a top-level help output.
// Only the help and bundle E2E suites parse the top-level help; allow until
// more do.
#[allow(dead_code)]
pub fn commands_section(stdout: &str) -> &str {
    stdout
        .split("\n\n")
        .find(|chunk| chunk.starts_with("Commands:"))
        .unwrap_or_else(|| panic!("no Commands: section:\n{stdout}"))
}

/// The command words of a top-level help's `Commands:` block, in order.
// Only the help and bundle E2E suites parse the top-level help; allow until
// more do.
#[allow(dead_code)]
pub fn commands_section_words(stdout: &str) -> Vec<&str> {
    commands_section(stdout)
        .lines()
        .skip(1)
        .map(|line| line.split_whitespace().next().expect("command word"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::timing_terminal_line;

    /// LABS-1120: the typed discriminator — a data row whose column is
    /// literally named `timings` is a row, never the terminal line (the
    /// raw-substring filter this replaced dropped it), and only the
    /// rider's exact two-key object is the terminal line.
    #[test]
    fn a_data_row_with_a_timings_column_stays_a_row() {
        assert!(
            !timing_terminal_line(r#"{"timings": 1}"#),
            "a single `timings` column is a data row"
        );
        assert!(
            !timing_terminal_line(r#"{"timings": 1, "a": 2}"#),
            "a `timings` column beside another is a data row"
        );
        assert!(
            timing_terminal_line(r#"{"timings":{"elapsed_ms":0.5},"planCache":"miss"}"#),
            "the rider's exact two-key object is the terminal line"
        );
        assert!(!timing_terminal_line(r#"{"a": 1}"#), "a plain row is a row");
    }
}

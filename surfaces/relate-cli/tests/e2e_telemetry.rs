//! End-to-end tests for the `telemetry` positional lifecycle command
//! (ADR-0069 grammar, ADR-0079 §6): `relate telemetry on|off|status`.
//!
//! Executes the actual `relate` binary against a fake `$HOME` (tempdir)
//! so the XDG config dir (`$HOME/.config/relate/`) is isolated; the
//! support harness points the Mixpanel endpoint at a refused address so
//! an enabled run's drain thread stays offline, and sets
//! `RELATE_TELEMETRY=off` unless a test calls
//! [`with_default_telemetry`](support::CliRunOptions::with_default_telemetry)
//! or points at the mock sink.
//!
//! Delivery is the spool model (ADR-0545): a run's exit appends its
//! events to a local spool file — never a network wait — and the next
//! enabled run's drain thread claims and POSTs them. The second half of
//! this file runs the binary against a mock Mixpanel sink
//! ([`MockMixpanel`]): a tiny HTTP server on 127.0.0.1 that captures
//! `/track` request bodies so the tests can assert the exact events and
//! property keys the real binary emits (ADR-0079 §4), that every opt-out
//! layer sends nothing at all (ADR-0079 §6), and that spooled events
//! reach the sink through a follow-up run (ADR-0545 §2).

#![cfg(feature = "telemetry")]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitStatus;

mod support;

use support::{
    run_cli, spool_files_in, tempdir, CliRunOptions as Opts, MockMixpanel, SPOOL_FILE_PREFIX,
};

/// The one-time first-run notice text (must match main.rs's `FIRST_RUN_NOTICE`).
const NOTICE_FRAGMENT: &str = "relate collects anonymous usage metrics";

/// Read the persisted `telemetry` key from the fake home's config.json.
fn config_telemetry(home: &Path) -> Option<bool> {
    let path = home.join(".config").join("relate").join("config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("telemetry")?.as_bool()
}

// --- on / off persist the config key ---

#[test]
fn test_telemetry_off_persists_false() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["telemetry", "off"]).in_home(home.path()));
    assert!(status.success(), "off exited non-zero: {stderr}");
    assert_eq!(config_telemetry(home.path()), Some(false));
    assert!(
        stdout.contains("disabled"),
        "expected a confirmation on stdout, got: {stdout}"
    );
}

#[test]
fn test_telemetry_on_persists_true() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["telemetry", "on"]).in_home(home.path()));
    assert!(status.success(), "on exited non-zero: {stderr}");
    assert_eq!(config_telemetry(home.path()), Some(true));
    assert!(
        stdout.contains("enabled"),
        "expected a confirmation on stdout, got: {stdout}"
    );
}

#[test]
fn test_telemetry_on_after_off_flips_the_key() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "off"]).in_home(home.path()));
    assert!(status.success(), "off failed: {stderr}");
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "on"]).in_home(home.path()));
    assert!(status.success(), "on failed: {stderr}");
    assert_eq!(config_telemetry(home.path()), Some(true));
}

// --- status reports the effective state and deciding layer ---

#[test]
fn test_preverb_namespace_flag_does_not_reject_telemetry() {
    // Only `pull` rejects a pre-verb --namespace (it always writes neo4j);
    // other command words never open a store, so the flag stays harmless.
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--namespace", "system", "telemetry", "status"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    assert!(status.success(), "status exited non-zero: {stderr}");
    assert!(
        stdout.contains("enabled (default)"),
        "expected `enabled (default)`, got stdout: {stdout} stderr: {stderr}"
    );
}

#[test]
fn test_telemetry_status_default_is_enabled() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["telemetry", "status"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    assert!(status.success(), "status exited non-zero: {stderr}");
    assert!(
        stdout.contains("enabled (default)"),
        "expected `enabled (default)`, got stdout: {stdout} stderr: {stderr}"
    );
}

#[test]
fn test_telemetry_status_reflects_config_file() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["telemetry", "off"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    assert!(status.success(), "off failed: {stderr}");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["telemetry", "status"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    assert!(status.success(), "status exited non-zero: {stderr}");
    assert!(
        stdout.contains("disabled (config file)"),
        "expected `disabled (config file)`, got: {stdout}"
    );
}

#[test]
fn test_telemetry_status_env_overrides_config_file() {
    let home = tempdir();
    // Config says on; RELATE_TELEMETRY=0 must win (ADR-0079 §6).
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "on"]).in_home(home.path()));
    assert!(status.success(), "on failed: {stderr}");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["telemetry", "status"])
            .in_home(home.path())
            .with_envs(&[("RELATE_TELEMETRY", "0")]),
    );
    assert!(status.success(), "status exited non-zero: {stderr}");
    assert!(
        stdout.contains("disabled (RELATE_TELEMETRY)"),
        "expected `disabled (RELATE_TELEMETRY)`, got: {stdout}"
    );
}

#[test]
fn test_telemetry_status_do_not_track_wins() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["telemetry", "status"])
            .in_home(home.path())
            .with_envs(&[("DO_NOT_TRACK", "1"), ("RELATE_TELEMETRY", "1")]),
    );
    assert!(status.success(), "status exited non-zero: {stderr}");
    assert!(
        stdout.contains("disabled (DO_NOT_TRACK)"),
        "expected `disabled (DO_NOT_TRACK)`, got: {stdout}"
    );
}

// --- usage guards for the positional grammar ---

#[test]
fn test_telemetry_requires_action() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("telemetry requires an action (on, off, or status)"),
        "stderr should demand an action: {stderr}"
    );
    assert!(
        stderr.contains("Usage: relate"),
        "usage error should print usage: {stderr}"
    );
}

#[test]
fn test_telemetry_unknown_action_is_usage_error() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "foo"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to telemetry: foo"),
        "stderr should reject the argument: {stderr}"
    );
}

#[test]
fn test_telemetry_rejects_extra_arguments_after_action() {
    let home = tempdir();
    let (status, _, stderr) =
        run_cli(Opts::new(&["telemetry", "on", ":memory:"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to telemetry: :memory:"),
        "stderr should reject the extra argument: {stderr}"
    );
}

// --- paths that look like the verb stay database paths (ADR-0069) ---

#[test]
fn test_dot_slash_telemetry_is_a_database_path_not_command_mode() {
    let home = tempdir();
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "./telemetry", "RETURN 1 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "./telemetry should open as a database path, got code {:?}, stderr: {stderr}",
        status.code()
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        dir.path().join("telemetry").is_dir(),
        "./telemetry should have been created as a database directory"
    );
    assert!(
        !stderr.contains("telemetry requires an action"),
        "./telemetry must not enter telemetry command mode: {stderr}"
    );
}

#[test]
fn test_database_flag_accepts_telemetry_as_store_path() {
    let home = tempdir();
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "telemetry", "RETURN 1 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "--database telemetry should open as a database path, got code {:?}, stderr: {stderr}",
        status.code()
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        dir.path().join("telemetry").is_dir(),
        "`--database telemetry` should have created a database directory named telemetry"
    );
}

// --- help lists the new command ---

#[test]
fn test_help_lists_telemetry_command() {
    let home = tempdir();
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["telemetry", "--help"]).in_home(home.path()));
    assert!(
        status.success(),
        "telemetry --help exited non-zero: {stderr}"
    );
    assert!(
        stdout.contains("relate telemetry on|off|status"),
        "telemetry --help should list the telemetry synopsis, got: {stdout}"
    );
}

// === mock Mixpanel sink: real HTTP delivery against 127.0.0.1 ===

/// Base dimensions on every event (ADR-0079 §4).
const BASE_KEYS: [&str; 9] = [
    "token",
    "time",
    "distinct_id",
    "$insert_id",
    "$os",
    "arch",
    "version",
    "uptime",
    "invoker",
];

/// Run `relate 'RETURN 1'` against the mock sink.
fn run_oneshot_against_mock(mock: &MockMixpanel, home: &Path) -> (ExitStatus, String, String) {
    run_cli(
        Opts::new(&["RETURN 1"])
            .in_home(home)
            .with_endpoint(&mock.url()),
    )
}

/// The exact property-key set of one captured event envelope.
fn property_keys(event: &serde_json::Value) -> BTreeSet<String> {
    event["properties"]
        .as_object()
        .expect("properties must be an object")
        .keys()
        .cloned()
        .collect()
}

/// The documented key set: base dimensions plus the event-specific keys.
fn expected_keys(event_specific: &[&str]) -> BTreeSet<String> {
    BASE_KEYS
        .iter()
        .chain(event_specific.iter())
        .map(|s| (*s).to_string())
        .collect()
}

/// Run a oneshot (asserting it), then deliver its spooled events through
/// follow-up runs until `pred` holds — the shared delivery loop is
/// [`MockMixpanel::deliver_spooled`]. The run under test is the asserted
/// oneshot; the follow-ups run a work query instead, because a
/// follow-up's whole job is to stay alive long enough for its drain
/// thread to POST, and engine work buys that window even on a saturated
/// runner.
fn oneshots_until_delivered(
    mock: &MockMixpanel,
    home: &Path,
    pred: impl Fn(&[serde_json::Value]) -> bool,
) -> Vec<serde_json::Value> {
    let run_under_test = || {
        let (status, stdout, stderr) = run_oneshot_against_mock(mock, home);
        assert!(status.success(), "oneshot exited non-zero: {stderr}");
        assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    };
    let followup = support::FollowupRun {
        args: &[support::DELIVERY_WORK_QUERY],
        envs: &[],
        home: Some(home),
        cwd: None,
    };
    mock.deliver_spooled(run_under_test, followup, pred)
}

/// Run a oneshot and deliver its spooled events through follow-up runs
/// until the mock holds an event named `name`; return the captured
/// events.
fn run_and_collect(mock: &MockMixpanel, name: &str) -> Vec<serde_json::Value> {
    let home = tempdir();
    oneshots_until_delivered(mock, home.path(), |events| {
        events.iter().any(|e| e["event"] == name)
    })
}

#[test]
fn test_mock_sink_receives_startup_and_query_events() {
    // Both events of the first run arrive together through a follow-up
    // run's drain (ADR-0545 §2), not during the run that emitted them.
    let mock = MockMixpanel::start();
    let home = tempdir();
    oneshots_until_delivered(&mock, home.path(), |events| {
        events.iter().any(|e| e["event"] == "RELATE-CLI_STARTUP")
            && events.iter().any(|e| e["event"] == "RELATE-CLI_QUERY")
    });
}

#[test]
fn test_startup_event_property_keys_match_taxonomy() {
    let mock = MockMixpanel::start();
    let events = run_and_collect(&mock, "RELATE-CLI_STARTUP");
    let startup = events
        .iter()
        .find(|e| e["event"] == "RELATE-CLI_STARTUP")
        .expect("startup event");
    // Exact key set (ADR-0079 §4): base dimensions + `kind`, nothing else.
    assert_eq!(property_keys(startup), expected_keys(&["kind"]));
    assert_eq!(startup["properties"]["kind"], "oneshot");
}

#[test]
fn test_query_event_property_keys_match_taxonomy() {
    let mock = MockMixpanel::start();
    let events = run_and_collect(&mock, "RELATE-CLI_QUERY");
    let query = events
        .iter()
        .find(|e| e["event"] == "RELATE-CLI_QUERY")
        .expect("query event");
    // Exact key set (ADR-0079 §4): base dimensions + the four query keys.
    assert_eq!(
        property_keys(query),
        expected_keys(&["success", "source", "storage", "output_mode"])
    );
    assert_eq!(query["properties"]["success"], true);
    assert_eq!(query["properties"]["source"], "oneshot");
    assert_eq!(query["properties"]["storage"], "memory");
    // Piped stdout defaults to JSONL (ADR-0107); telemetry and the formatter
    // resolve the mode from the same value.
    assert_eq!(query["properties"]["output_mode"], "jsonl");
}

#[test]
fn test_command_event_reaches_mock_with_documented_keys() {
    let mock = MockMixpanel::start();
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["telemetry", "on"])
            .in_home(home.path())
            .with_endpoint(&mock.url()),
    );
    assert!(status.success(), "telemetry on exited non-zero: {stderr}");
    // The COMMAND event spools at that run's exit; a follow-up enabled
    // run's drain delivers it (ADR-0545 §2).
    let events = oneshots_until_delivered(&mock, home.path(), |events| {
        events.iter().any(|e| e["event"] == "RELATE-CLI_COMMAND")
    });
    let command = events
        .iter()
        .find(|e| e["event"] == "RELATE-CLI_COMMAND")
        .expect("command event");
    assert_eq!(
        property_keys(command),
        expected_keys(&["command", "action", "success"])
    );
    assert_eq!(command["properties"]["command"], "telemetry");
    assert_eq!(command["properties"]["action"], "on");
    assert_eq!(command["properties"]["success"], true);
}

// --- every opt-out layer independently sends zero requests (ADR-0079 §6) ---

#[test]
fn test_no_requests_when_an_env_opt_out_layer_is_set() {
    // Each environment opt-out layer independently silences a run
    // (ADR-0079 §6); the config-file layer is covered below.
    for (var, value) in [("DO_NOT_TRACK", "1"), ("RELATE_TELEMETRY", "0")] {
        let mock = MockMixpanel::start();
        let home = tempdir();
        let (status, _, stderr) = run_cli(
            Opts::new(&["RETURN 1"])
                .in_home(home.path())
                .with_endpoint(&mock.url())
                .with_envs(&[(var, value)]),
        );
        assert!(
            status.success(),
            "oneshot with {var}={value} exited non-zero: {stderr}"
        );
        mock.assert_quiet(&format!("{var}={value}"));
    }
}

#[test]
fn test_telemetry_off_persists_config_and_silences_later_runs() {
    let mock = MockMixpanel::start();
    let home = tempdir();
    // `telemetry off` writes telemetry:false to config.json in the fake HOME
    // (this run itself is telemetry-off through the harness default, so it
    // emits nothing).
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "off"]).in_home(home.path()));
    assert!(status.success(), "off failed: {stderr}");
    assert_eq!(
        config_telemetry(home.path()),
        Some(false),
        "telemetry off must persist telemetry:false"
    );
    // A subsequent run with the same HOME sends nothing, even with the
    // endpoint pointed at a reachable mock.
    let (status, _, stderr) = run_oneshot_against_mock(&mock, home.path());
    assert!(status.success(), "oneshot exited non-zero: {stderr}");
    mock.assert_quiet("a run with telemetry off persisted in config");
}

// --- a rejected cleartext non-loopback endpoint warns and sends nothing ---

#[test]
fn test_rejects_nonloopback_http_endpoint_with_warning() {
    let mock = MockMixpanel::start();
    let home = tempdir();
    // `http://` to a non-loopback host is rejected by ADR-0263: the binary
    // must warn on stderr, disable telemetry, and still run the query with
    // exit code 0. The harness default endpoint is overridden last, so the
    // env entry here wins.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["RETURN 1"])
            .in_home(home.path())
            .with_default_telemetry()
            .with_envs(&[("RELATE_MIXPANEL_ENDPOINT", "http://203.0.0.1:9")]),
    );
    assert_eq!(
        status.code(),
        Some(0),
        "a rejected endpoint override must not fail the run: {stderr}"
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        stderr.contains("RELATE_MIXPANEL_ENDPOINT"),
        "warning must name the rejected env var, got: {stderr}"
    );
    assert!(
        stderr.contains("loopback"),
        "warning must explain the loopback restriction, got: {stderr}"
    );
    // Telemetry is disabled for the run, so nothing is ever delivered.
    mock.assert_quiet("a run with a rejected non-loopback endpoint");
}

// --- first-run notice: once per install, regardless of TTY (ADR-0079 §6) ---

#[test]
fn test_first_run_notice_prints_without_tty_and_only_once() {
    let home = tempdir();
    let script = "RETURN 1 AS n;";
    // Piped stdin: not a TTY, yet the notice must still be emitted.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query"])
            .in_home(home.path())
            .with_default_telemetry()
            .with_stdin(script.as_bytes()),
    );
    assert!(status.success(), "first piped run failed: {stderr}");
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        stderr.contains(NOTICE_FRAGMENT),
        "first piped run must print the notice to stderr, got: {stderr}"
    );

    // A second run with the same HOME claims nothing: the notice stays quiet.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query"])
            .in_home(home.path())
            .with_default_telemetry()
            .with_stdin(script.as_bytes()),
    );
    assert!(status.success(), "second piped run failed: {stderr}");
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        !stderr.contains(NOTICE_FRAGMENT),
        "the notice is claim-once and must not repeat, got: {stderr}"
    );
}

#[test]
fn test_first_run_notice_suppressed_when_telemetry_off() {
    let home = tempdir();
    // Opt out via the persisted config file, then run with piped stdin.
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "off"]).in_home(home.path()));
    assert!(status.success(), "telemetry off failed: {stderr}");
    let (status, _, stderr) = run_cli(
        Opts::new(&["query"])
            .in_home(home.path())
            .with_stdin(b"RETURN 1;"),
    );
    assert!(status.success(), "opted-out piped run failed: {stderr}");
    assert!(
        !stderr.contains(NOTICE_FRAGMENT),
        "an opted-out install must never print the notice, got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn test_telemetry_state_and_config_files_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempdir();
    // Enabled first run writes state.json (install UUID); `telemetry off`
    // writes config.json.
    let (status, _, stderr) = run_cli(
        Opts::new(&["RETURN 1;"])
            .in_home(home.path())
            .with_default_telemetry(),
    );
    assert!(status.success(), "state-writing run failed: {stderr}");
    // The same run's exit append wrote its spool file (ADR-0545 §1);
    // assert its privacy BEFORE the `telemetry off` run below, which
    // legitimately clears it (ADR-0545 §5).
    let dir = home.path().join(".config").join("relate");
    let spool = spool_files_in(&dir);
    assert_eq!(
        spool.len(),
        1,
        "the enabled run must have spooled: {spool:?}"
    );
    let mode = std::fs::metadata(&spool[0])
        .expect("spool file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the spool must be owner-only, got {mode:o}");
    let (status, _, stderr) = run_cli(Opts::new(&["telemetry", "off"]).in_home(home.path()));
    assert!(status.success(), "telemetry off failed: {stderr}");

    for name in ["state.json", "config.json"] {
        let path = dir.join(name);
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{} missing: {e}", path.display()))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "{name} must be owner-only (0600), got {mode:o}"
        );
    }
}

// --- telemetry must be invisible to the user (ADR-0079 §5) ---

#[test]
fn test_stdout_and_exit_code_identical_with_telemetry_disabled() {
    let mock = MockMixpanel::start();
    let home_on = tempdir();
    // Sanity first: the enabled run's spooled events really do reach the
    // mock through a follow-up run (ADR-0545 §2), so the comparison below
    // is not vacuous.
    let delivered = oneshots_until_delivered(&mock, home_on.path(), |events| !events.is_empty());
    assert!(!delivered.is_empty(), "the enabled run sent no events");

    let (status_on, stdout_on, stderr_on) = run_oneshot_against_mock(&mock, home_on.path());
    assert!(status_on.success(), "enabled run failed: {stderr_on}");

    let home_off = tempdir();
    let (status_off, stdout_off, stderr_off) = run_cli(
        Opts::new(&["RETURN 1"])
            .in_home(home_off.path())
            .with_endpoint(&mock.url())
            .with_envs(&[("RELATE_TELEMETRY", "0")]),
    );
    assert!(status_off.success(), "disabled run failed: {stderr_off}");

    assert_eq!(status_on.code(), status_off.code());
    // LABS-1120: the timing line carries per-run numbers, so the identity
    // comparison reads the data rows alone (the one typed discriminator,
    // `support::timing_terminal_line`).
    fn data(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .filter(|line| !support::timing_terminal_line(line))
            .collect()
    }
    assert_eq!(
        data(&stdout_on),
        data(&stdout_off),
        "stdout must be identical with and without telemetry"
    );
}

// --- the spool against the opt-out layers (ADR-0545 §5) ---

#[test]
fn test_an_opted_out_run_neither_drains_nor_clears_the_spool() {
    let mock = MockMixpanel::start();
    let home = tempdir();
    // Seed a spool file exactly as an earlier enabled run's exit append
    // would have left it, named from the one pinned prefix.
    let cfg = home.path().join(".config").join("relate");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let seeded = format!("{SPOOL_FILE_PREFIX}seeded.jsonl");
    std::fs::write(cfg.join(&seeded), "{\"event\":\"SEEDED\"}\n").expect("seed spool");
    // An opted-out run touches nothing: no drain, no clear — the spool
    // waits for a later enabled run.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["RETURN 1"])
            .in_home(home.path())
            .with_endpoint(&mock.url())
            .with_envs(&[("DO_NOT_TRACK", "1")]),
    );
    assert!(status.success(), "opted-out run failed: {stderr}");
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    mock.assert_quiet("an opted-out run");
    assert!(
        cfg.join(&seeded).exists(),
        "an opted-out run must leave the spool untouched"
    );
}

#[test]
fn test_telemetry_off_clears_a_seeded_spool_and_records_nothing_of_its_own() {
    // The ordered transition (ADR-0545 §5), end to end on a run that
    // STARTS enabled (with_default_telemetry, no forced off): the off
    // command disarms the pipeline before persisting, so the run that
    // turns telemetry off transmits nothing (the drain never starts),
    // clears the pending seed, and leaves no spool file of its own.
    let mock = MockMixpanel::start();
    let home = tempdir();
    let cfg = home.path().join(".config").join("relate");
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let seeded = format!("{SPOOL_FILE_PREFIX}seeded.jsonl");
    std::fs::write(cfg.join(&seeded), "{\"event\":\"SEEDED\"}\n").expect("seed spool");
    let (status, _, stderr) = run_cli(
        Opts::new(&["telemetry", "off"])
            .in_home(home.path())
            .with_endpoint(&mock.url()),
    );
    assert!(status.success(), "telemetry off failed: {stderr}");
    assert_eq!(config_telemetry(home.path()), Some(false));
    assert!(
        support::pending_telemetry_files_in(&cfg).is_empty(),
        "off must clear the seed and write no new spool file of its own"
    );
    mock.assert_quiet("the run that turns telemetry off");
}

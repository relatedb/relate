//! End-to-end tests for the `skill` positional lifecycle command (ADR-0069):
//! `relate skill install|remove [--agent NAME] [--force]`.
//!
//! Executes the actual `relate` binary against a fake `$HOME` (tempdir
//! with fake agent detect dirs) so install/remove/scoping/error paths are
//! verified without touching a real user home.

#![cfg(feature = "skill")]

use std::path::Path;

mod support;

use support::{run_cli, CliRunOptions as Opts};

/// Create a fake home with a detect dir for the given agent dirs (e.g.
/// `.claude`, `.cursor`).
fn fake_home_with(agent_dirs: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    for d in agent_dirs {
        std::fs::create_dir_all(tmp.path().join(d)).expect("create agent dir");
    }
    tmp
}

fn installed_skill_md(home: &Path, agent_dir: &str) -> Option<std::path::PathBuf> {
    let p = skill_dir(home, agent_dir).join("SKILL.md");
    p.is_file().then_some(p)
}

/// `<home>/<agent_dir>/skills/relate` — the bundle install destination.
fn skill_dir(home: &Path, agent_dir: &str) -> std::path::PathBuf {
    home.join(agent_dir).join("skills").join("relate")
}

/// Create an existing `<skills dir>/relate` with content but no
/// `.relate-installed` marker, simulating user-authored/third-party
/// content. Returns the directory and the sentinel file path.
fn precreate_unmarked_skill_dir(
    home: &Path,
    agent_dir: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = skill_dir(home, agent_dir);
    std::fs::create_dir_all(&dir).expect("create unmarked skill dir");
    let sentinel = dir.join("sentinel.txt");
    std::fs::write(&sentinel, "user-authored").expect("write sentinel");
    (dir, sentinel)
}

const MARKER: &str = ".relate-installed";

// --- install ---

#[test]
fn test_skill_install_writes_bundle_to_all_detected_agents() {
    let home = fake_home_with(&[".claude", ".cursor"]);
    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install exited non-zero: {stderr}");

    for agent_dir in [".claude", ".cursor"] {
        let skill = installed_skill_md(home.path(), agent_dir)
            .unwrap_or_else(|| panic!("SKILL.md missing under {agent_dir}"));
        assert!(skill.parent().unwrap().join("references").is_dir());
        for ref_file in [
            "vector.md",
            "spatial.md",
            "operations.md",
            "agent-safety.md",
            "pull.md",
        ] {
            assert!(
                skill
                    .parent()
                    .unwrap()
                    .join("references")
                    .join(ref_file)
                    .is_file(),
                "{ref_file} reference missing"
            );
        }
    }
}

/// Stock PowerShell and cmd on Windows set `%USERPROFILE%` but no `$HOME`:
/// `skill install --agent claude-code` must still resolve the agent's
/// directory, under the profile. Runs on every host (the profile fallback
/// is not Windows-only), with `HOME` removed from the child.
#[test]
fn test_skill_install_resolves_userprofile_without_home() {
    let profile = fake_home_with(&[]);
    let profile_str = profile.path().to_str().expect("utf-8 tempdir");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["skill", "install", "--agent", "claude-code"])
            .without_envs(&["HOME"])
            .with_envs(&[("USERPROFILE", profile_str)]),
    );
    assert!(status.success(), "install exited non-zero: {stderr}");
    assert!(
        installed_skill_md(profile.path(), ".claude").is_some(),
        "SKILL.md missing under %USERPROFILE%\\.claude: {stderr}"
    );
}

#[test]
fn test_skill_install_stamps_binary_version() {
    let home = fake_home_with(&[".claude"]);
    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install exited non-zero: {stderr}");

    let skill = installed_skill_md(home.path(), ".claude").expect("SKILL.md");
    let content = std::fs::read_to_string(&skill).expect("read SKILL.md");
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        content.contains(&format!("version: {version}")),
        "SKILL.md missing version line {version}: got:\n{content}"
    );
    assert!(
        !content.contains("{{VERSION}}"),
        "{{VERSION}} placeholder leaked into installed SKILL.md"
    );
}

// --- remove ---

#[test]
fn test_skill_remove_deletes_bundle_and_is_idempotent() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install failed: {stderr}");

    let (status, _, stderr) = run_cli(Opts::new(&["skill", "remove"]).in_home(home.path()));
    assert!(status.success(), "remove exited non-zero: {stderr}");
    assert!(
        installed_skill_md(home.path(), ".claude").is_none(),
        "bundle not removed"
    );

    // Idempotent: removing again exits 0.
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "remove"]).in_home(home.path()));
    assert!(
        status.success(),
        "idempotent remove exited non-zero: {stderr}"
    );
}

// --- ownership marker / unmarked dir protection (LABS-84) ---

#[test]
fn test_skill_install_writes_ownership_marker() {
    let home = fake_home_with(&[".claude"]);
    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install exited non-zero: {stderr}");
    assert!(
        skill_dir(home.path(), ".claude").join(MARKER).is_file(),
        "install must write the {MARKER} ownership marker"
    );
}

#[test]
fn test_skill_remove_marked_dir_succeeds() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install failed: {stderr}");
    let dir = skill_dir(home.path(), ".claude");
    assert!(dir.join(MARKER).is_file(), "marked install expected");

    let (status, _, stderr) = run_cli(Opts::new(&["skill", "remove"]).in_home(home.path()));
    assert!(status.success(), "marked remove exited non-zero: {stderr}");
    assert!(!dir.exists(), "marked dir should be removed");
}

#[test]
fn test_skill_remove_refuses_unmarked_dir() {
    let home = fake_home_with(&[".claude"]);
    let (dir, sentinel) = precreate_unmarked_skill_dir(home.path(), ".claude");

    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "remove"]).in_home(home.path()));
    assert_ne!(
        status.code(),
        Some(0),
        "unmarked remove must fail: {stderr}"
    );
    assert!(
        stderr.contains(MARKER),
        "stderr must name the marker file {MARKER}: {stderr}"
    );
    assert!(dir.exists(), "unmarked dir must not be deleted");
    assert!(
        sentinel.is_file(),
        "sentinel inside the unmarked dir must survive"
    );
}

#[test]
fn test_skill_remove_force_deletes_unmarked_dir() {
    let home = fake_home_with(&[".claude"]);
    let (dir, _sentinel) = precreate_unmarked_skill_dir(home.path(), ".claude");

    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "remove", "--force"]).in_home(home.path()));
    assert!(status.success(), "forced remove exited non-zero: {stderr}");
    assert!(!dir.exists(), "forced remove must delete the unmarked dir");
}

#[test]
fn test_skill_install_refuses_unmarked_dir() {
    let home = fake_home_with(&[".claude"]);
    let (dir, sentinel) = precreate_unmarked_skill_dir(home.path(), ".claude");

    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert_ne!(
        status.code(),
        Some(0),
        "unmarked install must fail: {stderr}"
    );
    assert!(
        stderr.contains(MARKER),
        "stderr must name the marker file {MARKER}: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel).expect("read sentinel"),
        "user-authored",
        "unmarked content must be untouched"
    );
    assert!(!dir.join("SKILL.md").exists(), "bundle must not be written");
    assert!(!dir.join(MARKER).exists(), "marker must not be written");
}

/// Regression (CI stellar-review, Medium): `run_skill` used to interleave the
/// ownership check with per-agent mutation, so an unmarked *later* target could
/// leave an earlier target half-applied. Detection order is `claude-code` then
/// `cursor` (AGENTS order), so here `claude-code` is markable while `cursor` is
/// unmarked: the pre-flight must refuse before touching `claude-code`.
#[test]
fn test_skill_install_preflight_leaves_earlier_target_untouched() {
    let home = fake_home_with(&[".claude", ".cursor"]);
    // The later-detected agent (cursor) holds unmarked user content.
    let (unmarked_dir, sentinel) = precreate_unmarked_skill_dir(home.path(), ".cursor");

    let (status, _stdout, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert_ne!(
        status.code(),
        Some(0),
        "install over an unmarked target must fail: {stderr}"
    );
    assert!(
        stderr.contains(MARKER),
        "stderr must name the marker file {MARKER}: {stderr}"
    );

    // The earlier (markable) target must be completely untouched: no bundle,
    // no marker — the command aborted during pre-flight.
    let earlier = skill_dir(home.path(), ".claude");
    assert!(
        !earlier.exists(),
        "pre-flight refusal must not create the earlier target: {} exists",
        earlier.display()
    );

    // The unmarked target's content also survives.
    assert!(unmarked_dir.exists(), "unmarked dir must not be deleted");
    assert!(
        sentinel.is_file(),
        "sentinel inside the unmarked dir must survive"
    );
}

#[test]
fn test_skill_install_force_overwrites_unmarked_dir() {
    let home = fake_home_with(&[".claude"]);
    let (dir, _sentinel) = precreate_unmarked_skill_dir(home.path(), ".claude");

    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", "--force"]).in_home(home.path()));
    assert!(status.success(), "forced install exited non-zero: {stderr}");
    assert!(
        dir.join(MARKER).is_file(),
        "forced install must write the marker"
    );
    assert!(
        dir.join("SKILL.md").is_file(),
        "forced install must write bundle"
    );
    assert!(
        !dir.join("sentinel.txt").exists(),
        "forced install must clean the previous contents"
    );
}

#[test]
fn test_skill_marked_reinstall_succeeds() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "first install failed: {stderr}");
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(
        status.success(),
        "marked reinstall (no --force) exited non-zero: {stderr}"
    );
}

#[test]
fn test_skill_flags_combine_in_either_order() {
    for args in [
        ["skill", "install", "--force", "--agent", "claude-code"],
        ["skill", "install", "--agent", "claude-code", "--force"],
    ] {
        let home = fake_home_with(&[".claude"]);
        let (status, _, stderr) = run_cli(Opts::new(&args).in_home(home.path()));
        assert!(status.success(), "flags {args:?} failed: {stderr}");
        assert!(
            skill_dir(home.path(), ".claude").join(MARKER).is_file(),
            "flags {args:?} did not install the marker"
        );
    }
}

#[test]
fn test_skill_help_mentions_force() {
    let home = fake_home_with(&[]);
    let (status, stdout, stderr) = run_cli(Opts::new(&["skill", "--help"]).in_home(home.path()));
    assert!(status.success(), "skill --help exited non-zero: {stderr}");
    assert!(stderr.is_empty(), "skill --help stderr: {stderr}");
    assert!(
        stdout.contains("--force"),
        "skill --help must document --force: {stdout}"
    );
    assert!(
        stdout.contains("skill install|remove"),
        "skill --help must show the skill usage line: {stdout}"
    );
}

// --- --agent scoping ---

#[test]
fn test_skill_install_agent_scoping_writes_only_that_agent() {
    let home = fake_home_with(&[".claude", ".cursor"]);
    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", "--agent", "cursor"]).in_home(home.path()));
    assert!(status.success(), "install exited non-zero: {stderr}");

    assert!(
        installed_skill_md(home.path(), ".cursor").is_some(),
        "cursor skill missing"
    );
    assert!(
        installed_skill_md(home.path(), ".claude").is_none(),
        "claude skill should NOT be installed"
    );
}

#[test]
fn test_skill_install_agent_is_case_insensitive() {
    let home = fake_home_with(&[".claude"]);
    // Case-insensitive agent lookup matches neo4j-cli semantics: the
    // --agent value is matched case-insensitively against the catalog.
    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", "--agent", "Claude-Code"]).in_home(home.path()));
    assert!(status.success(), "install exited non-zero: {stderr}");
    assert!(
        installed_skill_md(home.path(), ".claude").is_some(),
        "case-insensitive target not installed"
    );
}

// --- error paths ---

#[test]
fn test_skill_unknown_agent_is_usage_error() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", "--agent", "bogus"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unknown agent") && stderr.contains("claude-code"),
        "stderr should name the unknown agent and list valid agents: {stderr}"
    );
}

#[test]
fn test_skill_no_agents_detected_is_runtime_error() {
    let home = fake_home_with(&[]); // no agent dirs
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert_eq!(status.code(), Some(1), "expected runtime exit 1: {stderr}");
    assert!(
        stderr.contains("no supported agents detected"),
        "stderr should report no agents: {stderr}"
    );
}

// --- usage guards for the positional grammar ---

#[test]
fn test_skill_requires_action() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["skill"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("skill requires an action (install, remove, list or check)"),
        "stderr should demand an action: {stderr}"
    );
}

#[test]
fn test_skill_invalid_action_is_unexpected_argument() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["skill", "frobnicate"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to skill: frobnicate"),
        "stderr should reject the argument: {stderr}"
    );
}

#[test]
fn test_skill_rejects_extra_arguments_after_action() {
    let home = fake_home_with(&[".claude"]);
    // The skill command grammar is exactly `skill install|remove [--agent
    // NAME]`; a database path or query after it is a usage error.
    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", ":memory:", "RETURN 1"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to skill: :memory:"),
        "stderr should reject the extra argument: {stderr}"
    );
}

#[test]
fn test_skill_agent_flag_requires_value() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) =
        run_cli(Opts::new(&["skill", "install", "--agent"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(stderr.contains("requires a value"), "{}", stderr);
}

// --- removed flags: generic unknown-flag path, no migration hints (ADR-0069) ---

#[test]
fn test_removed_skill_flag_is_a_generic_unknown_flag() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["--skill", "install"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unknown flag: --skill") && stderr.contains("Usage: relate"),
        "removed --skill must hit the generic unknown-flag path: {stderr}"
    );
    // ADR-0069: no migration-hint special-casing for the removed spelling.
    let lower = stderr.to_lowercase();
    assert!(
        !lower.contains("did you mean") && !lower.contains("hint") && !lower.contains("instead"),
        "no migration hint may be printed for --skill: {stderr}"
    );
}

#[test]
fn test_removed_agent_flag_is_a_generic_unknown_flag() {
    let home = fake_home_with(&[".claude"]);
    let (status, _, stderr) = run_cli(Opts::new(&["--agent", "cursor"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unknown flag: --agent") && stderr.contains("Usage: relate"),
        "removed --agent must hit the generic unknown-flag path: {stderr}"
    );
    let lower = stderr.to_lowercase();
    assert!(
        !lower.contains("did you mean") && !lower.contains("hint") && !lower.contains("instead"),
        "no migration hint may be printed for --agent: {stderr}"
    );
}

// --- paths that look like the verb stay database paths (ADR-0069) ---

#[test]
fn test_dot_slash_skill_is_a_database_path_not_command_mode() {
    let home = fake_home_with(&[]);
    let dir = tempfile::tempdir().expect("tempdir");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "./skill", "RETURN 1 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "./skill should open as a database path, got code {:?}, stderr: {stderr}",
        status.code()
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        dir.path().join("skill").is_dir(),
        "./skill should have been created as a database directory"
    );
    assert!(
        !stderr.contains("skill requires an action"),
        "./skill must not enter skill command mode: {stderr}"
    );
}

#[test]
fn test_database_flag_accepts_skill_as_store_path() {
    let home = fake_home_with(&[]);
    let dir = tempfile::tempdir().expect("tempdir");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "skill", "RETURN 1 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "--database skill should open as a database path, got code {:?}, stderr: {stderr}",
        status.code()
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        dir.path().join("skill").is_dir(),
        "`--database skill` should have created a database directory named skill"
    );
}

// --- list / check (ADR-0242, LABS-236) ---

#[test]
fn test_skill_list_reports_install_state_per_agent() {
    let home = fake_home_with(&[".claude", ".cursor"]);
    let (status, stdout, stderr) = run_cli(Opts::new(&["skill", "list"]).in_home(home.path()));
    assert!(status.success(), "skill list failed: {stderr}");
    assert!(
        stdout.contains("claude-code: not installed"),
        "expected not-installed line, got: {stdout}"
    );
    assert!(
        stdout.contains("cursor: not installed"),
        "expected cursor line, got: {stdout}"
    );

    let (status, _out, stderr) =
        run_cli(Opts::new(&["skill", "install", "--agent", "claude-code"]).in_home(home.path()));
    assert!(status.success(), "install failed: {stderr}");
    let (status, stdout, stderr) = run_cli(Opts::new(&["skill", "list"]).in_home(home.path()));
    assert!(status.success(), "skill list failed: {stderr}");
    assert!(stdout.contains("claude-code: installed"), "got: {stdout}");
    assert!(stdout.contains("cursor: not installed"), "got: {stdout}");
}

#[test]
fn test_skill_check_clean_then_drifted() {
    let home = fake_home_with(&[".claude"]);
    let (status, _out, stderr) = run_cli(Opts::new(&["skill", "install"]).in_home(home.path()));
    assert!(status.success(), "install failed: {stderr}");

    // A fresh install of this binary's bundle is clean.
    let (status, stdout, stderr) = run_cli(Opts::new(&["skill", "check"]).in_home(home.path()));
    assert!(
        status.success(),
        "check of a fresh install failed: {stderr}"
    );
    assert!(stdout.contains("claude-code: up to date"), "got: {stdout}");

    // Touch a reference file and add a stray one: both are drift, exit 1.
    let dir = skill_dir(home.path(), ".claude");
    std::fs::write(dir.join("references").join("vector.md"), "tampered")
        .expect("tamper reference file");
    std::fs::write(dir.join("stray.txt"), "stray").expect("write stray file");
    let (status, stdout, _stderr) = run_cli(Opts::new(&["skill", "check"]).in_home(home.path()));
    assert_eq!(status.code(), Some(1), "drift must exit 1: {stdout}");
    assert!(stdout.contains("claude-code: drifted"), "got: {stdout}");
    assert!(
        stdout.contains("differs: references/vector.md"),
        "got: {stdout}"
    );
    assert!(stdout.contains("extra: stray.txt"), "got: {stdout}");

    // Removing the bundle returns check to the clean (not installed) state.
    let (status, _out, stderr) = run_cli(Opts::new(&["skill", "remove"]).in_home(home.path()));
    assert!(status.success(), "remove failed: {stderr}");
    let (status, stdout, _stderr) = run_cli(Opts::new(&["skill", "check"]).in_home(home.path()));
    assert!(
        status.success(),
        "check with nothing installed must pass: {stdout}"
    );
    assert!(
        stdout.contains("claude-code: not installed"),
        "got: {stdout}"
    );
}

#[test]
fn test_skill_list_check_reject_force_and_unknown_agents() {
    let home = fake_home_with(&[".claude"]);
    for action in ["list", "check"] {
        let (status, _out, _stderr) =
            run_cli(Opts::new(&["skill", action, "--force"]).in_home(home.path()));
        assert_eq!(
            status.code(),
            Some(2),
            "{action} --force must be a usage error"
        );
        let (status, _out, stderr) =
            run_cli(Opts::new(&["skill", action, "--agent", "bogus"]).in_home(home.path()));
        assert_eq!(status.code(), Some(2), "{action} --agent bogus must exit 2");
        assert!(stderr.contains("unknown agent"), "{action}: {stderr}");
    }
}

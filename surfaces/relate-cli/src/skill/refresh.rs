//! Post-upgrade skill-refresh subsystem (ADR-0261 / ADR-0262): classify every
//! detected agent's `<skills dir>/relate`, decide a refresh plan, and
//! spawn `relate skill install` from the freshly replaced binary.
//!
//! Compiled only under the `upgrade` feature; the sole caller is
//! `crate::upgrade_cmd`.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::skill::agents;
use crate::skill::{classify_skill_dir, SkillDirState, MARKER_FILE, SKILL_NAME};

/// The pinned install prompt (ADR-0261). The trailing space is part of the
/// contract: the user types on the same line.
const INSTALL_PROMPT: &str = "Install the relate skill? [Y/n] ";

/// The pinned hint for a non-interactive, all-missing refresh without `--yes`.
const HINT_INSTALL: &str = "run: relate skill install";

/// The pinned remediation shown after a [`foreign_note`], pointing at the
/// standalone command that can replace foreign content with `--force`.
const FOREIGN_REMEDIATION: &str =
    "to replace it with the relate skill bundle, run: relate skill install --force";

/// One detected agent's refresh target.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillTarget {
    /// Canonical agent id, passed to `skill install --agent`.
    name: &'static str,
    /// `<skills dir>/relate`.
    dest: PathBuf,
    state: SkillDirState,
}

/// A single `skill install` spawn the plan wants to run. The refresh never
/// passes `--force`: `skill install` adopts a recognized unmarked bundle on
/// its own (ADR-0262).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillSpawn {
    /// `Some(name)` targets one agent; `None` runs the unfiltered install.
    agent: Option<&'static str>,
}

/// The spawns a plan wants for a set of non-foreign targets (ADR-0261): one
/// unfiltered install when nothing foreign coexists, otherwise one
/// `--agent <name>` spawn per target so a replacement stays scoped to our own
/// dirs. The refresh never passes `--force` (ADR-0262).
fn spawns_for(non_foreign: &[&SkillTarget], has_foreign: bool) -> Vec<SkillSpawn> {
    if has_foreign {
        non_foreign
            .iter()
            .map(|t| SkillSpawn {
                agent: Some(t.name),
            })
            .collect()
    } else {
        vec![SkillSpawn { agent: None }]
    }
}

/// What the refresh does with its non-foreign targets (ADR-0261). The
/// consent mode is one enum, so the invalid "prompt and hint at once"
/// combination is unrepresentable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum RefreshAction {
    /// Run the spawns immediately: an auto-update of an installed bundle
    /// (no consent needed — the bundle is already ours), or an all-missing
    /// set with `--yes`.
    Run(Vec<SkillSpawn>),
    /// Interactive all-missing set without `--yes`: ask with
    /// [`INSTALL_PROMPT`] first, then run the spawns on consent.
    AskFirst(Vec<SkillSpawn>),
    /// Non-interactive all-missing set without `--yes`: print
    /// [`HINT_INSTALL`] and install nothing.
    Hint,
    /// Nothing to do: no detected agents, or only foreign destinations.
    #[default]
    Skip,
}

/// The pure refresh plan (ADR-0261).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SkillRefreshPlan {
    /// What to do with the non-foreign targets.
    action: RefreshAction,
    /// Foreign destinations to report (never touched).
    foreign: Vec<PathBuf>,
}

/// Resolve every detected agent's `<skills dir>/relate` and classify it.
fn detected_skill_targets() -> Vec<SkillTarget> {
    agents::detect_agents()
        .into_iter()
        .filter_map(|agent| {
            let dest = agent.skills_path()?.join(SKILL_NAME);
            let state = classify_skill_dir(&dest);
            Some(SkillTarget {
                name: agent.name,
                dest,
                state,
            })
        })
        .collect()
}

/// The pure decision (ADR-0261), given each target's classification plus the
/// interactivity and `--yes` inputs. Foreign targets are never spawned
/// against; they are only collected for reporting.
fn skill_refresh_decision(targets: &[SkillTarget], is_tty: bool, yes: bool) -> SkillRefreshPlan {
    if targets.is_empty() {
        // ADR-0069 §7: no supported agents detected -> skip silently.
        return SkillRefreshPlan::default();
    }

    let foreign: Vec<PathBuf> = targets
        .iter()
        .filter(|t| t.state == SkillDirState::Foreign)
        .map(|t| t.dest.clone())
        .collect();
    let non_foreign: Vec<&SkillTarget> = targets
        .iter()
        .filter(|t| t.state != SkillDirState::Foreign)
        .collect();

    // Every detected target is foreign: report only, never spawn.
    if non_foreign.is_empty() {
        return SkillRefreshPlan {
            foreign,
            ..SkillRefreshPlan::default()
        };
    }

    // The spawn set is identical for every consent branch; build it once.
    let spawns = spawns_for(&non_foreign, !foreign.is_empty());

    // Any installed (Owned/Legacy) non-foreign target -> auto-update, no prompt.
    // The install child adopts a recognized unmarked bundle on its own, so the
    // refresh passes no `--force`; per-agent spawns (used only when a Foreign
    // dir coexists) keep the replacement scoped to our dirs.
    if non_foreign.iter().any(|t| t.state.is_installed()) {
        return SkillRefreshPlan {
            action: RefreshAction::Run(spawns),
            foreign,
        };
    }

    // All non-foreign targets are Missing: install only with consent.
    if yes {
        SkillRefreshPlan {
            action: RefreshAction::Run(spawns),
            foreign,
        }
    } else if is_tty {
        SkillRefreshPlan {
            action: RefreshAction::AskFirst(spawns),
            foreign,
        }
    } else {
        SkillRefreshPlan {
            action: RefreshAction::Hint,
            foreign,
        }
    }
}

/// After a successful replace, refresh the installed skill bundle per
/// ADR-0261. Any failure to install never undoes the upgrade: warn and report
/// success (the exit code stays 0).
pub fn skill_refresh(yes: bool, exe: &Path) {
    let targets = detected_skill_targets();
    let plan = skill_refresh_decision(&targets, io::stdin().is_terminal(), yes);

    for path in &plan.foreign {
        println!("{}", foreign_note(path));
        println!("{FOREIGN_REMEDIATION}");
    }

    let spawns = match plan.action {
        RefreshAction::Skip => return,
        RefreshAction::Hint => return println!("{HINT_INSTALL}"),
        RefreshAction::Run(spawns) => spawns,
        RefreshAction::AskFirst(spawns) => {
            if !prompt_skill_refresh() {
                return;
            }
            spawns
        }
    };
    for spawn in &spawns {
        spawn_skill_install(exe, spawn.agent);
    }
}

/// The per-path `foreign_note` line (ADR-0261), with the path sanitized like
/// every other interpolated user-facing string.
fn foreign_note(path: &Path) -> String {
    let path = relate_formatter::strip_unsafe_output_chars(&path.display().to_string());
    format!(
        "note: {path} is not a recognizable relate skill (no {MARKER_FILE} marker, and SKILL.md does not declare name: {SKILL_NAME} with a version line); leaving it untouched"
    )
}

/// Ask [`INSTALL_PROMPT`]; empty, `y`, and `yes` (trimmed, any case) mean yes.
fn prompt_skill_refresh() -> bool {
    print!("{INSTALL_PROMPT}");
    let _ = io::stdout().flush();
    // The one terminal-prompt answer kit (fail-closed): an unread line and
    // an unrecognized answer both decline.
    crate::prompt::read_answer(&mut io::stdin().lock())
        .is_some_and(|line| crate::prompt::answer_is_yes(&line))
}

/// The argv (after the binary path) for a `skill install` spawn. Pure, so the
/// `--agent` ordering is unit-tested without spawning. The refresh never emits
/// `--force` (ADR-0262).
fn skill_install_args(agent: Option<&str>) -> Vec<String> {
    let mut args = vec!["skill".to_string(), "install".to_string()];
    if let Some(agent) = agent {
        args.push("--agent".to_string());
        args.push(agent.to_string());
    }
    args
}

/// Spawn the NEW binary (`exe` was just replaced) as `relate skill
/// install` with inherited stdio, so the new embedded skill bundle is what
/// gets installed; the old process only holds the old bundle.
fn spawn_skill_install(exe: &Path, agent: Option<&'static str>) {
    match std::process::Command::new(exe)
        .args(skill_install_args(agent))
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => crate::print_error(format!(
            "warning: `relate skill install` exited with {status}; the binary upgrade itself succeeded"
        )),
        Err(e) => crate::print_error(format!(
            "warning: could not run `relate skill install`: {e}; the binary upgrade itself succeeded"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a decision-matrix target with a path derived from its name.
    fn target(name: &'static str, state: SkillDirState) -> SkillTarget {
        SkillTarget {
            name,
            dest: PathBuf::from(format!("/skills/{name}")),
            state,
        }
    }

    fn spawn(agent: Option<&'static str>) -> SkillSpawn {
        SkillSpawn { agent }
    }

    #[test]
    fn skill_refresh_decision_skips_without_agents() {
        // No agents -> silent skip, regardless of tty/yes.
        for (is_tty, yes) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                skill_refresh_decision(&[], is_tty, yes),
                SkillRefreshPlan::default(),
                "tty={is_tty} yes={yes}"
            );
        }
    }

    #[test]
    fn skill_refresh_decision_installed_single_spawn() {
        // ADR-0262: the refresh never forces; the child adopts a recognized
        // unmarked bundle. Owned, Legacy, and both together are one spawn.
        let owned = [target("a", SkillDirState::Owned)];
        let legacy = [target("a", SkillDirState::Legacy)];
        let both = [
            target("a", SkillDirState::Owned),
            target("b", SkillDirState::Legacy),
        ];
        for targets in [&owned[..], &legacy[..], &both[..]] {
            let plan = skill_refresh_decision(targets, false, false);
            assert_eq!(plan.action, RefreshAction::Run(vec![spawn(None)]));
            assert!(plan.foreign.is_empty());
        }
    }

    #[test]
    fn skill_refresh_decision_installed_plus_foreign_per_agent_unforced() {
        let targets = [
            target("a", SkillDirState::Owned),
            target("b", SkillDirState::Legacy),
            target("c", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets, false, false);
        // One spawn per non-foreign agent, never forced.
        assert_eq!(
            plan.action,
            RefreshAction::Run(vec![spawn(Some("a")), spawn(Some("b"))])
        );
        assert_eq!(plan.foreign, vec![PathBuf::from("/skills/c")]);
    }

    #[test]
    fn skill_refresh_decision_all_missing_consent_matrix() {
        let targets = [target("a", SkillDirState::Missing)];
        // --yes installs regardless of interactivity.
        for is_tty in [true, false] {
            let plan = skill_refresh_decision(&targets, is_tty, true);
            assert_eq!(plan.action, RefreshAction::Run(vec![spawn(None)]));
        }
        // Interactive without --yes asks first.
        let plan = skill_refresh_decision(&targets, true, false);
        assert_eq!(plan.action, RefreshAction::AskFirst(vec![spawn(None)]));
    }

    #[test]
    fn skill_refresh_decision_all_missing_noninteractive_hints() {
        let targets = [target("a", SkillDirState::Missing)];
        let plan = skill_refresh_decision(&targets, false, false);
        assert_eq!(plan.action, RefreshAction::Hint);
    }

    #[test]
    fn skill_refresh_decision_all_missing_with_foreign_per_agent() {
        let targets = [
            target("a", SkillDirState::Missing),
            target("b", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets, false, true);
        assert_eq!(plan.action, RefreshAction::Run(vec![spawn(Some("a"))]));
        assert_eq!(plan.foreign, vec![PathBuf::from("/skills/b")]);
    }

    #[test]
    fn skill_refresh_decision_all_foreign_reports_only() {
        let targets = [
            target("a", SkillDirState::Foreign),
            target("b", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets, true, true);
        assert_eq!(plan.action, RefreshAction::Skip);
        assert_eq!(
            plan.foreign,
            vec![PathBuf::from("/skills/a"), PathBuf::from("/skills/b")]
        );
    }

    #[test]
    fn message_templates_match_the_adr_byte_for_byte() {
        assert_eq!(INSTALL_PROMPT, "Install the relate skill? [Y/n] ");
        assert_eq!(HINT_INSTALL, "run: relate skill install");
        assert_eq!(
            FOREIGN_REMEDIATION,
            "to replace it with the relate skill bundle, run: relate skill install --force"
        );
        assert_eq!(
            foreign_note(Path::new("/x/relate")),
            "note: /x/relate is not a recognizable relate skill \
             (no .relate-installed marker, and SKILL.md does not declare name: relate with a version line); leaving it untouched"
        );
    }

    #[test]
    fn skill_install_args_never_contains_force() {
        assert_eq!(skill_install_args(None), vec!["skill", "install"]);
        assert_eq!(
            skill_install_args(Some("claude-code")),
            vec!["skill", "install", "--agent", "claude-code"]
        );
        // ADR-0262: the refresh never builds a `--force` argument, for any
        // agent selection.
        for agent in [None, Some("claude-code")] {
            assert!(
                !skill_install_args(agent).iter().any(|a| a == "--force"),
                "refresh argv must not force: {:?}",
                skill_install_args(agent)
            );
        }
    }

    #[test]
    fn the_shared_answer_classifier_accepts_empty_y_and_yes() {
        for yes in ["", "  ", "y", "Y", "yes", "YES", " y ", " Yes ", "yEs\n"] {
            assert!(
                crate::prompt::answer_is_yes(yes),
                "expected yes for {yes:?}"
            );
        }
    }

    #[test]
    fn the_shared_answer_classifier_declines_everything_else() {
        for no in ["n", "N", "no", "NO", " no ", "nope", "ye", "yess", "1"] {
            assert!(!crate::prompt::answer_is_yes(no), "expected no for {no:?}");
        }
    }
}

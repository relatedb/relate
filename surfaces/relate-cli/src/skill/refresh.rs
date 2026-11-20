//! Post-upgrade skill-refresh subsystem (ADR-0261 / ADR-0262 / ADR-0632):
//! classify every detected agent's `<skills dir>/relate`, decide a refresh
//! plan — an unconditional auto-update of the installed targets, nothing
//! at all for the missing ones in a mixed set (ADR-0632: the installed
//! set is the user's curation; `upgrade` maintains it, never expands it),
//! the first-install consent matrix when nothing is installed anywhere —
//! and spawn `relate skill install` from the freshly replaced binary.
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

/// The consent mode for the first-install flow (ADR-0261 §3), derived
/// once in [`skill_refresh`] from `--yes` and stdin's tty-ness: the
/// decision takes one typed value instead of two booleans whose four
/// combinations model three outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consent {
    /// `--yes`: install the missing set without asking.
    Granted,
    /// Interactive without `--yes`: ask with [`INSTALL_PROMPT`] first.
    Prompt,
    /// Non-interactive without `--yes`: print [`HINT_INSTALL`] and
    /// install nothing.
    Hint,
}

/// The three-outcome consent matrix (ADR-0261 §3): `--yes` grants, an
/// interactive run prompts, anything else hints. Pure, so the matrix is
/// unit-pinned; [`skill_refresh`] feeds it the `--yes` flag and stdin's
/// tty-ness.
fn consent_mode(yes: bool, interactive: bool) -> Consent {
    if yes {
        Consent::Granted
    } else if interactive {
        Consent::Prompt
    } else {
        Consent::Hint
    }
}

/// The pure refresh plan (ADR-0261, scoped by ADR-0632). The `auto`
/// spawns run unconditionally; the `missing` spawns are the first-install
/// set — present only when nothing is installed anywhere, gated at
/// execution by the consent mode; the foreign destinations are reported
/// and never touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SkillRefreshPlan {
    /// Unconditional spawns: the auto-update of every installed target,
    /// never reaching a Missing one.
    auto: Vec<SkillSpawn>,
    /// The first-install spawns: empty unless nothing is installed
    /// anywhere (ADR-0261 §3's flow; ADR-0632's mixed sets never fill
    /// it).
    missing: Vec<SkillSpawn>,
    /// Foreign destinations to report (never touched).
    foreign: Vec<PathBuf>,
}

/// The one per-agent spawn list (ADR-0632's scoped form): every spawn
/// names its agent, so the child cannot reach a target the plan did not
/// decide. An empty partition yields no spawns.
fn per_agent(partition: &[&SkillTarget]) -> Vec<SkillSpawn> {
    partition
        .iter()
        .map(|t| SkillSpawn {
            agent: Some(t.name),
        })
        .collect()
}

/// The one unfiltered spawn (ADR-0261 §2's update-only form, or §3's
/// first install): legitimate only for the partition that is the sole
/// non-empty one — a property the decision's match guarantees by
/// construction, never a pairing a caller could get wrong.
fn whole_set() -> Vec<SkillSpawn> {
    vec![SkillSpawn { agent: None }]
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

/// The pure decision (ADR-0261, scoped by ADR-0632) over the classified
/// targets: one exhaustive pass sorts them into Foreign (reported, never
/// spawned against), installed (auto-update) and Missing, then one
/// exhaustive match over the partition triple builds the plan. ADR-0632's
/// safety condition — the unfiltered spawn only for the partition that
/// is the sole non-empty one — and its curation rule — a mixed set
/// updates the installed targets and leaves the missing ones strictly
/// alone — are properties of the match's arms, not of re-tested flags.
/// An empty detected set lands in the catch-all with every list empty:
/// ADR-0069 §7's silent skip.
fn skill_refresh_decision(targets: &[SkillTarget]) -> SkillRefreshPlan {
    // The one partition over the four-state classification vocabulary:
    // Foreign is collected for reporting (ADR-0261), Owned ∪ Legacy —
    // the installed states — auto-update (ADR-0261 §2), Missing consults
    // consent (§3). Exhaustive, so a future classification variant is a
    // compile error here, never a silently dropped target.
    let mut foreign: Vec<PathBuf> = Vec::new();
    let mut installed: Vec<&SkillTarget> = Vec::new();
    let mut missing: Vec<&SkillTarget> = Vec::new();
    for t in targets {
        match t.state {
            SkillDirState::Foreign => foreign.push(t.dest.clone()),
            SkillDirState::Missing => missing.push(t),
            SkillDirState::Owned | SkillDirState::Legacy => installed.push(t),
        }
    }

    match (installed.is_empty(), missing.is_empty(), foreign.is_empty()) {
        // Nothing installed anywhere, no Foreign target: the first-install
        // flow (ADR-0261 §3) over the whole detected set — the consent
        // mode gates these spawns at execution.
        (true, false, true) => SkillRefreshPlan {
            auto: Vec::new(),
            missing: whole_set(),
            foreign,
        },
        // Nothing installed anywhere, a Foreign target coexisting: the
        // first-install spawns stay per agent, scoped away from it.
        (true, false, false) => SkillRefreshPlan {
            auto: Vec::new(),
            missing: per_agent(&missing),
            foreign,
        },
        // Every detected target installed: the auto-update is the one
        // unfiltered spawn (ADR-0261 §2).
        (false, true, true) => SkillRefreshPlan {
            auto: whole_set(),
            missing: Vec::new(),
            foreign,
        },
        // Every other shape — a mixed set (ADR-0632's curation rule:
        // update the installed, leave the missing strictly alone),
        // installed targets with a Foreign one, only Foreign targets, or
        // nothing detected: the auto-update stays per agent (empty when
        // nothing is installed) and the missing targets are never
        // reached.
        _ => SkillRefreshPlan {
            auto: per_agent(&installed),
            missing: Vec::new(),
            foreign,
        },
    }
}

/// After a successful replace, refresh the installed skill bundle per
/// ADR-0261 as scoped by ADR-0632: the installed targets auto-update; the
/// missing ones are reached only through the first-install consent flow.
/// Any failure to install never undoes the upgrade: warn and report
/// success (the exit code stays 0).
pub fn skill_refresh(yes: bool, exe: &Path) {
    let targets = detected_skill_targets();
    // The one consent derivation (ADR-0261 §3), via the pure matrix.
    let consent = consent_mode(yes, io::stdin().is_terminal());
    let plan = skill_refresh_decision(&targets);

    for path in &plan.foreign {
        println!("{}", foreign_note(path));
        println!("{FOREIGN_REMEDIATION}");
    }

    run_spawns(exe, &plan.auto);
    // The first-install spawns exist only when nothing is installed
    // anywhere; the derived consent mode gates them here (ADR-0261 §3).
    if !plan.missing.is_empty() {
        match consent {
            Consent::Granted => run_spawns(exe, &plan.missing),
            Consent::Prompt => {
                if prompt_skill_refresh() {
                    run_spawns(exe, &plan.missing);
                }
            }
            Consent::Hint => println!("{HINT_INSTALL}"),
        }
    }
}

/// Run one spawn list through the new binary: the auto-update, the
/// `--yes` install, and the consented prompt install all pass through
/// here, so how the refresh spawns changes in one place.
fn run_spawns(exe: &Path, spawns: &[SkillSpawn]) {
    for spawn in spawns {
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

    /// ADR-0261's decision matrix, pinned as the plan's shapes: the
    /// whole decision is one exhaustive match, so every set shape's plan
    /// is asserted directly — the consent mode gates only the
    /// first-install spawns, at execution.
    #[test]
    fn skill_refresh_decision_skips_without_agents() {
        // No agents -> the silent skip (ADR-0069 §7): every list empty.
        assert_eq!(skill_refresh_decision(&[]), SkillRefreshPlan::default());
    }

    /// ADR-0261 §3's three-outcome matrix, pinned: `--yes` grants
    /// regardless of interactivity, an interactive run prompts, anything
    /// else hints.
    #[test]
    fn consent_mode_pins_the_adr_0261_matrix() {
        for interactive in [true, false] {
            assert_eq!(
                consent_mode(true, interactive),
                Consent::Granted,
                "interactive={interactive}"
            );
        }
        assert_eq!(consent_mode(false, true), Consent::Prompt);
        assert_eq!(consent_mode(false, false), Consent::Hint);
    }

    #[test]
    fn skill_refresh_decision_installed_single_spawn() {
        // ADR-0262: the refresh never forces; the child adopts a
        // recognized unmarked bundle. Owned, Legacy, and both together:
        // the installed partition is the whole set, so the auto-update
        // is the one unfiltered spawn.
        let owned = [target("a", SkillDirState::Owned)];
        let legacy = [target("a", SkillDirState::Legacy)];
        let both = [
            target("a", SkillDirState::Owned),
            target("b", SkillDirState::Legacy),
        ];
        for targets in [&owned[..], &legacy[..], &both[..]] {
            let plan = skill_refresh_decision(targets);
            assert_eq!(plan.auto, vec![spawn(None)]);
            assert_eq!(plan.missing, Vec::new());
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
        let plan = skill_refresh_decision(&targets);
        // One spawn per installed agent, never forced; the foreign dir is
        // reported and never spawned against.
        assert_eq!(plan.auto, vec![spawn(Some("a")), spawn(Some("b"))]);
        assert_eq!(plan.missing, Vec::new());
        assert_eq!(plan.foreign, vec![PathBuf::from("/skills/c")]);
    }

    /// ADR-0632: a mixed set (installed + missing) updates only the
    /// installed targets and leaves the missing ones untouched — the
    /// plan carries no first-install spawns, so no consent mode is ever
    /// consulted for them.
    #[test]
    fn skill_refresh_decision_mixed_scopes_auto_update_to_installed() {
        let targets = [
            target("a", SkillDirState::Owned),
            target("b", SkillDirState::Missing),
        ];
        let plan = skill_refresh_decision(&targets);
        assert_eq!(plan.auto, vec![spawn(Some("a"))]);
        assert_eq!(plan.missing, Vec::new());
        assert!(plan.foreign.is_empty());
    }

    /// ADR-0632: a mixed set with a Foreign target keeps the auto-update
    /// per-agent (scoped away from the foreign dir) and still leaves the
    /// missing target untouched.
    #[test]
    fn skill_refresh_decision_mixed_plus_foreign_updates_installed_only() {
        let targets = [
            target("a", SkillDirState::Owned),
            target("b", SkillDirState::Missing),
            target("c", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets);
        assert_eq!(plan.auto, vec![spawn(Some("a"))]);
        assert_eq!(plan.missing, Vec::new());
        assert_eq!(plan.foreign, vec![PathBuf::from("/skills/c")]);
    }

    /// The first-install flow (ADR-0261 §3): with nothing installed
    /// anywhere, the plan carries the whole detected set as the
    /// consent-gated spawns — one unfiltered spawn with no Foreign
    /// target, per-agent spawns with one.
    #[test]
    fn skill_refresh_decision_all_missing_is_the_first_install_set() {
        let targets = [target("a", SkillDirState::Missing)];
        let plan = skill_refresh_decision(&targets);
        assert_eq!(plan.auto, Vec::new());
        assert_eq!(plan.missing, vec![spawn(None)]);
        assert!(plan.foreign.is_empty());
    }

    #[test]
    fn skill_refresh_decision_all_missing_with_foreign_per_agent() {
        let targets = [
            target("a", SkillDirState::Missing),
            target("b", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets);
        assert_eq!(plan.auto, Vec::new());
        assert_eq!(plan.missing, vec![spawn(Some("a"))]);
        assert_eq!(plan.foreign, vec![PathBuf::from("/skills/b")]);
    }

    #[test]
    fn skill_refresh_decision_all_foreign_reports_only() {
        let targets = [
            target("a", SkillDirState::Foreign),
            target("b", SkillDirState::Foreign),
        ];
        let plan = skill_refresh_decision(&targets);
        assert_eq!(plan.auto, Vec::new());
        assert_eq!(plan.missing, Vec::new());
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

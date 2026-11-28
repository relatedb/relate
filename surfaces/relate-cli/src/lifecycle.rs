//! The lifecycle dispatch: the mode-gated command table's execution — the
//! per-command help pages, the feedback surfaces' mode gates, the skill /
//! upgrade / telemetry / pull / push / clone / info / extension verbs, and
//! the `RELATE-CLI_COMMAND` emission — extracted from `main` so the entry
//! point stays a prologue and a call.

#[cfg(feature = "agent-context")]
use crate::agent_context_cmd;
use crate::agent_feedback_cmd;
use crate::agent_mode;
use crate::clone_cmd;
use crate::exit_class;
#[cfg(feature = "extensions")]
use crate::extension_cmd;
#[cfg(any(feature = "skill", feature = "upgrade"))]
use crate::feedback_ask;
use crate::feedback_cmd;
use crate::flags;
use crate::help;
#[cfg(feature = "info")]
use crate::info_cmd;
#[cfg(feature = "pull")]
use crate::pull_cmd;
#[cfg(feature = "push")]
use crate::push_cmd;
#[cfg(feature = "serve")]
use crate::serve_cmd;
#[cfg(feature = "skill")]
use crate::skill_cmd;
use crate::telemetry;
#[cfg(feature = "telemetry")]
use crate::telemetry_cmd;
#[cfg(feature = "upgrade")]
use crate::upgrade_cmd;
use crate::LifecycleCommand;

/// The absent surface's one-line refusal: a mode-gated command typed in the
/// other mode is an unknown command naming the surface this run has, the
/// other surface, and that the grant is a person's act at a terminal.
/// Dispatch policy — it decides what an unknown command means for a run
/// that does not own a surface — so it lives beside the dispatcher; the
/// command dispatch arms and the agent-context readout share it. The words
/// come from their owning modules, the exit class from the one statement
/// of the split.
pub(crate) fn refuse_absent_surface(typed: &str, surface: agent_mode::FeedbackSurface) -> i32 {
    let person = format!("relate {}", feedback_cmd::WORD);
    let agent = format!("relate {}", agent_feedback_cmd::WORD);
    let enable = feedback_cmd::enable_invocation();
    // One sentence both arms share: the run's own surface and the other,
    // exactly what the doc above promises every refusal names.
    let (own, other) = match surface {
        agent_mode::FeedbackSurface::Human => (person, agent),
        agent_mode::FeedbackSurface::Agent => (agent, person),
    };
    crate::print_error(format!(
        "Error: unknown command: {typed}. this run's feedback surface is '{own}'; the \
         other mode's is '{other}', and the grant is managed by a person at a terminal \
         ('{enable}')"
    ));
    exit_class::ExitClass::Usage.code()
}

/// Run a parsed lifecycle command and exit with its status code: 0 on
/// success, 2 for usage errors (unknown agent) and commands not included in
/// this build, 1 for runtime errors. Emits `RELATE-CLI_COMMAND`
/// (ADR-0079 §4) and flushes before exiting.
pub(crate) fn run_lifecycle_command(
    cmd: LifecycleCommand,
    telemetry: &mut telemetry::Telemetry,
    surface: agent_mode::FeedbackSurface,
) -> ! {
    // The lifecycle run's delivery window (ADR-0615): the drain starts
    // here — the command's own runtime (a download, a copy, a serve
    // lifetime) is the window it POSTs in, instead of the microseconds
    // after the command body that the event's emit used to allow — and
    // the exit joins it. The off transition is the one carve-out
    // (the vocabulary predicate): a run about to forbid delivery never
    // starts one (ADR-0545 §5).
    if !cmd.forbids_delivery_window() {
        telemetry.deliver_backlog_at_exit();
    }
    // The command word for the RELATE-CLI_COMMAND event, captured before
    // the match consumes cmd — and the hint's prefill fact: a lifecycle
    // run's failures name the lifecycle word (the feedback surfaces' own
    // words are not visible commands, so their hints carry no --command).
    let name = cmd.name();
    agent_feedback_cmd::set_run_command(name);
    match cmd {
        // A per-command `--help` (after a compiled-in command word) prints
        // the command's help page and exits 0 through the shared flush path:
        // the same two events as the top-level help, no COMMAND event, and
        // nothing after the word parsed or executed. A mode-gated topic the
        // run does not own is the absent surface's unknown command instead
        // (ADR-0588 §3).
        LifecycleCommand::Help { topic } => match topic.surface() {
            Some(own) if own != surface => {
                let exit_code = refuse_absent_surface(topic.word(), surface);
                finish_command(telemetry, name, None, exit_code);
            }
            _ => crate::exit_help(telemetry, &help::command_help(topic)),
        },
        // The resolved action's pre-scanned page (`run <name> --help`):
        // the same help-exit sequence as every command's page — the two
        // help events, no COMMAND event, nothing runs (LABS-552).
        #[cfg(feature = "extensions")]
        LifecycleCommand::RunHelp { page } => crate::exit_help(telemetry, &page),
        LifecycleCommand::NotIncluded { command } => {
            crate::print_error(format!(
                "Error: the '{command}' command is not included in this build of relate"
            ));
            crate::exit_flushed(telemetry, exit_class::ExitClass::Usage.code());
        }
        #[cfg(feature = "skill")]
        LifecycleCommand::Skill { args } => {
            // The typed action, captured before run() consumes the args: the
            // ask gate reads the variant, the telemetry word derives from it
            // (LABS-1737).
            let action = args.action;
            let asking_run = matches!(
                action,
                skill_cmd::SkillAction::Mutate(skill_cmd::MutateAction::Install)
            );
            let exit_code = skill_cmd::run(args);
            feedback_ask::ask_after_command(exit_code, surface, asking_run);
            finish_command(telemetry, name, Some(action.as_str()), exit_code);
        }
        #[cfg(feature = "upgrade")]
        LifecycleCommand::Upgrade { opts } => {
            // LABS-1737: a real upgrade (never a --check) is the windowed
            // ask's resurface surface; the gate lives in the ask module.
            let check = opts.check;
            let exit_code = upgrade_cmd::run(opts);
            feedback_ask::ask_after_command(exit_code, surface, !check);
            finish_command(telemetry, name, check.then_some("check"), exit_code);
        }
        LifecycleCommand::Feedback { args } => {
            // The mode gate runs before the grammar: the absent surface is
            // an unknown command, never this grammar's usage error.
            if surface != agent_mode::FeedbackSurface::Human {
                let exit_code = refuse_absent_surface(feedback_cmd::WORD, surface);
                finish_command(telemetry, name, None, exit_code);
            }
            let action = match feedback_cmd::parse_action(&args) {
                Ok(action) => action,
                Err(err) => flags::command_usage_error(help::HelpTopic::Feedback, &err),
            };
            let action_word = action.as_str();
            let exit_code = feedback_cmd::run(action);
            finish_command(telemetry, name, Some(action_word), exit_code);
        }
        LifecycleCommand::AgentFeedback { args } => {
            if surface != agent_mode::FeedbackSurface::Agent {
                let exit_code = refuse_absent_surface(agent_feedback_cmd::WORD, surface);
                finish_command(telemetry, name, None, exit_code);
            }
            let input = match agent_feedback_cmd::parse(&args) {
                Ok(input) => input,
                Err(err) => flags::command_usage_error(help::HelpTopic::AgentFeedback, &err),
            };
            let exit_code = agent_feedback_cmd::run(&input);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "telemetry")]
        LifecycleCommand::Telemetry { action } => {
            // The command owns the handle: `off` disarms it (one ordered
            // transition, ADR-0545 §5) and `status` reports the state
            // this same handle resolved, so env layers override the
            // config file exactly as in Telemetry::new.
            let exit_code = telemetry_cmd::run(action, telemetry);
            finish_command(telemetry, name, Some(action.as_str()), exit_code);
        }
        #[cfg(feature = "pull")]
        LifecycleCommand::Pull { cmd } => {
            let exit_code = pull_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "push")]
        LifecycleCommand::Push { cmd } => {
            let exit_code = push_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        LifecycleCommand::Clone { cmd } => {
            let exit_code = clone_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "extensions")]
        LifecycleCommand::Run { cmd } => {
            // ADR-0429 §9: the report says command `run`, action
            // `extension` — never a user-chosen name (ADR-0079
            // anonymity), which is also what keeps the event's
            // `&'static str` fields honest: the installed name is
            // data, not a static.
            let exit_code = crate::extensions::run::run(cmd, telemetry);
            finish_command(telemetry, name, Some("extension"), exit_code);
        }
        #[cfg(feature = "info")]
        LifecycleCommand::Info { cmd } => {
            let exit_code = info_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "serve")]
        LifecycleCommand::Serve { cmd } => {
            // `serve` runs until SIGINT/SIGTERM (ADR-0232), then returns the
            // shutdown code; the COMMAND event and exit happen here.
            let exit_code = serve_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "agent-context")]
        LifecycleCommand::AgentContext { command } => {
            let exit_code = agent_context_cmd::run(command, surface);
            finish_command(telemetry, name, command.map(|spec| spec.word), exit_code);
        }
        #[cfg(feature = "extensions")]
        LifecycleCommand::Extension { action } => {
            // ADR-0431 §3: the action word is the event's action — an
            // `&'static str`, never a package name (ADR-0079 anonymity),
            // the `skill` command's pattern.
            let action_word = action.as_str();
            let exit_code = extension_cmd::run(action);
            finish_command(telemetry, name, Some(action_word), exit_code);
        }
    }
}

/// The shared tail of every [`run_lifecycle_command`] arm that ran a
/// command: emit `RELATE-CLI_COMMAND` (ADR-0079 §4) and exit through
/// [`exit_flushed`]. `name` is the command word as telemetry reports it
/// (`update` reports "upgrade"); `action` is the sub-action, if any.
fn finish_command(
    telemetry: &mut telemetry::Telemetry,
    name: &'static str,
    action: Option<&'static str>,
    exit_code: i32,
) -> ! {
    telemetry.emit(telemetry::Event::Command {
        command: name,
        action,
        success: exit_code == 0,
    });
    crate::exit_flushed(telemetry, exit_code);
}

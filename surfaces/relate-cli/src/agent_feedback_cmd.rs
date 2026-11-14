//! `relate agent-feedback` (ADR-0588 §3): the agent's channel. It takes the
//! structured report flags, requires the person's live standing grant, never
//! prompts, and sends one report through the crate's pipeline — opt-out,
//! authorization, validation and redaction, one POST, one local receipt.
//!
//! The surface exists only in agent mode: the mode gate is `main`'s, so the
//! absence of this module's command in a person's run is one unknown-command
//! refusal there, not a second check here.

use chrono::Utc;
use relate_feedback::authorization::{Authorization, FeedbackConsent};
use relate_feedback::report::{Category, Context, Input, Invoker};
use relate_feedback::sender::FeedbackSender;
use relate_feedback::state;
use relate_feedback::submit::{self, Request};

use crate::config_dir::env_var as env;
use crate::parse_error::{flag_value, set_once, ParseError};

/// The command word, also the value a report's `command_id` filter
/// excludes: a report never names a feedback command.
const WORD: &str = "agent-feedback";

/// Parse the argument tail after the `agent-feedback` verb. Pure — the
/// grammar is unit-testable; `main` turns `Err` into the usage-error exit.
///
/// `--category` defaults to [`Category::Friction`] (ADR-0588 §3); an
/// unknown value is left for the policy to reject, the same local-rejection
/// path every other invalid field takes.
pub fn parse(args: &[String]) -> Result<Input, ParseError> {
    let mut category = None;
    let mut expected = None;
    let mut observed = None;
    let mut command = None;
    let mut error_code = None;
    let mut model = None;
    let mut request_id = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--category" => set_once(
                WORD,
                &mut category,
                "--category",
                flag_value(&mut args, "--category", "a category")?.clone(),
            )?,
            "--expected" => set_once(
                WORD,
                &mut expected,
                "--expected",
                flag_value(&mut args, "--expected", "text")?.clone(),
            )?,
            "--observed" => set_once(
                WORD,
                &mut observed,
                "--observed",
                flag_value(&mut args, "--observed", "text")?.clone(),
            )?,
            "--command" => set_once(
                WORD,
                &mut command,
                "--command",
                flag_value(&mut args, "--command", "a command path")?.clone(),
            )?,
            "--error-code" => set_once(
                WORD,
                &mut error_code,
                "--error-code",
                flag_value(&mut args, "--error-code", "a code name")?.clone(),
            )?,
            "--model" => set_once(
                WORD,
                &mut model,
                "--model",
                flag_value(&mut args, "--model", "a model label")?.clone(),
            )?,
            "--request-id" => set_once(
                WORD,
                &mut request_id,
                "--request-id",
                flag_value(&mut args, "--request-id", "a UUID")?.clone(),
            )?,
            other => return Err(ParseError::unexpected(WORD, other)),
        }
    }
    Ok(Input {
        category: category.unwrap_or_else(|| Category::Friction.as_str().to_string()),
        expected: expected.ok_or("agent-feedback requires --expected")?,
        observed: observed.ok_or("agent-feedback requires --observed")?,
        command,
        error_code,
        agent_model: model,
        request_id,
    })
}

/// Run the surface: refuse without a live grant, otherwise send through the
/// pipeline and print its outcome document. Exit 0 for `sent` and `skipped`
/// (a local decision), 1 otherwise; a missing or expired grant is the loud
/// `not_enabled` refusal that names the person's `relate feedback enable`.
pub fn run(input: &Input) -> i32 {
    let dir = match crate::config_dir::config_dir() {
        Some(dir) => dir,
        None => return crate::config_dir::no_base(WORD),
    };
    let now = Utc::now();
    if !matches!(
        FeedbackConsent::load(&dir).authorization(now),
        Authorization::Authorized { .. }
    ) {
        crate::print_error(
            "Error: agent feedback is not enabled (not_enabled): the grant is a person's, \
             made at a terminal with 'relate feedback enable' (or answered at the first-run ask)",
        );
        return 1;
    }
    let sender = FeedbackSender::from_env(&env);
    let context = report_context(&env, invoker(&env));
    let doc = submit::submit(
        &Request {
            dir: &dir,
            input,
            context: &context,
            env: &env,
            config_telemetry: state::load_config_telemetry(&dir),
            now,
        },
        &sender,
    );
    println!(
        "{}",
        serde_json::to_string(&doc).expect("an outcome document is plain JSON")
    );
    if doc.outcome().is_success() {
        0
    } else {
        1
    }
}

/// The sender-owned report context. Both environment-derived fields read
/// through the caller's one lookup, so a caller can pin the record's
/// environment. The harness label is the raw `AI_AGENT` value: the crate's
/// policy is the single gate on what may be sent, so nothing is dropped here
/// twice.
fn report_context(env: &dyn Fn(&str) -> Option<String>, invoker: Invoker) -> Context {
    Context {
        cli_version: crate::VERSION.to_string(),
        os: os_label(),
        os_arch: arch_label(),
        invoker,
        agent_harness: env("AI_AGENT"),
        commands: visible_commands(),
        error_codes: crate::errors::error_code_names(),
    }
}

/// The report's `invoker` classification: the shared launch classifier,
/// spelled in the report's vocabulary (`ci` reads `script`).
pub(crate) fn invoker(env: &dyn Fn(&str) -> Option<String>) -> Invoker {
    // The agent surface exists only in agent mode, so the classifier sees a
    // harness marker for every run that reaches it; a script-driven one is
    // still reported honestly.
    match crate::agent_mode::launched_by(env) {
        crate::agent_mode::Launched::Agent => Invoker::Agent,
        crate::agent_mode::Launched::Ci => Invoker::Script,
        crate::agent_mode::Launched::Human => Invoker::Human,
    }
}

/// The visible command paths a report may name (`--command`): the compiled-in
/// command words, the two feedback surfaces excluded (a report never names a
/// feedback command).
fn visible_commands() -> Vec<String> {
    crate::help::commands()
        .iter()
        .filter(|spec| spec.topic.surface().is_none())
        .map(|spec| spec.word.to_string())
        .collect()
}

/// The report's `os` property: `linux`, `darwin` or `windows`; any other
/// platform is omitted rather than guessed.
fn os_label() -> Option<String> {
    match std::env::consts::OS {
        "linux" => Some("linux".to_string()),
        "macos" => Some("darwin".to_string()),
        "windows" => Some("windows".to_string()),
        _ => None,
    }
}

/// The report's `os_arch` property: `arm64` or `amd64`; any other
/// architecture is omitted rather than guessed.
fn arch_label() -> Option<String> {
    match std::env::consts::ARCH {
        "aarch64" => Some("arm64".to_string()),
        "x86_64" => Some("amd64".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    fn s(value: &str) -> String {
        value.to_string()
    }

    fn args(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|x| s(x)).collect()
    }

    #[test]
    fn parse_defaults_the_category_and_requires_the_two_texts() {
        let input = parse(&args(&["--expected", "x", "--observed", "y"])).unwrap();
        assert_eq!(input.category, "friction");
        assert_eq!(input.expected, "x");
        assert_eq!(input.observed, "y");
        for (rest, missing) in [
            (args(&[]), "--expected"),
            (args(&["--expected", "x"]), "--observed"),
        ] {
            let err = parse(&rest).unwrap_err().to_string();
            assert!(err.contains(missing), "{rest:?}: {err}");
        }
    }

    #[test]
    fn parse_accepts_every_flag_in_any_order() {
        let parsed = parse(&args(&[
            "--observed",
            "it printed help",
            "--category",
            "friction",
            "--expected",
            "the info command to print the version",
            "--command",
            "info",
            "--error-code",
            "usage_error",
            "--model",
            "claude-opus-5-5",
            "--request-id",
            "123e4567-e89b-42d3-a456-426614174000",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            Input {
                category: s("friction"),
                expected: s("the info command to print the version"),
                observed: s("it printed help"),
                command: Some(s("info")),
                error_code: Some(s("usage_error")),
                agent_model: Some(s("claude-opus-5-5")),
                request_id: Some(s("123e4567-e89b-42d3-a456-426614174000")),
            }
        );
    }

    #[test]
    fn parse_rejects_unknown_duplicate_and_extra_arguments() {
        assert_eq!(
            parse(&args(&["--bogus", "x"])),
            Err(ParseError::unexpected(WORD, "--bogus"))
        );
        assert_eq!(
            parse(&args(&["--expected", "x", "--observed", "y", "extra"])),
            Err(ParseError::unexpected(WORD, "extra"))
        );
        let duplicated = args(&["--category", "friction", "--category", "clarity"]);
        assert!(parse(&duplicated)
            .unwrap_err()
            .to_string()
            .contains("only once"));
    }

    #[test]
    fn invoker_classification() {
        assert_eq!(invoker(&env_of(&[])), Invoker::Human);
        assert_eq!(invoker(&env_of(&[("CI", "true")])), Invoker::Script);
        assert_eq!(invoker(&env_of(&[("CI", "0")])), Invoker::Human);
        assert_eq!(invoker(&env_of(&[("CLAUDECODE", "1")])), Invoker::Agent);
    }

    #[test]
    fn report_context_reads_the_supplied_environment() {
        let context = report_context(&env_of(&[("AI_AGENT", "claude-code")]), Invoker::Agent);
        assert_eq!(context.agent_harness.as_deref(), Some("claude-code"));
        assert_eq!(context.invoker, Invoker::Agent);
        assert_eq!(context.cli_version, crate::VERSION);
        assert_eq!(
            report_context(&env_of(&[]), Invoker::Human).agent_harness,
            None
        );
        // The harness label crosses the trust boundary in the crate's policy,
        // not here: the raw value is carried and the policy decides.
        assert_eq!(
            report_context(&env_of(&[("AI_AGENT", "Claude Code")]), Invoker::Agent).agent_harness,
            Some("Claude Code".to_string())
        );
    }

    #[test]
    fn the_error_code_vocabulary_is_the_exit_table() {
        let names = crate::errors::error_code_names();
        assert!(names.contains(&s("timeout")), "ADR-0241 filled code 7");
        assert!(!names.contains(&s("rate_limited")), "no such code");
        assert!(
            names.iter().all(|name| name != "success"),
            "exit 0 is not an error code"
        );
    }

    #[test]
    fn visible_commands_exclude_both_feedback_surfaces() {
        let commands = visible_commands();
        assert!(commands.iter().any(|word| word == "query"));
        for word in ["feedback", "agent-feedback"] {
            assert!(commands.iter().all(|name| name != word), "{word}");
        }
    }

    #[test]
    fn os_and_arch_labels_are_the_report_vocabulary() {
        for label in [os_label(), arch_label()].into_iter().flatten() {
            assert!(
                ["linux", "darwin", "windows", "arm64", "amd64"].contains(&label.as_str()),
                "{label}"
            );
        }
    }
}

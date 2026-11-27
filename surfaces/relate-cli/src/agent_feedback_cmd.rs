//! `relate agent-feedback` (ADR-0588 §3): the agent's channel. It takes the
//! structured report flags, requires the person's live standing grant, never
//! prompts, and sends one report through the crate's pipeline — opt-out,
//! authorization, validation and redaction, one POST, one local receipt.
//!
//! The surface exists only in agent mode: the mode gate is `main`'s, so the
//! absence of this module's command in a person's run is one unknown-command
//! refusal there, not a second check here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::Utc;
use relate_feedback::authorization::{Authorization, FeedbackConsent, DEFAULT_AUTHORIZATION_DAYS};
use relate_feedback::opt_out;
use relate_feedback::report::{Category, Context, Input, Invoker};
use relate_feedback::sender::FeedbackSender;
use relate_feedback::state;
use relate_feedback::submit::{self, Request};

use crate::config_dir::env_var as env;
use crate::parse_error::{flag_value, set_once, ParseError};

/// The command word, also the value a report's `command_id` filter
/// excludes: a report never names a feedback command. Named by `parse`,
/// the hint's prefill and the absent-surface refusal (the one place the
/// two surface words meet).
pub(crate) const WORD: &str = "agent-feedback";

/// The reporting rule every surface carries (the hint, the agent-context
/// readout, the skill bundle's shortened form), lead clause included: report
/// friction before moving on; one sentence each for what was expected and
/// what happened, nothing sensitive in them; the send waits for the
/// person's approval. One home for the wording, so the surfaces cannot
/// drift apart.
pub(crate) const REPORT_RULE: &str = "report failing or confusing commands before moving on — \
     draft one sentence each for --expected and --observed (no secrets, hosts, paths, Cypher, \
     or pasted output), show the person, and send only after their approval";

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
    crate::config_dir::with_config_dir(WORD, |dir| run_in(dir, input))
}

fn run_in(dir: &std::path::Path, input: &Input) -> i32 {
    let now = Utc::now();
    if !matches!(
        FeedbackConsent::load(dir).authorization(now),
        Authorization::Authorized { .. }
    ) {
        crate::print_error(format!(
            "Error: agent feedback is not enabled (not_enabled): the grant is a person's, \
             made at a terminal with '{}' ({DEFAULT_AUTHORIZATION_DAYS} days \
             per grant), or answered at the install/upgrade ask",
            crate::feedback_cmd::enable_invocation()
        ));
        return 1;
    }
    let sender = FeedbackSender::from_env(&env);
    let context = report_context(&env, invoker(&env));
    let doc = submit::submit(
        &Request {
            dir,
            input,
            context: &context,
            env: &env,
            config_telemetry: state::load_config_telemetry(dir),
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

/// The report's `os` property: `linux`, `darwin` or `windows` (the shared
/// `platform` vocabulary); any other platform is omitted rather than
/// guessed.
fn os_label() -> Option<String> {
    crate::platform::os_word(std::env::consts::OS).map(str::to_string)
}

/// The report's `os_arch` property: `arm64` or `amd64` (the shared
/// `platform` vocabulary); any other architecture is omitted rather than
/// guessed.
fn arch_label() -> Option<String> {
    crate::platform::arch_word(std::env::consts::ARCH).map(str::to_string)
}

/// The agent-run feedback pointer (LABS-1736, ADR-0588 §4): one line, agent
/// runs only, once per run — the process is the unit, because one
/// installation serves many different agents and each new process is a new
/// agent context to reach. This module owns the gate: the command the hint
/// advertises lives here, and the writers that carry diagnostics
/// (`print_error`, the query-error stderr path) only ask for the line.
static AGENT_RUN: AtomicBool = AtomicBool::new(false);
static HINT_SHOWN: AtomicBool = AtomicBool::new(false);

/// The run's command word, recorded once by the dispatch (`main`'s two
/// surface resolutions — the lifecycle table's own word, or `"query"` for a
/// statement run) and prefilled into the hint — run-scoped state like
/// [`AGENT_RUN`]: the dispatch owns the fact, the emission reads it. The
/// CLI's own `&'static` word, never raw argv — [`hint_text`] still refuses
/// to print a word that is not a visible command, so the closed set is
/// enforced at the one interpolation site. The error code is *not* stored:
/// each report path holds it and passes it to [`write_hint`], so a path
/// that forgets it fails to compile instead of silently dropping the
/// prefill.
static RUN_COMMAND: Mutex<Option<&'static str>> = Mutex::new(None);

/// Record the run's surface for the pointer gate — called once, from `main`,
/// from the one surface resolution.
pub(crate) fn set_agent_run(on: bool) {
    AGENT_RUN.store(on, Ordering::Relaxed);
}

/// Record the run's command word for the hint's prefill — the dispatch's
/// one call per process, with the CLI's own word.
pub(crate) fn set_run_command(word: &'static str) {
    *RUN_COMMAND
        .lock()
        .expect("the run-command slot is never poisoned") = Some(word);
}

/// The grant state the hint varies on (ADR-0588 §3): an opted-out
/// telemetry ladder emits nothing at all; a live grant gets the full
/// report-first instruction with the prefilled command; anything else
/// points at the person's `relate feedback enable` and never tells the
/// agent to draft a report it cannot send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintGrant {
    /// The telemetry ladder is off (`DO_NOT_TRACK`, `RELATE_TELEMETRY`, or
    /// the config file): no hint, not even once.
    OptedOut,
    /// The person's live standing grant.
    Granted,
    /// No grant, an expired one, or a recorded decline.
    NotGranted,
}

/// The grant state the process's next hint would carry: the telemetry
/// ladder first (an opted-out run shows nothing), then the consent record.
/// The same closed decisions the submit pipeline reads — the ladder through
/// `opt_out::resolve`, the grant through `FeedbackConsent::authorization`
/// plus the recorded decline — so the hint's promise can never disagree
/// with what `agent-feedback` would actually do.
fn hint_grant() -> HintGrant {
    let dir = crate::config_dir::config_dir();
    let ladder = opt_out::resolve(
        &env,
        dir.as_deref()
            .map(state::load_config_telemetry)
            .unwrap_or(state::ConfigTelemetry::Missing),
    );
    if !ladder.enabled() {
        return HintGrant::OptedOut;
    }
    let consent = dir
        .as_deref()
        .map(FeedbackConsent::load)
        .unwrap_or_default();
    // The one canonical grant verdict: `FeedbackConsent::authorization`,
    // exactly as `run_in` above and the crate's submit pipeline read it —
    // so the hint's promise can never disagree with what `agent-feedback`
    // would actually do.
    if matches!(
        consent.authorization(Utc::now()),
        Authorization::Authorized { .. }
    ) {
        HintGrant::Granted
    } else {
        HintGrant::NotGranted
    }
}

/// The once-per-run hint line, `None` when this run has had it, is a
/// person's, or is opted out. The cheap gate and the latch come first — a
/// person's run and a spent latch pay no config reads — then the grant
/// verdict and the run's command word. `error_code` is the report path's
/// own fact, passed in.
fn error_hint(error_code: Option<&'static str>) -> Option<String> {
    if !AGENT_RUN.load(Ordering::Relaxed) || HINT_SHOWN.swap(true, Ordering::Relaxed) {
        return None;
    }
    let grant = hint_grant();
    if grant == HintGrant::OptedOut {
        return None;
    }
    let command = *RUN_COMMAND
        .lock()
        .expect("the run-command slot is never poisoned");
    hint_text(grant, command, error_code)
}

/// The pointer's one emission: gate, latch, line and write in this one
/// function. Every process-stderr diagnostic binding calls it (LABS-1736)
/// — the lifecycle sink (`print_error`), the connect-failure and
/// store-open reports, and the statement run — so the emission exists once
/// and a new report path can neither re-spell it nor silently omit it.
pub(crate) fn write_hint(sink: &mut dyn std::io::Write, error_code: Option<&'static str>) {
    if let Some(hint) = error_hint(error_code) {
        crate::write_error(sink, hint);
    }
}

/// Append one prefill fact as ` <word> <value>`, or nothing when the fact is
/// absent — the one spelling of the granted hint's append idiom (every part
/// of the line, the command word included, is appended through it).
fn push_fact(line: &mut String, flag: &str, value: Option<&'static str>) {
    if let Some(value) = value {
        line.push(' ');
        line.push_str(flag);
        line.push(' ');
        line.push_str(value);
    }
}

/// [`Self::error_hint`]'s pure renderer: the granted line carries the
/// prefilled command — every interpolated value is a closed-set one (the
/// command word is printed only when it names a visible command, the code
/// only when it names a nonzero exit-table row), so no user text can ride
/// the hint — and the report-first, approval-gated wording: the agent
/// drafts and shows the person, and sends only after a yes (ADR-0588 §3;
/// the wording is about reporting, never about sending without the
/// person's approval). The gate, the latch and the opted-out verdict are
/// [`Self::error_hint`]'s; `HintGrant::OptedOut` never reaches here.
fn hint_text(
    grant: HintGrant,
    command: Option<&'static str>,
    error_code: Option<&'static str>,
) -> Option<String> {
    match grant {
        // The gate resolves this before the renderer; a direct caller gets
        // the handled no-hint answer, never a panic.
        HintGrant::OptedOut => None,
        HintGrant::NotGranted => Some(format!(
            "hint: agent feedback is not enabled — ask the person to run \
             '{}' before drafting a report",
            crate::feedback_cmd::enable_invocation()
        )),
        HintGrant::Granted => {
            // The rule sentence is the shared constant, the command word
            // the module's own, and the category the crate's typed
            // vocabulary (`Category::as_str`, the one wire-spelling author)
            // — the remaining flags are the descriptor's, and the e2e pins
            // this exact line, so a rename fails loudly.
            let mut line = format!("hint: {REPORT_RULE}:");
            push_fact(&mut line, "relate", Some(WORD));
            push_fact(
                &mut line,
                "--category",
                Some(Category::UnclearError.as_str()),
            );
            // The one append idiom: each fact filters itself against its own
            // closed set; the helper owns the spelling of the appended pair.
            push_fact(
                &mut line,
                "--command",
                command.filter(|word| visible_commands().iter().any(|candidate| candidate == word)),
            );
            push_fact(
                &mut line,
                "--error-code",
                error_code.filter(|code| {
                    crate::errors::error_code_names()
                        .iter()
                        .any(|candidate| candidate == code)
                }),
            );
            push_fact(&mut line, "--expected", Some("\"...\""));
            push_fact(&mut line, "--observed", Some("\"...\""));
            Some(line)
        }
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

    /// The granted hint prefills from the closed sets only: the visible
    /// command word and the exit-table code name ride it; a word outside
    /// `visible_commands()` or an unknown code is dropped, never echoed
    /// (the wording is report-first and approval-gated).
    #[test]
    fn the_granted_hint_prefills_from_the_closed_sets_only() {
        let line = hint_text(
            HintGrant::Granted,
            Some("query"),
            Some(crate::errors::USAGE_ERROR_CODE),
        )
        .unwrap();
        assert!(
            line.contains(&format!(
                "relate {WORD} --category unclear_error --command query --error-code {}",
                crate::errors::USAGE_ERROR_CODE
            )),
            "{line}"
        );
        assert!(
            line.contains("show the person") && line.contains("only after their approval"),
            "the consent framing is report-first, approval-gated: {line}"
        );
        // Closed sets: an unknown command word and an unknown code name are
        // dropped from the prefill, not echoed.
        let line = hint_text(HintGrant::Granted, Some("feedback"), Some("nope")).unwrap();
        assert!(
            !line.contains("--command") && !line.contains("--error-code"),
            "unknown words never ride the hint: {line}"
        );
    }

    /// Without a live grant the hint points at the person's enable and
    /// never tells the agent to draft a report it cannot send (the
    /// not_enabled nudge, REL8-1735; the once-per-process gate and the
    /// person's-run silence are pinned by `e2e_feedback`).
    #[test]
    fn a_missing_grant_nudges_the_persons_enable_and_never_a_draft() {
        let line = hint_text(HintGrant::NotGranted, Some("query"), None).unwrap();
        assert!(line.contains("relate feedback enable"), "{line}");
        assert!(
            !line.contains("agent-feedback --"),
            "the not-granted hint must not advertise a sendable command: {line}"
        );
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

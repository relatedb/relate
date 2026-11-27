//! `relate feedback` (ADR-0588 §3): the person's channel. One optional TEXT
//! (or its explicit `--observed` spelling) becomes a prefilled public GitHub
//! issue — nothing is sent from here; the person reviews and submits the
//! form. The `enable`/`disable` action words grant and revoke the agent
//! channel, and the bare command prints the standing state.
//!
//! The surface exists only while the run is the person's (agent mode off):
//! the mode gate is `main`'s, so agent mode answers `feedback` as an unknown
//! command naming `relate agent-feedback`. `enable` additionally needs a
//! terminal — the grant is a person's act, never a script's.

use std::io::{self, BufRead, IsTerminal};
use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{DateTime, SecondsFormat, Utc};
use relate_feedback::authorization::{Authorization, FeedbackConsent, DEFAULT_AUTHORIZATION_DAYS};
use relate_feedback::opt_out;
use relate_feedback::report::Category;
use relate_feedback::sender;
use relate_feedback::state;

use crate::config_dir::env_var as env;
use crate::parse_error::{flag_value, set_once, ParseError};

/// The command word, also the config-dir error's command prefix.
pub(crate) const WORD: &str = "feedback";

/// The two action words. They are always actions: text equal to one is sent
/// through `--observed` (ADR-0069's lifecycle-verb grammar).
const ACTIONS: [&str; 2] = ["enable", "disable"];

/// The repository's new-issue form: the human surface's whole destination.
const ISSUE_URL: &str = "https://github.com/relatedb/relate/issues/new";

/// The longest one-line title, in characters.
const TITLE_CHARS: usize = 80;

/// What `relate feedback` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackAction {
    /// File a prefilled GitHub issue: the freed text plus the optional
    /// category prefix and expected-behaviour paragraph.
    Issue {
        text: String,
        category: Category,
        expected: Option<String>,
    },
    /// Grant the agent feedback authorization for the default window.
    Enable,
    /// Revoke the agent feedback authorization.
    Disable,
    /// Print the standing state.
    State,
}

impl FeedbackAction {
    /// The CLI spelling of the action (also the telemetry event value).
    pub fn as_str(&self) -> &'static str {
        match self {
            FeedbackAction::Issue { .. } => "issue",
            FeedbackAction::Enable => "enable",
            FeedbackAction::Disable => "disable",
            FeedbackAction::State => "state",
        }
    }
}

/// Parse the argument tail after the `feedback` verb. Pure — returns the
/// usage-error message instead of exiting, so the grammar is unit-testable;
/// `main` turns `Err` into the hand-rolled parser's exit-2 convention.
///
/// Grammar: `[TEXT | enable | disable] [--category NAME] [--expected TEXT]
/// [--observed TEXT]`. The action words are recognized only as the whole
/// first positional; a text equal to one travels through `--observed`.
pub fn parse_action(rest: &[String]) -> Result<FeedbackAction, ParseError> {
    let mut positional = None;
    let mut category = None;
    let mut expected = None;
    let mut observed = None;
    let mut args = rest.iter();
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
            other if other.starts_with('-') => return Err(ParseError::unexpected(WORD, other)),
            other => {
                if positional.is_some() {
                    return Err(ParseError::unexpected(WORD, other));
                }
                positional = Some(other.to_string());
            }
        }
    }

    let category_given = category.is_some();
    let category = match category {
        Some(raw) => Category::parse(&raw).ok_or_else(|| {
            format!(
                "'{raw}' is not a category; use one of {}",
                Category::ALL
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
        None => Category::Friction,
    };

    if positional
        .as_deref()
        .is_some_and(|word| ACTIONS.contains(&word))
    {
        let action = positional.expect("checked above");
        if category_given || expected.is_some() || observed.is_some() {
            return Err(format!("feedback {action} takes no flags").into());
        }
        return Ok(if action == "enable" {
            FeedbackAction::Enable
        } else {
            FeedbackAction::Disable
        });
    }

    let text = match (positional, observed) {
        (Some(_), Some(_)) => {
            return Err(
                "feedback takes the text either as its argument or with --observed, \
                        not both"
                    .into(),
            )
        }
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) if category_given || expected.is_some() => {
            return Err("feedback needs TEXT (or --observed) to file an issue".into())
        }
        (None, None) => return Ok(FeedbackAction::State),
    };
    if text.trim().is_empty() {
        return Err("feedback needs non-empty TEXT".into());
    }
    Ok(FeedbackAction::Issue {
        text,
        category,
        expected,
    })
}

/// Run the action. Returns the process exit code; the caller (main) owns
/// `std::process::exit`.
pub fn run(action: FeedbackAction) -> i32 {
    match action {
        FeedbackAction::Issue {
            text,
            category,
            expected,
        } => submit_issue(&text, category, expected.as_deref(), &open_in_browser),
        FeedbackAction::Enable => crate::config_dir::with_config_dir("feedback enable", |dir| {
            enable(dir, &env, io::stdin().is_terminal())
        }),
        FeedbackAction::Disable => crate::config_dir::with_config_dir("feedback disable", disable),
        FeedbackAction::State => crate::config_dir::with_config_dir("feedback state", |dir| {
            state_with(
                dir,
                &env,
                io::stdin().is_terminal(),
                &mut io::stdin().lock(),
            )
        }),
    }
}

/// The person's enable invocation — the one spelling the refusals, the
/// agent hint and the agent-context readout all name, built from this
/// module's own verb and action words.
pub(crate) fn enable_invocation() -> String {
    format!("relate {} {}", WORD, FeedbackAction::Enable.as_str())
}

/// File a GitHub issue: derive the title and body, build and print the
/// prefilled form's URL, and hand it to the browser. The open is the seam a
/// test replaces; a failed open leaves the printed URL as the fallback, so
/// the exit stays 0 either way.
fn submit_issue(
    text: &str,
    category: Category,
    expected: Option<&str>,
    open: &dyn Fn(&str) -> io::Result<()>,
) -> i32 {
    let (title, body) = issue_text(text, category, expected);
    let url = issue_url(&title, &body);
    println!("{url}");
    let _ = open(&url);
    0
}

/// Open `url` with the platform's handler, detached: the child's output is
/// discarded and its failure is not this command's error (the URL was
/// printed).
fn open_in_browser(url: &str) -> io::Result<()> {
    // No shell on Windows: the URL carries `&` and `%`, which `cmd` would
    // split on and expand, so `start` would receive a truncated URL and the
    // form would open without the report. `explorer` hands the argument to
    // the default handler as one argv entry.
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The GitHub issue text from the freed text (ADR-0588 §3): a multi-line
/// text's first line is the title and the remainder the body; a one-line
/// text's title is its first [`TITLE_CHARS`] characters cut back to a
/// whitespace boundary (an unbroken word fills the title to the cap), while
/// the body keeps the full text, so a cut title never loses content.
fn issue_text(text: &str, category: Category, expected: Option<&str>) -> (String, String) {
    let (title, mut body) = match text.split_once('\n') {
        Some((first, rest)) => (first.trim().to_string(), rest.to_string()),
        None => (one_line_title(text), text.to_string()),
    };
    if let Some(expected) = expected {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(expected);
    }
    (format!("[{}] {title}", category.as_str()), body)
}

/// The one-line title: the whole line when it fits, else the first
/// [`TITLE_CHARS`] characters cut back to the last whitespace inside them.
fn one_line_title(text: &str) -> String {
    let Some((cut, _)) = text.char_indices().nth(TITLE_CHARS) else {
        return text.to_string();
    };
    let head = &text[..cut];
    match head.rfind(char::is_whitespace) {
        Some(at) => head[..at].trim_end().to_string(),
        None => head.to_string(),
    }
}

/// The prefilled new-issue URL. `title` and `body` are percent-encoded over
/// RFC 3986's unreserved set; nothing is capped or truncated — the person
/// edits the form before submitting and GitHub's own limits govern, so a
/// body large enough to push the URL past a browser's practical length is
/// still printed and opened whole.
fn issue_url(title: &str, body: &str) -> String {
    format!("{ISSUE_URL}?title={}&body={}", encode(title), encode(body))
}

/// Percent-encode one URL query value.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `feedback enable`: record the agent grant with the destination the
/// reports would use. Only a person's terminal may grant it, and the
/// confirmation line naming the resolved destination and the
/// [`DEFAULT_AUTHORIZATION_DAYS`] window is printed once the record is
/// written.
fn enable(dir: &Path, env: &dyn Fn(&str) -> Option<String>, stdin_is_tty: bool) -> i32 {
    if !stdin_is_tty {
        return refuse_enable(
            "a terminal is required: the grant is a person's decision, not a script's",
        );
    }
    let endpoint = match sender::resolve_feedback_endpoint(env) {
        Ok(Some(endpoint)) => endpoint,
        Ok(None) => {
            return refuse_enable(
                "no destination is configured: set RELATE_FEEDBACK_TOKEN or \
                 RELATE_FEEDBACK_ENDPOINT",
            )
        }
        Err(url) => {
            return refuse_enable(&format!(
                "RELATE_FEEDBACK_ENDPOINT={url}: https on a Mixpanel host or http on \
                 loopback hosts is required"
            ))
        }
    };
    // A grant overwrites every field of the record, so it starts from the
    // empty one rather than loading a record it would replace.
    let mut record = FeedbackConsent::default();
    let until = record.authorize(Utc::now(), endpoint.clone());
    // Persist first, then confirm: a confirmation printed before the write
    // would tell the person a grant landed that did not. `disable` reads the
    // same way round.
    if let Err(code) = crate::feedback_ask::save(&record, dir, "enable") {
        return code;
    }
    println!(
        "feedback enabled until {}: agent reports go to {endpoint} for \
         {DEFAULT_AUTHORIZATION_DAYS} days",
        until.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    0
}

/// The one `feedback enable` refusal: nothing is recorded, exit 1.
fn refuse_enable(detail: &str) -> i32 {
    crate::print_error(format!("Error: feedback enable: refusing {detail}"));
    1
}

/// `feedback disable`: clear the consent record, which revokes the grant.
fn disable(dir: &Path) -> i32 {
    if let Err(code) = crate::feedback_ask::save(&FeedbackConsent::default(), dir, "disable") {
        return code;
    }
    println!("feedback disabled");
    0
}

/// `feedback` bare: ask the one-time question (a person's terminal only),
/// then print the standing state and the synopsis.
fn state_with(
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    stdin: &mut dyn BufRead,
) -> i32 {
    let mut record = FeedbackConsent::load(dir);
    if let Err(code) =
        crate::feedback_ask::ask_first_run(dir, env, stdin_is_tty, &mut record, stdin)
    {
        return code;
    }
    let telemetry = opt_out::resolve(env, state::load_config_telemetry(dir));
    println!("{}", state_line(telemetry, &record, Utc::now()));
    println!(
        "{}",
        crate::help::command_usage(crate::help::HelpTopic::Feedback)
    );
    0
}

/// The standing state line: the telemetry opt-out layer first (it suppresses
/// every agent send), then the grant's window and destination.
fn state_line(
    telemetry: opt_out::EffectiveState,
    record: &FeedbackConsent,
    now: DateTime<Utc>,
) -> String {
    if !telemetry.enabled() {
        return format!("feedback {}", telemetry.status_line());
    }
    match record.authorization(now) {
        Authorization::Authorized { until, endpoint } => format!(
            "feedback enabled until {} ({endpoint})",
            until.to_rfc3339_opts(SecondsFormat::Secs, true)
        ),
        Authorization::Expired => "feedback disabled (authorization expired)".to_string(),
        Authorization::Missing if record.declined => "feedback disabled (declined)".to_string(),
        Authorization::Missing => "feedback disabled (not enabled)".to_string(),
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

    fn endpoint_env() -> impl Fn(&str) -> Option<String> {
        env_of(&[("RELATE_FEEDBACK_ENDPOINT", "http://127.0.0.1:9/track")])
    }
    #[test]
    fn bare_feedback_prints_the_state() {
        assert_eq!(parse_action(&[]), Ok(FeedbackAction::State));
        assert_eq!(FeedbackAction::State.as_str(), "state");
    }

    #[test]
    fn action_words_are_always_actions() {
        assert_eq!(parse_action(&args(&["enable"])), Ok(FeedbackAction::Enable));
        assert_eq!(
            parse_action(&args(&["disable"])),
            Ok(FeedbackAction::Disable)
        );
        assert_eq!(FeedbackAction::Enable.as_str(), "enable");
        assert_eq!(FeedbackAction::Disable.as_str(), "disable");
        // The action-word collision rule: the text travels through
        // `--observed`.
        assert_eq!(
            parse_action(&args(&["--observed", "enable"])),
            Ok(FeedbackAction::Issue {
                text: s("enable"),
                category: Category::Friction,
                expected: None,
            })
        );
        assert_eq!(
            parse_action(&args(&["--observed", "disable"])),
            Ok(FeedbackAction::Issue {
                text: s("disable"),
                category: Category::Friction,
                expected: None,
            })
        );
    }

    #[test]
    fn the_text_is_the_positional_or_its_explicit_spelling() {
        assert_eq!(
            parse_action(&args(&["the info command printed help"])),
            Ok(FeedbackAction::Issue {
                text: s("the info command printed help"),
                category: Category::Friction,
                expected: None,
            })
        );
        assert_eq!(
            parse_action(&args(&[
                "the info command printed help",
                "--category",
                "unclear_error",
                "--expected",
                "the version",
            ])),
            Ok(FeedbackAction::Issue {
                text: s("the info command printed help"),
                category: Category::UnclearError,
                expected: Some(s("the version")),
            })
        );
        assert_eq!(
            parse_action(&args(&["one", "two"])),
            Err(ParseError::unexpected(WORD, "two"))
        );
        assert_eq!(
            parse_action(&args(&["--observed", "one", "two"])),
            Err(
                "feedback takes the text either as its argument or with --observed, \
                 not both"
                    .into()
            ),
            "a positional beside --observed is the text given twice"
        );
        assert_eq!(
            parse_action(&args(&["text", "--observed", "other"])),
            Err(
                "feedback takes the text either as its argument or with --observed, \
                 not both"
                    .into()
            )
        );
    }

    #[test]
    fn flags_without_text_and_empty_text_are_usage_errors() {
        assert_eq!(
            parse_action(&args(&["--category", "friction"])),
            Err("feedback needs TEXT (or --observed) to file an issue".into())
        );
        assert_eq!(
            parse_action(&args(&["   "])),
            Err("feedback needs non-empty TEXT".into())
        );
        assert_eq!(
            parse_action(&args(&["enable", "--expected", "x"])),
            Err("feedback enable takes no flags".into())
        );
    }

    #[test]
    fn the_category_is_the_enum_vocabulary_and_defaults_to_friction() {
        let err = parse_action(&args(&["text", "--category", "other_friction"]))
            .unwrap_err()
            .to_string();
        for name in [
            "friction",
            "unclear_error",
            "unexpected_behavior",
            "missing_capability",
        ] {
            assert!(err.contains(name), "{name} missing: {err}");
        }
        assert!(parse_action(&args(&["text", "--bogus", "x"])).is_err());
        let duplicated = args(&["text", "--category", "friction", "--category", "clarity"]);
        assert!(parse_action(&duplicated)
            .unwrap_err()
            .to_string()
            .contains("only once"));
    }

    /// A multi-line text's first line is the title and the remainder the
    /// body; the category prefixes the title and `--expected` joins the
    /// body.
    #[test]
    fn multiline_text_splits_at_the_first_newline() {
        assert_eq!(
            issue_text(
                "info prints help\nExpected the version.\nThen a table.",
                Category::Friction,
                None
            ),
            (
                s("[friction] info prints help"),
                s("Expected the version.\nThen a table.")
            )
        );
        assert_eq!(
            issue_text("title\nexpected body", Category::Friction, Some("and more")),
            (s("[friction] title"), s("expected body\n\nand more"))
        );
    }

    /// A one-line text keeps its full text as the body and derives the title
    /// from it.
    #[test]
    fn one_line_text_keeps_the_full_text_in_the_body() {
        let text = "the info command printed help instead of the version";
        assert_eq!(
            issue_text(text, Category::UnclearError, None),
            (format!("[unclear_error] {text}"), text.to_string())
        );
    }

    #[test]
    fn a_one_line_title_cuts_back_to_a_whitespace_boundary() {
        let text = format!("{} tail", "a".repeat(TITLE_CHARS - 1));
        let (title, body) = issue_text(&text, Category::Friction, None);
        assert_eq!(title, format!("[friction] {}", "a".repeat(TITLE_CHARS - 1)));
        assert_eq!(body, text, "the body keeps the full text");
    }

    #[test]
    fn an_unbroken_word_fills_the_title_to_the_cap() {
        let text = "b".repeat(TITLE_CHARS + 20);
        let (title, body) = issue_text(&text, Category::Friction, None);
        assert_eq!(title, format!("[friction] {}", "b".repeat(TITLE_CHARS)));
        assert_eq!(body, text);
    }

    /// A multi-line text whose first line is the whole content has an empty
    /// body; `--expected` then fills it.
    #[test]
    fn an_empty_body_is_kept_and_expected_fills_it() {
        assert_eq!(
            issue_text("only a title\n", Category::Friction, None),
            (s("[friction] only a title"), s(""))
        );
        assert_eq!(
            issue_text("only a title\n", Category::Friction, Some("expected")),
            (s("[friction] only a title"), s("expected"))
        );
    }

    #[test]
    fn the_url_encodes_the_query_values() {
        let url = issue_url("[friction] a & b", "line one\nline two");
        assert_eq!(
            url,
            "https://github.com/relatedb/relate/issues/new\
             ?title=%5Bfriction%5D%20a%20%26%20b&body=line%20one%0Aline%20two"
        );
        assert!(url.contains("%20") && !url.contains(' '));
        // Unreserved characters pass through; multibyte text is encoded per
        // byte.
        assert_eq!(encode("a-Z_9.~"), "a-Z_9.~");
        assert_eq!(encode("é"), "%C3%A9");
    }

    /// Nothing is capped: a long body arrives whole, however long the URL
    /// gets (the person edits the form; GitHub's limits govern).
    #[test]
    fn a_long_body_is_never_truncated() {
        let body = "x".repeat(10_000);
        let url = issue_url("t", &body);
        assert!(url.contains(&body), "the full body is in the URL");
        assert!(url.len() > 10_000);
    }

    #[test]
    fn the_issue_is_printed_and_handed_to_the_browser_seam() {
        let opened = std::cell::RefCell::new(None);
        let record = |url: &str| {
            *opened.borrow_mut() = Some(url.to_string());
            Ok(())
        };
        // The seam records instead of opening a browser; the URL is the
        // function's own derivation.
        let (title, body) = issue_text("hello", Category::Friction, None);
        let expected = issue_url(&title, &body);
        assert_eq!(submit_issue("hello", Category::Friction, None, &record), 0);
        assert_eq!(*opened.borrow(), Some(expected));
    }

    #[test]
    fn piped_enable_is_refused_and_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(enable(dir, &endpoint_env(), false), 1);
        assert!(
            FeedbackConsent::load(dir).authorization(Utc::now()) == Authorization::Missing,
            "a refused grant records nothing"
        );
    }

    #[test]
    fn enable_records_the_resolved_destination_and_clears_a_decline() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        FeedbackConsent {
            declined: true,
            ..FeedbackConsent::default()
        }
        .save(dir)
        .unwrap();
        assert_eq!(enable(dir, &endpoint_env(), true), 0);
        let record = FeedbackConsent::load(dir);
        assert!(!record.declined);
        assert!(matches!(
            record.authorization(Utc::now()),
            Authorization::Authorized { .. }
        ));
    }

    #[test]
    fn enable_with_no_destination_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // The compiled-in project token resolves a destination by default;
        // the no-destination shape needs the explicit empty override.
        let no_destination = env_of(&[("RELATE_FEEDBACK_TOKEN", "")]);
        assert_eq!(enable(dir, &no_destination, true), 1);
        assert!(FeedbackConsent::load(dir).authorization(Utc::now()) == Authorization::Missing);
    }

    #[test]
    fn disable_clears_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        enable(dir, &endpoint_env(), true);
        assert_eq!(disable(dir), 0);
        assert_eq!(FeedbackConsent::load(dir), FeedbackConsent::default());
    }

    /// The standing state line names the deciding layer, the grant window
    /// and the destination (restored after the ask split — it is the
    /// retained `state_line`'s only test).
    #[test]
    fn state_line_reports_the_layer_the_window_and_the_destination() {
        let now = DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let enabled = opt_out::EffectiveState::Default;
        let authorized = |until: &str| FeedbackConsent {
            endpoint: Some("https://api.mixpanel.com/track".to_string()),
            authorized_until: Some(until.to_string()),
            ..FeedbackConsent::default()
        };
        assert_eq!(
            state_line(enabled, &authorized("2026-12-12T12:00:00Z"), now),
            "feedback enabled until 2026-12-12T12:00:00Z \
             (https://api.mixpanel.com/track)"
        );
        assert_eq!(
            state_line(enabled, &FeedbackConsent::default(), now),
            "feedback disabled (not enabled)"
        );
        let declined = FeedbackConsent {
            declined: true,
            ..FeedbackConsent::default()
        };
        assert_eq!(
            state_line(enabled, &declined, now),
            "feedback disabled (declined)"
        );
        assert_eq!(
            state_line(enabled, &authorized("2026-11-01T12:00:00Z"), now),
            "feedback disabled (authorization expired)"
        );
        for disabled in [
            opt_out::EffectiveState::DoNotTrack,
            opt_out::EffectiveState::Env(false),
            opt_out::EffectiveState::ConfigFile(false),
        ] {
            assert_eq!(
                state_line(disabled, &authorized("2026-12-12T12:00:00Z"), now),
                format!("feedback {}", disabled.status_line())
            );
        }
    }
}

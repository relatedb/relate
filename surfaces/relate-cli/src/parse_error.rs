//! What a command parser reports when it rejects its arguments (ADR-0192),
//! the value-taking-flag primitive every command parser shares, and the
//! access-flag policies both `--read-only` grammars share (ADR-0480): the
//! freshness value and its sighting parse, the once-only policy, and the
//! one spelling the conflict messages read.
//! A neutral home: the command modules (`clone_cmd`, `pull_cmd`, ...) depend
//! on this, not on the dispatcher in `flags.rs`, and the did-you-mean matching
//! in `suggest.rs` stays a pure helper.

use crate::suggest::suggest_flag;

/// The store spelling ADR-0383 removed, written out once so every negative
/// test and every rejection branch — the extension actions'
/// host's — name the real spelling. The parsers reject it without a
/// near-miss hint (ADR-0383); nothing accepts it.
#[cfg(feature = "extensions")]
pub(crate) const REMOVED_DB_SPELLING: &str = "--db";

/// A command parser's failure.
///
/// Most failures are just a message. A rejected argument additionally carries
/// the argument itself, as data, so nothing re-parses text the parsers format.
/// `From<String>`/`From<&str>` keep `?` and `.into()` at the existing error
/// sites working unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// Any failure with no rejected argument: a missing value or positional,
    /// an invalid value.
    Message(String),
    /// `command` does not accept `arg`: an unknown flag, an extra positional,
    /// or a stray word.
    Rejected { command: String, arg: String },
    /// `command` does not accept `arg`, a deliberately removed spelling: the
    /// same unknown-argument error as [`Self::Rejected`], but it never draws
    /// a near-miss hint (ADR-0383). Gated with its constructors — the
    /// parser's and the extension host's — the two grammars that remove a
    /// spelling; no other grammar does.
    #[cfg(feature = "extensions")]
    Removed { command: String, arg: String },
}

impl ParseError {
    /// The error for an argument `command` does not accept.
    pub(crate) fn unexpected(command: &str, arg: &str) -> Self {
        Self::Rejected {
            command: command.to_string(),
            arg: arg.to_string(),
        }
    }

    /// The error for a spelling `command` deliberately removed: still the
    /// unknown-argument wording, but [`Self::suggestion`] declines it so a
    /// removed flag never hints at its replacement (ADR-0383).
    #[cfg(feature = "extensions")]
    pub(crate) fn removed(command: &str, arg: &str) -> Self {
        Self::Removed {
            command: command.to_string(),
            arg: arg.to_string(),
        }
    }

    /// The suggestion for this error, matched against the command's own
    /// `known` flags. Only a rejected argument can be a misspelled flag: a
    /// message gets none (so a flag-shaped value another flag swallowed never
    /// draws one), and `suggest_flag` itself declines anything that does not
    /// start with `-`. `aliases` are the caller surface's exact-match
    /// spellings (the top-level query run passes `help::FLAG_ALIASES`, a
    /// subcommand `&[]`), keeping this shared type free of top-level flag
    /// knowledge. The candidates borrow from the caller's list — a static
    /// flag table or a manifest's flag entries — so the suggestion is
    /// candidate text itself, never a copy.
    pub(crate) fn suggestion<'a>(
        &self,
        known: &[&'a str],
        aliases: &[(&str, &'a str)],
    ) -> Option<&'a str> {
        match self {
            Self::Rejected { arg, .. } => suggest_flag(arg, known, aliases),
            Self::Message(_) => None,
            #[cfg(feature = "extensions")]
            Self::Removed { .. } => None,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(msg) => f.write_str(msg),
            Self::Rejected { command, arg } => unknown_argument(f, command, arg),
            #[cfg(feature = "extensions")]
            Self::Removed { command, arg } => unknown_argument(f, command, arg),
        }
    }
}

/// The unknown-argument wording both the rejected and the deliberately
/// removed arms render (ADR-0383): one formatter, so the removed spelling's
/// message cannot drift from the ordinary rejected one.
fn unknown_argument(f: &mut std::fmt::Formatter<'_>, command: &str, arg: &str) -> std::fmt::Result {
    write!(f, "unexpected argument to {command}: {arg}")
}

impl From<String> for ParseError {
    fn from(msg: String) -> Self {
        Self::Message(msg)
    }
}

impl From<&str> for ParseError {
    fn from(msg: &str) -> Self {
        Self::Message(msg.to_string())
    }
}

/// Advance a command parser's argument iterator past a flag to its value, or
/// fail with the shared "requires" text.
pub(crate) fn flag_value<'a>(
    args: &mut impl Iterator<Item = &'a String>,
    flag: &str,
    what: &str,
) -> Result<&'a String, ParseError> {
    args.next()
        .ok_or_else(|| format!("{flag} requires {what}").into())
}

/// The `-d` / `--database` value, reported as `--database` for both spellings:
/// the one value policy behind every grammar that takes a store path.
/// `serve` composes the same policy through its own `value_flag` arm
/// (flag_value + set_once) rather than this helper, so it is not a
/// consumer here. Gated with the grammars that call it: a bundle that
/// compiles none of `pull`, `push`, `info` or `extensions` never parses
/// a store flag.
#[cfg(any(
    feature = "pull",
    feature = "push",
    feature = "info",
    feature = "extensions"
))]
pub(crate) fn database_flag<'a>(
    args: &mut impl Iterator<Item = &'a String>,
) -> Result<&'a String, ParseError> {
    flag_value(args, "--database", "a path")
}

/// The one access flag's spelling (ADR-0480) — the single vocabulary fact
/// behind every message that names it: the once-only repeat error, the
/// pre-verb riders' conflict message, and the serve parse tail's post-verb
/// naming. Ungated: the shared conflict half reads it in serve-off builds
/// too.
pub(crate) const ACCESS_FLAG: &str = "--read-only";

/// The freshness value of the one access flag (ADR-0480): `--read-only`
/// opens a frozen snapshot (ADR-0136, the default), `--read-only=live`
/// tails the WAL beside a writer (ADR-0137, the former `--follow`).
/// "snapshot" and "live" are the user-facing words; the engine's
/// `OpenMode::ReadOnly` / `OpenMode::Follow` stay the internal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// `snapshot` — bare `--read-only` and `--read-only=snapshot`.
    Snapshot,
    /// `live` — `--read-only=live`.
    Live,
}

impl Freshness {
    /// The open mode this freshness selects.
    fn open_mode(&self) -> relate::OpenMode {
        match self {
            Freshness::Snapshot => relate::OpenMode::ReadOnly,
            Freshness::Live => relate::OpenMode::Follow,
        }
    }
}

/// Parse one freshness value — the one statement of the vocabulary,
/// consulted by both the attached-value parse and the space-separated
/// refusal guard below, so the two cannot disagree on what a value is.
fn parse_freshness(value: &str) -> Option<Freshness> {
    match value {
        "snapshot" => Some(Freshness::Snapshot),
        "live" => Some(Freshness::Live),
        _ => None,
    }
}

/// The one token grammar of the access flag (ADR-0480 §3), stated once:
/// the bare spelling, the attached `=value`, or a token that is not a
/// sighting. Both [`is_access_sighting`] and [`parse_access_sighting`]
/// consult it, so recognition and parsing cannot disagree on the shapes.
enum AccessSighting<'a> {
    /// The bare `--read-only`.
    Bare,
    /// The attached `--read-only=<value>`.
    Attached(&'a str),
    /// Not a sighting — the grammar arms' unknown-flag path.
    No,
}

/// Split one token into its [`AccessSighting`] shape, off
/// [`ACCESS_FLAG`].
fn split_access_sighting(token: &str) -> AccessSighting<'_> {
    match token.strip_prefix(ACCESS_FLAG) {
        Some("") => AccessSighting::Bare,
        Some(rest) => match rest.strip_prefix('=') {
            Some(value) => AccessSighting::Attached(value),
            None => AccessSighting::No,
        },
        None => AccessSighting::No,
    }
}

/// Whether `token` is a `--read-only` sighting (ADR-0480 §3): the bare
/// spelling or the attached `--read-only=<value>`. Derived from the one
/// [`split_access_sighting`] grammar, stated beside [`ACCESS_FLAG`] so
/// neither grammar's arm re-types the spelling — the two grammars are
/// exactly what must not drift.
pub(crate) fn is_access_sighting(token: &str) -> bool {
    !matches!(split_access_sighting(token), AccessSighting::No)
}

/// Parse one `--read-only` sighting (ADR-0480 §3): `token` is the argv
/// token — bare `--read-only` (the default snapshot) or the attached
/// `--read-only=snapshot` / `--read-only=live` — and `next` the following
/// token when one exists. The value is attached-form only: a
/// space-separated `live`/`snapshot` is a usage error naming the correct
/// spelling, never a silently-swallowed database path or query, and any
/// other value is a usage error listing the two. Total over its input: a
/// token that is not a sighting (per [`is_access_sighting`]) is an error,
/// never a panic. Returns the module's own [`ParseError`] contract, like
/// every sibling parser here. Shared by both access grammars (the query
/// session's pre-verb flags and `serve`'s post-verb ones), so the two
/// cannot disagree on the spelling rules.
pub(crate) fn parse_access_sighting(
    token: &str,
    next: Option<&str>,
) -> Result<Freshness, ParseError> {
    match split_access_sighting(token) {
        // The attached `--read-only=<value>`.
        AccessSighting::Attached(value) => parse_freshness(value).ok_or_else(|| {
            format!("invalid {ACCESS_FLAG}: '{value}' is not a freshness (snapshot or live)").into()
        }),
        // The bare spelling. A space-separated value never silently becomes
        // the path or the query — both values are refused naming the
        // attached form.
        AccessSighting::Bare => match next.filter(|n| parse_freshness(n).is_some()) {
            Some(value) => {
                Err(format!("{ACCESS_FLAG} takes its value attached: {ACCESS_FLAG}={value}").into())
            }
            None => Ok(Freshness::Snapshot),
        },
        // Not a sighting; the grammar arms match `is_access_sighting`
        // first, so this arm is the total-parse backstop, never the live
        // path.
        AccessSighting::No => Err(format!("invalid access flag: {token}").into()),
    }
}

/// Park one `--read-only` sighting's mode in the access slot (ADR-0480 §4):
/// [`set_once`]'s once-only policy with the freshness mapped to its open
/// mode. `None` is the no-flag `OpenMode::Writable` default, so the slot
/// carries the supplied-ness and the mode as one fact — both access
/// grammars park through here, and there is no fold (ADR-0306 §1's
/// precedence rule died with the second flag).
pub(crate) fn set_access_once(
    command: &str,
    slot: &mut Option<relate::OpenMode>,
    freshness: Freshness,
) -> Result<(), ParseError> {
    set_once(command, slot, ACCESS_FLAG, freshness.open_mode())
}

/// The access slot's one resolution (ADR-0480): `None` is the no-flag
/// `OpenMode::Writable` default. Stated once beside the slot's park
/// ([`set_access_once`]) and consulted by the two readers that open a
/// store — the query session's `Flags::open_mode` and `serve`'s command
/// construction. (The conflict decision takes the slot itself and never
/// resolves.)
pub(crate) fn resolve_access(slot: Option<relate::OpenMode>) -> relate::OpenMode {
    slot.unwrap_or(relate::OpenMode::Writable)
}

/// The once-only policy's single guard, consulted by both helpers below
/// (the LABS-465 finding: the guard was hand-rolled per flag and per
/// command). A flag sighted when it was already taken is a usage error
/// naming the `command` and the `flag` (`push --to may be given only
/// once`) — the policy's one spelling of its prose, so [`set_once`] and
/// [`flag_once`] cannot drift apart.
fn once(command: &str, flag: &str, taken: bool) -> Result<(), ParseError> {
    if taken {
        return Err(format!("{command} {flag} may be given only once").into());
    }
    Ok(())
}

/// The once-only policy for a repeated **value** flag: park `value` in
/// `slot`, or refuse through [`once`] when the slot already holds one.
/// `command` and `flag` name the offender in the usage error. The `T` is
/// the flag's value type — one generic helper serves every value flag
/// (`String`, an enum, a parsed size), inferred at each call site.
///
/// Ungated: the access flag's slot parks through it (`set_access_once`) in
/// every build, which keeps it live in singleton bundles. The command
/// grammars that call it directly are `push`, `info`, `extensions`, and
/// `serve` (every serve value flag parks through it since LABS-638) — the
/// `extension` grammars (`install`'s `--url`/`--from`/`--version`,
/// `init`'s `--lang`) parse in every build that carries the command
/// (ADR-0137), not only in the one that carries the capability, and
/// `extension-install` reaches them through its own `extensions`
/// dependency (the arms list only the entry features).
pub(crate) fn set_once<T>(
    command: &str,
    slot: &mut Option<T>,
    flag: &str,
    value: T,
) -> Result<(), ParseError> {
    once(command, flag, slot.is_some())?;
    *slot = Some(value);
    Ok(())
}

/// The once-only policy for a repeated **valueless** flag (a boolean
/// rider): refuse through [`once`] when `seen` is already set, else set
/// it. Gated with the grammars that call it — `push`, `info`,
/// `extensions`; a bundle that carries none of them never parses a
/// valueless flag.
#[cfg(any(feature = "push", feature = "info", feature = "extensions"))]
pub(crate) fn flag_once(command: &str, flag: &str, seen: &mut bool) -> Result<(), ParseError> {
    once(command, flag, *seen)?;
    *seen = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: [&str; 2] = ["--wal-dir", "--to-snapshot-format"];

    #[test]
    fn a_rejected_flag_is_suggested_from_the_commands_own_flags() {
        let err = |arg| ParseError::unexpected("clone", arg);
        assert_eq!(err("--wal-dri").suggestion(&KNOWN, &[]), Some("--wal-dir"));
        assert_eq!(
            err("--to-snapshot-formt").suggestion(&KNOWN, &[]),
            Some("--to-snapshot-format")
        );
    }

    #[test]
    fn nothing_but_an_unknown_flag_gets_a_suggestion() {
        // A rejected positional, a lone dash, a far flag, and a plain message
        // (a missing argument, or a flag-shaped value another flag swallowed)
        // get none.
        for err in [
            ParseError::unexpected("clone", "frobnicate"),
            ParseError::unexpected("clone", "-"),
            ParseError::unexpected("clone", "--bogus"),
            ParseError::from("clone requires SOURCE and DEST"),
            ParseError::from("--wal-dir requires a value"),
        ] {
            assert_eq!(err.suggestion(&KNOWN, &[]), None, "{err}");
        }
    }

    #[test]
    fn display_keeps_the_established_wording() {
        assert_eq!(
            ParseError::unexpected("run fs-to-graph", "--database-pth").to_string(),
            "unexpected argument to run fs-to-graph: --database-pth"
        );
        assert_eq!(
            ParseError::unexpected("clone", "./b").to_string(),
            "unexpected argument to clone: ./b"
        );
        assert_eq!(
            ParseError::from("clone requires X").to_string(),
            "clone requires X"
        );
    }

    #[test]
    fn flag_value_reports_the_missing_value_as_a_message() {
        let args: Vec<String> = vec![];
        let err = flag_value(&mut args.iter(), "--wal-dir", "a path").unwrap_err();
        assert_eq!(err.to_string(), "--wal-dir requires a path");
        assert_eq!(err.suggestion(&KNOWN, &[]), None);
    }

    /// The sighting parse (ADR-0480 §3): bare is the default snapshot, the
    /// attached values select their freshness, the space-separated spelling
    /// is refused loudly naming the attached form, and any other value is
    /// refused listing the two.
    #[test]
    fn the_access_sighting_parse_covers_the_one_flags_forms() {
        use relate::OpenMode;
        assert_eq!(
            parse_access_sighting("--read-only", None),
            Ok(Freshness::Snapshot)
        );
        assert_eq!(
            parse_access_sighting("--read-only", Some("--database")),
            Ok(Freshness::Snapshot)
        );
        assert_eq!(
            parse_access_sighting("--read-only=snapshot", None),
            Ok(Freshness::Snapshot)
        );
        assert_eq!(
            parse_access_sighting("--read-only=live", None),
            Ok(Freshness::Live)
        );
        // A space-separated value never silently becomes the path or the
        // query: both values are refused naming the attached spelling.
        assert_eq!(
            parse_access_sighting("--read-only", Some("live")),
            Err("--read-only takes its value attached: --read-only=live".into())
        );
        assert_eq!(
            parse_access_sighting("--read-only", Some("snapshot")),
            Err("--read-only takes its value attached: --read-only=snapshot".into())
        );
        assert_eq!(
            parse_access_sighting("--read-only=stale", None),
            Err("invalid --read-only: 'stale' is not a freshness (snapshot or live)".into())
        );
        // The parse is total over its input: a token that is not a
        // sighting is an error, never a panic (the arms match
        // `is_access_sighting` first).
        assert_eq!(
            parse_access_sighting("--read-onlyx", None),
            Err("invalid access flag: --read-onlyx".into())
        );
        // The freshness selects the mode — no fold, no second flag.
        assert_eq!(Freshness::Snapshot.open_mode(), OpenMode::ReadOnly);
        assert_eq!(Freshness::Live.open_mode(), OpenMode::Follow);
    }

    /// The once-only park (ADR-0480 §4, `set_access_once` over `set_once`):
    /// the first sighting parks its mode in the slot, a repeat is refused
    /// naming the one flag.
    #[test]
    fn the_access_once_policy_refuses_a_repeat() {
        use relate::OpenMode;
        let mut slot: Option<OpenMode> = None;
        set_access_once("serve", &mut slot, Freshness::Live).expect("the first sighting parks");
        assert_eq!(slot, Some(OpenMode::Follow));
        assert_eq!(
            set_access_once("serve", &mut slot, Freshness::Snapshot)
                .unwrap_err()
                .to_string(),
            "serve --read-only may be given only once"
        );
    }

    /// The one recognition fact (ADR-0480 §3): the bare spelling, the
    /// attached value, and nothing else — a `--read-onlyx` typo is not a
    /// sighting and reaches the unknown-flag path, not this parse.
    #[test]
    fn the_access_sighting_recognition_is_the_bare_or_attached_form() {
        assert!(is_access_sighting("--read-only"));
        assert!(is_access_sighting("--read-only=live"));
        assert!(!is_access_sighting("--read-onlyx"));
        assert!(!is_access_sighting("--follow"));
        assert!(!is_access_sighting("-d"));
    }

    /// The one access-flag vocabulary: the shared conflict message's
    /// parenthetical names exactly the one spelling (ADR-0480) — pinned
    /// here as data, in every build.
    #[test]
    fn the_access_flag_vocabulary_is_the_one_spelling() {
        assert_eq!(ACCESS_FLAG, "--read-only");
    }
}

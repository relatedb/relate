//! The `.bolt` script grammar (boltkit's `scripting.py`, turned around):
//! `!:` directive lines, `C:` lines the harness sends, `S:` lines it
//! expects back, role-less continuation lines that inherit the previous
//! role, and the `<RAW>` / `<NOOP>` / `<EXIT>` commands. Parsing lowers a
//! script to a flat list of wire steps; the directives become the steps
//! they imply (handshake, HELLO, LOGON, GOODBYE). `README.md` beside
//! this file is the authoring reference.

use std::path::{Path, PathBuf};

use super::messages::{self, Role};
use super::{hex, value, ScriptError};
use crate::common::packstream::{try_struct_msg, Pack};
use crate::common::MAGIC;

/// Server flags for a script without a `!: SERVER_ARGS` line.
const DEFAULT_SERVER_ARGS: [&str; 4] = ["--uri", ":memory:", "--auth", "none"];
/// The version an automatic handshake proposes without a `!: BOLT` line.
const DEFAULT_VERSION: Version = Version { major: 4, minor: 4 };
/// The `user_agent` of the HELLO an `!: AUTO HELLO` sends — and, from
/// 5.x, its `bolt_agent` product too.
const AUTO_HELLO_USER_AGENT: &str = "relate-bolt-compat/1.0";
/// Version proposals in a handshake; unused slots are zero.
const PROPOSALS: usize = 4;

/// A parsed script: the server to start and the steps to replay.
pub(crate) struct Script {
    pub(crate) path: PathBuf,
    pub(crate) server_args: Vec<String>,
    pub(crate) steps: Vec<Step>,
}

/// One wire step and the script line it came from.
pub(crate) struct Step {
    pub(crate) at: Origin,
    pub(crate) action: Action,
}

/// A script line: its number and its text as written (trimmed), quoted
/// when a step from it fails.
#[derive(Clone)]
pub(crate) struct Origin {
    pub(crate) line: usize,
    pub(crate) text: String,
}

pub(crate) enum Action {
    /// Write these bytes (an encoded message, `<RAW>`, `<NOOP>`, the
    /// handshake).
    Send(Vec<u8>),
    /// Read exactly these bytes.
    ExpectBytes(Vec<u8>),
    /// Read one message that [`value::matches`] this structure.
    ExpectMessage(Pack),
    /// Read end of stream: the server closed the connection.
    ExpectClose,
}

#[derive(Clone, Copy)]
struct Version {
    major: u8,
    minor: u8,
}

impl Version {
    /// The 4-byte wire form, as proposed and as selected: `00 00 minor major`.
    fn wire(self) -> [u8; 4] {
        [0, 0, self.minor, self.major]
    }

    /// Whether the version has the 5.x session shape (HELLO without
    /// auth, then LOGON) — the one spelling of the fact in this module.
    fn speaks_5x(self) -> bool {
        self.major >= 5
    }
}

/// What a line's role marker says it is.
#[derive(Clone, Copy)]
enum LineRole {
    Directive,
    Message(Role),
}

/// How the first client line starts the exchange: a leading `C: <RAW>`
/// replaces the automatic handshake (ADR-0304); any other first client line
/// takes it.
enum FirstClient {
    /// The first client line is a `<RAW>` byte write on this script line.
    Raw(usize),
    /// The first client line is an ordinary message.
    Plain,
}

/// One lowered `C:`/`S:` line: a `<RAW>` byte string or a named message
/// (or `<NOOP>`/`<EXIT>` command), each carrying its wire action.
enum MessageLine {
    Raw(Action),
    Named(Action),
}

impl MessageLine {
    fn action(self) -> Action {
        match self {
            MessageLine::Raw(action) | MessageLine::Named(action) => action,
        }
    }
}

/// The header directives, each set at most once, with the line that set it.
#[derive(Default)]
struct Header {
    version: Option<(Origin, Version)>,
    handshake: Option<(Origin, [u8; 4])>,
    auto_hello: Option<(Origin, ())>,
    auto_logon: Option<(Origin, ())>,
    auto_goodbye: Option<(Origin, ())>,
    server_args: Option<(Origin, Vec<String>)>,
}

/// Parse `source`, the text of the script at `path`.
pub(crate) fn parse(path: &Path, source: &str) -> Result<Script, ScriptError> {
    let mut header = Header::default();
    let mut body: Vec<Step> = Vec::new();
    // The first C: line and whether it is a <RAW>, which replaces the
    // automatic handshake.
    let mut first_client: Option<FirstClient> = None;
    let mut last_role = None;
    for (index, raw_line) in source.lines().enumerate() {
        let at = Origin {
            line: index + 1,
            text: raw_line.trim().to_string(),
        };
        if at.text.is_empty() || at.text.starts_with('#') {
            continue;
        }
        let fail = |message: String| ScriptError::new(path, Some(at.line), message);
        let (marker, rest) = split_role(&at.text).map_err(fail)?;
        let Some(role) = marker.or(last_role) else {
            return Err(fail(
                "line has no role and no earlier line to continue".into(),
            ));
        };
        last_role = Some(role);
        match role {
            LineRole::Directive if !body.is_empty() => {
                return Err(fail("directives belong before the first C:/S: line".into()));
            }
            LineRole::Directive => directive(&mut header, &at, rest).map_err(fail)?,
            LineRole::Message(role) => {
                let line = message_line(role, rest).map_err(fail)?;
                if matches!(role, Role::Client) && first_client.is_none() {
                    first_client = Some(match line {
                        MessageLine::Raw(_) => FirstClient::Raw(at.line),
                        MessageLine::Named(_) => FirstClient::Plain,
                    });
                }
                body.push(Step {
                    at,
                    action: line.action(),
                });
            }
        }
    }
    assemble(path, header, body, first_client)
}

/// Split the `X:` role marker off a line; `None` for a continuation line.
fn split_role(text: &str) -> Result<(Option<LineRole>, &str), String> {
    let (first, rest) = split_word(text);
    let Some(marker) = first.strip_suffix(':') else {
        return Ok((None, text));
    };
    let role = match marker {
        "!" => LineRole::Directive,
        "C" => LineRole::Message(Role::Client),
        "S" => LineRole::Message(Role::Server),
        _ => return Err(format!("unknown role `{first}` (expected !:, C: or S:)")),
    };
    Ok((Some(role), rest))
}

/// The first whitespace-separated word and the trimmed remainder.
fn split_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (text, ""),
    }
}

fn directive(header: &mut Header, at: &Origin, rest: &str) -> Result<(), String> {
    let (name, arg) = split_word(rest);
    match name {
        "BOLT" => set(&mut header.version, at, version(arg)?, "BOLT"),
        "HANDSHAKE" => {
            let reply = hex::decode(arg)?;
            let reply = <[u8; 4]>::try_from(reply.as_slice())
                .map_err(|_| format!("HANDSHAKE takes 4 bytes, got {}", reply.len()))?;
            set(&mut header.handshake, at, reply, "HANDSHAKE")
        }
        "AUTO" => match arg {
            "HELLO" => set(&mut header.auto_hello, at, (), "AUTO HELLO"),
            "LOGON" => set(&mut header.auto_logon, at, (), "AUTO LOGON"),
            "GOODBYE" => set(&mut header.auto_goodbye, at, (), "AUTO GOODBYE"),
            _ => Err(format!(
                "unknown AUTO value `{arg}` (expected HELLO, LOGON or GOODBYE)"
            )),
        },
        "SERVER_ARGS" => {
            let args: Vec<String> = arg.split_whitespace().map(str::to_string).collect();
            if args.is_empty() {
                return Err("SERVER_ARGS needs at least one argument".into());
            }
            set(&mut header.server_args, at, args, "SERVER_ARGS")
        }
        _ => Err(format!(
            "unknown directive `{name}` (expected BOLT, HANDSHAKE, AUTO or SERVER_ARGS)"
        )),
    }
}

fn set<T>(slot: &mut Option<(Origin, T)>, at: &Origin, value: T, name: &str) -> Result<(), String> {
    if let Some((earlier, _)) = slot {
        return Err(format!(
            "duplicate `!: {name}` (first set on line {})",
            earlier.line
        ));
    }
    *slot = Some((at.clone(), value));
    Ok(())
}

fn version(arg: &str) -> Result<Version, String> {
    let parsed = arg
        .split_once('.')
        .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)));
    match parsed {
        Some((major, minor)) => Ok(Version { major, minor }),
        None => Err(format!("BOLT takes a MAJOR.MINOR version, got `{arg}`")),
    }
}

/// Lower the body of one `C:`/`S:` line: a `<RAW>` byte string or a named
/// message/command, each carrying its wire action.
fn message_line(role: Role, rest: &str) -> Result<MessageLine, String> {
    let (name, args) = split_word(rest);
    let bare = |action| {
        if args.is_empty() {
            Ok(MessageLine::Named(action))
        } else {
            Err(format!("`{name}` takes no arguments, got `{args}`"))
        }
    };
    match (role, name) {
        (Role::Client, "<RAW>") => Ok(MessageLine::Raw(Action::Send(hex::decode(args)?))),
        (Role::Client, "<NOOP>") => bare(Action::Send(vec![0, 0])),
        (Role::Server, "<RAW>") => Ok(MessageLine::Raw(Action::ExpectBytes(hex::decode(args)?))),
        (Role::Server, "<EXIT>") => bare(Action::ExpectClose),
        (Role::Client, _) if name.starts_with('<') => Err(format!(
            "unknown C: command `{name}` (expected <RAW> or <NOOP>)"
        )),
        (Role::Server, _) if name.starts_with('<') => Err(format!(
            "unknown S: command `{name}` (expected <RAW> or <EXIT>)"
        )),
        (Role::Client, _) => {
            let message = encode(role.tag(name)?, value::parse_fields(args)?)?;
            Ok(MessageLine::Named(Action::Send(message)))
        }
        (Role::Server, _) => {
            let tag = role.tag(name)?;
            let fields = value::parse_fields(args)?;
            Ok(MessageLine::Named(Action::ExpectMessage(Pack::Struct {
                tag,
                fields,
            })))
        }
    }
}

fn encode(tag: u8, fields: Vec<Pack>) -> Result<Vec<u8>, String> {
    try_struct_msg(tag, fields).map_err(|e| format!("cannot encode message: {e}"))
}

/// The HELLO extra an `!: AUTO HELLO` sends at `version`: the 4.4 auth
/// form (`user_agent` + `scheme none`), or from 5.x the no-auth form —
/// `user_agent` plus the `bolt_agent` map 5.3 made mandatory, with no
/// credential, which Bolt 5.1 moved to LOGON (ADR-0484 decision 3).
fn auto_hello_map(version: Version) -> Pack {
    let user_agent = Pack::String(AUTO_HELLO_USER_AGENT.into());
    if version.speaks_5x() {
        Pack::Map(vec![
            (Pack::String("user_agent".into()), user_agent),
            (
                Pack::String("bolt_agent".into()),
                Pack::Map(vec![(
                    Pack::String("product".into()),
                    Pack::String(AUTO_HELLO_USER_AGENT.into()),
                )]),
            ),
        ])
    } else {
        Pack::Map(vec![
            (Pack::String("user_agent".into()), user_agent),
            (Pack::String("scheme".into()), Pack::String("none".into())),
        ])
    }
}

/// The reply an automatic exchange expects: any SUCCESS, whatever
/// metadata it carries. A script whose case is that metadata hand-writes
/// the exchange and pins it.
fn expect_success() -> Action {
    Action::ExpectMessage(Pack::Struct {
        tag: messages::SUCCESS,
        fields: vec![Pack::Map(vec![])],
    })
}

/// Push one automatic directive's exchange: the encoded `tag` message as
/// the Send step, then `reply` — the choreography every `!: AUTO`
/// directive lowers to. `what` names the directive in the encode panic.
fn push_exchange(
    steps: &mut Vec<Step>,
    at: Origin,
    what: &str,
    tag: u8,
    fields: Vec<Pack>,
    reply: Action,
) {
    let send = encode(tag, fields).unwrap_or_else(|e| panic!("the {what} encodes: {e:?}"));
    steps.push(Step {
        at: at.clone(),
        action: Action::Send(send),
    });
    steps.push(Step { at, action: reply });
}

/// Wrap the script's own steps in the ones its directives imply: the
/// handshake, the AUTO HELLO and AUTO LOGON before, the AUTO GOODBYE
/// after.
fn assemble(
    path: &Path,
    header: Header,
    body: Vec<Step>,
    first_client: Option<FirstClient>,
) -> Result<Script, ScriptError> {
    let Header {
        version,
        handshake,
        auto_hello,
        auto_logon,
        auto_goodbye,
        server_args,
    } = header;
    // Directives precede the body, so the earliest of these is the first
    // step after the handshake.
    let first_line = [
        auto_hello.as_ref().map(|(at, _)| at.line),
        auto_logon.as_ref().map(|(at, _)| at.line),
        body.first().map(|step| step.at.line),
        auto_goodbye.as_ref().map(|(at, _)| at.line),
    ]
    .into_iter()
    .flatten()
    .min()
    .ok_or_else(|| {
        ScriptError::new(
            path,
            None,
            "script has no C:/S: lines and no AUTO directive".into(),
        )
    })?;
    // The version the automatic handshake offers — and the automatic
    // HELLO speaks: the declared one, or the 4.4 default reported at the
    // first step without a `!: BOLT` line. Resolved once here, so the
    // offer and the HELLO form cannot drift apart.
    let (handshake_at, spoken) = match &version {
        Some((at, v)) => (at.clone(), *v),
        None => {
            let Version { major, minor } = DEFAULT_VERSION;
            (
                Origin {
                    line: first_line,
                    text: format!("(automatic Bolt {major}.{minor} handshake)"),
                },
                DEFAULT_VERSION,
            )
        }
    };
    let mut steps = Vec::new();
    if let Some(FirstClient::Raw(raw_line)) = first_client {
        let conflict = [
            version.as_ref().map(|(at, _)| (at.line, "BOLT")),
            handshake.as_ref().map(|(at, _)| (at.line, "HANDSHAKE")),
            auto_hello.as_ref().map(|(at, _)| (at.line, "AUTO HELLO")),
        ];
        if let Some((line, name)) = conflict.into_iter().flatten().next() {
            return Err(ScriptError::new(
                path,
                Some(line),
                format!(
                    "`!: {name}` conflicts with the leading `C: <RAW>` on line {raw_line}, \
                     which replaces the automatic handshake"
                ),
            ));
        }
    } else {
        steps.extend(handshake_steps(handshake_at, spoken, handshake));
    }
    // LOGON answers the AUTHENTICATION state the 5.x HELLO leaves, so an
    // automatic LOGON rides an automatic HELLO (ADR-0484 decision 4).
    if let (Some((at, ())), None) = (&auto_logon, &auto_hello) {
        return Err(ScriptError::new(
            path,
            Some(at.line),
            "`!: AUTO LOGON` needs `!: AUTO HELLO` — LOGON answers the AUTHENTICATION \
             state the 5.x HELLO leaves"
                .into(),
        ));
    }
    // At 4.4 the AUTO HELLO is the auth form (the credential rides the
    // HELLO), so there is no AUTHENTICATION state for a LOGON to answer.
    if let (Some((at, ())), true) = (&auto_logon, !spoken.speaks_5x()) {
        return Err(ScriptError::new(
            path,
            Some(at.line),
            "`!: AUTO LOGON` needs Bolt 5.1 or later (`!: BOLT 5.4`) — at 4.4 the AUTO HELLO \
             authenticates"
                .into(),
        ));
    }
    if let Some((at, ())) = auto_hello {
        push_exchange(
            &mut steps,
            at,
            "AUTO HELLO",
            messages::HELLO,
            vec![auto_hello_map(spoken)],
            expect_success(),
        );
    }
    if let Some((at, ())) = auto_logon {
        push_exchange(
            &mut steps,
            at,
            "AUTO LOGON",
            messages::LOGON,
            vec![Pack::Map(vec![(
                Pack::String("scheme".into()),
                Pack::String("none".into()),
            )])],
            expect_success(),
        );
    }
    steps.extend(body);
    if let Some((at, ())) = auto_goodbye {
        push_exchange(
            &mut steps,
            at,
            "AUTO GOODBYE",
            messages::GOODBYE,
            vec![],
            Action::ExpectClose,
        );
    }
    let server_args = match server_args {
        Some((_, args)) => args,
        None => DEFAULT_SERVER_ARGS.iter().map(|a| a.to_string()).collect(),
    };
    Ok(Script {
        path: path.to_path_buf(),
        server_args,
        steps,
    })
}

/// The automatic handshake: the magic plus one version proposal (zero
/// padded to four), then the expected 4-byte version reply. Reported at
/// the origin the caller resolved for the offered version.
fn handshake_steps(at: Origin, version: Version, reply: Option<(Origin, [u8; 4])>) -> [Step; 2] {
    let mut offer = MAGIC.to_vec();
    offer.extend(version.wire());
    offer.resize(MAGIC.len() + 4 * PROPOSALS, 0);
    let expect = match reply {
        Some((reply_at, bytes)) => Step {
            at: reply_at,
            action: Action::ExpectBytes(bytes.to_vec()),
        },
        None => Step {
            at: at.clone(),
            action: Action::ExpectBytes(version.wire().to_vec()),
        },
    };
    [
        Step {
            at,
            action: Action::Send(offer),
        },
        expect,
    ]
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;

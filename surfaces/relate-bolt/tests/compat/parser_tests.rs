use std::path::Path;

use super::{messages, parse, Action, Script};
use crate::common::packstream::{decode, Pack};

const PATH: &str = "unit/script.bolt";

fn parsed(source: &str) -> Script {
    parse(Path::new(PATH), source).unwrap_or_else(|e| panic!("{e}"))
}

/// The parse error for `source`, which must name `PATH:line` and mention
/// `needle`.
fn rejects(source: &str, line: usize, needle: &str) {
    let Err(error) = parse(Path::new(PATH), source) else {
        panic!("expected a parse error for:\n{source}");
    };
    let text = error.to_string();
    assert!(
        text.starts_with(&format!("{PATH}:{line}: ")),
        "expected {PATH}:{line}: in `{text}`"
    );
    assert!(text.contains(needle), "expected `{needle}` in `{text}`");
}

fn sent(action: &Action) -> &[u8] {
    match action {
        Action::Send(bytes) => bytes,
        _ => panic!("expected a send step"),
    }
}

fn expected_message(action: &Action) -> &Pack {
    match action {
        Action::ExpectMessage(message) => message,
        _ => panic!("expected a message expectation"),
    }
}

/// The message a Send step's chunked bytes carry: a named message the
/// parser encodes is one chunk — a 2-byte size header, then the body —
/// so the message starts after the header. The one place the chunk
/// framing of a sent message is spelled.
fn decoded_message(bytes: &[u8]) -> Pack {
    decode(&bytes[2..]).expect("a sent message decodes")
}

#[test]
fn unknown_directive_is_rejected() {
    rejects(
        "!: BOLT 4.4\n!: PORT 7687\nC: RESET",
        2,
        "unknown directive `PORT`",
    );
}

#[test]
fn unknown_auto_value_is_rejected() {
    rejects("!: AUTO RESET\nC: RESET", 1, "unknown AUTO value `RESET`");
}

#[test]
fn unknown_message_name_is_rejected() {
    rejects(
        "!: BOLT 4.4\n\nC: FROB {}",
        3,
        "unknown client message `FROB`",
    );
    // Names are per side: a client request is no server response.
    rejects("C: RESET\nS: HELLO {}", 2, "unknown server message `HELLO`");
}

#[test]
fn malformed_json_is_rejected() {
    rejects(
        "C: RUN \"RETURN 1\" {} {}\nS: SUCCESS {\"fields\": [\"1\"}",
        2,
        "malformed JSON field",
    );
}

#[test]
fn malformed_lines_are_rejected() {
    rejects("   RUN \"RETURN 1\" {} {}", 1, "no role");
    rejects("X: RUN \"RETURN 1\" {} {}", 1, "unknown role `X:`");
    rejects("C: <SLEEP> 1", 1, "unknown C: command `<SLEEP>`");
    rejects("C: RESET\nS: <NOOP>", 2, "unknown S: command `<NOOP>`");
    rejects("C: <RAW> 6060B01", 1, "odd number of hex digits");
    rejects("C: <RAW> 6060XX17", 1, "`X` is not a hex digit");
    rejects("C: <NOOP> 00", 1, "takes no arguments");
    rejects("C: 0x1 {}", 1, "bad message tag `0x1`");
    rejects("C: RUN {\"#4E\": 1}", 1, "takes a JSON list of fields");
    rejects("C: RUN {\"#4\": []}", 1, "bad structure tag `#4`");
}

#[test]
fn malformed_directives_are_rejected() {
    rejects("!: BOLT 4\nC: RESET", 1, "MAJOR.MINOR");
    rejects("!: HANDSHAKE 00 00 04\nC: RESET", 1, "4 bytes, got 3");
    rejects("!: SERVER_ARGS\nC: RESET", 1, "at least one argument");
    rejects(
        "!: BOLT 4.4\n!: BOLT 4.4\nC: RESET",
        2,
        "first set on line 1",
    );
    rejects(
        "C: RESET\n!: AUTO GOODBYE",
        2,
        "before the first C:/S: line",
    );
    rejects(
        "!: BOLT 4.4\nC: <RAW> 6060B017",
        1,
        "conflicts with the leading `C: <RAW>` on line 2",
    );
    rejects("!: AUTO LOGON\nC: RESET", 1, "needs `!: AUTO HELLO`");
    rejects(
        "!: AUTO HELLO\n!: AUTO LOGON\nC: RESET",
        2,
        "needs Bolt 5.1 or later",
    );
}

#[test]
fn an_empty_script_is_rejected() {
    let error = parse(Path::new(PATH), "# nothing\n!: BOLT 4.4\n")
        .err()
        .expect("a script with no steps is rejected");
    assert_eq!(
        error.to_string(),
        format!("{PATH}: script has no C:/S: lines and no AUTO directive")
    );
}

#[test]
fn bolt_directive_sends_one_padded_proposal_and_expects_it_back() {
    let script = parsed("!: BOLT 4.4\nC: RESET");
    let mut offer = vec![0x60, 0x60, 0xB0, 0x17, 0, 0, 4, 4];
    offer.resize(20, 0);
    assert_eq!(sent(&script.steps[0].action), offer);
    assert!(matches!(&script.steps[1].action, Action::ExpectBytes(b) if b == &[0, 0, 4, 4]));
    assert_eq!(script.steps[0].at.line, 1);
}

#[test]
fn handshake_directive_sets_the_expected_reply() {
    let script = parsed("!: HANDSHAKE 00000000\nC: RESET");
    assert!(matches!(&script.steps[1].action, Action::ExpectBytes(b) if b == &[0; 4]));
    assert_eq!(script.steps[1].at.line, 1);
}

#[test]
fn a_leading_raw_line_replaces_the_handshake() {
    let script = parsed("C: <RAW> 60 60 B0 17\n   <RAW> 00000404\nS: <RAW> 00 00 04 04");
    assert_eq!(script.steps.len(), 3);
    assert_eq!(sent(&script.steps[0].action), [0x60, 0x60, 0xB0, 0x17]);
    assert_eq!(sent(&script.steps[1].action), [0, 0, 4, 4]);
}

#[test]
fn auto_directives_wrap_the_body_in_hello_and_goodbye() {
    let script = parsed("!: AUTO HELLO\n!: AUTO GOODBYE\nC: RESET\nS: SUCCESS {}");
    let actions: Vec<&Action> = script.steps.iter().map(|s| &s.action).collect();
    // handshake (2), HELLO + SUCCESS, RESET + SUCCESS, GOODBYE + close.
    assert_eq!(actions.len(), 8);
    let hello = decoded_message(sent(actions[2]));
    let Pack::Struct {
        tag: messages::HELLO,
        fields,
    } = hello
    else {
        panic!("AUTO HELLO sends a HELLO");
    };
    assert_eq!(fields[0].get("scheme"), Some(&Pack::String("none".into())));
    assert!(fields[0].get("user_agent").is_some());
    assert_eq!(expected_message(actions[3]), &success());
    assert_eq!(sent(actions[6]), [0x00, 0x02, 0xB0, 0x02, 0x00, 0x00]);
    assert!(matches!(actions[7], Action::ExpectClose));
    assert_eq!(script.steps[6].at.text, "!: AUTO GOODBYE");
}

/// At a declared 5.x version the AUTO HELLO is the no-auth form:
/// `user_agent` from the parser's own constant plus the `bolt_agent`
/// map 5.3 made mandatory, with no `scheme` (ADR-0484 decision 3).
#[test]
fn auto_hello_at_5_4_sends_the_no_auth_form() {
    let script = parsed("!: BOLT 5.4\n!: AUTO HELLO\nC: RESET\nS: SUCCESS {}");
    let hello = decoded_message(sent(&script.steps[2].action));
    let Pack::Struct {
        tag: messages::HELLO,
        fields,
    } = hello
    else {
        panic!("AUTO HELLO sends a HELLO");
    };
    let agent = Pack::String(super::AUTO_HELLO_USER_AGENT.into());
    assert_eq!(fields[0].get("user_agent"), Some(&agent));
    assert_eq!(
        fields[0].get("bolt_agent"),
        Some(&Pack::Map(vec![(Pack::String("product".into()), agent)]))
    );
    assert_eq!(fields[0].get("scheme"), None);
}

/// `!: AUTO LOGON` sends the scheme none LOGON after the AUTO HELLO
/// exchange and expects any SUCCESS (ADR-0484 decision 4).
#[test]
fn auto_logon_sends_the_none_scheme_after_the_auto_hello() {
    let script = parsed("!: BOLT 5.4\n!: AUTO HELLO\n!: AUTO LOGON\nC: RESET\nS: SUCCESS {}");
    // handshake (2), HELLO + SUCCESS, LOGON + SUCCESS, RESET + SUCCESS.
    assert_eq!(script.steps.len(), 8);
    let logon = decoded_message(sent(&script.steps[4].action));
    let Pack::Struct {
        tag: messages::LOGON,
        fields,
    } = logon
    else {
        panic!("AUTO LOGON sends a LOGON");
    };
    assert_eq!(
        fields[0],
        Pack::Map(vec![(
            Pack::String("scheme".into()),
            Pack::String("none".into())
        )])
    );
    assert_eq!(expected_message(&script.steps[5].action), &success());
    assert_eq!(script.steps[4].at.text, "!: AUTO LOGON");
}

#[test]
fn continuation_lines_inherit_the_previous_role() {
    let script =
        parsed("C: RUN \"RETURN 1\" {} {}\n   PULL {\"n\": -1}\nS: SUCCESS {}\n   RECORD [1]");
    let body = &script.steps[2..];
    assert!(matches!(body[0].action, Action::Send(_)));
    assert!(matches!(body[1].action, Action::Send(_)));
    assert_eq!(body[1].at.line, 2);
    assert_eq!(body[1].at.text, "PULL {\"n\": -1}");
    assert_eq!(
        expected_message(&body[3].action),
        &Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
}

#[test]
fn comments_blank_lines_hex_tags_and_server_args() {
    let script = parsed(
        "# header\n\n!: SERVER_ARGS --uri :memory:  --auth none\n  # note\nC: 0x0f\nS: 0x70 {}",
    );
    assert_eq!(script.server_args, ["--uri", ":memory:", "--auth", "none"]);
    assert_eq!(
        sent(&script.steps[2].action),
        [0x00, 0x02, 0xB0, 0x0F, 0x00, 0x00]
    );
    assert_eq!(expected_message(&script.steps[3].action), &success());
}

/// ADR-0484: the 5.x message names LOGON, LOGOFF and TELEMETRY join the
/// `C:` vocabulary at their wire tags — notation, not a version gate, so
/// they encode at any negotiated version like a `0xNN` tag does.
#[test]
fn the_5_x_message_names_encode_to_their_tags() {
    let script = parsed("C: LOGON {\"scheme\": \"none\"}\nC: LOGOFF\nC: TELEMETRY 0");
    // steps[0] is the automatic handshake offer; the body starts after it.
    let tags: Vec<u8> = script.steps[1..]
        .iter()
        .filter_map(|step| match &step.action {
            Action::Send(bytes) => match decoded_message(bytes) {
                Pack::Struct { tag, .. } => Some(tag),
                _ => panic!("a sent message is a structure"),
            },
            _ => None,
        })
        .collect();
    assert_eq!(tags, [0x6A, 0x6B, 0x54]);
}

#[test]
fn default_server_args_are_an_in_memory_store_without_auth() {
    let script = parsed("C: RESET");
    assert_eq!(script.server_args, ["--uri", ":memory:", "--auth", "none"]);
}

fn success() -> Pack {
    Pack::Struct {
        tag: 0x70,
        fields: vec![Pack::Map(vec![])],
    }
}

//! The client's wire vocabulary: the messages it sends (HELLO, LOGON, RUN,
//! PULL and the chunked RUN+PULL frame) and the readers its responses go
//! through.

use relate_bolt::packstream::{decode, try_struct_msg, Pack};

use super::version::BoltVersion;

/// The client's message tags — the one spelling of its wire vocabulary: the
/// encoders below send by name and the session matches responses by name, so a
/// tag value is written once and every reader derives from it.
pub(super) mod tag {
    /// HELLO: opens the session with the version's auth shape.
    pub(crate) const HELLO: u8 = 0x01;
    /// GOODBYE: closes the session.
    pub(crate) const GOODBYE: u8 = 0x02;
    /// RESET: clears the failed state.
    pub(crate) const RESET: u8 = 0x0F;
    /// RUN: one statement.
    pub(crate) const RUN: u8 = 0x10;
    /// PULL: the records of the last RUN.
    pub(crate) const PULL: u8 = 0x3F;
    /// LOGON: the Bolt 5.x credential check.
    pub(crate) const LOGON: u8 = 0x6A;
    /// SUCCESS: the server accepted the message.
    pub(crate) const SUCCESS: u8 = 0x70;
    /// RECORD: one result row.
    pub(crate) const RECORD: u8 = 0x71;
    /// IGNORED: the server's failed state answered for a later message.
    pub(crate) const IGNORED: u8 = 0x7E;
    /// FAILURE: the server rejected the message.
    pub(crate) const FAILURE: u8 = 0x7F;
}

/// The client's `user_agent` (ADR-0313): required at both versions, and the
/// `product` the 5.x HELLO's `bolt_agent` map carries (ADR-0484 decision 3).
pub(super) const USER_AGENT: &str = "relate-datasets/1.0";

/// An integer parameter map as packstream pairs: the one conversion the
/// integer call sites go through.
pub(super) fn int_params(params: &[(&str, i64)]) -> Vec<(Pack, Pack)> {
    params
        .iter()
        .map(|(name, value)| (Pack::String((*name).to_string()), Pack::Int(*value)))
        .collect()
}

/// The HELLO's extra entries (ADR-0313, ADR-0484 decision 3): `user_agent`
/// at every version, plus the 5.x `bolt_agent` map — a string-to-string map
/// required since Bolt 5.3. At 4.4 the credentials ride the same map
/// (ADR-0126); the 5.x HELLO does not authenticate, so they wait for LOGON.
pub(super) fn hello_entries(version: BoltVersion, auth: Option<(&str, &str)>) -> Vec<(Pack, Pack)> {
    let mut entries = vec![(
        Pack::String("user_agent".into()),
        Pack::String(USER_AGENT.into()),
    )];
    if version.speaks_5x() {
        entries.push((
            Pack::String("bolt_agent".into()),
            Pack::Map(vec![(
                Pack::String("product".into()),
                Pack::String(USER_AGENT.into()),
            )]),
        ));
    } else if let Some((user, password)) = auth {
        entries.extend(basic_auth_entries(user, password));
    }
    entries
}

/// The 5.x LOGON token (ADR-0484 decision 4): the anonymous connect's
/// explicit `none` scheme, else the `basic` triple.
pub(super) fn logon_entries(auth: Option<(&str, &str)>) -> Vec<(Pack, Pack)> {
    match auth {
        Some((user, password)) => basic_auth_entries(user, password),
        None => vec![(Pack::String("scheme".into()), Pack::String("none".into()))],
    }
}

/// The ADR-0126 `basic` credential entries — scheme, principal and
/// credentials — the one spelling the 4.4 HELLO and the 5.x LOGON share.
fn basic_auth_entries(user: &str, password: &str) -> Vec<(Pack, Pack)> {
    vec![
        (Pack::String("scheme".into()), Pack::String("basic".into())),
        (Pack::String("principal".into()), Pack::String(user.into())),
        (
            Pack::String("credentials".into()),
            Pack::String(password.into()),
        ),
    ]
}

/// RUN's three fields: the statement, its parameter map, empty extra.
///
/// The parameter map is consumed, never cloned: a pipelined import batch
/// carries its whole `UNWIND` row list in one value, and copying it per
/// RUN would double the cost of every batch.
pub(super) fn run_fields(cypher: &str, params: Vec<(Pack, Pack)>) -> Vec<Pack> {
    vec![
        Pack::String(cypher.into()),
        Pack::Map(params),
        Pack::Map(vec![]),
    ]
}

/// PULL's one field: `{n: -1}`, every remaining record of the result.
pub(super) fn pull_fields() -> Vec<Pack> {
    vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])]
}

/// The chunked RUN+PULL pair one pipelined import enqueues.
pub(super) fn run_pull_frame(cypher: &str, params: Vec<(Pack, Pack)>) -> Result<Vec<u8>, String> {
    let mut frame = try_struct_msg(tag::RUN, run_fields(cypher, params))?;
    frame.extend(try_struct_msg(tag::PULL, pull_fields())?);
    Ok(frame)
}

/// The structure tag of one decoded server message.
pub(super) fn message_tag(msg: &[u8]) -> Option<u8> {
    match decode(msg) {
        Ok(Pack::Struct { tag, .. }) => Some(tag),
        _ => None,
    }
}

/// Whether one decoded server message is SUCCESS.
pub(super) fn is_success(msg: &[u8]) -> bool {
    message_tag(msg) == Some(tag::SUCCESS)
}

/// One server message as text, for a refusal: its decoded structure, or the
/// decode error and the raw bytes when it is not packstream at all.
pub(super) fn describe(msg: &[u8]) -> String {
    match decode(msg) {
        Ok(pack) => format!("{pack:?}"),
        Err(err) => format!("{err} ({msg:?})"),
    }
}

/// The single integer of a RECORD message's single field, or `None` when the
/// record is not exactly that — the count probe asked for a count.
pub(super) fn record_int(record: &[u8]) -> Option<i64> {
    let Ok(Pack::Struct {
        tag: tag::RECORD,
        fields,
    }) = decode(record)
    else {
        return None;
    };
    let [Pack::List(fields)] = fields.as_slice() else {
        return None;
    };
    let [Pack::Int(value)] = fields.as_slice() else {
        return None;
    };
    Some(*value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next message of a frame, through the crate's canonical reader.
    fn next_message(frame: &mut &[u8]) -> Vec<u8> {
        relate_bolt::protocol::read_message(frame).unwrap()
    }

    /// One RUN+PULL pair is exactly the two structures both spoken versions
    /// share: RUN with the integer parameter map and an empty extra, then PULL
    /// `{n: -1}`.
    #[test]
    fn run_pull_frame_carries_the_parameter_map_and_pull_all() {
        let frame = run_pull_frame("RETURN $id", int_params(&[("id", 7)])).unwrap();
        let mut rest = frame.as_slice();
        let run = next_message(&mut rest);
        let pull = next_message(&mut rest);
        assert!(rest.is_empty(), "the frame is exactly one RUN and one PULL");
        assert_eq!(
            decode(&run).unwrap(),
            Pack::Struct {
                tag: tag::RUN,
                fields: vec![
                    Pack::String("RETURN $id".into()),
                    Pack::Map(vec![(Pack::String("id".into()), Pack::Int(7))]),
                    Pack::Map(vec![]),
                ],
            }
        );
        // PULL pinned to its wire bytes: a tiny map with the tiny string key
        // `n` and the tiny integer -1, the crate packstream's own forms.
        assert_eq!(pull, [0xB1, 0x3F, 0xA1, 0x81, 0x6E, 0xFF]);
    }

    /// A parameterless statement still carries an empty parameter map, never
    /// a null: RUN's second field is a map at every call site.
    #[test]
    fn run_pull_frame_encodes_no_params_as_an_empty_map() {
        let frame = run_pull_frame("RETURN 1", int_params(&[])).unwrap();
        let mut rest = frame.as_slice();
        let run = next_message(&mut rest);
        assert_eq!(
            decode(&run).unwrap(),
            Pack::Struct {
                tag: tag::RUN,
                fields: vec![
                    Pack::String("RETURN 1".into()),
                    Pack::Map(vec![]),
                    Pack::Map(vec![]),
                ],
            }
        );
    }

    /// A record carrying exactly one integer field decodes to it; anything
    /// else — a second field, another type, a different tag — is not a count.
    #[test]
    fn record_int_reads_one_single_integer_field() {
        // The unchunked payload `read_message` hands back, never a framed
        // message: the one shape `record_int` sees in production.
        let record = |fields: Vec<Pack>| {
            relate_bolt::packstream::encode(&Pack::Struct {
                tag: tag::RECORD,
                fields: vec![Pack::List(fields)],
            })
            .expect("a RECORD payload")
        };
        assert_eq!(record_int(&record(vec![Pack::Int(42)])), Some(42));
        assert_eq!(
            record_int(&record(vec![Pack::Int(1), Pack::Int(2)])),
            None,
            "a multi-column row is not a count"
        );
        assert_eq!(
            record_int(&record(vec![Pack::String("42".into())])),
            None,
            "a string is not an integer"
        );
        assert_eq!(
            record_int(
                &relate_bolt::packstream::encode(&Pack::Struct {
                    tag: tag::SUCCESS,
                    fields: vec![],
                })
                .unwrap()
            ),
            None,
            "SUCCESS is not a record"
        );
    }
}

//! Unit and wire tests for `patch_bolt` negotiation (ADR-0329).

use super::*;
use crate::packstream::Pack;
use crate::testutil::{connected_client_with_hello, read_msg, send_struct, success_map};
use std::net::TcpStream;

/// The module's HELLO extra: the canonical `user_agent` map (ADR-0313)
/// plus `scheme` and the caller's extra `entries`.
fn hello_extra(entries: Vec<(&str, Pack)>) -> Pack {
    let mut all = vec![("scheme", Pack::String("none".into()))];
    all.extend(entries);
    crate::testutil::hello_extra(all)
}

fn strings(names: &[&str]) -> Pack {
    Pack::List(names.iter().map(|n| Pack::String((*n).into())).collect())
}

#[test]
fn only_known_patch_names_apply() {
    let utc = BoltPatches::from_hello(["utc"]);
    assert!(utc.applies(BoltPatch::Utc));
    let mixed = BoltPatches::from_hello(["elementid", "utc"]);
    assert_eq!(mixed, BoltPatches::utc());
    assert_eq!(BoltPatches::from_hello([]), BoltPatches::default());
    assert_eq!(
        BoltPatches::from_hello(["UTC", "elementid"]),
        BoltPatches::default()
    );
}

#[test]
fn the_echo_lists_exactly_the_applied_patches() {
    assert_eq!(BoltPatches::utc().hello_metadata(), Some(vec!["utc"]));
    assert_eq!(BoltPatches::default().hello_metadata(), None);
}

/// The wire vocabulary is [`BoltPatch::name`] and nothing else: `from_name`
/// accepts exactly the names `name` spells, and `applies` reads the
/// negotiated patch.
#[test]
fn the_wire_name_is_the_only_vocabulary() {
    assert_eq!(BoltPatch::Utc.name(), "utc");
    assert_eq!(BoltPatch::from_name("utc"), Some(BoltPatch::Utc));
    assert_eq!(
        BoltPatch::from_name(BoltPatch::Utc.name()),
        Some(BoltPatch::Utc)
    );
    assert_eq!(BoltPatch::from_name("UTC"), None);
    assert_eq!(BoltPatch::from_name("elementid"), None);
    assert!(BoltPatches::utc().applies(BoltPatch::Utc));
    assert!(!BoltPatches::default().applies(BoltPatch::Utc));
}

/// A connection past HELLO carrying `patch_bolt` as its patch entry;
/// returns the HELLO SUCCESS.
fn client_with_hello(patch_bolt: Pack) -> (TcpStream, Pack) {
    let db = std::sync::Arc::new(relate::Database::open(":memory:").unwrap());
    connected_client_with_hello(
        db,
        Default::default(),
        hello_extra(vec![("patch_bolt", patch_bolt)]),
    )
}

/// RUN `cypher` with `params`, PULL everything, and return the first
/// RECORD's single value; both SUCCESS replies are consumed.
fn single_value(client: &mut TcpStream, cypher: &str, params: Vec<(Pack, Pack)>) -> Pack {
    send_struct(
        client,
        0x10,
        vec![Pack::String(cypher.into()), Pack::Map(params)],
    );
    success_map(read_msg(client));
    send_struct(
        client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
    let Pack::Struct { tag: 0x71, fields } = read_msg(client) else {
        panic!("expected a RECORD for {cypher}");
    };
    success_map(read_msg(client));
    let Some(Pack::List(mut values)) = fields.into_iter().next() else {
        panic!("a RECORD carries a list");
    };
    values.remove(0)
}

#[test]
fn a_utc_hello_is_echoed_and_switches_datetimes_to_utc_structures() {
    let (mut client, hello) = client_with_hello(strings(&["utc"]));
    assert_eq!(hello.get("patch_bolt"), Some(&strings(&["utc"])));
    assert_eq!(
        single_value(
            &mut client,
            "RETURN datetime('2024-01-02T03:04:05.123456789+01:00') AS d",
            vec![]
        ),
        Pack::Struct {
            tag: 0x49,
            fields: vec![Pack::Int(1704161045), Pack::Int(123456789), Pack::Int(3600)],
        }
    );
    // A 0x49 parameter echoes back as itself.
    let utc = Pack::Struct {
        tag: 0x49,
        fields: vec![Pack::Int(1704161045), Pack::Int(123456789), Pack::Int(3600)],
    };
    assert_eq!(
        single_value(
            &mut client,
            "RETURN $d AS d",
            vec![(Pack::String("d".into()), utc.clone())]
        ),
        utc
    );
}

#[test]
fn without_the_patch_there_is_no_echo_and_datetimes_stay_legacy() {
    let (mut client, hello) = client_with_hello(strings(&["elementid"]));
    assert_eq!(hello.get("patch_bolt"), None);
    assert_eq!(
        single_value(
            &mut client,
            "RETURN datetime('2024-01-02T03:04:05.123456789+01:00') AS d",
            vec![]
        ),
        Pack::Struct {
            tag: 0x46,
            fields: vec![Pack::Int(1704164645), Pack::Int(123456789), Pack::Int(3600)],
        }
    );
}

/// The HELLO extra shapes that negotiate nothing: a `patch_bolt` that is
/// not a list, and a list without a recognized string entry; a list mixing
/// a non-string entry with `utc` still applies `utc` (ADR-0329).
#[test]
fn only_a_lists_string_entries_are_negotiated() {
    for patch_bolt in [Pack::String("utc".into()), Pack::List(vec![Pack::Int(1)])] {
        let (_client, hello) = client_with_hello(patch_bolt);
        assert_eq!(hello.get("patch_bolt"), None);
    }
    let (_client, hello) = client_with_hello(Pack::List(vec![
        Pack::String("elementid".into()),
        Pack::Int(1),
        Pack::String("utc".into()),
    ]));
    assert_eq!(hello.get("patch_bolt"), Some(&strings(&["utc"])));
}

//! Client-side Bolt exchange helpers used by the CLI's Bolt integration tests.
//!
//! These stay in the CLI test crate so lean CLI bundle tests do not enable
//! `relate-bolt` (and therefore the engine's `parquet` feature) through
//! the shared test-support crate.

use std::io::{Read, Write};

use relate_bolt::packstream::Pack;

pub fn send_struct<S: Write>(client: &mut S, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&relate_bolt::packstream::try_struct_msg(tag, fields).unwrap())
        .unwrap();
    client.flush().unwrap();
}

/// The `user_agent` the serve e2e HELLOs carry (Bolt 4.4 requires one,
/// ADR-0313).
const USER_AGENT: &str = "e2e-serve";

/// A basic-auth HELLO message's fields: the required `user_agent` (ADR-0313)
/// plus the scheme/principal/credentials entries.
pub fn hello(user: &str, password: &str) -> Vec<Pack> {
    vec![Pack::Map(vec![
        (
            Pack::String("user_agent".into()),
            Pack::String(USER_AGENT.into()),
        ),
        (Pack::String("scheme".into()), Pack::String("basic".into())),
        (Pack::String("principal".into()), Pack::String(user.into())),
        (
            Pack::String("credentials".into()),
            Pack::String(password.into()),
        ),
    ])]
}

/// Send `hello(user, password)` (tag 0x01).
pub fn send_hello<S: Write>(client: &mut S, user: &str, password: &str) {
    send_struct(client, 0x01, hello(user, password));
}

pub fn read_msg<S: Read>(client: &mut S) -> Pack {
    let payload = relate_bolt::protocol::read_message(client).unwrap();
    relate_bolt::packstream::decode(&payload).unwrap()
}

/// Unpack a response struct whose tag is `tag` into its metadata map — the
/// one unpack policy both [`success_map`] and [`failure_map`] are.
fn tagged_map(msg: Pack, tag: u8, label: &str) -> Pack {
    let Pack::Struct { tag: got, fields } = msg else {
        panic!("expected {label}, got {msg:?}");
    };
    assert_eq!(got, tag, "expected {label}, got tag {got:#04X}");
    let Some(Pack::Map(map)) = fields.into_iter().next() else {
        panic!("expected {label} map");
    };
    Pack::Map(map)
}

/// The SUCCESS metadata as a [`Pack::Map`], so a test reads fields through
/// [`field`] (or the canonical `Pack::get`) without re-wrapping the pairs.
pub fn success_map(msg: Pack) -> Pack {
    tagged_map(msg, 0x70, "SUCCESS")
}

/// The FAILURE metadata as a [`Pack::Map`], so a test reads fields through
/// [`field`] (or the canonical `Pack::get`) without re-wrapping the pairs.
pub fn failure_map(msg: Pack) -> Pack {
    tagged_map(msg, 0x7F, "FAILURE")
}

/// A string field of a metadata [`Pack::Map`]; panics when absent or not a
/// string.
pub fn field<'a>(map: &'a Pack, key: &str) -> &'a str {
    map.get(key)
        .and_then(Pack::as_str)
        .unwrap_or_else(|| panic!("missing string field {key}"))
}

pub fn handshake<S: Read + Write>(stream: &mut S) {
    let mut offer = [0u8; 20];
    offer[..4].copy_from_slice(&[0x60, 0x60, 0xB0, 0x17]);
    offer[4..8].copy_from_slice(&[0, 0, 4, 4]);
    stream.write_all(&offer).unwrap();
    stream.flush().unwrap();

    let mut selected = [0u8; 4];
    stream.read_exact(&mut selected).unwrap();
    assert_eq!(selected, [0, 0, 4, 4], "unsupported Bolt version");
}

/// Connect to a served Bolt port, run the version handshake, and send the
/// HELLO carrying `auth` (`user:password`) as basic credentials and the
/// required `user_agent` (ADR-0313). The caller reads the HELLO reply —
/// each test asserts its own metadata.
pub fn connect_and_hello(port: u16, auth: &str) -> std::net::TcpStream {
    let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    handshake(&mut client);
    let (user, password) = auth.split_once(':').unwrap();
    send_hello(&mut client, user, password);
    client
}

/// On the session [`connect_and_hello`] leaves open (HELLO already
/// answered): RUN `RETURN 1 AS n`, PULL it, and assert the RECORD carries
/// the 1. The one copy of the RUN/PULL/RECORD choreography the serve e2e
/// tests drive.
pub fn assert_run_pull_returns_one_row<S: Read + Write>(client: &mut S) {
    send_struct(
        client,
        0x10, // RUN
        vec![
            Pack::String("RETURN 1 AS n".into()),
            Pack::Map(vec![]),
            Pack::Map(vec![]),
        ],
    );
    success_map(read_msg(client));
    send_struct(client, 0x3F, vec![Pack::Map(vec![])]); // PULL
    let record = read_msg(client);
    assert!(
        matches!(&record, Pack::Struct { tag: 0x71, fields } if fields.first() == Some(&Pack::List(vec![Pack::Int(1)]))),
        "the RECORD carries 1: {record:?}"
    );
}

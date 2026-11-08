use super::*;
use crate::packstream::Pack;
use crate::testutil::{connect, read_msg, send_struct, spawn_handler, success_map, try_handshake};
use clap::Parser;
use relate_serve::origin::OriginFlagValue;
use relate_test_support::EnvRestore;
use std::net::TcpStream;

#[test]
fn bolt_return_one() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(db, ConnConfig::default());
    let mut client = connect(addr);
    crate::testutil::handshake(&mut client);
    send_struct(
        &mut client,
        0x01,
        vec![Pack::Map(vec![(
            Pack::String("user_agent".into()),
            Pack::String("test".into()),
        )])],
    );
    // HELLO SUCCESS: with no --identity the server agent is the honest
    // default, single-sourced from CARGO_PKG_VERSION (ADR-0293).
    let m = Pack::Map(success_map(read_msg(&mut client)));
    // Metadata reads go through the canonical accessor (Pack::get, via
    // testutil::field's panicking absence).
    let server = crate::testutil::field(&m, "server");
    assert_eq!(server, concat!("Relate/", env!("CARGO_PKG_VERSION")));
    assert_eq!(server, crate::protocol::default_server_identity());
    // The HELLO SUCCESS also carries a `connection_id` (ADR-0293): the
    // Java driver's HelloResponseHandler requires the key.
    assert!(
        crate::testutil::field(&m, "connection_id").starts_with("conn-"),
        "connection_id: {m:?}"
    );

    send_struct(
        &mut client,
        0x10,
        vec![Pack::String("RETURN 1 AS n".into()), Pack::Map(vec![])],
    );
    let _ = read_msg(&mut client);

    send_struct(
        &mut client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
    // RECORD then SUCCESS with t_last as elapsed milliseconds.
    let rec = read_msg(&mut client);
    assert!(matches!(rec, Pack::Struct { tag: 0x71, .. }));
    let m = success_map(read_msg(&mut client));
    let t_last = m
        .iter()
        .find(|(k, _)| k.as_str() == Some("t_last"))
        .and_then(|(_, v)| match v {
            Pack::Int(i) => Some(*i),
            _ => None,
        });
    assert!(matches!(t_last, Some(ms) if ms >= 0));
}

/// ADR-0293: a configured identity is sent verbatim as the HELLO SUCCESS
/// `server` metadata — the opt-in shape for tooling (cypher-shell) that
/// hard-checks the agent string.
#[test]
fn hello_carries_the_configured_identity() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let config = ConnConfig {
        server_identity: "Neo4j/5.26.0".to_string(),
        ..ConnConfig::default()
    };
    let addr = spawn_handler(db, config);
    let mut client = connect(addr);
    crate::testutil::handshake(&mut client);
    send_struct(
        &mut client,
        0x01,
        vec![Pack::Map(vec![(
            Pack::String("user_agent".into()),
            Pack::String("test".into()),
        )])],
    );
    let m = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(crate::testutil::field(&m, "server"), "Neo4j/5.26.0");
    // The identity override does not touch `connection_id`.
    assert!(
        crate::testutil::field(&m, "connection_id").starts_with("conn-"),
        "connection_id: {m:?}"
    );
}

/// ADR-0293: connection ids come from a counter shared across clones of
/// one `ConnConfig` (the `serve_listener` shape), so every connection a
/// listener serves gets a distinct id; a fresh config restarts the
/// sequence (uniqueness is per listener, like Neo4j's per-server ids).
#[test]
fn connection_ids_are_unique_per_listener() {
    let config = ConnConfig::default();
    let first = config.next_connection_id();
    let second = config.clone().next_connection_id();
    assert_ne!(first, second);
    assert_eq!(first, "conn-1");
    assert_eq!(second, "conn-2");
    assert_eq!(ConnConfig::default().next_connection_id(), "conn-1");
}

#[test]
fn load_csv_flag_scopes_imports_and_defaults_to_denied() {
    // No other test in this binary writes RELATE_LOAD_CSV_DIR. Declare
    // order matters: `_restore` drops first (restoring the var while the
    // dirs still exist), so a concurrently opening Database never reads a
    // path whose TempDir was already dropped.
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let _restore = EnvRestore::new("RELATE_LOAD_CSV_DIR");
    std::fs::write(dir.path().join("rows.csv"), "Ada\n").unwrap();
    let query = "LOAD CSV FROM 'rows.csv' AS row RETURN row";
    let dir_str = dir.path().to_str().unwrap();

    let enabled = Args::try_parse_from(["relate-bolt", "--load-csv-dir", dir_str]).unwrap();
    let db = Database::open_with(":memory:", enabled.open_options()).unwrap();
    assert_eq!(db.connect().unwrap().query(query).unwrap().rows.len(), 1);
    assert!(db
        .connect()
        .unwrap()
        .query("LOAD CSV FROM '../rows.csv' AS row RETURN row")
        .unwrap_err()
        .to_string()
        .contains("LOAD CSV file unavailable"));

    let disabled = Args::try_parse_from(["relate-bolt"]).unwrap();
    assert!(disabled.load_csv_dir.is_none());
    std::env::remove_var("RELATE_LOAD_CSV_DIR");
    let denied = Database::open_with(":memory:", disabled.open_options()).unwrap();
    assert!(denied
        .connect()
        .unwrap()
        .query(query)
        .unwrap_err()
        .to_string()
        .contains("LOAD CSV file unavailable"));
    std::env::set_var("RELATE_LOAD_CSV_DIR", dir_str);
    let from_env = Database::open_with(":memory:", disabled.open_options()).unwrap();
    assert_eq!(
        from_env.connect().unwrap().query(query).unwrap().rows.len(),
        1
    );
    std::env::set_var("RELATE_LOAD_CSV_DIR", other.path());
    let flag_wins = Database::open_with(":memory:", enabled.open_options()).unwrap();
    assert_eq!(
        flag_wins
            .connect()
            .unwrap()
            .query(query)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(Args::try_parse_from(["relate-bolt", "--load-csv-dir"]).is_err());
    let help = Args::try_parse_from(["relate-bolt", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("--load-csv-dir <PATH>"));
    assert!(help.contains("RELATE_LOAD_CSV_DIR"));
}

#[test]
fn durability_mode_flag_parses_and_defaults_full() {
    // ADR-0470: the flag defers to RELATE_DURABILITY_MODE, then the
    // engine default (full) — `None` hands the resolution to the open.
    let defaults = Args::try_parse_from(["relate-bolt"]).unwrap();
    assert_eq!(defaults.durability_mode, None);
    for &(raw, expected) in relate_test_support::durability::DURABILITY_MODES {
        let args = Args::try_parse_from(["relate-bolt", "--durability-mode", raw]).unwrap();
        assert_eq!(args.durability_mode, Some(expected));
    }
    assert!(Args::try_parse_from(["relate-bolt", "--durability-mode", "later"]).is_err());
    let help = Args::try_parse_from(["relate-bolt", "--help"])
        .unwrap_err()
        .to_string();
    assert!(help.contains("--durability-mode <DURABILITY_MODE>"));
    assert!(help.contains("RELATE_DURABILITY_MODE"));
}

#[test]
fn bolt_load_csv_opt_in_reaches_client_records() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("people.csv"), "Ada\nLin\n").unwrap();
    let args = Args::try_parse_from([
        "relate-bolt",
        "--load-csv-dir",
        dir.path().to_str().unwrap(),
    ])
    .unwrap();
    let db = Arc::new(Database::open_with(":memory:", args.open_options()).unwrap());
    let addr = spawn_handler(Arc::clone(&db), ConnConfig::default());
    let mut client = connect(addr);
    crate::testutil::handshake(&mut client);
    send_struct(&mut client, 0x01, crate::testutil::hello_fields());
    success_map(read_msg(&mut client));
    send_struct(
        &mut client,
        0x10,
        vec![
            Pack::String("LOAD CSV FROM 'people.csv' AS row RETURN row[0] AS name".into()),
            Pack::Map(vec![]),
        ],
    );
    success_map(read_msg(&mut client));
    send_struct(
        &mut client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
    for expected in ["Ada", "Lin"] {
        assert_eq!(
            read_msg(&mut client),
            Pack::Struct {
                tag: 0x71,
                fields: vec![Pack::List(vec![Pack::String(expected.into())])],
            }
        );
    }
    success_map(read_msg(&mut client));
}

#[test]
fn governance_flags_parse_with_documented_defaults() {
    use crate::cli_args::{
        parse_cli_size, DEFAULT_HANDSHAKE_TIMEOUT_SECS, DEFAULT_IDLE_TIMEOUT_SECS,
        DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_RESULT_BYTES, DEFAULT_MAX_RESULT_BYTES_STR,
        DEFAULT_QUERY_TIMEOUT_SECS,
    };
    // The clap defaults and `ConnLimits::default()` are single-sourced in
    // these consts. `--max-result-bytes` is the exception: clap's
    // `default_value` needs a string literal, so its "256mib" is pinned to
    // the parsed const here.
    assert_eq!(
        parse_cli_size(DEFAULT_MAX_RESULT_BYTES_STR),
        Ok(DEFAULT_MAX_RESULT_BYTES)
    );
    let default_limits = ConnLimits::default();
    assert_eq!(
        default_limits.handshake_timeout,
        Some(std::time::Duration::from_secs(
            DEFAULT_HANDSHAKE_TIMEOUT_SECS
        ))
    );
    assert_eq!(
        default_limits.idle_timeout,
        Some(std::time::Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS))
    );
    assert_eq!(default_limits.query_timeout, None);
    assert_eq!(default_limits.max_result_bytes, DEFAULT_MAX_RESULT_BYTES);
    assert_eq!(default_limits.max_connections, DEFAULT_MAX_CONNECTIONS);

    let defaults = Args::try_parse_from(["relate-bolt"]).unwrap();
    assert_eq!(defaults.max_connections, DEFAULT_MAX_CONNECTIONS);
    assert_eq!(
        defaults.handshake_timeout_secs,
        DEFAULT_HANDSHAKE_TIMEOUT_SECS
    );
    assert_eq!(defaults.idle_timeout_secs, DEFAULT_IDLE_TIMEOUT_SECS);
    assert_eq!(defaults.query_timeout_secs, DEFAULT_QUERY_TIMEOUT_SECS);
    assert_eq!(defaults.max_result_bytes, DEFAULT_MAX_RESULT_BYTES);

    let overridden = Args::try_parse_from([
        "relate-bolt",
        "--max-connections",
        "4",
        "--handshake-timeout-secs",
        "1",
        "--idle-timeout-secs",
        "0",
        "--query-timeout-secs",
        "1",
        "--max-result-bytes",
        "1mib",
    ])
    .unwrap();
    assert_eq!(overridden.max_connections, 4);
    assert_eq!(overridden.handshake_timeout_secs, 1);
    assert_eq!(overridden.idle_timeout_secs, 0);
    assert_eq!(overridden.query_timeout_secs, 1);
    assert_eq!(overridden.max_result_bytes, 1 << 20);

    let help = Args::try_parse_from(["relate-bolt", "--help"])
        .unwrap_err()
        .to_string();
    for flag in [
        "--max-connections",
        "--handshake-timeout-secs",
        "--idle-timeout-secs",
        "--query-timeout-secs",
        "--max-result-bytes",
    ] {
        assert!(help.contains(flag), "{flag} missing from help:\n{help}");
    }
}

#[test]
fn websocket_flags_parse_and_appear_in_help() {
    let defaults = Args::try_parse_from(["relate-bolt"]).unwrap();
    assert!(!defaults.no_websocket);
    assert!(defaults.ws_allow_origin.is_empty());
    // The WebSocket session cap is opt-in: unset is the shared unlimited
    // default (ADR-0459).
    assert_eq!(
        defaults.ws_max_connections,
        relate_serve::DEFAULT_WS_MAX_CONNECTIONS
    );

    let args = Args::try_parse_from([
        "relate-bolt",
        "--no-websocket",
        "--ws-max-connections",
        "4",
        "--ws-allow-origin",
        "https://app.example.com",
        "--ws-allow-origin",
        "*",
    ])
    .unwrap();
    assert!(args.no_websocket);
    assert_eq!(args.ws_max_connections, 4);
    assert_eq!(
        args.ws_allow_origin,
        [
            OriginFlagValue::Url("https://app.example.com".to_string()),
            OriginFlagValue::Wildcard,
        ]
    );
    // The value is parsed by the shared origin grammar: it is stored
    // canonically, and an out-of-shape value is a usage error.
    let args = Args::try_parse_from([
        "relate-bolt",
        "--ws-allow-origin",
        "HTTP://APP.example.com:7474/",
    ])
    .unwrap();
    assert_eq!(
        args.ws_allow_origin,
        [OriginFlagValue::Url(
            "http://app.example.com:7474".to_string()
        )]
    );
    assert!(
        Args::try_parse_from(["relate-bolt", "--ws-allow-origin", "localhost"]).is_err(),
        "a bare host is outside the origin grammar"
    );

    let help = Args::try_parse_from(["relate-bolt", "--help"])
        .unwrap_err()
        .to_string();
    assert!(
        help.contains("--no-websocket"),
        "--no-websocket missing from help:\n{help}"
    );
    assert!(
        help.contains("--ws-allow-origin <ORIGIN>"),
        "--ws-allow-origin missing from help:\n{help}"
    );
    assert!(
        help.contains("--ws-max-connections <WS_MAX_CONNECTIONS>"),
        "--ws-max-connections <WS_MAX_CONNECTIONS> missing from help:\n{help}"
    );
}

#[test]
fn connection_cap_rejects_over_cap_and_recovers() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let limits = ConnLimits {
        handshake_timeout: Some(std::time::Duration::from_secs(5)),
        idle_timeout: Some(std::time::Duration::from_secs(5)),
        query_timeout: None,
        max_result_bytes: crate::cli_args::DEFAULT_MAX_RESULT_BYTES,
        max_connections: 1,
        ..ConnLimits::default()
    };
    std::thread::spawn(move || {
        let _ = serve_listener(
            listener,
            crate::transport::Transport {
                tls: None,
                plaintext_allowed: true,
                websocket: None,
            },
            db,
            ConnConfig {
                limits,
                auth: Arc::new(crate::auth::AuthConfig::None),
                ..ConnConfig::default()
            },
        );
    });

    // The first connection is served.
    let mut first = connect(addr);
    crate::testutil::handshake(&mut first);
    send_struct(&mut first, 0x01, crate::testutil::hello_fields());
    success_map(read_msg(&mut first));

    // The second is accepted then immediately closed.
    let mut second = connect(addr);
    let rejected = try_handshake(&mut second);
    assert!(
        rejected.is_err(),
        "an over-cap connection must be closed, got {rejected:?}"
    );

    // The first keeps being served while the over-cap client is gone.
    send_struct(&mut first, 0x01, crate::testutil::hello_fields());
    success_map(read_msg(&mut first));

    // After the first disconnects, a new connection is served.
    drop(first);
    let mut served = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut third = match TcpStream::connect(addr) {
            Ok(s) => s,
            Err(_) => continue,
        };
        third
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        if try_handshake(&mut third).is_ok() {
            served = true;
            break;
        }
    }
    assert!(
        served,
        "a new connection must be served after the first disconnects"
    );
}

#[test]
fn read_only_flag_refuses_writes_and_keeps_the_next_flag() {
    let args = Args::try_parse_from(["relate-bolt", "--read-only", "--uri", ":memory:"]).unwrap();
    assert!(args.read_only);
    assert_eq!(args.uri, ":memory:");
    let db = Database::open_with(&args.uri, args.open_options()).unwrap();
    let mut conn = db.connect().unwrap();
    conn.query("RETURN 1 AS n").unwrap();
    let err = conn.query("CREATE ()").unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");

    let writable = Args::try_parse_from(["relate-bolt", "--uri", ":memory:"]).unwrap();
    assert!(!writable.read_only);
    let db = Database::open_with(&writable.uri, writable.open_options()).unwrap();
    db.connect().unwrap().query("CREATE ()").unwrap();
}

#[test]
fn workspace_limit_flag_rejects_zero_and_parses_a_size() {
    let err = Args::try_parse_from(["relate-bolt", "--concurrent-logical-workspace-limit", "0"])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("invalid --concurrent-logical-workspace-limit: must be positive"),
        "{err}"
    );
    let args = Args::try_parse_from([
        "relate-bolt",
        "--concurrent-logical-workspace-limit",
        "2mib",
    ])
    .unwrap();
    assert_eq!(
        args.concurrent_logical_workspace_limit,
        Some(2 * 1024 * 1024)
    );
}

/// The recording fixture separates a deliberate close from a genuine
/// session error: a non-Bolt preamble is closed cleanly (the session ends with no error).
#[test]
fn recorded_fixture_ends_without_an_error_on_a_clean_close() {
    use std::io::Write;
    let db = Arc::new(Database::open(":memory:").unwrap());
    let (addr, handle) = crate::testutil::spawn_handler_recorded(db, ConnConfig::default());
    let mut client = connect(addr);
    client.write_all(&[0xFF; 20]).unwrap();
    // The server logs and closes; the join is the deterministic wait.
    assert_eq!(handle.join().unwrap(), None);
}

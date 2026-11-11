//! `serve` command tests — the process-orchestration half (the binds, the
//! URL rendering, the banner, setup, the surface configs). The
//! argument-grammar tests live in serve_args/tests.rs, beside the grammar
//! they pin (LABS-392); both files follow the skill_cmd dir-module
//! pattern to stay under the repo's 1k-line file budget.

#[cfg(feature = "mcp")]
use super::spawn::mcp_config;
use super::*;
#[cfg(feature = "surface")]
use crate::args;
#[cfg(feature = "surface")]
use crate::serve_args::*;

#[cfg(feature = "http")]
#[test]
fn bind_reports_the_os_assigned_port_for_port_zero() {
    let (port, listener) = bind("--http", DEFAULT_HOST, 0).unwrap();
    assert_ne!(port, 0, "the announced port is the bound one");
    assert_eq!(listener.local_addr().unwrap().port(), port);
}

#[test]
fn non_loopback_warning_only_fires_off_loopback() {
    for host in ["127.0.0.1", "localhost", "::1", "[::1]", "127.0.1.1"] {
        assert!(non_loopback_warning(host).is_none(), "{host}");
    }
    // A hostname that merely STARTS with a loopback address is not
    // loopback — the parse-based predicate must not be fooled.
    for host in [
        "0.0.0.0",
        "::",
        "192.168.1.10",
        "docker0",
        "127.0.0.1.evil.com",
    ] {
        let warning = non_loopback_warning(host).expect(host);
        assert!(warning.contains("WARNING"), "{warning}");
        assert!(warning.contains("PLAINTEXT"), "{warning}");
        assert!(warning.contains(host), "{warning}");
    }
}

#[cfg(feature = "surface")]
#[test]
fn url_host_brackets_ipv6_literals_for_url_rendering() {
    // The one IPv6-bracketing fact: a literal renders bracketed, a
    // hostname or IPv4 renders as-is.
    assert_eq!(url_host("::1"), "[::1]");
    assert_eq!(url_host("127.0.0.1"), "127.0.0.1");
    // A `--host ::1` serve renders its surface lines through the helper —
    // `http://::1:7474` is not a URL; the bracketed form is.
    assert_eq!(
        format!("http://{}:7474", url_host("::1")),
        "http://[::1]:7474"
    );
    assert_eq!(
        format!("bolt://{}:7687", url_host("::1")),
        "bolt://[::1]:7687"
    );
}

#[cfg(feature = "http")]
#[test]
fn bind_uses_the_given_host() {
    // Explicit loopback binds like the default; a bogus host is a loud
    // bind error (exit 1), not a panic.
    let (_port, listener) = bind("--http", "127.0.0.1", 0).unwrap();
    assert!(listener.local_addr().unwrap().is_ipv4());
    assert!(bind("--http", "no.such.host.invalid", 0).is_err());
}

#[cfg(feature = "bolt")]
#[test]
fn resolve_bolt_identity_precedence_is_flag_then_env_then_default() {
    let default = relate_bolt::protocol::default_server_identity();
    assert!(default.starts_with("Relate/"), "{default}");
    assert_eq!(resolve_bolt_identity(None, None), default);
    assert_eq!(
        resolve_bolt_identity(None, Some("Neo4j/4.4.0")),
        "Neo4j/4.4.0"
    );
    assert_eq!(
        resolve_bolt_identity(Some("Neo4j/5.26.0"), Some("Neo4j/4.4.0")),
        "Neo4j/5.26.0"
    );
}

#[test]
fn the_banner_names_a_nonwritable_mode_and_nothing_for_writable() {
    // ADR-0306 §5: exactly one extra line per non-writable mode — the
    // exact strings are outputs later layers pin.
    assert_eq!(access_banner_line(relate::OpenMode::Writable), None);
    assert_eq!(
        access_banner_line(relate::OpenMode::ReadOnly),
        Some(
            "serve: opened the store read-only — no writer lease and \
             never creates the store; every write fails"
        )
    );
    assert_eq!(
        access_banner_line(relate::OpenMode::Follow),
        Some(
            "serve: opened the store read-only and following the WAL — \
             the writer's committed frames appear as they land"
        )
    );
}

#[cfg(feature = "surface")]
#[test]
fn setup_opens_the_store_with_the_parsed_access_mode() {
    // The handle's is_read_only comes from the open itself, so it pins
    // that the parsed mode reached Database::open_with (ADR-0306 §2). The
    // store must exist first: read-only never creates (ADR-0136).
    // The surface flag is whichever surface this build compiled in
    // (the test's cfg gate compiles one).
    let surface = any_surface_flag();
    let dir = tempfile::tempdir().expect("tempdir");
    let store = dir.path().join("store");
    let store = relate_test_support::uri(&store);
    drop(relate::Database::open(store).expect("create the store"));
    let setup_with = |extra: &[&str]| {
        let mut rest = vec!["--database", store, surface, "0"];
        rest.extend_from_slice(extra);
        setup(&parse_serve_args(&args(&rest)).expect("the grammar accepts the tail"))
    };
    for (extra, mode) in [
        (&[][..], relate::OpenMode::Writable),
        (&["--read-only"][..], relate::OpenMode::ReadOnly),
        (&["--read-only=live"][..], relate::OpenMode::Follow),
    ] {
        // The surface flag makes every run here a Listening serve.
        let (db, banner) = match setup_with(extra) {
            Ok(ready) => match ready.kind {
                #[cfg(feature = "mcp")]
                ReadyKind::StdioOnly => panic!("{extra:?}: a listener serve is not StdioOnly"),
                ReadyKind::Listening { .. } => (ready.db, ready.banner),
            },
            Err(code) => panic!("{extra:?}: the store should open, got exit {code}"),
        };
        assert_eq!(
            db.is_read_only(),
            mode != relate::OpenMode::Writable,
            "{extra:?}"
        );
        // ADR-0306 §5: a non-writable mode's banner line rides the banner
        // Vec at the FRONT — run's one banner print shows it after the
        // host warning and before the auth lines. A writable serve adds
        // no banner line (access_banner_line's `None`, pinned above in
        // the_banner_names_a_nonwritable_mode_and_nothing_for_writable).
        if let Some(line) = access_banner_line(mode) {
            assert_eq!(banner.first().map(String::as_str), Some(line), "{extra:?}");
        }
    }
    // Read-only never creates: a missing store under --read-only is a loud
    // startup error, where the writable default would create it.
    let missing = dir.path().join("missing-store");
    let missing = relate_test_support::uri(&missing);
    let rest = vec!["--database", missing, surface, "0", "--read-only"];
    assert!(setup(&parse_serve_args(&args(&rest)).unwrap()).is_err());
}

/// ADR-0470: every durability mode reaches the served store's open —
/// `store_options` carries the parsed flag value into
/// `OpenOptions::durability_mode`, and an omitted flag leaves the option
/// unset so the engine's own resolution (environment, then `full`)
/// applies at the open. The engine's per-mode behavior is pinned by the
/// engine crate's own suites (`tests/persist.rs`,
/// `commit/tests/concurrency.rs`); this pin is the serve wiring.
#[cfg(feature = "surface")]
#[test]
fn store_options_carries_each_durability_mode_to_the_open() {
    let surface = any_surface_flag();
    for &(value, mode) in relate_test_support::durability::DURABILITY_MODES {
        let rest = vec![
            "--database",
            ":memory:",
            surface,
            "1",
            "--durability-mode",
            value,
        ];
        let cmd = parse_serve_args(&args(&rest)).expect("the grammar accepts the tail");
        let options = store_options(&cmd).expect("the mode resolves");
        assert_eq!(options.durability_mode, Some(mode), "{value}");
    }
    // Omitted: the option defers to the environment, then the engine
    // default — the shared resolution, not a serve-local default.
    let rest = vec!["--database", ":memory:", surface, "1"];
    let cmd = parse_serve_args(&args(&rest)).expect("the grammar accepts the tail");
    let options = store_options(&cmd).expect("the default resolves");
    assert_eq!(options.durability_mode, None);
}

#[cfg(feature = "surface")]
#[test]
fn setup_resolves_auth_only_after_the_open_and_the_binds() {
    // F-2-5: the generated-token path (no --auth, no RELATE_AUTH)
    // persists the token file at relate_auth::token_path, creating its
    // parent directory — the one side-effectful setup step. It must run
    // last: a failed open and a taken port both have to fail first and
    // leave no token file behind. (The identity and connection-cap arms
    // of the same invariant — a malformed RELATE_IDENTITY or
    // RELATE_MAX_CONNECTIONS must fail before auth runs — need the
    // process environment, so they are pinned race-free by e2e_serve.rs's
    // test_malformed_identity_fails_before_auth_persists_the_token and
    // test_malformed_max_connections_fails_before_auth_persists_the_token
    // instead of in-process here.)
    // The surface flag is whichever surface this build compiled in
    // (the test's cfg gate compiles one).
    let surface = any_surface_flag();
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = |store: &str| {
        relate_auth::token_path(store)
            .expect("a local store URI classifies")
            .expect("a persistent store has a token path")
    };

    // A failed open: read-only never creates the missing store, so the
    // open errors before auth could resolve.
    let missing = dir.path().join("missing");
    let missing = relate_test_support::uri(&missing);
    let rest = vec!["--database", missing, surface, "0", "--read-only"];
    assert!(matches!(
        setup(&parse_serve_args(&args(&rest)).unwrap()),
        Err(1)
    ));
    assert!(
        !token_file(missing).exists(),
        "a failed open must leave no token file behind"
    );

    // A taken port: the open succeeds, the bind fails, and the token file
    // still must not appear.
    let store = dir.path().join("store");
    let store = relate_test_support::uri(&store);
    let taken = std::net::TcpListener::bind((DEFAULT_HOST, 0)).expect("hold a port");
    let port = taken.local_addr().unwrap().port().to_string();
    let rest = vec!["--database", store, surface, port.as_str()];
    assert!(matches!(
        setup(&parse_serve_args(&args(&rest)).unwrap()),
        Err(1)
    ));
    assert!(
        !token_file(store).exists(),
        "a taken port must leave no token file behind"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_config_advertises_the_opened_handles_mode() {
    // ADR-0306 §3: write-cypher is listed exactly when the store accepts
    // writes — the config's read_only follows the handle.
    let auth = relate_auth::AuthConfig::None;
    let admission = crate::serve_cmd::Admission {
        connection_cap: Arc::new(relate_serve::ConnectionCap::new(4)),
        #[cfg(feature = "bolt")]
        ws_max_connections: 0,
        header_deadline: relate_serve::DEFAULT_HEADER_DEADLINE,
        body_deadline: relate_serve::DEFAULT_BODY_DEADLINE,
    };
    let writable = Arc::new(relate::Database::open(":memory:").expect("memory open"));
    assert!(!mcp_config(&writable, &auth, &admission).read_only);
    let read_only =
        Arc::new(relate::Database::open_read_only(":memory:").expect("read-only memory open"));
    assert!(mcp_config(&read_only, &auth, &admission).read_only);
}

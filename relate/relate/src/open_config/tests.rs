use relate_core::PreviewFeature;

use super::*;
use crate::{Error, OpenOptions};

#[test]
fn non_utf8_preview_setting_fails_open_naming_the_variable() {
    // The payload is never inspected: the resolver's contract keys on
    // the `VarError` variant, and `OsString::from_vec` (the realistic
    // invalid-bytes construction) does not exist on Windows. This
    // variant is exactly what `std::env::var` returns for a variable
    // whose bytes are not UTF-8.
    let err = preview_features_from_setting(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("not utf-8"),
    )))
    .unwrap_err();
    assert!(matches!(err, Error::Semantic(_)), "{err}");
    let display = err.to_string();
    assert!(display.contains("RELATE_PREVIEW"), "{display}");
    assert!(display.contains("not valid UTF-8"), "{display}");
}

#[test]
fn preview_setting_keeps_unset_empty_and_unknown_contract() {
    assert_eq!(
        preview_features_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        PreviewFeatures::none(),
    );
    assert_eq!(
        preview_features_from_setting(Ok(String::new())).unwrap(),
        PreviewFeatures::none(),
    );
    assert_eq!(
        preview_features_from_setting(Ok("   ".to_string())).unwrap(),
        PreviewFeatures::none(),
    );
    let enabled = preview_features_from_setting(Ok("ontology".to_string())).unwrap();
    assert!(enabled.enabled(PreviewFeature::Ontology));
    let err = preview_features_from_setting(Ok("bogus".to_string())).unwrap_err();
    assert!(
        err.to_string().contains("unknown preview feature 'bogus'"),
        "{err}"
    );
}

#[test]
fn markdown_bodies_setting_parses_unset_and_fails_open() {
    // The env contract of the sibling resolvers: unset/blank is None
    // (the sidecar governs), a good spec parses with the flag
    // grammar, a bad spec is its parse error, and non-UTF-8 fails
    // open naming the variable.
    assert_eq!(
        markdown_bodies_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        None
    );
    assert_eq!(
        markdown_bodies_from_setting(Ok(String::new())).unwrap(),
        None
    );
    assert_eq!(
        markdown_bodies_from_setting(Ok("   ".to_string())).unwrap(),
        None
    );
    let config = markdown_bodies_from_setting(Ok("summary,notes".to_string()))
        .unwrap()
        .expect("a good spec parses");
    assert_eq!(config.default, vec!["summary", "notes"]);
    let bad = markdown_bodies_from_setting(Ok("summary,,notes".to_string())).unwrap_err();
    assert!(
        bad.to_string().contains("an entry names no property"),
        "{bad}"
    );
    let utf8 = markdown_bodies_from_setting(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("not utf-8"),
    )))
    .unwrap_err();
    assert!(
        utf8.to_string()
            .contains("RELATE_MARKDOWN_BODY_PROPERTIES: value is not valid UTF-8"),
        "{utf8}"
    );
}

#[test]
fn bad_bulk_batch_setting_fails_open() {
    let err = positive_u64_setting("RELATE_BULK_BATCH_THRESHOLD", Some("1tb"), 7).unwrap_err();
    assert!(
        err.to_string().contains("RELATE_BULK_BATCH_THRESHOLD"),
        "{err}"
    );
    let err = positive_u64_setting("RELATE_BULK_BATCH_BUDGET", Some("0"), 7).unwrap_err();
    assert!(
        err.to_string().contains("RELATE_BULK_BATCH_BUDGET"),
        "{err}"
    );
    assert_eq!(positive_u64_setting("X", None, 7).unwrap(), 7);
    assert_eq!(positive_u64_setting("X", Some("3"), 7).unwrap(), 3);
}

#[test]
fn bad_memory_limit_setting_is_the_parse_error() {
    let err = memory_limit_from_setting(Some("1tb")).unwrap_err();
    assert_eq!(
        err,
        Error::semantic("invalid memory limit unit 'tb'; use bytes, mb, mib, gb, or gib")
    );
    assert_eq!(memory_limit_from_setting(None).unwrap(), 0);
    let ignored = renamed_env_error(
        "RELATE_MEMORY_LIMIT",
        "RELATE_LOGICAL_GRAPH_SIZE_LIMIT",
        Ok("1mib".into()),
    )
    .unwrap_err();
    assert!(
        ignored
            .to_string()
            .contains("RELATE_MEMORY_LIMIT is set but ignored"),
        "{ignored}"
    );
    assert!(renamed_env_error(
        "RELATE_MEMORY_LIMIT",
        "RELATE_LOGICAL_GRAPH_SIZE_LIMIT",
        Err(std::env::VarError::NotPresent),
    )
    .is_ok());
    assert_eq!(memory_limit_from_setting(Some("2mb")).unwrap(), 2_000_000);
    // An explicit OpenOptions value wins; the env string is not consulted.
    let opts = OpenOptions {
        logical_graph_size_limit: Some(1),
        ..Default::default()
    };
    assert_eq!(resolve_memory_limit(&opts).unwrap(), 1);
}

#[test]
fn expression_limit_resolution_has_a_generous_default_and_an_opt_out() {
    // Unset (or blank) leaves the engine default to the resolver.
    assert_eq!(
        expression_limit_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        None
    );
    assert_eq!(
        expression_limit_from_setting(Ok(String::new())).unwrap(),
        None
    );
    // The default is the measured 256 MiB.
    assert_eq!(relate_executor::EXPRESSION_BUDGET_BYTES, 256 << 20);
    // Explicit values: a raise, and the spelled/zero opt-out.
    assert_eq!(
        expression_limit_from_setting(Ok("512mib".into())).unwrap(),
        Some(512 << 20)
    );
    assert_eq!(
        expression_limit_from_setting(Ok("none".into())).unwrap(),
        Some(0)
    );
    assert_eq!(
        expression_limit_from_setting(Ok("NONE".into())).unwrap(),
        Some(0)
    );
    assert_eq!(
        expression_limit_from_setting(Ok("0".into())).unwrap(),
        Some(0)
    );
    // A bad value is the shared parse error; a non-UTF-8 value fails
    // open rather than silently reading as unset.
    let err = expression_limit_from_setting(Ok("1tb".into())).unwrap_err();
    assert!(
        err.to_string()
            .contains("invalid RELATE_EXPRESSION_MEMORY_LIMIT:"),
        "{err}"
    );
    assert!(
        expression_limit_from_setting(Err(std::env::VarError::NotUnicode(
            std::ffi::OsString::from("x")
        )))
        .is_err()
    );
    // An explicit OpenOptions value wins over the env/default.
    let opts = OpenOptions {
        expression_memory_limit: Some(42),
        ..Default::default()
    };
    assert_eq!(resolve_expression_memory_limit(&opts).unwrap(), Some(42));
    let opts = OpenOptions {
        expression_memory_limit: Some(0),
        ..Default::default()
    };
    assert_eq!(resolve_expression_memory_limit(&opts).unwrap(), None);
}

#[test]
fn workspace_limit_setting_enables_only_when_positive() {
    assert_eq!(
        workspace_limit_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        None
    );
    assert_eq!(
        workspace_limit_from_setting(Ok(String::new())).unwrap(),
        None
    );
    assert_eq!(
        workspace_limit_from_setting(Ok("   ".into())).unwrap(),
        None
    );
    assert_eq!(
        workspace_limit_from_setting(Ok("4096".into())).unwrap(),
        Some(4096)
    );
    assert_eq!(
        workspace_limit_from_setting(Ok("2mib".into())).unwrap(),
        Some(2 * 1024 * 1024)
    );
    let zero = workspace_limit_from_setting(Ok("0".into())).unwrap_err();
    assert!(
        zero.to_string()
            .contains("invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT: must be positive"),
        "{zero}"
    );
    let bad = workspace_limit_from_setting(Ok("1tb".into())).unwrap_err();
    assert!(
        bad.to_string()
            .contains("invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT:"),
        "{bad}"
    );
    let utf8 = workspace_limit_from_setting(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("not utf-8"),
    )))
    .unwrap_err();
    assert!(
        utf8.to_string().contains(
            "invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT: value is not valid UTF-8"
        ),
        "{utf8}"
    );
    let ignored = renamed_env_error(
        "RELATE_TOKEN_ADMISSION",
        "RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT",
        Ok("1".into()),
    )
    .unwrap_err();
    assert!(
        ignored
            .to_string()
            .contains("RELATE_TOKEN_ADMISSION is set but ignored"),
        "{ignored}"
    );
    let on = resolve_token_limits(&OpenOptions {
        concurrent_logical_workspace_limit: Some(8192),
        ..OpenOptions::default()
    })
    .unwrap();
    assert_eq!(on.capacity, Some(8192));
    let zero_option = resolve_token_limits(&OpenOptions {
        concurrent_logical_workspace_limit: Some(0),
        ..OpenOptions::default()
    })
    .unwrap_err();
    assert!(
        zero_option
            .to_string()
            .contains("invalid concurrent_logical_workspace_limit: must be positive"),
        "{zero_option}"
    );
}

/// LABS-610: the traversal-depth knob resolves flag first, then the
/// environment, then 500; 0 and anything over the 2000 ceiling fail.
#[test]
fn traversal_depth_resolves_default_flag_and_env_contract() {
    assert_eq!(
        max_traversal_depth_from_setting(None).unwrap(),
        relate_core::MAX_TRAVERSAL_DEPTH
    );
    assert_eq!(
        max_traversal_depth_from_setting(Some("2000")).unwrap(),
        2000
    );
    for bad in ["0", "2001", "nope"] {
        let err = max_traversal_depth_from_setting(Some(bad)).unwrap_err();
        assert!(
            err.to_string().contains("RELATE_MAX_TRAVERSAL_DEPTH"),
            "{bad}: {err}"
        );
    }
    let explicit = resolve_max_traversal_depth(&OpenOptions {
        max_traversal_depth: Some(1200),
        ..OpenOptions::default()
    })
    .unwrap();
    assert_eq!(explicit, 1200);
    let zero = resolve_max_traversal_depth(&OpenOptions {
        max_traversal_depth: Some(0),
        ..OpenOptions::default()
    })
    .unwrap_err();
    assert!(
        zero.to_string()
            .contains("invalid max_traversal_depth: must be between 1 and 2000"),
        "{zero}"
    );
}

/// ADR-0470: the durability mode's setting contract — unset/blank is
/// `None`, each mode name parses (case-insensitively, the
/// `DurabilityMode::from_str` grammar), a bad value fails naming the
/// variable and the mode vocabulary, and non-UTF-8 fails open naming
/// the variable.
#[test]
fn durability_mode_setting_parses_unset_and_fails_open() {
    use crate::DurabilityMode;
    assert_eq!(
        durability_mode_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        None
    );
    assert_eq!(
        durability_mode_from_setting(Ok(String::new())).unwrap(),
        None
    );
    assert_eq!(
        durability_mode_from_setting(Ok("   ".to_string())).unwrap(),
        None
    );
    // The shared table names the mode through the `relate` the
    // test-support crate links — a distinct crate instance from this
    // unit-test build's — so the pin compares the derived `Debug`
    // (the variant name), independent of the Display/FromStr
    // spellings under test.
    for &(raw, mode) in relate_test_support::durability::DURABILITY_MODES {
        let parsed = durability_mode_from_setting(Ok(raw.to_string()))
            .unwrap()
            .expect("a present mode name parses");
        assert_eq!(format!("{parsed:?}"), format!("{mode:?}"), "{raw}");
    }
    // Case-insensitive, the `from_str` grammar.
    assert_eq!(
        durability_mode_from_setting(Ok("Snapshot".to_string())).unwrap(),
        Some(DurabilityMode::Snapshot)
    );
    let bad = durability_mode_from_setting(Ok("everysecnd".to_string())).unwrap_err();
    let display = bad.to_string();
    assert!(display.contains("RELATE_DURABILITY_MODE"), "{display}");
    assert!(
        display.contains("expected full, everysecond, or snapshot"),
        "{display}"
    );
    let utf8 = durability_mode_from_setting(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("not utf-8"),
    )))
    .unwrap_err();
    assert!(
        utf8.to_string()
            .contains("RELATE_DURABILITY_MODE: value is not valid UTF-8"),
        "{utf8}"
    );
}

/// ADR-0470: the resolution ladder — the option's `Some(mode)` wins
/// (the environment is not consulted for it), and the default option
/// resolves to the engine default when the variable is unset in this
/// process (the variable's own parse contract is pinned above, pure
/// in the raw value).
#[test]
fn durability_mode_resolves_flag_first_then_env_then_default() {
    use crate::DurabilityMode;
    for &(raw, _) in relate_test_support::durability::DURABILITY_MODES {
        let mode: DurabilityMode = raw.parse().expect("a mode name parses");
        assert_eq!(
            resolve_durability_mode(&OpenOptions {
                durability_mode: Some(mode),
                ..OpenOptions::default()
            })
            .unwrap(),
            mode
        );
    }
    assert_eq!(
        resolve_durability_mode(&OpenOptions::default()).unwrap(),
        DurabilityMode::Full
    );
}

/// ADR-0519: the diagnostic pattern-engine switch's setting contract
/// — unset/blank/`auto` is today's routing exactly, `nfa` and `walk`
/// parse, a bad value fails open naming the variable and the three
/// valid values, and non-UTF-8 fails open naming the variable.
#[test]
fn pattern_engine_setting_parses_unset_and_fails_open() {
    assert_eq!(
        pattern_engine_from_setting(Err(std::env::VarError::NotPresent)).unwrap(),
        PatternEngine::Auto
    );
    assert_eq!(
        pattern_engine_from_setting(Ok(String::new())).unwrap(),
        PatternEngine::Auto
    );
    assert_eq!(
        pattern_engine_from_setting(Ok("   ".to_string())).unwrap(),
        PatternEngine::Auto
    );
    assert_eq!(
        pattern_engine_from_setting(Ok("auto".to_string())).unwrap(),
        PatternEngine::Auto
    );
    assert_eq!(
        pattern_engine_from_setting(Ok("nfa".to_string())).unwrap(),
        PatternEngine::Nfa
    );
    assert_eq!(
        pattern_engine_from_setting(Ok("walk".to_string())).unwrap(),
        PatternEngine::Walk
    );
    let bad = pattern_engine_from_setting(Ok("dfs".to_string())).unwrap_err();
    let display = bad.to_string();
    assert!(display.contains("RELATE_PATTERN_ENGINE"), "{display}");
    assert!(display.contains("expected auto, nfa, or walk"), "{display}");
    let utf8 = pattern_engine_from_setting(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("not utf-8"),
    )))
    .unwrap_err();
    assert!(
        utf8.to_string()
            .contains("RELATE_PATTERN_ENGINE: value is not valid UTF-8"),
        "{utf8}"
    );
}

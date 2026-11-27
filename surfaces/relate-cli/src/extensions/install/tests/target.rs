use super::*;

// --- the name checks (ADR-0431 §3, §7) ---

#[test]
fn a_name_that_is_not_a_single_segment_refuses() {
    for name in ["", ".", "..", "a/b", "a\\b", "../evil"] {
        let err = refusal(validate_package_name(name));
        assert_eq!(err.exit_code(), 2, "{name}: {}", err.stderr_message());
        assert!(
            err.stderr_message().contains("is not a valid package name"),
            "{name}: {}",
            err.stderr_message()
        );
    }
    validate_package_name("greet").unwrap();
    validate_package_name("git-to-graph").unwrap();
}

#[test]
fn a_reserved_word_name_refuses_with_the_registrys_text() {
    // The same pinned text the load-time refusal prints (§4 step 5).
    let err = refusal(check_reserved_words("serve"));
    assert_eq!(err.exit_code(), 2);
    assert_eq!(
        err.stderr_message(),
        "Error: extension install: extension name 'serve' collides with the reserved command word 'serve'"
    );
}

// With the `lab` surface retired (ADR-0431 §12) there are no built-in
// action words: the importer names are installable package names now,
// refused (or installed) by the installed-set checks below.

// --- the target-decision matrix (§3 reconciled with §7) ---

#[test]
fn a_fresh_target_installs() {
    let world = World::new();
    check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    )
    .unwrap();
}

#[test]
fn a_foreign_target_refuses_without_force_and_clobbers_with_it() {
    let world = World::new();
    // A directory sits at the target without a marker: not ours to touch
    // (the skill install refusal, ADR-0261; ADR-0431 §3).
    world.install_fixture("greet", &manifest_text("greet", "1.0.0"), None);
    let err = refusal(check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    ));
    assert_eq!(err.exit_code(), 1);
    assert!(
        err.stderr_message().contains("refusing to touch"),
        "{}",
        err.stderr_message()
    );
    assert!(
        err.stderr_message().contains("pass --force to override"),
        "{}",
        err.stderr_message()
    );
    // --force clobbers exactly this shape (§3).
    check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        true,
        &world.registry(),
    )
    .unwrap();
}

#[test]
fn a_same_tier_reinstall_replaces_cleanly() {
    // §3: "a marked install of the same name replaces cleanly (reinstall
    // or version change)".
    let world = World::new();
    world.install_fixture(
        "greet",
        &manifest_text("greet", "1.0.0"),
        Some(&Provenance {
            tier: Tier::Local,
            source: "/elsewhere/pkg".to_string(),
            pin: None,
        }),
    );
    check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    )
    .unwrap();
}

#[test]
fn a_cross_tier_reinstall_refuses_naming_both_sides() {
    // §7: a first-party package can never be replaced by a third-party
    // one, or the reverse, without an explicit remove.
    let world = World::new();
    let provenance = Provenance {
        tier: Tier::ThirdParty,
        source: "https://example.test/greet.tar.gz".to_string(),
        pin: None,
    };
    world.install_fixture("greet", &manifest_text("greet", "1.0.0"), Some(&provenance));
    let err = refusal(check_target(
        &world.extensions_dir(),
        "greet",
        Tier::FirstParty,
        false,
        &world.registry(),
    ));
    assert_eq!(err.exit_code(), 1);
    assert_eq!(
        err.stderr_message(),
        "Error: extension install: extension 'greet' is installed third-party \
         (source: https://example.test/greet.tar.gz); refusing a first-party install over it — \
         remove it first: relate extension remove greet"
    );
    // The same tier still replaces cleanly.
    check_target(
        &world.extensions_dir(),
        "greet",
        Tier::ThirdParty,
        false,
        &world.registry(),
    )
    .unwrap();
}

/// The cross-tier clobber race: `check_target` decides on the target's
/// state before the fetch, so a same-name install that lands in between is
/// invisible to it. The land-time re-decision reads the package actually
/// parked aside and refuses the tier-crossing replace — the third-party
/// install never shadows the first-party one — restoring the parked
/// package whole.
#[test]
fn a_tier_crossing_replace_refuses_at_land_time_even_though_check_target_saw_the_older_state() {
    let world = World::new();
    // check_target's snapshot: no target yet, so the incoming tier is
    // accepted.
    check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    )
    .expect("a fresh target accepts the incoming tier");
    // The race: a first-party install of the same name lands after the
    // snapshot and before the replace.
    world.install_fixture(
        "greet",
        &manifest_text("greet", "1.0.0"),
        Some(&Provenance {
            tier: Tier::FirstParty,
            source: "relate-extensions-greet-1.0.0".to_string(),
            pin: None,
        }),
    );
    let source = world.source_package("greet", "2.0.0");
    let staged = staging_dir(&world);
    let trust_file = world.trust_file();
    let mut trust = TrustStore::load(&trust_file).unwrap();
    let args = install_args(
        "greet",
        InstallSource::From(source.display().to_string()),
        None,
    );

    let err = stage_and_land(
        &world.extensions_dir(),
        &GithubRoute::from_env,
        &args,
        &mut trust,
        &staged,
    )
    .expect_err("the tier-crossing replace refuses at land time");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(
        err.stderr_message(),
        "Error: extension install: extension 'greet' is installed first-party \
         (source: relate-extensions-greet-1.0.0); refusing a local install over it — \
         remove it first: relate extension remove greet"
    );

    // The parked first-party package is back in place, untouched.
    let target = world.extensions_dir().join("greet");
    let provenance = Provenance::read(&target)
        .unwrap()
        .expect("the marker is back in place");
    assert_eq!(provenance.tier, Tier::FirstParty);
    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(target.join("extension.json")).unwrap())
            .unwrap();
    let Manifest::Command(command) = &manifest;
    assert_eq!(command.version, "1.0.0");
    // No parked-aside or staging sibling was left behind.
    let siblings: Vec<_> = std::fs::read_dir(world.extensions_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| crate::staging::is_staging_name(&entry.file_name()))
        .collect();
    assert!(siblings.is_empty(), "installer residue: {siblings:?}");
}

#[test]
fn a_name_claimed_from_another_directory_is_a_true_collision() {
    // A hand-copied package directory claiming the mount name: the
    // load-time collision's text, naming the other package directory.
    let world = World::new();
    world.install_fixture(
        "greet-copy",
        &manifest_text("greet", "1.0.0"),
        Some(&Provenance {
            tier: Tier::Local,
            source: "/x".to_string(),
            pin: None,
        }),
    );
    let err = refusal(check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    ));
    assert_eq!(err.exit_code(), 1);
    assert_eq!(
        err.stderr_message(),
        "Error: extension install: extension name 'greet' collides with the \
         already-installed extension (package 'greet-copy')"
    );
}

#[test]
fn a_broken_marker_at_the_target_refuses() {
    let world = World::new();
    world.install_fixture("greet", &manifest_text("greet", "1.0.0"), None);
    std::fs::write(
        world.extensions_dir().join("greet").join(MARKER_FILE),
        "{ nope",
    )
    .unwrap();
    let err = refusal(check_target(
        &world.extensions_dir(),
        "greet",
        Tier::Local,
        false,
        &world.registry(),
    ));
    assert_eq!(err.exit_code(), 1);
    assert!(
        err.stderr_message().contains("invalid provenance marker"),
        "{}",
        err.stderr_message()
    );
}

#[test]
fn the_loud_line_names_the_tier_the_version_and_the_source() {
    // §6: "prints one line naming the package, its version, and the URL
    // it came from" — realized per tier (§7's source reference).
    assert_eq!(
        loud_line(
            "git-to-graph",
            "1.0.0",
            &Provenance {
                tier: Tier::FirstParty,
                source: "relate-extensions-git-to-graph-1.0.0".to_string(),
                pin: None
            }
        ),
        "installed first-party extension git-to-graph 1.0.0 from the signed channel \
         (tag relate-extensions-git-to-graph-1.0.0)"
    );
    assert_eq!(
        loud_line(
            "greet",
            "1.2.3",
            &Provenance {
                tier: Tier::ThirdParty,
                source: "https://example.test/greet.tar.gz".to_string(),
                pin: None
            }
        ),
        "installed third-party extension greet 1.2.3 from https://example.test/greet.tar.gz"
    );
}

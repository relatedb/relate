//! Hidden REPL dot commands (ADR-0543). Runs in every bundle: off a terminal
//! the egg names are ordinary unknown commands, whether or not the `eggs`
//! feature is compiled in, and no banner art ever reaches a one-shot or
//! piped run.

mod support;

use support::{run_cli, tempdir, CliRunOptions as Opts};

#[test]
fn test_egg_names_are_unknown_commands_off_a_terminal() {
    let home = tempdir();
    for command in [".archie", ".archie s"] {
        let (status, stdout, stderr) = run_cli(
            Opts::new(&["query"])
                .in_home(home.path())
                .with_stdin(command.as_bytes()),
        );
        assert_eq!(status.code(), Some(1), "{command}: {stderr}");
        assert!(stdout.is_empty(), "{command} printed to stdout: {stdout}");
        assert!(
            stderr.contains("unknown command .archie"),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn test_text_egg_names_are_unknown_commands_off_a_terminal() {
    let home = tempdir();
    for command in [".matrix", ".cowsay hi", ".fortune"] {
        let (status, stdout, stderr) = run_cli(
            Opts::new(&["query"])
                .in_home(home.path())
                .with_stdin(command.as_bytes()),
        );
        assert_eq!(status.code(), Some(1), "{command}: {stderr}");
        assert!(stdout.is_empty(), "{command} printed to stdout: {stdout}");
        let name = command.split(' ').next().unwrap();
        assert!(
            stderr.contains(&format!("unknown command {name}")),
            "{command}: {stderr}"
        );
    }
}

#[test]
fn test_one_shot_runs_print_no_banner_art_even_when_forced() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query", "RETURN 1 AS n;"])
            .in_home(home.path())
            .with_envs(&[("RELATE_BANNER", "1")]),
    );
    assert!(status.success(), "{stderr}");
    assert!(!stdout.contains('\u{2593}') && !stdout.contains('\u{2580}'));
    assert!(!stdout.contains('\x1b'));
}

#[test]
fn test_eggs_are_not_listed_in_help() {
    let home = tempdir();
    let (status, stdout, _) = run_cli(Opts::new(&["--help"]).in_home(home.path()));
    assert!(status.success());
    assert!(!stdout.contains("archie"));
}

/// The real binary on a PTY: banner art, the tinted glyph prompt, and
/// `.archie` under truecolor, then the NO_COLOR plain fallbacks.
#[cfg(all(feature = "eggs", unix))]
#[test]
fn test_archie_banner_prompt_and_command_on_a_terminal() {
    let binary = env!("CARGO_BIN_EXE_relate");
    support::assert_pty_driver_ok("pty_eggs.py", &[binary]);
}

/// `.fortune`, `.cowsay` and `.matrix` on a PTY, plain under NO_COLOR.
#[cfg(all(feature = "eggs", unix))]
#[test]
fn test_text_eggs_on_a_terminal() {
    let binary = env!("CARGO_BIN_EXE_relate");
    let fortunes = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../relate-eggs/assets/fortunes.txt"
    );
    support::assert_pty_driver_ok("pty_eggs_text.py", &[binary, fortunes]);
}

/// `.explain-art` on a PTY: the flame under truecolor and NO_COLOR, and a
/// typed refusal (nothing executed) for a write.
#[cfg(all(feature = "eggs", unix))]
#[test]
fn test_explain_art_draws_a_flame_and_refuses_writes_on_a_terminal() {
    let binary = env!("CARGO_BIN_EXE_relate");
    support::assert_pty_driver_ok("pty_eggs_explain_art.py", &[binary]);
}

/// The real binary on a PTY: each game takes the alternate screen and gives
/// the terminal back on `q` and on Ctrl-C; a small window is refused.
#[cfg(all(feature = "eggs", unix))]
#[test]
fn test_games_restore_the_terminal_on_a_terminal() {
    let binary = env!("CARGO_BIN_EXE_relate");
    support::assert_pty_driver_ok("pty_games.py", &[binary]);
}

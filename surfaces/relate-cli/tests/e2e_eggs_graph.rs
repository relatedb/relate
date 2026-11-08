//! The hidden `.lolwut` and `.graph` dot commands (ADR-0543). Runs in every
//! bundle: off a terminal they are ordinary unknown commands; on a PTY (with
//! the `eggs` feature) they draw.

mod support;

use support::{run_cli, tempdir, CliRunOptions as Opts};

#[test]
fn test_lolwut_and_graph_are_unknown_commands_off_a_terminal() {
    let home = tempdir();
    for (command, name) in [
        (".lolwut", "lolwut"),
        (".graph MATCH (n) RETURN n", "graph"),
    ] {
        let (status, stdout, stderr) = run_cli(
            Opts::new(&["query"])
                .in_home(home.path())
                .with_stdin(command.as_bytes()),
        );
        assert_eq!(status.code(), Some(1), "{command}: {stderr}");
        assert!(stdout.is_empty(), "{command} printed to stdout: {stdout}");
        assert!(
            stderr.contains(&format!("unknown command .{name}")),
            "{command}: {stderr}"
        );
    }
}

/// The real binary on a PTY: braille art plus version, then a drawn graph
/// with labels and the "showing N of M" footer.
#[cfg(all(unix, feature = "eggs"))]
#[test]
fn test_lolwut_and_graph_draw_on_a_terminal() {
    let binary = env!("CARGO_BIN_EXE_relate");
    support::assert_pty_driver_ok("pty_eggs_graph.py", &[binary]);
}

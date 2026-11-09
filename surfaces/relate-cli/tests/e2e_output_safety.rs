//! End-to-end tests for CLI output safety: CSV stdout must never emit raw
//! control bytes, and error/echo paths must sanitize terminal escape
//! sequences before they reach stderr.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

#[test]
fn test_csv_stdout_neutralizes_control_bytes() {
    // A `--csv` query whose string values carry real ESC (0x1B), BEL (0x07) and
    // NUL (0x00) bytes must print the braced escapes, never the raw bytes: a
    // downstream terminal or parser must not see an escape sequence. The Cypher
    // lexer decodes `\uXXXX` in the query literal to the real byte, so the
    // value reaching the formatter is genuinely unsafe.
    let query = "RETURN 'a\\u001Bb' AS esc, 'x\\u0007y' AS bel, 'p\\u0000q' AS nul;";
    let (status, stdout, stderr) = run_cli(Opts::new(&["-csv", query]));
    assert!(status.success(), "csv control query failed: {stderr}");
    assert!(
        stdout.contains("\\u{1B}"),
        "expected the literal ESC escape in CSV stdout, got: {stdout:?}"
    );
    assert!(
        stdout.contains("\\u{7}"),
        "expected the literal BEL escape in CSV stdout, got: {stdout:?}"
    );
    assert!(
        stdout.contains("\\u{0}"),
        "expected the literal NUL escape in CSV stdout, got: {stdout:?}"
    );
    assert!(
        !stdout.bytes().any(|b| b < 0x20 && b != b'\n'),
        "CSV stdout must contain no raw control byte other than LF, got: {stdout:?}"
    );
}

#[test]
fn test_unknown_dot_command_echo_is_sanitized() {
    // A `.`-prefixed token is echoed verbatim in the unknown-command error, so
    // a crafted ESC sequence from stdin must be stripped by the shared
    // `strip_unsafe_output_chars`, not passed through to the terminal raw.
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["query"]).with_stdin(".evil\x1b[31m\n".as_bytes()));
    assert_eq!(status.code(), Some(1), "unknown command should exit 1");
    assert!(
        !stderr.contains('\x1b'),
        "raw ESC survived into stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("unknown command .evil[31m"),
        "expected the sanitized echo in stderr, got: {stderr:?}"
    );
}

#[test]
fn test_argv_error_echoes_are_sanitized() {
    // The flag parser echoes the offending argv token in its error: an
    // unknown flag and an invalid flag value. A crafted ESC (0x1B) inside
    // either token must be stripped by `print_error` before it reaches the
    // terminal; the sanitized remainder stays, proving the byte was removed
    // rather than never present.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--evil\x1b[31m"]));
    assert_eq!(status.code(), Some(2), "unknown flag should exit 2");
    assert!(
        !stderr.as_bytes().contains(&0x1b),
        "raw ESC survived into stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("unknown flag: --evil[31m"),
        "expected the sanitized flag echo in stderr, got: {stderr:?}"
    );

    let (status, _stdout, stderr) = run_cli(Opts::new(&["--mode", "ta\x1b[31mble"]));
    assert_eq!(status.code(), Some(2), "invalid mode should exit 2");
    assert!(
        !stderr.as_bytes().contains(&0x1b),
        "raw ESC survived into stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("invalid mode 'ta[31mble'"),
        "expected the sanitized mode echo in stderr, got: {stderr:?}"
    );
}

#[test]
fn test_multiline_engine_error_renders_legibly_on_one_line() {
    // The composite-database errors are the engine's first multi-line error
    // texts: their message embeds a raw LF. The stderr sanitizer must map
    // that newline to a space — not delete it — so the rendered error keeps
    // a word boundary (". Attempted", not ".Attempted"), while still never
    // emitting a raw control byte.
    let query = "RETURN graph.byElementId('4:rl-id:n1')";
    let (status, _stdout, stderr) = run_cli(Opts::new(&["-c", query]));
    assert_eq!(
        status.code(),
        Some(6),
        "composite-only query is a rejected statement (ADR-0194)"
    );
    assert!(
        stderr.contains(
            "composite databases. Attempted to access graph graph.byElementId('4:rl-id:n1')"
        ),
        "expected the multi-line error joined with a space, got: {stderr:?}"
    );
    // The joined text is one physical line: no raw LF survives inside it.
    let line = stderr
        .lines()
        .find(|l| l.contains("composite databases"))
        .expect("the composite error line must exist");
    assert!(
        line.contains("byElementId('4:rl-id:n1')"),
        "the error must render on one line, got: {stderr:?}"
    );
    assert!(
        !stderr.bytes().any(|b| b < 0x20 && b != b'\n'),
        "stderr must contain no raw control byte other than the trailing LF, got: {stderr:?}"
    );
}

#[test]
fn test_query_parse_error_does_not_echo_raw_escape() {
    // A parse error quotes a snippet of the offending source text. With a real
    // ESC (0x1B) inside that source, the shared strip must remove it before the
    // message reaches the terminal. `RETURN 'x<ESC>y` is an unterminated
    // string, so the error echoes the snippet; the sanitized snippet keeps
    // `'xy` adjacent, proving the ESC between them was stripped rather than
    // never present.
    let query = "RETURN 'x\u{1b}y";
    let (_status, _stdout, stderr) = run_cli(Opts::new(&[query]));
    assert!(
        stderr.contains("unterminated string"),
        "expected a parse error that echoes the source, got: {stderr:?}"
    );
    assert!(
        !stderr.as_bytes().contains(&0x1b),
        "raw ESC survived into stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("RETURN 'xy"),
        "expected the sanitized source snippet in stderr, got: {stderr:?}"
    );
}

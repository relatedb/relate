//! End-to-end coverage for CLI `--param` bindings and parameter errors.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

// --- --param flag (repeatable N=V bindings) ---

#[test]
fn test_param_int() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--param", "x=42", "RETURN $x AS v"]));
    assert!(status.success(), "param int failed: {stderr}");
    assert!(stdout.contains("42"), "expected 42, got: {stdout}");
}

#[test]
fn test_param_float_decimal() {
    // `43.0` must bind as a decimal-preserving Float (JSON emits `43.0`, not `43`).
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--param", "x=43.0", "-json", "RETURN $x AS v"]));
    assert!(status.success(), "param float failed: {stderr}");
    assert!(stdout.contains("43.0"), "expected 43.0, got: {stdout}");
}

#[test]
fn test_param_string_bare() {
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--param", "name=Alice", "RETURN $name AS v"]));
    assert!(status.success(), "param bare string failed: {stderr}");
    assert!(stdout.contains("Alice"), "expected Alice, got: {stdout}");
}

#[test]
fn test_param_string_quoted() {
    // `name="Alice"` is JSON-parsed to the string Alice.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--param",
        "name=\"Alice\"",
        "RETURN $name AS v",
    ]));
    assert!(status.success(), "param quoted string failed: {stderr}");
    assert!(stdout.contains("Alice"), "expected Alice, got: {stdout}");
}

#[test]
fn test_param_multiple() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--param",
        "name=Bob",
        "--param",
        "age=25",
        "-json",
        "CREATE (p:Person {name:$name, age:$age}) RETURN p.name AS n, p.age AS a",
    ]));
    assert!(status.success(), "param multiple failed: {stderr}");
    assert!(stdout.contains("Bob"), "expected Bob, got: {stdout}");
    assert!(stdout.contains("25"), "expected 25, got: {stdout}");
}

#[test]
fn test_param_tagged_objects_are_ordinary_maps_and_typed_values_use_cypher_constructors() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
            "--param",
            r#"p={"__type":"point","x":1}"#,
            "--param",
            r#"complete={"__type":"point","srid":7203,"x":1,"y":2}"#,
            "--param",
            r#"nested={"items":[{"__type":"date","value":"bad"}]}"#,
            "--param",
            r#"coords={"x":1.5,"y":2.5}"#,
            "--param",
            "iso=\"2024-03-14\"",
            "--json",
            "RETURN $p AS p, $complete AS complete, $nested AS nested, point($coords) AS spatial, date($iso) AS temporal",
        ]));
    assert_eq!(status.code(), Some(0), "parameter query failed: {stderr}");
    // LABS-1120: the rows ride under `data` beside the timing rider.
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        rows["data"][0]["p"],
        serde_json::json!({"__type":"point","x":1})
    );
    assert_eq!(
        rows["data"][0]["complete"],
        serde_json::json!({"__type":"point","srid":7203,"x":1,"y":2})
    );
    assert_eq!(
        rows["data"][0]["nested"],
        serde_json::json!({"items":[{"__type":"date","value":"bad"}]})
    );
    assert_eq!(
        rows["data"][0]["spatial"],
        /* the ADR-0223 wire form, LABS-107 */
        serde_json::json!({"crs": "cartesian", "srid": 7203, "x": 1.5, "y": 2.5})
    );
    assert_eq!(rows["data"][0]["temporal"], "2024-03-14");
}

#[test]
fn test_param_invalid_json_and_unrepresentable_values_are_usage_errors() {
    for (param, reason) in [
        (r#"p={"x":}"#, "invalid parameter JSON"),
        ("p=1e400", "invalid parameter JSON"),
        (
            r#"p={"nested":[18446744073709551615]}"#,
            "parameter p: $.nested[0]: integer exceeds i64 range",
        ),
        ("p=18446744073709551615", "integer exceeds i64 range"),
    ] {
        let (status, stdout, stderr) =
            run_cli(Opts::new(&["--param", param, "--json", "RETURN 1 AS v"]));
        assert_eq!(status.code(), Some(2), "{param}: {stderr}");
        assert!(stdout.is_empty(), "{param} unexpectedly executed: {stdout}");
        assert!(
            stderr.contains("Error: invalid --param value: parameter p:")
                && stderr.contains(reason),
            "{param}: {stderr}"
        );
    }
}

#[test]
fn test_param_missing_value() {
    let (status, _stdout, _stderr) = run_cli(Opts::new(&["--param"]));
    assert!(
        !status.success(),
        "--param with no value must exit non-zero"
    );
    assert_eq!(
        status.code(),
        Some(2),
        "--param with no value should exit code 2"
    );
}

#[test]
fn test_param_invalid_no_eq() {
    let (status, _stdout, _stderr) = run_cli(Opts::new(&["--param", "justname", "RETURN 1"]));
    assert_eq!(
        status.code(),
        Some(2),
        "--param without '=' should exit code 2"
    );
}

#[test]
fn test_unbound_param_fails_with_parameter_missing() {
    // ADR-0308: a referenced parameter that was not supplied fails the
    // statement (a validation error, exit 6), as in Neo4j — never null.
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--param", "x=1", "-json", "RETURN $y AS v"]));
    assert_eq!(status.code(), Some(6), "stdout: {stdout} stderr: {stderr}");
    assert!(
        stderr.contains("ParameterMissing: Expected parameter(s): y"),
        "expected ParameterMissing for unbound $y, got: {stderr}"
    );
    assert!(
        stdout.contains("\"code\":\"validation_error\""),
        "expected the validation_error envelope, got: {stdout}"
    );
}

#[test]
fn test_param_overflow_not_int_zero() {
    // Regression for audit finding 1: a 20-digit integer param
    // (99999999999999999999 = 1e20) overflows u64/i64, so serde_json stores it
    // as an f64 and as_i64()/as_u64() both return None. The Number branch must
    // then bind a Float -- it MUST NOT silently coerce to Int(0).
    //
    // JSON wire mode: serde_json renders the Float 1e20 as `1e+20`. Parse the
    // output and compare numerically to prove the bound value is 1e20, not Int(0).
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--param",
        "x=99999999999999999999",
        "-json",
        "RETURN $x AS v",
    ]));
    assert!(
        status.success(),
        "overflow param must bind (as Float), failed: {stderr}"
    );
    assert!(
        stdout.contains("1e+20"),
        "expected the f64 rendering of 1e20 in JSON wire output, got: {stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    let arr = parsed["data"].as_array().expect("expected the data array");
    assert_eq!(
        arr[0]["v"],
        serde_json::json!(1e20),
        "bound value must be the 1e20 Float, not Int(0): {stdout}"
    );

    // Table mode renders the same Float with its full digit run
    // (100000000000000000000.0); the digit run is the f64 rendering of 1e20.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "table",
        "--param",
        "x=99999999999999999999",
        "RETURN $x AS v",
    ]));
    assert!(
        status.success(),
        "table-mode overflow param failed: {stderr}"
    );
    assert!(
        stdout.contains("100000000000000000000"),
        "expected the 1e20 digit run in table output, got: {stdout}"
    );
}

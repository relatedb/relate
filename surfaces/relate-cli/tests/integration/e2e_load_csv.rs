//! The documented CLI opt-in is passed through to the engine (ADR-0081).

use std::process::Output;

use crate::support;

fn run(args: &[&str], env_root: Option<&str>) -> Output {
    let mut cmd = support::cli_command();
    cmd.args(args).env_remove("RELATE_LOAD_CSV_DIR");
    if let Some(root) = env_root {
        cmd.env("RELATE_LOAD_CSV_DIR", root);
    }
    cmd.output().unwrap()
}

#[test]
fn load_csv_opt_in_default_denial_and_flag_precedence() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("names.csv"), "Ada\n").unwrap();
    std::fs::write(other.path().join("names.csv"), "Lin\n").unwrap();
    let root = root.path().to_str().unwrap();
    let other = other.path().to_str().unwrap();
    let query = "LOAD CSV FROM 'names.csv' AS row RETURN row[0] AS name";

    let denied = run(&[query], None);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("LOAD CSV file unavailable"));

    let env = run(&[query], Some(root));
    assert!(
        env.status.success(),
        "{}",
        String::from_utf8_lossy(&env.stderr)
    );
    assert!(String::from_utf8_lossy(&env.stdout).contains("Ada"));

    let flag = run(&["--load-csv-dir", other, query], Some(root));
    assert!(
        flag.status.success(),
        "{}",
        String::from_utf8_lossy(&flag.stderr)
    );
    let stdout = String::from_utf8_lossy(&flag.stdout);
    assert!(stdout.contains("Lin"));
    assert!(!stdout.contains("Ada"));

    let unknown = run(&["-load-csv-dir", root, query], None);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown flag"));

    let missing_value = run(&["--load-csv-dir"], None);
    assert_eq!(missing_value.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_value.stderr).contains("requires a path"));

    let help = run(&["--help"], None);
    assert!(String::from_utf8_lossy(&help.stdout).contains("--load-csv-dir PATH"));
}

#[test]
fn load_csv_file_url_of_an_in_root_file_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("names.csv");
    std::fs::write(&path, "Ada\n").unwrap();
    let root = root.path().to_str().unwrap();
    // file:/// + an absolute path is the documented local URL spelling.
    let path = path.to_str().unwrap().replace('\\', "/");
    let absolute = format!("file:///{}", path.trim_start_matches('/'));
    let mut urls = vec![absolute];
    // A `file:///name.csv` URL reads a path relative to the policy root, so it
    // names the same file as the bare `names.csv` spelling. Windows file URLs
    // stay drive-rooted, so this Unix spelling is not accepted there.
    #[cfg(not(windows))]
    urls.push("file:///names.csv".to_owned());
    for url in urls {
        let query = format!("LOAD CSV FROM '{url}' AS row RETURN row[0] AS name");
        let output = run(&["--load-csv-dir", root, &query], None);
        assert!(
            output.status.success(),
            "{url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Ada"),
            "{url}"
        );
    }
}

#[test]
fn load_csv_default_byte_limit_rejects_one_extra_byte_without_leaking_paths() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("too_large.csv"), vec![b'x'; 8_388_609]).unwrap();
    let root_path = root.path().to_str().unwrap();
    let output = run(
        &[
            "--load-csv-dir",
            root_path,
            "LOAD CSV FROM 'too_large.csv' AS row RETURN row",
        ],
        None,
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("LOAD CSV file exceeds byte limit"),
        "{error}"
    );
    assert!(!error.contains(root_path), "{error}");
    assert!(!error.contains("too_large.csv"), "{error}");
}

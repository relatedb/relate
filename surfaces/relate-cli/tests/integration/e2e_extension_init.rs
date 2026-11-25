//! `relate extension init` end to end (ADR-0431 §3): the scaffold → edit
//! → install `--from` → run loop through the real binary. The Python (the
//! default) and bash templates are driven to completion into a store — the
//! generated glue is the package's whole client, so a green run proves the
//! vendored sources speak the mount. TypeScript and Rust are validated as
//! far as this lane can take them: their manifests must load, and the
//! TypeScript template runs too wherever `node` 22.18+ is on the PATH.

#![cfg(all(feature = "extensions", unix))]

use crate::support;

use std::path::Path;
#[cfg(feature = "extension-install")]
use std::process::ExitStatus;

use support::extensions::run_in_xdg;
use support::{run_cli, tempdir, CliRunOptions as Opts};

/// Scaffold `name` with `--lang` into `dir` (the init's working directory)
/// and return the package directory.
fn init(dir: &Path, name: &str, lang: Option<&str>) -> std::path::PathBuf {
    let mut args = vec!["extension", "init", name];
    if let Some(lang) = lang {
        args.push("--lang");
        args.push(lang);
    }
    let (status, stdout, stderr) = run_cli(Opts::new(&args).in_dir(dir));
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");
    let package = dir.join(name);
    assert!(package.is_dir(), "the scaffold wrote {}", package.display());
    assert!(
        stdout.contains(&format!(
            "created {} extension '{name}'",
            lang.unwrap_or("py")
        )),
        "{stdout}"
    );
    package
}

/// Install the scaffolded package through the CLI — the command the
/// scaffold itself printed. The install verbs ride `extension-install`; a
/// build with `extensions` alone scaffolds but cannot install.
#[cfg(feature = "extension-install")]
fn install(package: &Path, name: &str, xdg: &str) -> (ExitStatus, String, String) {
    let from = package.display().to_string();
    run_in_xdg(&["extension", "install", name, "--from", &from], xdg)
}

/// Whether a program is on the PATH — the lane's optional-runtime gate.
#[cfg(all(feature = "extension-install", unix))]
fn have(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Whether this lane's `node` can strip the template's type annotations —
/// the same capability `scripts/check-extension-examples.sh` probes (Node
/// 22.13+ ships `stripTypeScriptTypes`).
#[cfg(feature = "extension-install")]
fn node_can_strip_types() -> bool {
    have("node")
        && std::process::Command::new("node")
            .args([
                "--no-warnings",
                "-e",
                "process.exit(typeof require('node:module').stripTypeScriptTypes === 'function' ? 0 : 1)",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

/// The Python template (the default `--lang`) is installable and runnable as
/// scaffolded: the generated glue completes the handshake, writes the sample
/// node, and reports the summary the host prints.
#[cfg(feature = "extension-install")]
#[test]
fn test_the_python_template_installs_and_runs_as_scaffolded() {
    if !have("python3") {
        eprintln!("skip: python3 is not on the PATH");
        return;
    }
    let tmp = tempdir();
    let package = init(tmp.path(), "scaffold-py", None);
    assert!(package.join("scaffold-py.py").is_file());

    let (_xdg, xdg_root) = support::extensions::xdg_root();
    let (status, stdout, stderr) = install(&package, "scaffold-py", &xdg_root);
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("installed local extension scaffold-py 0.1.0"),
        "{stdout}"
    );

    // The sample's one flag travels the whole way: argv → the host's
    // envelope → the walk's `$name` parameter → the greeting the summary
    // prints → the property the write set.
    let store = tmp.path().join("store-py");
    let store = store.display().to_string();
    let (status, stdout, stderr) = run_in_xdg(
        &["run", "scaffold-py", "--name", "Alex", "-d", &store],
        &xdg_root,
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout
            .contains("scaffold-py: Hello Alex world! — wrote 1 sample node(s), the store holds 1"),
        "{stdout}"
    );

    // The graph the walk wrote is in the store.
    let row = support::query_single_row(
        tmp.path(),
        Path::new(&store),
        &[],
        "MATCH (n:Scaffold) RETURN n.id AS id, n.note AS note",
    );
    assert_eq!(row["id"], "example");
    assert_eq!(row["note"], "Alex");
}

/// The bash template is the canonical kit inlined: installed as scaffolded
/// and driven to completion into a store.
#[cfg(feature = "extension-install")]
#[test]
fn test_the_bash_template_installs_and_runs_as_scaffolded() {
    let tmp = tempdir();
    let package = init(tmp.path(), "scaffold-sh", Some("sh"));
    assert!(package.join("scaffold-sh.sh").is_file());
    assert!(package.join("extension.json").is_file());
    assert!(package.join("README.md").is_file());

    let (_xdg, xdg_root) = support::extensions::xdg_root();
    let (status, stdout, stderr) = install(&package, "scaffold-sh", &xdg_root);
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");

    let store = tmp.path().join("store-sh");
    let store = store.display().to_string();
    let (status, stdout, stderr) = run_in_xdg(&["run", "scaffold-sh", "-d", &store], &xdg_root);
    assert_eq!(status.code(), Some(0), "{stderr}");
    // No `--name`: the manifest's default (`there`) reaches the walk.
    assert!(
        stdout.contains("scaffold-sh: Hello there world! — wrote 1 sample node(s)"),
        "{stdout}"
    );

    let row = support::query_single_row(
        tmp.path(),
        Path::new(&store),
        &[],
        "MATCH (n:Scaffold) RETURN count(n) AS nodes",
    );
    assert_eq!(row["nodes"], 1);

    // A flag value that is JSON-hostile still travels intact: `json_string`
    // encodes it into the arguments object and `mcp_string` decodes the
    // result, so the quote neither breaks the request nor truncates the
    // greeting.
    let quoted = tmp.path().join("store-sh-quoted");
    let quoted = quoted.display().to_string();
    let (status, stdout, stderr) = run_in_xdg(
        &["run", "scaffold-sh", "--name", "A\"B", "-d", &quoted],
        &xdg_root,
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("scaffold-sh: Hello A\"B world! — wrote 1 sample node(s)"),
        "{stdout}"
    );
}

/// The TypeScript template declares the host's MCP revision and, where the
/// lane has `node`, runs exactly like the other two — the generated glue is
/// dependency-free, so nothing is installed first.
#[cfg(feature = "extension-install")]
#[test]
fn test_the_typescript_template_loads_and_runs_where_node_is_available() {
    let tmp = tempdir();
    let package = init(tmp.path(), "scaffold-ts", Some("ts"));
    assert!(package.join("scaffold-ts.ts").is_file());
    let manifest = std::fs::read_to_string(package.join("extension.json")).unwrap();
    assert!(
        manifest.contains("\"protocol\": \"2025-06-18\""),
        "{manifest}"
    );
    assert!(
        manifest.contains("\"executable\": \"scaffold-ts.ts\""),
        "{manifest}"
    );

    let (_xdg, xdg_root) = support::extensions::xdg_root();
    let (status, stdout, stderr) = install(&package, "scaffold-ts", &xdg_root);
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");

    if !node_can_strip_types() {
        eprintln!(
            "skip: no node with type stripping on the PATH (the manifest is validated above)"
        );
        return;
    }
    let store = tmp.path().join("store-ts");
    let store = store.display().to_string();
    let (status, stdout, stderr) = run_in_xdg(&["run", "scaffold-ts", "-d", &store], &xdg_root);
    assert_eq!(status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("scaffold-ts: Hello there world! — wrote 1 sample node(s)"),
        "{stdout}"
    );
}

/// The Rust template is a crate: its manifest decodes through the registry
/// (it needs no executable to be listed), it names the binary the build
/// produces, and the source is `serve` plus the walk.
#[test]
fn test_the_rust_template_declares_a_loadable_crate_package() {
    let tmp = tempdir();
    let package = init(tmp.path(), "scaffold-rs", Some("rust"));
    for path in ["extension.json", "Cargo.toml", "src/main.rs", "README.md"] {
        assert!(package.join(path).is_file(), "{path} is missing");
    }
    let manifest = std::fs::read_to_string(package.join("extension.json")).unwrap();
    assert!(
        manifest.contains("\"executable\": \"scaffold-rs\""),
        "{manifest}"
    );
    let main = std::fs::read_to_string(package.join("src/main.rs")).unwrap();
    assert!(
        main.contains("serve(NAME, env!(\"CARGO_PKG_VERSION\"), walk)"),
        "{main}"
    );

    // The registry load is the manifest's real check: place the package
    // where the walk looks and list it. No spawn happens (the binary is the
    // author's build), so the executable's absence is not a load refusal.
    let (xdg, xdg_root) = support::extensions::xdg_root();
    let installed = support::extensions::extensions_dir(xdg.path()).join("scaffold-rs");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("extension.json"),
        std::fs::read_to_string(package.join("extension.json")).unwrap(),
    )
    .unwrap();
    let (status, stdout, stderr) = run_in_xdg(&["extension", "list"], &xdg_root);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("scaffold-rs 0.1.0 unmarked"), "{stdout}");
}

/// The usage errors: a missing name, an unknown language, a reserved word,
/// and the existing-directory refusal — exit 2 for the grammar, exit 1 for
/// the directory, nothing written.
#[test]
fn test_init_usage_errors_and_the_existing_directory_refusal() {
    let tmp = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["extension", "init"]).in_dir(tmp.path()));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("extension init requires a package name"),
        "{stderr}"
    );

    let (status, _, stderr) =
        run_cli(Opts::new(&["extension", "init", "x", "--lang", "go"]).in_dir(tmp.path()));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unknown --lang value 'go' (expected sh, py, ts or rust)"),
        "{stderr}"
    );

    let (status, _, stderr) = run_cli(Opts::new(&["extension", "init", "run"]).in_dir(tmp.path()));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("collides with the reserved command word 'run'"),
        "{stderr}"
    );

    // A name is a plain file name: a path is refused before anything is
    // written.
    let (status, _, stderr) =
        run_cli(Opts::new(&["extension", "init", "../escape"]).in_dir(tmp.path()));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("is not a valid package name"), "{stderr}");
    assert!(!tmp.path().join("..").join("escape").exists());

    // The first scaffold lands; the second refuses the directory it finds.
    let _package = init(tmp.path(), "taken", None);
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["extension", "init", "taken"]).in_dir(tmp.path()));
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(
        stderr.contains("Error: extension init: "),
        "one Error line: {stderr}"
    );
}

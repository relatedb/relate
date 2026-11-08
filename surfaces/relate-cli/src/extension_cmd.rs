//! `relate extension init|install|list|remove|check` orchestration
//! (ADR-0431 §3): the lifecycle verbs that scaffold a package, put one on
//! the machine, list what is on it (and what the signed channel offers),
//! take one off, and check a package directory against the mount's contract
//! before it is installed. `init` is dependency-free — it writes generated
//! files and nothing else ([`crate::extension_init`]); `check` is
//! dependency-free too (no store, no network); the network rides the install
//! verbs only.
//!
//! `list` and `remove` are dependency-free (the `extensions` feature):
//! they read the local registry and the provenance markers, offline,
//! never a fetch (ADR-0431 §4: the network is touched only by the
//! install verb and `list --available`). `install` and `--available`
//! ride the `extension-install` feature — the network dependencies gate
//! the install verbs only (§3).
//!
//! Exit codes follow ADR-0431 §3 (ADR-0430 §5's split): the parse's flag
//! problems and the manifest problems exit 2 through the hand-rolled
//! parser's usage-error convention; name-resolution, environment, network,
//! verification and unpack failures exit 1 through this module's error
//! returns. A build with `extensions` but without `extension-install`
//! keeps `list` and `remove` and answers `install` with the not-included
//! error for the action (§3) — one level under the command's own
//! not-included wording (ADR-0104), printed here because the action is
//! parsed but the feature is off.

use relate_formatter::sanitize_error_text;

use crate::parse_error::ParseError;

use crate::extension_init::InitSpec;
use crate::extensions::provenance::{Provenance, MARKER_FILE};
use crate::extensions::registry::ActionRegistry;
use crate::extensions::source::{is_full_commit_sha, GithubSpec, InstallArgs, InstallSource};
use crate::extensions::trust::{trust_path, TrustStore};

/// The action word the telemetry event carries (ADR-0431 §3: the `skill`
/// command's pattern — `&'static str`s, never a package name, ADR-0079
/// anonymity).
const ACTION_INIT: &str = "init";
const ACTION_INSTALL: &str = "install";
const ACTION_LIST: &str = "list";
const ACTION_REMOVE: &str = "remove";
const ACTION_CHECK: &str = "check";

/// What `relate extension` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionAction {
    /// `extension init <name> [--lang sh|py|ts|rust]` (ADR-0431 §3):
    /// scaffold a package directory from a language template. Parsed and
    /// run in every build that carries `extensions` — it needs no network
    /// and no store (ADR-0137).
    Init(InitSpec),
    /// `extension install <name> [--github REPO@TAG|@SHA] [--url URL]
    /// [--from DIR] [--version V] [--force] [--re-approve | --approve SHA]`
    /// (ADR-0431 §3, ADR-0449). Parsed in every build; the install itself
    /// rides the `extension-install` feature and answers with the
    /// not-included error in a build without it (ADR-0137).
    Install(InstallArgs),
    /// `extension list [--available | --trust]` (ADR-0431 §9, ADR-0449):
    /// the installed set, offline; with `--available` (compiled with
    /// `extension-install`) the signed channel's catalog; with `--trust`
    /// the recorded GitHub pins.
    List {
        /// `--available`: read the channel index (verified as the install
        /// reads it) and answer what exists to install.
        available: bool,
        /// `--trust`: print the recorded `--github` pins (the per-user
        /// trust store), offline.
        trust: bool,
    },
    /// `extension remove <name> [--forget-pin]` (ADR-0431 §3, ADR-0449):
    /// take one package off, optionally clearing its recorded pin.
    Remove {
        /// The mount name.
        name: String,
        /// `--forget-pin`: also clear the trust store entry — without it
        /// the pin outlives the package and a reinstall is still compared.
        forget_pin: bool,
    },
    /// `extension check <dir>` (ADR-0429 §6, ADR-0447): run a package
    /// directory through the mount's six stages — load, executable, spawn,
    /// the handshake, the tool table — without a store. Dependency-free
    /// like `list` and `remove`: no network, no store, nothing written.
    Check { dir: String },
}

impl ExtensionAction {
    /// The CLI spelling of the action (also the telemetry event value).
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ExtensionAction::Init(_) => ACTION_INIT,
            ExtensionAction::Install(_) => ACTION_INSTALL,
            ExtensionAction::List { .. } => ACTION_LIST,
            ExtensionAction::Remove { .. } => ACTION_REMOVE,
            ExtensionAction::Check { .. } => ACTION_CHECK,
        }
    }
}

/// Parse the argument tail after the `extension` verb: `init <name>
/// [--lang sh|py|ts|rust]`, `install <name> [--github REPO@TAG|@SHA]
/// [--url URL] [--from DIR] [--version V] [--force] [--re-approve |
/// --approve SHA]`, `list [--available | --trust]`, `remove <name>
/// [--forget-pin]`, `check <dir>` (ADR-0431 §3, ADR-0449 — the
/// resource-action shape: exactly one positional (the name) on `init`,
/// `install` and `remove`, one (the directory) on `check`, none on `list`;
/// the action names are closed). Pure — returns the usage-error message
/// instead of exiting, so the grammar is unit-testable; the lifecycle
/// wrapper turns `Err` into the exit-2 convention.
pub fn parse_extension_args(rest: &[String]) -> Result<ExtensionAction, ParseError> {
    let Some((action_arg, rest)) = rest.split_first() else {
        return Err("extension requires an action (init, install, list, remove or check)".into());
    };
    match action_arg.as_str() {
        ACTION_INIT => crate::extension_init::parse_init_args(rest).map(ExtensionAction::Init),
        ACTION_INSTALL => parse_install(rest),
        ACTION_LIST => parse_list(rest),
        ACTION_REMOVE => parse_remove(rest),
        ACTION_CHECK => parse_check(rest),
        other => Err(ParseError::unexpected("extension", other)),
    }
}

/// The `install` grammar: one positional (the package name) and the
/// source flags — `--github`, `--url` and `--from` are mutually exclusive
/// (a usage error naming the pair), `--version` is a usage error beside
/// any of them (the source the flag names already determines the
/// version, ADR-0431 §3), and the re-approval flags require `--github`
/// (ADR-0449). Parsed identically in every build (ADR-0137): the
/// typed [`InstallSource`](crate::extensions::install::InstallSource)
/// is derived from the checked flags at run time, in the build that
/// carries the capability — never re-matched from a nullable pair.
fn parse_install(rest: &[String]) -> Result<ExtensionAction, ParseError> {
    use crate::parse_error::{flag_once, set_once};
    let mut name: Option<String> = None;
    let mut url: Option<String> = None;
    let mut from: Option<String> = None;
    let mut version: Option<String> = None;
    let mut github: Option<GithubSpec> = None;
    let mut re_approve = false;
    let mut approve: Option<String> = None;
    let mut force = false;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--url",
                    "a value (the package tarball URL)",
                )?;
                set_once("extension install", &mut url, "--url", value.clone())?;
            }
            "--from" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--from",
                    "a value (a package directory)",
                )?;
                set_once("extension install", &mut from, "--from", value.clone())?;
            }
            "--version" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--version",
                    "a value (a channel version)",
                )?;
                set_once(
                    "extension install",
                    &mut version,
                    "--version",
                    value.clone(),
                )?;
            }
            "--github" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--github",
                    "a value (owner/repo@tag or owner/repo@<full-commit-sha>)",
                )?;
                let spec = GithubSpec::parse(value)
                    .map_err(|message| ParseError::from(format!("extension install: {message}")))?;
                if github.replace(spec).is_some() {
                    return Err("extension install --github may be given only once".into());
                }
            }
            "--re-approve" => {
                flag_once("extension install", "--re-approve", &mut re_approve)?;
            }
            "--approve" => {
                let value = crate::parse_error::flag_value(
                    &mut args,
                    "--approve",
                    "a value (the full 40-hex commit sha to re-approve)",
                )?;
                if !is_full_commit_sha(value) {
                    return Err(
                        "extension install --approve requires a full 40-hex commit sha".into(),
                    );
                }
                set_once(
                    "extension install",
                    &mut approve,
                    "--approve",
                    value.to_ascii_lowercase(),
                )?;
            }
            "--force" => {
                flag_once("extension install", "--force", &mut force)?;
            }
            other => {
                if name.is_none() {
                    name = Some(other.to_string());
                } else {
                    return Err(ParseError::unexpected("extension install", other));
                }
            }
        }
    }
    // The source flags are mutually exclusive: they both name the
    // install source (ADR-0431 §3, ADR-0449).
    if url.is_some() && from.is_some() {
        return Err(
            "extension install: --url and --from are mutually exclusive (they both name \
             the install source)"
                .into(),
        );
    }
    if github.is_some() && (url.is_some() || from.is_some()) {
        return Err(
            "extension install: --github is mutually exclusive with --url and --from \
             (each names the install source)"
                .into(),
        );
    }
    // A re-approval is a `--github` pin operation; it means nothing for
    // the other sources, which have no recorded pin.
    if (re_approve || approve.is_some()) && github.is_none() {
        return Err(
            "extension install: --re-approve and --approve <commit-sha> only apply to \
             --github installs"
                .into(),
        );
    }
    if re_approve && approve.is_some() {
        return Err(
            "extension install: --re-approve and --approve are mutually exclusive \
             (both re-approve a resolved pin)"
                .into(),
        );
    }
    // The source the flag names already determines the version
    // (ADR-0431 §3).
    if version.is_some() && (url.is_some() || from.is_some() || github.is_some()) {
        return Err(
            "extension install: --version is not valid with --url, --from or --github \
             (the named source already determines the version)"
                .into(),
        );
    }
    let Some(name) = name else {
        return Err("extension install requires a package name".into());
    };
    // The typed source, decided once here at the grammar boundary: the
    // checks above guarantee exactly one shape holds.
    let source = if let Some(url) = url {
        InstallSource::Url(url)
    } else if let Some(from) = from {
        InstallSource::From(from)
    } else if let Some(github) = github {
        InstallSource::Github(github)
    } else {
        InstallSource::Channel { version }
    };
    Ok(ExtensionAction::Install(InstallArgs {
        name,
        source,
        force,
        re_approve,
        approve,
    }))
}

/// The `list` grammar: no positional; `--available` once (ADR-0431 §9)
/// or `--trust` once (ADR-0449) — the two flags ask different questions
/// and are mutually exclusive. Parsed in every build (ADR-0137); the
/// channel listing `--available` asks for rides `extension-install` and
/// answers with the not-included error in a build without it, while
/// `--trust` is dependency-free.
fn parse_list(rest: &[String]) -> Result<ExtensionAction, ParseError> {
    use crate::parse_error::flag_once;
    let mut available = false;
    let mut trust = false;
    for arg in rest {
        match arg.as_str() {
            "--available" => flag_once("extension list", "--available", &mut available)?,
            "--trust" => flag_once("extension list", "--trust", &mut trust)?,
            other => return Err(ParseError::unexpected("extension list", other)),
        }
    }
    if available && trust {
        return Err(
            "extension list: --trust and --available ask different questions (recorded \
             pins vs the channel catalog)"
                .into(),
        );
    }
    Ok(ExtensionAction::List { available, trust })
}

/// The `remove` grammar: exactly one positional (the package name) and
/// `--forget-pin` (ADR-0449) — `remove` carries no `--force`: a foreign
/// or unknown directory is a refusal naming what it found (ADR-0431 §3).
fn parse_remove(rest: &[String]) -> Result<ExtensionAction, ParseError> {
    use crate::parse_error::flag_once;
    let mut name: Option<String> = None;
    let mut forget_pin = false;
    for arg in rest {
        match arg.as_str() {
            "--forget-pin" => flag_once("extension remove", "--forget-pin", &mut forget_pin)?,
            other if name.is_none() => name = Some(other.to_string()),
            other => return Err(ParseError::unexpected("extension remove", other)),
        }
    }
    let Some(name) = name else {
        return Err("extension remove requires a package name".into());
    };
    Ok(ExtensionAction::Remove { name, forget_pin })
}

/// The `check` grammar: exactly one positional (the package directory) and
/// no flags — the verb reads a directory and opens no store, so there is no
/// store flag to accept: `-d`/`--database` is an unexpected argument here,
/// never a path to check a package against. Parsed in every build
/// (ADR-0137); the verb itself is dependency-free, like `list` and
/// `remove`.
fn parse_check(rest: &[String]) -> Result<ExtensionAction, ParseError> {
    let mut dir: Option<String> = None;
    for arg in rest {
        if dir.is_none() && !arg.starts_with('-') {
            dir = Some(arg.clone());
        } else {
            return Err(ParseError::unexpected("extension check", arg));
        }
    }
    let Some(dir) = dir else {
        return Err("extension check requires a package directory".into());
    };
    Ok(ExtensionAction::Check { dir })
}

/// Run the parsed `extension` command and return the process exit code
/// (the pattern every lifecycle command module follows). Runtime
/// failures print one line through the shared sanitizer and exit 1;
/// the install's exit-2 class
/// ([`crate::extensions::install::error::InstallError`])
/// carries its own code.
pub fn run(action: ExtensionAction) -> i32 {
    match action {
        ExtensionAction::Init(spec) => crate::extension_init::run(spec),
        ExtensionAction::Install(spec) => run_install_action(spec),
        ExtensionAction::List { available, trust } => run_list(available, trust),
        ExtensionAction::Remove { name, forget_pin } => run_remove(&name, forget_pin),
        ExtensionAction::Check { dir } => run_check(&dir),
    }
}

/// The install itself, in the build that carries it: a straight hand-off
/// of the parse's typed [`InstallArgs`] — the grammar boundary already
/// decided the source, nothing is re-derived (or copied) here.
#[cfg(feature = "extension-install")]
fn run_install_action(args: InstallArgs) -> i32 {
    match crate::extensions::install::run_install(&args) {
        Ok(()) => 0,
        Err(err) => {
            crate::print_error(err.stderr_message());
            err.exit_code()
        }
    }
}

/// The not-included error for the action (ADR-0431 §3) in the build
/// without `extension-install`: the command is compiled in, the action's
/// feature is not — the same shape as the command-level wording
/// (ADR-0104), naming the feature that carries it.
#[cfg(not(feature = "extension-install"))]
fn run_install_action(_args: InstallArgs) -> i32 {
    crate::print_error(
        "Error: the 'extension install' action is not included in this build of \
         relate (enable the \"extension-install\" feature)",
    );
    2
}

/// `extension list` (ADR-0431 §9, ADR-0449): the installed set — name,
/// version, tier, source — from the local registry and markers: offline,
/// dependency-free, never a fetch. Zero installed extensions prints
/// nothing (exit 0). `--available` (the `extension-install` build) answers
/// the other question — what exists to install — from the verified
/// channel index, hidden packages staying hidden in no listing, and
/// `--trust` prints the recorded `--github` pins (name, repo, tag,
/// commit, asset SHA256) from the per-user trust store.
fn run_list(available: bool, trust: bool) -> i32 {
    // The channel listing rides the feature; a build without it answers
    // the parsed flag with the not-included error (ADR-0137), never a
    // silent fallback to the installed set.
    #[cfg(not(feature = "extension-install"))]
    if available {
        crate::print_error(
            "Error: the 'extension list --available' listing is not included in this build \
             of relate (enable the \"extension-install\" feature)",
        );
        return 2;
    }
    // The recorded pins are the trust store's rows — offline and
    // dependency-free in every `extensions` build (ADR-0449).
    #[cfg(feature = "extension-install")]
    let rows = if trust {
        trust_rows()
    } else {
        list_rows(available)
    };
    #[cfg(not(feature = "extension-install"))]
    let rows = if trust {
        trust_rows()
    } else {
        installed_rows()
    };
    match rows {
        Ok(rows) => {
            for row in rows {
                println!("{row}");
            }
            0
        }
        Err(err) => {
            crate::print_error(format!("Error: extension list: {err}"));
            1
        }
    }
}

/// The list's rows: the installed set, or — with `extension-install` —
/// the channel's catalog (§9).
#[cfg(feature = "extension-install")]
fn list_rows(available: bool) -> Result<Vec<String>, String> {
    if !available {
        return installed_rows();
    }
    let index =
        crate::extensions::channel::fetch_verified_index().map_err(|err| err.to_string())?;
    let registry = ActionRegistry::load().map_err(|err| err.to_string())?;
    let installed = |name: &str| registry.find(name).is_some();
    Ok(crate::extensions::channel::available_rows(
        &index, &installed,
    ))
}

/// The installed set's rows (ADR-0431 §9): the registry's load-order
/// walk, one row per package from its manifest and marker. A broken
/// installed set or a broken marker is the refusal naming the offender
/// (the registry's own pinned texts) — never a partial listing.
fn installed_rows() -> Result<Vec<String>, String> {
    let registry = ActionRegistry::load().map_err(|err| err.to_string())?;
    registry.extension_actions().map(list_row).collect()
}

/// The recorded pins' rows (ADR-0449): one line per trust-store entry —
/// name, repo, tag, commit, asset SHA256 — for `extension list --trust`.
/// Offline and dependency-free in every `extensions` build; no config dir
/// resolving is the same environment failure the install verbs answer.
fn trust_rows() -> Result<Vec<String>, String> {
    let Some(config_dir) = crate::config_dir::config_dir() else {
        return Err(format!(
            "no config directory resolves: {}",
            crate::config_dir::NO_BASE_HINT
        ));
    };
    let store = TrustStore::load(&trust_path(&config_dir)).map_err(|e| e.to_string())?;
    Ok(store
        .rows()
        .map(|(name, pin)| {
            format!(
                "{} {} {} {} {}",
                sanitize_error_text(name),
                sanitize_error_text(&pin.repo),
                sanitize_error_text(&pin.tag),
                sanitize_error_text(&pin.commit),
                sanitize_error_text(&pin.asset_sha256)
            )
        })
        .collect())
}

/// The one line `extension list` prints per installed package (ADR-0431
/// §9): name, version, tier, source — from the local registry and
/// markers, offline. An unmarked package (no marker) lists the copied-in
/// state honestly; a broken marker is the caller's refusal (the error
/// text names the file), never a silent unmarked row.
///
/// Every package-authored fragment — the manifest's name and version,
/// the marker's source — passes through the shared sanitizer on its way
/// into the row, the `print_error` doctrine applied to the listing: a
/// package can never forge a second line or control characters into the
/// verb's output.
fn list_row(entry: &crate::extensions::registry::ExtensionEntry) -> Result<String, String> {
    let command = entry.command();
    let source = match Provenance::read(&entry.package_dir).map_err(|e| e.to_string())? {
        Some(Provenance {
            tier,
            source,
            pin: _,
        }) => {
            format!("{} {}", tier.as_str(), sanitize_error_text(&source))
        }
        None => "unmarked".to_string(),
    };
    Ok(format!(
        "{} {} {}",
        sanitize_error_text(&command.name),
        sanitize_error_text(&command.version),
        source
    ))
}

/// `extension remove <name>` (ADR-0431 §3): the registry resolves the
/// name (a mount name is the one key the dispatch paths resolve,
/// ADR-0429); the marker is required — a foreign or unknown directory is
/// a refusal naming what it found, and there is no `--force`.
fn run_remove(name: &str, forget_pin: bool) -> i32 {
    match remove(name, forget_pin) {
        Ok(()) => 0,
        Err(err) => {
            crate::print_error(format!("Error: extension remove: {err}"));
            1
        }
    }
}

/// The remove flow: resolve `name` through the registry, require the
/// provenance marker, remove the package directory.
fn remove(name: &str, forget_pin: bool) -> Result<(), String> {
    let registry = ActionRegistry::load().map_err(|err| err.to_string())?;
    let Some(entry) = registry.find(name) else {
        return Err(format!(
            "no installed extension '{name}' (relate extension list prints the installed set)"
        ));
    };
    let provenance = Provenance::read(&entry.package_dir).map_err(|e| e.to_string())?;
    if provenance.is_none() {
        return Err(format!(
            "refusing to remove {}: no {} marker present (the directory was not installed by \
             relate; remove it manually)",
            entry.package_dir.display(),
            MARKER_FILE
        ));
    }
    // A pin clear is loaded before the package goes, so a malformed trust
    // store refuses while nothing has been removed yet. Without
    // --forget-pin the recorded pin outlives the package (ADR-0449: a
    // reinstall is still compared against it).
    let forget = if forget_pin {
        let Some(config_dir) = crate::config_dir::config_dir() else {
            return Err(format!(
                "no config directory resolves: {}",
                crate::config_dir::NO_BASE_HINT
            ));
        };
        let mut store = TrustStore::load(&trust_path(&config_dir)).map_err(|e| e.to_string())?;
        let cleared = store.remove(name).is_some();
        Some((store, cleared))
    } else {
        None
    };
    std::fs::remove_dir_all(&entry.package_dir)
        .map_err(|e| format!("removing {}: {e}", entry.package_dir.display()))?;
    println!("removed extension {name} ({})", entry.package_dir.display());
    if let Some((store, cleared)) = forget {
        store
            .write()
            .map_err(|e| format!("writing the extension trust store: {e}"))?;
        if cleared {
            println!("forgot the recorded pin for {}", sanitize_error_text(name));
        }
    }
    Ok(())
}

/// `extension check <dir>` (ADR-0429 §6, ADR-0447): the package check. One
/// report line per stage on stdout — the report is the verb's output, not a
/// diagnostic stream — with the failing stage's line naming the stage and
/// its refusal; the exit code is ADR-0430 §5's split for that stage's class
/// (2 for the package's declaration and contents, 1 for what it does when
/// run, 0 for a clean check). The verb is a read of the package directory:
/// it opens no store, writes nothing, and runs no tool.
fn run_check(dir: &str) -> i32 {
    use crate::extensions::check::check;
    use crate::extensions::protocol::READ_DEADLINE;
    let report = check(std::path::Path::new(dir), READ_DEADLINE);
    for line in &report.lines {
        println!("{line}");
    }
    report.exit_code()
}

#[cfg(test)]
mod tests;

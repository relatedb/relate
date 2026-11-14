//! `relate skill install|remove` orchestration: install or remove the
//! embedded relate agent-skill bundle into the detected AI agents' skill
//! directories.

use std::path::{Path, PathBuf};

use crate::parse_error::ParseError;
use crate::skill::agents;
use crate::skill::{classify_skill_dir, SkillDirState, MARKER_FILE, SKILL_NAME};

/// The embedded skill-bundle file tree. Keys are destination-relative paths
/// under `<agent skills dir>/relate/`; values are the bundle bytes, baked
/// into the binary at compile time via `include_str!` (no runtime filesystem
/// read, no extra dependency).
const BUNDLE_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("skill/bundle/SKILL.md")),
    (
        "references/pull.md",
        include_str!("skill/bundle/references/pull.md"),
    ),
    (
        "references/push.md",
        include_str!("skill/bundle/references/push.md"),
    ),
    (
        "references/run-git-to-graph.md",
        include_str!("skill/bundle/references/run-git-to-graph.md"),
    ),
    (
        "references/vector.md",
        include_str!("skill/bundle/references/vector.md"),
    ),
    (
        "references/spatial.md",
        include_str!("skill/bundle/references/spatial.md"),
    ),
    (
        "references/operations.md",
        include_str!("skill/bundle/references/operations.md"),
    ),
    (
        "references/agent-safety.md",
        include_str!("skill/bundle/references/agent-safety.md"),
    ),
    (
        "references/client-javascript.md",
        include_str!("skill/bundle/references/client-javascript.md"),
    ),
    (
        "references/client-java.md",
        include_str!("skill/bundle/references/client-java.md"),
    ),
    (
        "references/client-go.md",
        include_str!("skill/bundle/references/client-go.md"),
    ),
    (
        "references/client-wasm.md",
        include_str!("skill/bundle/references/client-wasm.md"),
    ),
    (
        "references/client-swift.md",
        include_str!("skill/bundle/references/client-swift.md"),
    ),
    (
        "references/client-python.md",
        include_str!("skill/bundle/references/client-python.md"),
    ),
    (
        "references/client-c.md",
        include_str!("skill/bundle/references/client-c.md"),
    ),
    (
        "references/client-extensions.md",
        include_str!("skill/bundle/references/client-extensions.md"),
    ),
    (
        "references/client-cli.md",
        include_str!("skill/bundle/references/client-cli.md"),
    ),
];

/// The exact binary version injected into the installed SKILL.md frontmatter,
/// so a future CLI upgrade can detect drift and re-install automatically.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `relate skill` should do. The mutating pair is carried in the
/// type (`Mutate`), so `--force` acceptance and the [`run_skill`] dispatch
/// read the rule from the variant itself — a new action cannot forget to
/// classify itself (ADR-0242).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAction {
    /// A mutating action (install or remove).
    Mutate(MutateAction),
    /// Print each target agent's install state (ADR-0242).
    List,
    /// Verify installed bundles match this binary's bundle (ADR-0242).
    Check,
}

impl SkillAction {
    /// The CLI spelling of the action (also the telemetry event value).
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillAction::Mutate(MutateAction::Install) => "install",
            SkillAction::Mutate(MutateAction::Remove) => "remove",
            SkillAction::List => "list",
            SkillAction::Check => "check",
        }
    }
}

/// The mutating `skill` actions — the only input [`run_skill`] accepts, so
/// the (preflight, apply) selection is exhaustive with no panic arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutateAction {
    /// Copy the embedded bundle into every target agent's skills dir.
    Install,
    /// Recursively remove the installed bundle from every target agent.
    Remove,
}

/// Parsed `skill` command (see [`parse_skill_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArgs {
    /// Install or remove.
    pub action: SkillAction,
    /// `--agent NAME`: target one agent (default: all detected).
    pub agent: Option<String>,
    /// `--force`: clobber an unmarked destination.
    pub force: bool,
}

/// Parse the argument tail after the `skill` verb: `install|remove [--agent
/// NAME] [--force]`, flags in any order. Pure — returns the usage-error
/// message instead of exiting, so the grammar is unit-testable; `main` turns
/// `Err` into the hand-rolled parser's exit-2 convention.
pub fn parse_skill_args(rest: &[String]) -> Result<SkillArgs, ParseError> {
    let Some((action_arg, rest)) = rest.split_first() else {
        return Err("skill requires an action (install, remove, list or check)".into());
    };
    let action = match action_arg.as_str() {
        "install" => SkillAction::Mutate(MutateAction::Install),
        "remove" => SkillAction::Mutate(MutateAction::Remove),
        "list" => SkillAction::List,
        "check" => SkillAction::Check,
        other => return Err(ParseError::unexpected("skill", other)),
    };
    let mut agent: Option<String> = None;
    let mut force = false;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--agent" => {
                agent = Some(
                    crate::parse_error::flag_value(
                        &mut args,
                        "--agent",
                        "a value (name of an AI agent)",
                    )?
                    .clone(),
                );
            }
            // --force is the clobber for a refused destination; the
            // read-only actions have nothing to clobber (ADR-0242).
            "--force" if matches!(action, SkillAction::Mutate(_)) => force = true,
            other => return Err(ParseError::unexpected("skill", other)),
        }
    }
    Ok(SkillArgs {
        action,
        agent,
        force,
    })
}

/// Run the parsed `skill` command and return the process exit code (the
/// pattern every lifecycle command module follows). One exit-code decision
/// per action (ADR-0242): the mutating actions go through [`run_skill`] with
/// the type-narrowed [`MutateAction`]; `check` reports drift as exit 1
/// without an error object. A [`SkillError`] prints its pre-sanitized,
/// multi-line-by-design [`SkillError::stderr_message`] raw (not through
/// `crate::print_error`, which would strip the `\n`).
pub fn run(args: SkillArgs) -> i32 {
    // Every arm yields the success exit code (check maps drift to 1), so the
    // single tail below owns both the error print and the code mapping.
    let result: Result<i32, SkillError> = match args.action {
        SkillAction::Mutate(mutate) => {
            run_skill(mutate, args.agent.as_deref(), args.force).map(|()| 0)
        }
        SkillAction::List => run_skill_list(args.agent.as_deref()).map(|()| 0),
        SkillAction::Check => run_skill_check(args.agent.as_deref()).map(|clean| i32::from(!clean)),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}", e.stderr_message());
            e.exit_code()
        }
    }
}

/// Error from the `skill` flow whose stderr text is user-facing.
#[derive(Debug)]
pub enum SkillError {
    /// `--agent <name>` did not match a catalog entry.
    UnknownAgent(String),
    /// No `--agent` filter and no supported agent detected on the host.
    NoAgentsDetected,
    /// The destination exists but carries no `.relate-installed` marker,
    /// so it may be user-authored content relate must not touch without
    /// `--force`.
    NotOwned(std::path::PathBuf),
    /// A filesystem operation failed while installing/removing.
    FileSystem(String),
}

impl SkillError {
    /// The exact text `main` writes to stderr before `exit(1)`. Two variants
    /// are multi-line by design (`...\nvalid agents: ...`), so `main` prints
    /// this raw instead of routing it through `print_error` (which strips
    /// `\n`); every interpolated value is therefore pre-sanitized here with
    /// the shared strip, and only the static skeleton reaches the terminal
    /// unsanitized.
    pub fn stderr_message(&self) -> String {
        match self {
            SkillError::UnknownAgent(name) => format!(
                "Error: skill: unknown agent: '{}'\nvalid agents: {}",
                relate_formatter::strip_unsafe_output_chars(name),
                agents::agent_names().join(", ")
            ),
            SkillError::NoAgentsDetected => format!(
                "Error: skill: no supported agents detected\nvalid agents: {}",
                agents::agent_names().join(", ")
            ),
            SkillError::NotOwned(path) => format!(
                "Error: skill: refusing to touch {}: no {} marker present \
                 (directory was not created by relate); pass --force to override",
                relate_formatter::strip_unsafe_output_chars(&path.display().to_string()),
                MARKER_FILE
            ),
            SkillError::FileSystem(e) => format!(
                "Error: skill: {}",
                relate_formatter::strip_unsafe_output_chars(e)
            ),
        }
    }

    /// The process exit code for this error (ADR-0242): an unknown `--agent`
    /// name is a usage error (2); everything else is a runtime failure (1).
    /// The variant→code classification lives here, next to the variants, so
    /// the dispatch in [`run`] has no error-mapping logic of its own.
    pub fn exit_code(&self) -> i32 {
        match self {
            SkillError::UnknownAgent(_) => 2,
            SkillError::NoAgentsDetected | SkillError::NotOwned(_) | SkillError::FileSystem(_) => 1,
        }
    }
}

/// The `skillName` skill is named `SKILL_NAME`; write it into `<skills dir>`.
fn install_dir_for(skills_dir: &Path) -> std::path::PathBuf {
    skills_dir.join(SKILL_NAME)
}

/// Resolve the target agents for an install/remove, honoring `agent_filter`.
///
/// - `Some(name)` → case-insensitive catalog lookup; unknown name is an error.
/// - `None` → every detected agent; zero detected is an error.
fn resolve_targets(agent_filter: Option<&str>) -> Result<Vec<&'static agents::Agent>, SkillError> {
    match agent_filter {
        Some(name) => match agents::find_agent(name) {
            Some(a) => Ok(vec![a]),
            None => Err(SkillError::UnknownAgent(name.to_string())),
        },
        None => {
            let detected = agents::detect_agents();
            if detected.is_empty() {
                Err(SkillError::NoAgentsDetected)
            } else {
                Ok(detected)
            }
        }
    }
}

/// Resolve every target's skills dir up front, so a resolution failure
/// cannot leave an earlier target half-mutated (install/remove) or
/// half-reported (list/check).
fn resolve_dirs(
    agent_filter: Option<&str>,
) -> Result<Vec<(&'static agents::Agent, PathBuf)>, SkillError> {
    resolve_targets(agent_filter)?
        .into_iter()
        .map(|agent| {
            let skills_dir = agent.skills_path().ok_or_else(|| {
                SkillError::FileSystem(format!("cannot resolve skills path for {}", agent.name))
            })?;
            Ok((agent, skills_dir))
        })
        .collect()
}

/// Run the `skill` install/remove flow. The caller owns the actual
/// `std::process::exit`. Takes the mutating-action subset only — list and
/// check are unrepresentable here (ADR-0242).
pub fn run_skill(
    action: MutateAction,
    agent_filter: Option<&str>,
    force: bool,
) -> Result<(), SkillError> {
    let resolved = resolve_dirs(agent_filter)?;

    // Select the (preflight check, apply) pair once: install adopts a
    // recognizable bundle, remove stays strict (ADR-0262).
    type Op = fn(&Path, bool) -> Result<(), SkillError>;
    let (check, apply): (Op, Op) = match action {
        MutateAction::Install => (ensure_installable, install_bundle),
        MutateAction::Remove => (ensure_removable, remove_bundle),
    };

    // Pre-flight the (pure) ownership check for *all* targets before mutating
    // any. Without this, a NotOwned refusal on a later target would leave an
    // earlier multi-agent install/remove half-applied.
    for (_, skills_dir) in &resolved {
        let dest = install_dir_for(skills_dir);
        check(&dest, force)?;
    }

    for (_, skills_dir) in &resolved {
        apply(skills_dir, force)?;
    }
    Ok(())
}

/// One `<agent>: <state> (<dir>)` line, the shared wording of
/// `skill list` and `skill check` (ADR-0242).
fn print_state_line(name: &str, state: crate::skill::SkillDirState, dest: &Path) {
    println!("{}: {} ({})", name, state.label(), dest.display());
}

/// `skill list` (ADR-0242): one line per target agent — name, install state
/// per the shared classifier's [`SkillDirState::label`], and the bundle
/// path. Read-only.
pub fn run_skill_list(agent_filter: Option<&str>) -> Result<(), SkillError> {
    for (agent, skills_dir) in resolve_dirs(agent_filter)? {
        let dest = install_dir_for(&skills_dir);
        let state = classify_skill_dir(&dest);
        print_state_line(agent.name, state, &dest);
    }
    Ok(())
}

/// `skill check` (ADR-0242): drift detection. Every installed bundle
/// (`Owned` or `Legacy`) is compared against the bundle embedded in this
/// binary and any missing, differing, or extra file is reported. Returns
/// `Ok(true)` when every installed bundle is clean and `Ok(false)` on any
/// drift; a missing or foreign destination is reported (with the shared
/// state label) but is not drift. Read-only.
pub fn run_skill_check(agent_filter: Option<&str>) -> Result<bool, SkillError> {
    let mut clean = true;
    for (agent, skills_dir) in resolve_dirs(agent_filter)? {
        let dest = install_dir_for(&skills_dir);
        let state = classify_skill_dir(&dest);
        match state {
            SkillDirState::Missing | SkillDirState::Foreign => {
                print_state_line(agent.name, state, &dest)
            }
            SkillDirState::Owned | SkillDirState::Legacy => {
                let drift = bundle_drift(&dest)?;
                if drift.is_empty() {
                    println!("{}: up to date ({})", agent.name, dest.display());
                } else {
                    clean = false;
                    println!("{}: drifted ({})", agent.name, dest.display());
                    for line in drift {
                        println!("  {line}");
                    }
                }
            }
        }
    }
    Ok(clean)
}

/// The on-disk bytes of one bundle file: `SKILL.md` gets the running
/// binary's version stamped into its frontmatter, every other file is
/// verbatim. The one rule shared by the install writer and the drift
/// checker, so `skill check` can never disagree with `skill install`.
fn bundle_file_bytes(rel: &str, contents: &str) -> Vec<u8> {
    if rel == "SKILL.md" {
        inject_version(contents, VERSION)
    } else {
        contents.as_bytes().to_vec()
    }
}

/// The drift lines for one installed bundle: `missing: <rel>`,
/// `differs: <rel>`, `extra: <rel>`. The extras walk is
/// [`walk_rel_file_paths`], whose no-symlink-follow policy and
/// fail-on-unreadable rule apply here.
fn bundle_drift(dest: &Path) -> Result<Vec<String>, SkillError> {
    let mut drift = Vec::new();
    for (rel, contents) in BUNDLE_FILES {
        let expected = bundle_file_bytes(rel, contents);
        let path = dest.join(rel);
        match std::fs::read(&path) {
            Ok(actual) if actual == expected => {}
            Ok(_) => drift.push(format!("differs: {rel}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                drift.push(format!("missing: {rel}"))
            }
            Err(e) => {
                return Err(SkillError::FileSystem(format!(
                    "reading {}: {}",
                    path.display(),
                    e
                )))
            }
        }
    }
    // Extra files: anything under `dest` that is neither a bundle file nor
    // the ownership marker.
    for rel in walk_rel_file_paths(dest)? {
        if rel != MARKER_FILE && !BUNDLE_FILES.iter().any(|(f, _)| *f == rel) {
            drift.push(format!("extra: {rel}"));
        }
    }
    Ok(drift)
}

/// Every regular file under `root`, as a `/`-separated path relative to
/// `root`, from one stack-based walk. Directory entries are classified by
/// `file_type` (no symlink following): a symlinked entry is a file, so a
/// symlink cycle cannot loop the walk. An unreadable entry is a filesystem
/// error, never a silent pass — a check that under-reports is worse than one
/// that fails. The one traversal shared by [`bundle_drift`] and the
/// drift-guard test, so both apply the same symlink policy.
fn walk_rel_file_paths(root: &Path) -> Result<Vec<String>, SkillError> {
    let mut rel_paths = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| SkillError::FileSystem(format!("reading {}: {}", dir.display(), e)))?;
        for entry in entries {
            let entry = entry
                .map_err(|e| SkillError::FileSystem(format!("reading {}: {}", dir.display(), e)))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| {
                SkillError::FileSystem(format!("reading {}: {}", path.display(), e))
            })?;
            if kind.is_dir() {
                stack.push(path);
                continue;
            }
            rel_paths.push(
                path.strip_prefix(root)
                    .expect("walked entries are under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(rel_paths)
}

/// Install-side ownership boundary (ADR-0262): install **adopts** a
/// recognizable but unmarked relate bundle (`Legacy`) without `--force`.
///
/// The shared classifier (ADR-0261 / ADR-0262) and its policy method
/// [`SkillDirState::permits_install`](crate::skill::SkillDirState::permits_install)
/// are the single source of truth, so this
/// cannot drift from `upgrade`'s recognition: `Missing`, `Owned`, and
/// `Legacy` are installable, and `Foreign` — including an uninspectable
/// path — is refused without `--force` (fail closed: the directory is
/// user-authored/third-party content relate must not touch).
fn ensure_installable(dest: &Path, force: bool) -> Result<(), SkillError> {
    if force || classify_skill_dir(dest).permits_install() {
        Ok(())
    } else {
        Err(SkillError::NotOwned(dest.to_path_buf()))
    }
}

/// Remove-side ownership boundary (ADR-0262): remove stays **strict** — only
/// relate's marker (or `--force`) permits deleting the directory.
///
/// Same classifier, one state narrower than [`ensure_installable`] via
/// [`SkillDirState::permits_remove`](crate::skill::SkillDirState::permits_remove):
/// a `Legacy` bundle — recognizable yet
/// unmarked — is refused alongside `Foreign`, because deleting a directory
/// relate never marked is irreversible. That single-state difference is
/// the ADR-0262 install-adopts/remove-strict asymmetry.
fn ensure_removable(dest: &Path, force: bool) -> Result<(), SkillError> {
    if force || classify_skill_dir(dest).permits_remove() {
        Ok(())
    } else {
        Err(SkillError::NotOwned(dest.to_path_buf()))
    }
}

/// Install (or update) the embedded bundle into `skills_dir`, stamping the
/// binary version into the SKILL.md frontmatter. The previous bundle is
/// replaced whole, so removed reference files never linger.
///
/// The bundle is written into a unique sibling staging directory and
/// [`land_replacing`] moves the previous install aside before renaming the
/// staged bundle into place: a concurrent reader never observes a
/// half-written bundle, and a failed landing restores the previous install
/// instead of leaving the destination uninstalled.
///
/// If the destination already exists without relate's ownership marker it
/// is refused (user-authored content) unless `force` is set — or, adopting
/// ADR-0262, unless it is a recognizable relate bundle.
fn install_bundle(skills_dir: &Path, force: bool) -> Result<(), SkillError> {
    let dest = install_dir_for(skills_dir);
    ensure_installable(&dest, force)?;

    // The agent's skills dir may not exist yet; the staging dir is created with
    // `create_dir` so a pre-existing (collision) name is an error rather than
    // silently reused.
    std::fs::create_dir_all(skills_dir)
        .map_err(|e| SkillError::FileSystem(format!("creating {}: {}", skills_dir.display(), e)))?;

    // The one staging sweep and sibling (the shared staging convention,
    // ADR-0261 generalized by ADR-0431 §4 step 4): ours by naming
    // convention; individual removal errors are ignored.
    crate::staging::sweep_stale(skills_dir);

    let staging = crate::staging::staging_dir(skills_dir);
    std::fs::create_dir(&staging)
        .map_err(|e| SkillError::FileSystem(format!("creating {}: {}", staging.display(), e)))?;

    // Stage → write → land in one closure so every error path funnels into
    // the single best-effort staging cleanup below.
    let staged = (|| -> Result<(), SkillError> {
        write_bundle(&staging)?;
        land_replacing(&dest, &staging, skills_dir)
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    println!("installed relate skill into {}", dest.display());
    Ok(())
}

/// Land the staged bundle at `dest`, parking any previous install aside
/// first — the park-aside replace policy the extensions installer and this
/// install share through [`crate::staging::land_parked`] (ADR-0431 §4
/// step 4): a failed landing restores the previous bundle whole rather
/// than deleting it before the rename, and the parked copy is swept
/// best-effort once the new bundle has landed (a sweep failure must not
/// fail a successful install; the next install's
/// [`crate::staging::sweep_stale`] reclaims it once it ages).
///
/// A brief window remains in which `dest` does not exist at all, between
/// parking the old bundle and landing the new one — POSIX cannot rename
/// over a non-empty directory. The replace is crash-recoverable and never
/// half-written, but a concurrent reader in that window sees no bundle,
/// not the old one.
fn land_replacing(dest: &Path, staging: &Path, skills_dir: &Path) -> Result<(), SkillError> {
    let rename_error = |from: &Path, to: &Path, e: &std::io::Error| {
        SkillError::FileSystem(format!(
            "renaming {} to {}: {}",
            from.display(),
            to.display(),
            e
        ))
    };
    if !dest.exists() {
        return std::fs::rename(staging, dest).map_err(|e| rename_error(staging, dest, &e));
    }
    let aside = crate::staging::replaced_dir(skills_dir);
    std::fs::rename(dest, &aside).map_err(|e| rename_error(dest, &aside, &e))?;
    crate::staging::land_parked(
        dest,
        staging,
        &aside,
        |e| rename_error(staging, dest, &e),
        |e, restore| {
            SkillError::FileSystem(format!(
                "renaming {} to {}: {}; restoring the previous install from {} failed too \
                 ({}): the previous install is stranded there, recover it manually",
                staging.display(),
                dest.display(),
                e,
                aside.display(),
                restore
            ))
        },
    )
}

/// Write the embedded bundle into `dir`, stamping the binary version into the
/// SKILL.md frontmatter and writing the ownership marker last: only a fully
/// written bundle is marked as relate-owned, so a half-finished install
/// never claims a directory it may not later be allowed to clean up.
fn write_bundle(dir: &Path) -> Result<(), SkillError> {
    for (rel_path, contents) in BUNDLE_FILES {
        let dst = dir.join(rel_path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SkillError::FileSystem(format!("creating {}: {}", parent.display(), e))
            })?;
        }
        let data = bundle_file_bytes(rel_path, contents);
        std::fs::write(&dst, data)
            .map_err(|e| SkillError::FileSystem(format!("writing {}: {}", dst.display(), e)))?;
    }
    let marker = dir.join(MARKER_FILE);
    std::fs::write(&marker, b"")
        .map_err(|e| SkillError::FileSystem(format!("writing {}: {}", marker.display(), e)))?;
    Ok(())
}

/// Recursively remove the installed bundle from `skills_dir`. Idempotent: a
/// missing install dir is `Ok`, matching neo4j-cli's remove semantics.
///
/// If the destination exists without relate's ownership marker it is
/// refused (user-authored content) unless `force` is set.
fn remove_bundle(skills_dir: &Path, force: bool) -> Result<(), SkillError> {
    let dest = install_dir_for(skills_dir);
    ensure_removable(&dest, force)?;
    remove_dir_all(&dest)?;
    println!("removed relate skill from {}", dest.display());
    Ok(())
}

/// remove_dir_all that treats a missing path as success (idempotent).
fn remove_dir_all(path: &Path) -> Result<(), SkillError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SkillError::FileSystem(format!(
            "removing {}: {}",
            path.display(),
            e
        ))),
    }
}

/// Replace the SKILL.md frontmatter `version:` line with `version`, inserting
/// it just before the closing `---` when absent. Mirrors neo4j-cli's
/// `injectVersion`: if the body has no YAML frontmatter block, it is returned
/// unchanged.
///
/// Tolerates both LF and CRLF line endings: on Windows CI, `core.autocrlf`
/// can convert the embedded bundle to CRLF at checkout, so `include_str!`
/// bakes in `\r\n`. Without this, the `---\n` prefix match would fail and the
/// version would never be stamped.
fn inject_version(contents: &str, version: &str) -> Vec<u8> {
    // Frontmatter is a leading block between `---` fences. Detect the line
    // ending in use so the fence/line matching is ending-agnostic.
    let body_after_open = contents
        .strip_prefix("---\n")
        .map(|b| (b, "\n"))
        .or_else(|| contents.strip_prefix("---\r\n").map(|b| (b, "\r\n")));
    let Some((body, eol)) = body_after_open else {
        return contents.as_bytes().to_vec();
    };
    let close_marker = format!("{eol}---");
    let Some(close) = body.find(&close_marker) else {
        return contents.as_bytes().to_vec();
    };
    let inner = &body[..close];

    let version_line = format!("version: {version}");
    let new_inner = if let Some(start) = inner.find("version:") {
        // Replace up to the end of that line (old value, incl. `{{VERSION}}`),
        // preserving the line ending that followed it.
        let rest = &inner[start..];
        let end = rest.find(eol).map(|i| start + i).unwrap_or(inner.len());
        format!("{}{}{}", &inner[..start], version_line, &inner[end..])
    } else {
        // No version line: append before the closing fence, using the
        // detected line ending.
        let trimmed = inner.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            version_line.clone()
        } else {
            format!("{trimmed}{eol}{version_line}")
        }
    };

    let mut out = String::with_capacity(contents.len() + version_line.len());
    out.push_str("---");
    out.push_str(eol);
    out.push_str(&new_inner);
    out.push_str(&body[close..]);
    out.into_bytes()
}

#[cfg(test)]
mod tests;

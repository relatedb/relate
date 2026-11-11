//! The action's help text, rendered from the manifest (ADR-0429 §5): the
//! synopsis its usage errors print, and the `run <name> --help` page.

use relate_formatter::sanitize_error_text;

use crate::extensions::manifest::FlagSpec;
use crate::extensions::registry::ExtensionEntry;

// ---------------------------------------------------------------------
// The help text (ADR-0429 §5): rendered from the table
// ---------------------------------------------------------------------

/// One flag-table entry's `--name` token with its kind's value
/// placeholder — the spelling the usage line and the help page share.
fn flag_token(spec: &FlagSpec) -> String {
    match spec {
        FlagSpec::String(_) | FlagSpec::Repeatable(_) => format!("--{} VALUE", spec.name()),
        FlagSpec::Integer(_) => format!("--{} N", spec.name()),
        FlagSpec::Boolean(_) => format!("--{}", spec.name()),
    }
}

/// The action's `Usage:` line — the synopsis its help page opens with and
/// its usage errors print, one line so the two cannot drift (the
/// `HelpScope::usage` role for a dynamic action). Every manifest-authored
/// fragment is sanitized where it is placed, so the line is safe to print
/// raw the way the static commands' synopses are.
pub(crate) fn action_usage(entry: &ExtensionEntry) -> String {
    let command = entry.command();
    let mut usage = format!(
        "Usage: relate run {}",
        sanitize_error_text(entry.mount_name())
    );
    if let Some(declared) = &command.positional {
        usage.push_str(&format!(" [{}]", sanitize_error_text(&declared.name)));
    }
    for spec in &command.flags {
        usage.push_str(&format!(" [{}]", sanitize_error_text(&flag_token(spec))));
    }
    usage
}

/// The action's help page (ADR-0429 §5): the manifest's own data rendered
/// as the CLI's help conventions — the synopsis, the positional, the
/// shared `-d`/`--database` block every import grammar documents, and one
/// entry per flag with its kind and its default. `run <name> --help`
/// prints it (the verb mount, LABS-552); this is the renderer.
///
/// Every manifest-authored fragment — the mount name, the version, flag
/// names, defaults, the positional's name — is sanitized where it is
/// placed, so a page renders what the table says on the lines the host
/// structured: a package author cannot forge a second line, ANSI, or
/// control characters into the host's output (ADR-0430 §5's rule,
/// applied to the page the way `print_error` applies it to errors).
pub(crate) fn action_help(entry: &ExtensionEntry) -> String {
    let command = entry.command();
    let mut page = format!("{}\n\n", action_usage(entry));
    page.push_str(&sanitize_error_text(&format!(
        "Run the installed extension '{}' (v{}) as a batched import into a store the host owns.",
        entry.mount_name(),
        sanitize_error_text(&command.version)
    )));
    page.push('\n');
    page.push_str("The child walks its source and drives the import as an MCP client; the\n");
    page.push_str("host alone owns the store, the caps and the exit codes.\n\n");
    if let Some(declared) = &command.positional {
        page.push_str(&help_entry(
            &sanitize_error_text(&declared.name),
            "The action's positional (at most one)",
        ));
    }
    for line in crate::help::DATABASE_FLAG_HELP {
        page.push_str(line);
        page.push('\n');
    }
    for spec in &command.flags {
        page.push_str(&flag_help_entry(spec));
    }
    if command.requires_fresh_store {
        page.push_str(
            "\nThis action requires a fresh store: a target that already holds nodes is\nrefused before the child runs.\n",
        );
    }
    page
}

/// One help entry line: the two-space indent, the token padded to the
/// 21-column field, the description (LABS-421's layout).
fn help_entry(token: &str, description: &str) -> String {
    format!("  {token:<21} {description}\n")
}

/// One flag-table entry's help line: its `--name` token and its kind's
/// description with the manifest's default stated (the typed default,
/// sanitized — it is package data).
fn flag_help_entry(spec: &FlagSpec) -> String {
    let token = sanitize_error_text(&flag_token(spec));
    match spec {
        FlagSpec::String(declared) => help_entry(
            &token,
            &format!(
                "A string flag (default '{}')",
                sanitize_error_text(&declared.default)
            ),
        ),
        FlagSpec::Integer(declared) => help_entry(
            &token,
            &format!("An integer flag (default {})", declared.default),
        ),
        FlagSpec::Boolean(declared) => help_entry(
            &token,
            &format!("A boolean flag (default {})", declared.default),
        ),
        FlagSpec::Repeatable(declared) => {
            let default = if declared.default.is_empty() {
                "none".to_string()
            } else {
                declared
                    .default
                    .iter()
                    .map(|value| format!("'{}'", sanitize_error_text(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            help_entry(
                &token,
                &format!("A repeatable string flag (default {default})"),
            )
        }
    }
}

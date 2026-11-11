//! The `serve` command help page (ADR-0232): the shared synopsis plus the
//! surface flags compiled into this build.

/// One network surface's `serve` segments: the synopsis segment, the
/// suggestion flags, and the help rows — stated once per surface, so
/// `serve_usage`, `serve_flags`, and `serve_help` all render from one
/// descriptor instead of three hand-maintained, per-site cfg-gated
/// enumerations (LABS-405).
struct ServeSurface {
    /// The synopsis segment(s) this surface adds.
    usage: &'static str,
    /// The flags this surface contributes to the suggestion list.
    flags: &'static [&'static str],
    /// The help-row block, ending without a newline — `serve_help` owns
    /// the separators.
    rows: &'static str,
}

#[cfg(feature = "http")]
const HTTP: ServeSurface = ServeSurface {
    usage: " [--http PORT] [--origin ORIGIN]",
    flags: &[
        "--http",
        "--origin",
        "--tx-idle-timeout-secs",
        "--max-open-transactions",
    ],
    rows: "  --http PORT    HTTP Query API v2 endpoint (POST /db/<name>/query/v2; plus the
                       public GET / discovery document) on --host:PORT
  --origin ORIGIN  Allow a browser Origin on the HTTP surface (repeatable;
                   an exact URL or * for any). Default: loopback origins plus
                   https://browser.neo4j.io and https://console.neo4j.io
  --tx-idle-timeout-secs N
                   Expire an HTTP managed transaction after N seconds idle; the
                   next registry operation sweeps it. Default:
                   RELATE_TX_IDLE_TIMEOUT_SECS, else 60. 0 disables expiry
  --max-open-transactions N
                   Cap on open HTTP managed transactions. Over cap
                   is a typed 429 naming this flag. Default:
                   RELATE_MAX_OPEN_TRANSACTIONS, else 128. 0 is unlimited",
};

#[cfg(feature = "mcp")]
const MCP: ServeSurface = ServeSurface {
    usage: " [--mcp PORT|stdio]",
    flags: &["--mcp"],
    rows: "  --mcp PORT|stdio
                   MCP server: PORT serves the streamable HTTP transport
                   on --host:PORT; the `stdio` keyword serves the stdio
                   transport in the foreground (newline-delimited JSON-RPC
                   on stdin/stdout, for MCP hosts) until the host closes
                   stdin — no auth challenge",
};

#[cfg(feature = "bolt")]
const BOLT: ServeSurface = ServeSurface {
    usage: " [--bolt PORT [--bolt-identity STRING] [--ws-allow-origin ORIGIN] [--ws-max-connections N]]",
    flags: &[
        "--bolt",
        "--bolt-identity",
        "--ws-allow-origin",
        "--ws-max-connections",
    ],
    rows: "  --bolt PORT    Bolt server (Bolt + Bolt-over-WebSocket) on --host:PORT.
                   WebSocket Origins default to loopback (any port) plus the
                   hosted Neo4j Browser and Neo4j Console; another Origin
                   needs --ws-allow-origin
  --ws-allow-origin ORIGIN
                   Allow this Origin on Bolt-over-WebSocket upgrades
                   (repeatable; an exact http(s) URL or * for any). Overrides
                   the default: loopback plus https://browser.neo4j.io and
                   https://console.neo4j.io with auth on, loopback only with
                   --auth none
  --bolt-identity STRING
                   HELLO `server` agent string for the Bolt surface.
                   Default: RELATE_IDENTITY, else
                   Relate/<version>. Sent verbatim — a Neo4j-shaped
                   value (Neo4j/5.26.0) admits strict tooling like
                   cypher-shell without claiming feature parity
  --ws-max-connections N
                   Opt-in cap on concurrent WebSocket sessions.
                   Default: RELATE_WS_MAX_CONNECTIONS, else unlimited —
                   bulk data transfers legitimately run many sessions. The
                   process-wide --max-connections cap still applies",
};

/// The surfaces compiled into this build, in help order.
const SURFACES: &[ServeSurface] = &[
    #[cfg(feature = "http")]
    HTTP,
    #[cfg(feature = "mcp")]
    MCP,
    #[cfg(feature = "bolt")]
    BOLT,
];

/// The base pair every serve build shares: the store and the bind host,
/// stated once — both consumers seed from it.
const BASE_SEGMENT: (&str, &[&str]) = (" --database PATH [--host HOST]", &["--database", "--host"]);

/// One build-independent command segment: its synopsis segment (empty for
/// flags with no synopsis spelling of their own, like the session caps), the
/// flags it contributes to the suggestion list, and its help rows. Every
/// consumer — [`serve_usage`], [`serve_flags`], [`serve_help`] — renders
/// from this one table, so a common row is written once, beside its flags,
/// not spliced into the help template.
struct CommonSegment {
    /// The synopsis segment(s) this group adds.
    usage: &'static str,
    /// The flags this group contributes to the suggestion list.
    flags: &'static [&'static str],
    /// The help-row block, ending without a newline — the renderer owns the
    /// separator.
    rows: String,
}

/// The build-independent segments in help order. Built per call because the
/// deadline rows interpolate their defaults from the enforced constants;
/// the access flag is one group — the segment renders the once-only policy
/// once (ADR-0480: one flag, `--read-only[=snapshot|live]`).
fn common_segments() -> Vec<CommonSegment> {
    let mut segments = Vec::new();
    segments.push(CommonSegment {
        usage: " [--read-only[=FRESHNESS]]",
        flags: &["--read-only"],
        rows: "  --read-only[=FRESHNESS]  Open the existing store read-only:
                   no writer lease, never creates it; every
                   write fails. FRESHNESS is snapshot (default: a frozen
                   snapshot at open) or live: tail the WAL and
                   follow the writer's commits; refuses .rl and :memory:.
                   Attach the value; once per run"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: " [--load SOURCE]",
        flags: &["--load"],
        rows: "  --load SOURCE   Load the served store's content from SOURCE — a
                   .rl bundle, a store directory, or an https URL — into
                   --database. A :memory:
                   target (the default) turns the source into the
                   in-memory store: read-only, nothing held open or
                   locked, a fetched copy released after the load. A path
                   target creates the store there (the path must not
                   exist) and opens it writable by default"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: " [--auth USER:PASSWORD]",
        flags: &["--auth"],
        rows: "  --auth USER:PASSWORD  Basic-auth credential shared by every served
                       surface; `none` disables auth. Default:
                       RELATE_AUTH, else a generated token printed at
                       startup (persisted as <dir>.auth-token, 0600)"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: "",
        flags: &["--max-connections"],
        rows: format!(
            "  --max-connections N  Process-wide session cap shared by every surface.
                       Over cap is accept-then-close. Default:
                       RELATE_MAX_CONNECTIONS, else {}",
            relate_serve::DEFAULT_MAX_CONNECTIONS
        ),
    });
    // The HTTP/MCP-only admission deadlines: neither has a meaning for
    // Bolt's handshake (ADR-0141 owns that bar), so the rows compile with
    // the surfaces that read them.
    #[cfg(any(feature = "http", feature = "mcp"))]
    segments.push(CommonSegment {
        usage: "",
        flags: &["--header-deadline-secs", "--body-deadline-secs"],
        rows: format!(
            "  --header-deadline-secs N
                   Absolute deadline for a complete HTTP/MCP request header
                   block. Trickled headers are dropped at the
                   deadline. Default: RELATE_HEADER_DEADLINE_SECS, else {}. 0 disables the deadline
  --body-deadline-secs N
                   Absolute deadline for a complete HTTP/MCP request body.
                   Trickled bodies are dropped at the deadline.
                   Default: RELATE_BODY_DEADLINE_SECS, else {}. 0 disables the deadline",
            relate_serve::DEFAULT_HEADER_DEADLINE.as_secs(),
            relate_serve::DEFAULT_BODY_DEADLINE.as_secs()
        ),
    });
    segments.push(CommonSegment {
        usage: " [--concurrent-logical-workspace-limit SIZE]",
        flags: &["--concurrent-logical-workspace-limit"],
        rows: "  --concurrent-logical-workspace-limit SIZE
                       Admit queries against a shared logical-workspace
                       pool (bytes or mb/mib/gb/gib). A positive
                       value turns admission on. Omit, or an unset
                       RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT, leaves
                       it off. 0 is rejected. Same resolution as the query
                       session's flag"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: " [--max-traversal-depth N]",
        flags: &["--max-traversal-depth"],
        // The bounds render from the engine's own constants — the row can
        // never drift from the admission limit it documents.
        rows: format!(
            "  --max-traversal-depth N
                       Largest explicit traversal hop bound a served
                       statement may ask for (1 through {}). Default:
                       RELATE_MAX_TRAVERSAL_DEPTH, else {}. An *1..N or
                       {{1,N}} bound above it fails at admission with
                       TraversalDepthExceeded",
            relate::MAX_TRAVERSAL_DEPTH_CEILING,
            relate::MAX_TRAVERSAL_DEPTH
        ),
    });
    segments.push(CommonSegment {
        usage: " [--expression-memory-limit SIZE]",
        flags: &["--expression-memory-limit"],
        rows: format!(
            "  --expression-memory-limit SIZE
                       Per-value expression build cap for the served store
                       (bytes, mb/mib/gb/gib, or none). Default:
                       {} MiB, or raise/lower it here; none disables the
                       cap for known bulk pipelines",
            relate::EXPRESSION_BUDGET_BYTES >> 20
        ),
    });
    segments.push(CommonSegment {
        usage: " [--safe | --no-safe]",
        flags: &["--safe", "--no-safe"],
        rows: "  --safe, --no-safe
                       Refuse agent-typical dangerous statement shapes on
                       the served store. Default:
                       RELATE_SAFE_MODE=1/0, else off. --no-safe
                       negates an enabled environment default"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: " [--logical-graph-size-limit SIZE]",
        flags: &["--logical-graph-size-limit"],
        rows: "  --logical-graph-size-limit SIZE
                       Live graph + index cap for the served store (bytes
                       or mb/mib/gb/gib). Default:
                       RELATE_LOGICAL_GRAPH_SIZE_LIMIT, else unlimited —
                       the same resolution as the query session's flag"
            .to_string(),
    });
    segments.push(CommonSegment {
        usage: " [--durability-mode MODE]",
        flags: &["--durability-mode"],
        // The default renders from the engine's own `Default`, so the
        // help cannot drift from the resolution it documents.
        rows: format!(
            "  --durability-mode MODE
                       Commit durability policy for the served store:
                       full, everysecond, or snapshot. Default:
                       RELATE_DURABILITY_MODE, else {} — the same
                       resolution as the query session's flag",
            relate::DurabilityMode::default()
        ),
    });
    // The values render from the one vocabulary module (the parser's
    // own), so the help cannot drift from what the parser accepts.
    segments.push(CommonSegment {
        usage: " [--snapshot-format F]",
        flags: &["--snapshot-format"],
        rows: format!(
            "  --snapshot-format F
                       Snapshot format for the served store ({f}).
                       Default: RELATE_SNAPSHOT_FORMAT (a codec name), else
                       the build default — the same grammar as the query
                       session's flag",
            f = crate::flags::codec_names("|"),
        ),
    });
    segments
}

/// The `serve` synopsis, shared by its help page and its usage errors,
/// rendered from the surface table plus the build-independent segments.
pub(super) fn serve_usage() -> String {
    let mut usage = String::from("Usage: relate serve");
    usage.push_str(BASE_SEGMENT.0);
    for surface in SURFACES {
        usage.push_str(surface.usage);
    }
    for segment in common_segments() {
        usage.push_str(segment.usage);
    }
    usage
}

/// Flags the `serve` command accepts for suggestions: the base pair, the
/// surface flags compiled into this build, and every common segment's flags
/// — the access pair and `--auth` (ADR-0232, ADR-0306; accepted in every
/// serve build, so not per-surface gated), `--max-connections` (ADR-0333),
/// the admission deadlines (ADR-0451, compiled with the HTTP-based
/// surfaces), `--concurrent-logical-workspace-limit` (ADR-0452),
/// `--max-traversal-depth` (LABS-610), `--safe`/`--no-safe` (LABS-680),
/// `--logical-graph-size-limit` (ADR-0390), and `--durability-mode`
/// (ADR-0470). Plainly built per call — the
/// spec's `flags` field is a `Cow` for exactly this (as `usage` already
/// is).
pub(super) fn serve_flags() -> Vec<&'static str> {
    let mut flags = Vec::from(BASE_SEGMENT.1);
    flags.extend(SURFACES.iter().flat_map(|surface| surface.flags));
    flags.extend(
        common_segments()
            .iter()
            .flat_map(|segment| segment.flags.iter().copied()),
    );
    flags
}

pub(super) fn serve_help() -> String {
    // One loop renders every row: the surface descriptors and then the
    // build-independent segments. The newline is owned here, not by a row
    // literal, so a compiled-out segment leaves no blank-line artifact.
    let mut rows = String::new();
    for surface in SURFACES {
        rows.push_str(surface.rows);
        rows.push('\n');
    }
    for segment in common_segments() {
        rows.push_str(&segment.rows);
        rows.push('\n');
    }
    let usage = serve_usage();
    format!(
        "{usage}

serve hosts Relate's network surfaces on one open database until
SIGINT/SIGTERM. At least one surface flag is required; ports
have no defaults, and a taken port is a startup error.
  -d, --database PATH  The store to serve (directory, .rl bundle, or :memory:)
  --host HOST    Bind host for every served surface. Default
                       127.0.0.1 (loopback only). Any other host deliberately
                       exposes the PLAINTEXT surfaces to the network — serve
                       has no TLS — and a loud WARNING prints at startup
{rows}",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::help::{command_help, HelpScope, HelpTopic};

    #[test]
    fn serve_help_lists_the_shared_synopsis_and_its_flags() {
        let help = command_help(HelpTopic::Serve);
        crate::help::assert_single_usage(&help, &serve_usage());
        for present in [
            "--database PATH",
            "--auth USER:PASSWORD",
            "At least one surface flag is required",
            "SIGINT",
        ] {
            assert!(help.contains(present), "{present:?} missing:\n{help}");
        }
        // Surface membership is pinned constructionally, from the table the
        // file owns: every compiled-in surface's segment and rows appear.
        for surface in SURFACES {
            assert!(
                help.contains(surface.usage),
                "{} missing:\n{help}",
                surface.usage
            );
            assert!(help.contains(surface.rows), "rows missing:\n{help}");
        }
        #[cfg(not(feature = "http"))]
        assert!(!help.contains("--http PORT"));
        assert_eq!(
            HelpScope::Command(HelpTopic::Serve).hint(),
            "Run 'relate serve --help' for more information."
        );
        // Every suggested flag appears in the help.
        for flag in serve_flags() {
            assert!(help.contains(flag), "{flag:?} missing:\n{help}");
        }
    }

    /// The HTTP rows state their defaults as prose, so pin them to the
    /// enforced constants (ADR-0435): a changed constant must fail here.
    #[cfg(feature = "http")]
    #[test]
    fn serve_help_renders_the_http_defaults_from_the_shared_constants() {
        let help = command_help(HelpTopic::Serve);
        for expected in [
            format!("else {}.", relate_http::DEFAULT_TX_IDLE_TIMEOUT_SECS),
            format!("else {}.", relate_http::DEFAULT_MAX_OPEN_TRANSACTIONS),
        ] {
            assert!(help.contains(&expected), "{expected:?} missing:\n{help}");
        }
    }

    /// The admission defaults (ADR-0451) and the traversal bounds are
    /// rendered from the shared constants the surfaces and the engine
    /// enforce: a changed cap, deadline, or bound fails here.
    #[cfg(any(feature = "http", feature = "mcp"))]
    #[test]
    fn serve_help_renders_the_admission_defaults_from_the_shared_constants() {
        let help = command_help(HelpTopic::Serve);
        for expected in [
            relate_serve::DEFAULT_MAX_CONNECTIONS.to_string(),
            format!("else {}.", relate_serve::DEFAULT_HEADER_DEADLINE.as_secs()),
            format!("else {}.", relate_serve::DEFAULT_BODY_DEADLINE.as_secs()),
            format!("(1 through {}).", relate::MAX_TRAVERSAL_DEPTH_CEILING),
            format!("else {}. An", relate::MAX_TRAVERSAL_DEPTH),
        ] {
            assert!(help.contains(&expected), "{expected:?} missing:\n{help}");
        }
    }

    #[test]
    fn serve_help_documents_the_access_flags() {
        // ADR-0306: the synopsis segment plus one row per flag, in every
        // serve build (the flags are not per-surface).
        let help = command_help(HelpTopic::Serve);
        assert!(help.contains("[--read-only[=FRESHNESS]]"), "{help}");
        assert!(help.contains("frozen"), "{help}");
        assert!(help.contains("refuses .rl"), "{help}");
        assert!(help.contains(":memory:"), "{help}");
        assert!(help.contains("no writer lease"), "{help}");
    }
}

use crate::LoadError;

/// Neo4j / Aura connection fields (ADR-0080).
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub uri: String,
    pub username: String,
    pub password: String,
    pub database: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("uri", &self.uri)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .finish()
    }
}

impl Credentials {
    /// Parse a `KEY=VALUE` env file (`neo4j-cli --env-out-file` / Aura Created).
    pub fn from_env_file(path: &str) -> Result<Self, LoadError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| LoadError::Io(format!("failed to read env file {path}: {e}")))?;
        warn_permissive_env_file(path);
        let mut uri = None;
        let mut username = None;
        let mut password = None;
        let mut database = None;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "NEO4J_URI" => uri = Some(strip_assigned(v, "NEO4J_URI")),
                "NEO4J_USERNAME" => username = Some(strip_assigned(v, "NEO4J_USERNAME")),
                "NEO4J_USER" => username = Some(strip_assigned(v, "NEO4J_USER")),
                "NEO4J_PASSWORD" => password = Some(strip_assigned(v, "NEO4J_PASSWORD")),
                "NEO4J_DATABASE" => database = Some(strip_assigned(v, "NEO4J_DATABASE")),
                _ => {}
            }
        }
        finish(uri, username, password, database, "env file")
    }

    /// Process environment (`NEO4J_*`) when `--from` is omitted.
    pub fn from_process_env() -> Result<Self, LoadError> {
        let uri = std::env::var("NEO4J_URI")
            .ok()
            .map(|s| strip_assigned(&s, "NEO4J_URI"));
        Self::from_uri_and_process_env(uri, "environment")
    }

    /// A credentials flag value: a Query API URI (any `scheme://` spelling
    /// — the scheme policy and its error live in `crate::http`'s URI
    /// validation) or a path to an env file (no scheme). The one place the
    /// URI-vs-env-file discrimination lives — callers never re-derive it
    /// by substring sniffing. `source` labels the flag in errors, the
    /// surface's spelling (e.g. `"push --to"`), never baked in here.
    pub fn from_flag_value(to: &str, source: &str) -> Result<Self, LoadError> {
        if split_scheme(to).0.is_some() {
            Self::from_uri_and_process_env(Some(to.to_string()), source)
        } else {
            Self::from_env_file(to)
        }
    }

    /// A URI given explicitly (`push --to URI`), with username, password,
    /// and database from the process environment — the same variable
    /// policy as [`Self::from_process_env`], the one place it lives.
    pub fn from_uri_and_process_env(uri: Option<String>, source: &str) -> Result<Self, LoadError> {
        let username = std::env::var("NEO4J_USERNAME")
            .ok()
            .map(|s| strip_assigned(&s, "NEO4J_USERNAME"))
            .or_else(|| {
                std::env::var("NEO4J_USER")
                    .ok()
                    .map(|s| strip_assigned(&s, "NEO4J_USER"))
            });
        let password = std::env::var("NEO4J_PASSWORD")
            .ok()
            .map(|s| strip_assigned(&s, "NEO4J_PASSWORD"));
        let database = std::env::var("NEO4J_DATABASE")
            .ok()
            .map(|s| strip_assigned(&s, "NEO4J_DATABASE"));
        finish(uri, username, password, database, source)
    }
}

/// Warn (non-fatally) when a `--from` env file is group- or world-readable
/// (ADR-0122). The mode check is Unix-only; on other hosts it is a no-op.
fn warn_permissive_env_file(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            if let Some(warning) = permissive_env_file_warning(path, meta.permissions().mode()) {
                eprintln!("{warning}");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Pure warning-text helper for the Unix env-file mode check (ADR-0122): the
/// file is too permissive when any group/other bit (`0o077`) is set. Kept
/// separate from the filesystem read so it is unit-testable.
#[cfg(unix)]
fn permissive_env_file_warning(path: &str, mode: u32) -> Option<String> {
    if mode & 0o077 != 0 {
        Some(format!(
            "warning: env file {path} is group/world-readable (mode {:04o}); credentials may be exposed to other local users — run `chmod 600 {path}`",
            mode & 0o7777
        ))
    } else {
        None
    }
}

fn finish(
    uri: Option<String>,
    username: Option<String>,
    password: Option<String>,
    database: Option<String>,
    source: &str,
) -> Result<Credentials, LoadError> {
    Ok(Credentials {
        uri: require(uri, "NEO4J_URI", source)?,
        username: require(username, "NEO4J_USERNAME", source)?,
        password: require(password, "NEO4J_PASSWORD", source)?,
        database: validate_database(database, source)?,
    })
}

/// Validate `NEO4J_DATABASE` at the config boundary.
///
/// The value is interpolated raw into the Query API v2 URL path
/// (`/db/{database}/query/v2`), so it must not be able to alter the request.
/// Allowed: non-empty, at most 63 characters, `[A-Za-z0-9._-]`. Invalid values
/// fail fast with [`LoadError::Env`] — never percent-encoded or repaired. An
/// absent value keeps the `neo4j` default (ADR-0080).
fn validate_database(database: Option<String>, source: &str) -> Result<String, LoadError> {
    let Some(d) = database else {
        return Ok("neo4j".to_string());
    };
    if let Some(err) = database_name_error(&d) {
        return Err(LoadError::Env(format!(
            "NEO4J_DATABASE from {source} {err}"
        )));
    }
    Ok(d)
}

/// The database-name rule violation — the typed form of the shared rule
/// every Query API URL enforces (ADR-0080); each boundary frames its own
/// `NEO4J_DATABASE …` prefix, the rule owns its wording.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DatabaseNameError {
    #[error("must not be empty")]
    Empty,
    #[error("must be at most 63 characters (got {0})")]
    TooLong(usize),
    #[error("contains invalid character {0:?} (allowed: A-Z a-z 0-9 . _ -)")]
    InvalidChar(char),
}

/// The database-name rule every Query API URL shares: non-empty, at most 63
/// characters, `[A-Za-z0-9._-]` (ADR-0080). `validate_database` frames the
/// violation with the value's source, and `query_api_url` enforces the same
/// rule at its pub boundary, whose callers may bypass `Credentials`.
pub(crate) fn database_name_error(database: &str) -> Option<DatabaseNameError> {
    if database.is_empty() {
        return Some(DatabaseNameError::Empty);
    }
    let len = database.chars().count();
    if len > 63 {
        return Some(DatabaseNameError::TooLong(len));
    }
    database
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .map(DatabaseNameError::InvalidChar)
}

fn require(v: Option<String>, key: &str, source: &str) -> Result<String, LoadError> {
    match v {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(LoadError::Env(format!("{key} missing from {source}"))),
    }
}

/// Strip quotes/whitespace and a leading `{field_key}=` paste for that field only.
pub fn strip_assigned(raw: &str, field_key: &str) -> String {
    let mut s = strip_quotes_ws(raw);
    while let Some(rest) = strip_key_eq(&s, field_key) {
        s = strip_quotes_ws(rest);
    }
    s
}

fn strip_quotes_ws(s: &str) -> String {
    s.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .to_string()
}

fn strip_key_eq<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let eq = key.len();
    if s.len() <= eq || s.as_bytes().get(eq) != Some(&b'=') {
        return None;
    }
    if s[..eq].eq_ignore_ascii_case(key) {
        Some(&s[eq + 1..])
    } else {
        None
    }
}

/// Split a URI into its optional scheme (without the `://`) and the
/// remainder. A URI without `://` returns `(None, uri)`.
fn split_scheme(uri: &str) -> (Option<&str>, &str) {
    match uri.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, uri),
    }
}

/// Split a URI into its optional scheme and authority segment (up to the
/// first `/` or `?`). The path/query tail is discarded.
///
/// The scheme is returned without the `://`; the authority is a slice of the
/// input, untrimmed.
fn split_authority(uri: &str) -> (Option<&str>, &str) {
    let (scheme, rest) = split_scheme(uri);
    // The authority runs to the first `/` or `?`.
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    (scheme, &rest[..auth_end])
}

/// Scheme (if present), host, and port slot — path and query stripped.
///
/// Userinfo is NOT stripped here: `query_api_url` refuses any userinfo URI
/// before this runs (`reject_uri_userinfo`, ADR-0122), so an `@` in the
/// authority is malformed input, not credentials to carve away. The other
/// callers (`ensure_cleartext_basic_allowed`) only ever see
/// userinfo-free URLs.
pub(crate) struct UriAuthority {
    pub scheme: Option<String>,
    pub host: String,
    /// The port slot, kept distinct from "absent" (ADR-0122, refuse at the
    /// boundary): port text that is present but not a `u16` parses as
    /// `Malformed`, which the consumer refuses — folding it into `Absent`
    /// would let the Query API policy silently remap `neo4j://host:abc`
    /// to a default port.
    pub port: UriPort,
}

/// The port slot of a [`UriAuthority`]: absent, a parsed port, or text that
/// is present but not a `u16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UriPort {
    Absent,
    Port(u16),
    Malformed,
}

/// Map bare port text to its [`UriPort`] slot — the one port-text→slot
/// policy, shared by the bracketed and unbracketed arms of
/// [`uri_authority`]. A `u16` is `Port`; anything else (present but not a
/// number) is `Malformed`, never `Absent` (ADR-0122, refuse at the
/// boundary).
fn port_slot(text: &str) -> UriPort {
    match text.parse::<u16>() {
        Ok(port) => UriPort::Port(port),
        Err(_) => UriPort::Malformed,
    }
}

/// The port slot of a bracketed host's suffix after `]` (ADR-0122,
/// refuse at the boundary): empty is `Absent`, `:port` is the shared
/// [`port_slot`] mapping, anything else is `Malformed` — never silently
/// truncated and never funneled into the same `None` as an empty
/// authority.
fn port_suffix(suffix: &str) -> UriPort {
    match suffix.strip_prefix(':') {
        Some(p) => port_slot(p),
        None if suffix.is_empty() => UriPort::Absent,
        None => UriPort::Malformed,
    }
}

pub(crate) fn uri_authority(uri: &str) -> Option<UriAuthority> {
    let uri = uri.trim();
    let (scheme, authority) = split_authority(uri);
    let rest = authority.trim();
    if rest.is_empty() {
        return None;
    }
    let (host, port) = if rest.starts_with('[') {
        // Bracketed IPv6 literal: the host runs to `]`; the suffix after
        // it is the one port-slot rule. Splitting on the first `:` would
        // carve up the address.
        match rest.find(']') {
            Some(end) => (rest[..=end].to_string(), port_suffix(&rest[end + 1..])),
            // No closing bracket: keep the whole segment as the "host"; the
            // caller's host-charset validation rejects it.
            None => (rest.to_string(), UriPort::Absent),
        }
    } else {
        match rest.split_once(':') {
            Some((h, p)) => {
                let h = h.trim();
                if h.is_empty() {
                    return None;
                }
                (h.to_string(), port_slot(p))
            }
            None => (rest.to_string(), UriPort::Absent),
        }
    };
    Some(UriAuthority {
        scheme: scheme.map(str::to_string),
        host,
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_flag_value_discriminates_uri_from_env_file_by_scheme() {
        // A scheme makes it a URI (validation of the scheme's spelling
        // stays in the URI layer); no scheme is an env-file path.
        for uri in [
            "https://example.databases.neo4j.io",
            "http://localhost:7474",
            "bolt://localhost:7687",
        ] {
            // The URI arm reaches the URI policy: an unschemed username
            // env cannot satisfy it, so it errors with the URI-arm
            // message, never the env-file one.
            let err = Credentials::from_flag_value(uri, "push --to")
                .unwrap_err()
                .to_string();
            assert!(!err.contains("failed to read env file"), "{uri}: {err}");
        }
        let err = Credentials::from_flag_value("/nonexistent/creds.env", "push --to")
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed to read env file"), "{err}");
    }

    #[test]
    fn from_env_file_reads_keys_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            "# comment\nNEO4J_URI=neo4j://localhost:7687\nNEO4J_USERNAME=\"neo4j\"\nNEO4J_PASSWORD='s3cret'\n\nNEO4J_DATABASE=movies\n",
        )
        .unwrap();
        let c = Credentials::from_env_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.uri, "neo4j://localhost:7687");
        assert_eq!(c.username, "neo4j");
        assert_eq!(c.password, "s3cret");
        assert_eq!(c.database, "movies");
    }

    #[test]
    fn from_env_file_strips_pasted_password_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            "NEO4J_URI=neo4j://db\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=NEO4J_PASSWORD=s3cret\n",
        )
        .unwrap();
        let c = Credentials::from_env_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.password, "s3cret");
    }

    #[test]
    fn strip_assigned_only_own_key() {
        assert_eq!(
            strip_assigned("USER=hunter2", "NEO4J_PASSWORD"),
            "USER=hunter2"
        );
        assert_eq!(
            strip_assigned("NEO4J_PASSWORD=s3cret", "NEO4J_PASSWORD"),
            "s3cret"
        );
    }

    #[test]
    fn from_env_file_defaults_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            "NEO4J_URI=neo4j://db\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n",
        )
        .unwrap();
        let c = Credentials::from_env_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.database, "neo4j");
    }

    /// Build credentials from an env file with `database` interpolated (the
    /// same `finish()` boundary both env paths use).
    fn creds_with_database(database: &str) -> Result<Credentials, LoadError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            format!(
                "NEO4J_URI=neo4j://db\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\nNEO4J_DATABASE={database}\n"
            ),
        )
        .unwrap();
        Credentials::from_env_file(path.to_str().unwrap())
    }

    #[test]
    fn from_env_file_rejects_unsafe_database_charset() {
        for value in ["a/b", "a?b", "a#b", "a b", "caf\u{e9}", "a:b"] {
            let err = creds_with_database(value).unwrap_err();
            assert!(matches!(err, LoadError::Env(_)), "value {value:?}");
            assert!(
                err.to_string().contains("NEO4J_DATABASE"),
                "value {value:?}: {err}"
            );
        }
    }

    #[test]
    fn from_env_file_rejects_empty_database() {
        let err = creds_with_database("").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert!(err.to_string().contains("NEO4J_DATABASE"));
    }

    #[test]
    fn from_env_file_rejects_overlong_database() {
        let err = creds_with_database(&"a".repeat(64)).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert!(err.to_string().contains("NEO4J_DATABASE"));
        // 63 characters is the boundary and stays valid.
        let ok = creds_with_database(&"a".repeat(63)).unwrap();
        assert_eq!(ok.database, "a".repeat(63));
    }

    #[test]
    fn from_env_file_accepts_valid_database_names() {
        for value in ["neo4j", "movies", "my-db_1.2"] {
            assert_eq!(creds_with_database(value).unwrap().database, value);
        }
    }

    #[test]
    fn finish_rejects_empty_database_from_process_env_shape() {
        let err = finish(
            Some("neo4j://db".into()),
            Some("neo4j".into()),
            Some("pw".into()),
            Some(String::new()),
            "environment",
        )
        .unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert!(err.to_string().contains("environment"));
    }

    #[test]
    fn from_env_file_missing_uri_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(&path, "NEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n").unwrap();
        let err = Credentials::from_env_file(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert!(err.to_string().contains("NEO4J_URI"));
    }

    #[test]
    fn missing_file_is_io() {
        let err = Credentials::from_env_file("/nonexistent/n.env").unwrap_err();
        assert!(matches!(err, LoadError::Io(_)));
    }

    #[test]
    fn debug_redacts_password() {
        let c = Credentials {
            uri: "neo4j://db".into(),
            username: "neo4j".into(),
            password: "s3cret-password".into(),
            database: "neo4j".into(),
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("s3cret-password"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[test]
    fn uri_authority_strips_path_and_port() {
        let a = uri_authority("neo4j://localhost:7687/db?x=1").unwrap();
        assert_eq!(a.scheme.as_deref(), Some("neo4j"));
        assert_eq!(a.host, "localhost");
        assert_eq!(a.port, UriPort::Port(7687));
    }

    #[test]
    fn uri_authority_does_not_strip_userinfo() {
        // Userinfo URIs are refused by `query_api_url` before `uri_authority`
        // runs (ADR-0122), so this parser no longer carves credentials away:
        // `user:pw@localhost:7687` parses host `user` with an unparseable
        // port. One boundary rule, not strip-plus-parse.
        let a = uri_authority("neo4j://user:pw@localhost:7687").unwrap();
        assert_eq!(a.host, "user");
        assert_eq!(a.port, UriPort::Malformed);
    }

    #[test]
    fn uri_authority_keeps_malformed_ports_distinct_from_absent() {
        // Port text that is present but not a `u16` is `Malformed`, never
        // folded into `Absent` — the consumer refuses it (ADR-0122) instead
        // of silently remapping to a default port.
        for uri in ["neo4j://host:abc", "neo4j://host:99999", "http://host:abc"] {
            assert_eq!(
                uri_authority(uri).unwrap().port,
                UriPort::Malformed,
                "uri {uri:?}"
            );
        }
    }

    #[test]
    fn uri_authority_parses_bracketed_ipv6_host_and_port() {
        let a = uri_authority("http://[::1]:7474/db/neo4j/query/v2").unwrap();
        assert_eq!(a.scheme.as_deref(), Some("http"));
        assert_eq!(a.host, "[::1]");
        assert_eq!(a.port, UriPort::Port(7474));
        let a = uri_authority("neo4j://[fe80::1]").unwrap();
        assert_eq!(a.host, "[fe80::1]");
        assert_eq!(a.port, UriPort::Absent);
    }

    #[test]
    fn uri_authority_carries_bracket_junk_as_a_malformed_port() {
        // A suffix after `]` that is not `:port` is never silently
        // dropped and never funneled into the same None as an empty
        // authority: the bracketed host survives with a malformed port
        // slot, so the caller's fixed invalid-port text names the real
        // violation (ADR-0122, refuse at the boundary).
        for (uri, host) in [
            ("http://[::1]evil.com", "[::1]"),
            ("neo4j://[fe80::1]x", "[fe80::1]"),
            ("http://[::1]:evil.com", "[::1]"),
            ("http://[::1]:7474x", "[::1]"),
        ] {
            let a = uri_authority(uri).unwrap();
            assert_eq!(a.host, host, "uri {uri:?}");
            assert_eq!(a.port, UriPort::Malformed, "uri {uri:?}");
        }
        // The valid shapes keep working.
        assert_eq!(uri_authority("http://[::1]:7474").unwrap().host, "[::1]");
        assert_eq!(uri_authority("http://[::1]").unwrap().host, "[::1]");
        assert_eq!(uri_authority("http://[::1]").unwrap().port, UriPort::Absent);
    }

    #[cfg(unix)]
    #[test]
    fn permissive_env_file_warns_naming_path_and_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            "NEO4J_URI=neo4j://db\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let warning = permissive_env_file_warning(path.to_str().unwrap(), mode)
            .expect("0o644 file should warn");
        assert!(warning.contains(path.to_str().unwrap()), "{warning}");
        assert!(warning.contains("chmod 600"), "{warning}");
    }

    #[cfg(unix)]
    #[test]
    fn private_env_file_does_not_warn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(&path, "NEO4J_URI=neo4j://db\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert!(permissive_env_file_warning(path.to_str().unwrap(), mode).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn warning_is_non_fatal_and_credentials_still_load() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.env");
        std::fs::write(
            &path,
            "NEO4J_URI=neo4j://db\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\nNEO4J_DATABASE=movies\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let c = Credentials::from_env_file(path.to_str().unwrap()).unwrap();
        assert_eq!(c.username, "neo4j");
        assert_eq!(c.password, "pw");
        assert_eq!(c.database, "movies");
    }
}

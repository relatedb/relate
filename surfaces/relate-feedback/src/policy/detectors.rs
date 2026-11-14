//! The redaction detectors (ADR-0588 §1): the one place that decides what
//! report text may not leave the machine.
//!
//! Every detector is a pure predicate over one field's text. The URL pass
//! runs first because an allowed documentation link is replaced by the
//! words of its path before the other detectors see the text; every other
//! URL is a hit. A detector reports only its name, never the matched text.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use super::Detector;

// --- token boundaries ----------------------------------------------------
//
// Three detectors split text into whole words, each with a deliberately
// different boundary set; the base helper is shared and each difference is
// stated where it lives, so the sets cannot drift apart unseen.

/// The characters that end a word token in prose: whitespace and the
/// punctuation that brackets or separates a token.
fn is_token_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
        )
}

/// A file path may follow a shell pipe with no space (`cat a|/etc/passwd`),
/// so the path tokenizer also splits on `|`.
fn is_path_boundary(c: char) -> bool {
    c == '|' || is_token_boundary(c)
}

/// A URL candidate ends where a path token ends, except for two characters:
/// comma and semicolon stay inside a URL — they can appear mid-URL, and the
/// trailing-punct trimmer removes them when they end the candidate.
fn is_url_boundary(c: char) -> bool {
    is_path_boundary(c) && !matches!(c, ',' | ';')
}

/// The one trailing-punctuation trimmer for a tokenized candidate.
fn trim_token_punct(token: &str) -> &str {
    token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}'])
}

/// The authority at the start of a token: a host with a path, query or
/// fragment after it is still one token (`grafana.internal/d/abc`,
/// `localhost:8080/admin`), and the host is what precedes the first `/`, `?`
/// or `#` — the same cut the endpoint parser makes. The host and IP
/// detectors both classify that authority, so neither misses a host that
/// merely has a path attached.
fn authority(token: &str) -> &str {
    token.split(['/', '?', '#']).next().unwrap_or(token)
}

/// One detector with the predicate that fires it.
type DetectorRow = (Detector, fn(&str) -> bool);

/// Every detector that runs over the scrubbed text, in the order a report's
/// hits are reported. One row pairs a detector with its predicate, so the two
/// cannot be copy-pasted apart and a new detector is one row.
const DETECTORS: [DetectorRow; 10] = [
    (Detector::Email, |text| EMAIL.is_match(text)),
    (Detector::HostPort, has_host_port),
    (Detector::IpAddress, has_ip),
    (Detector::FilePath, has_file_path),
    (Detector::DataFilename, has_data_filename),
    (Detector::Secret, has_secret),
    (Detector::Identifier, has_identifier),
    (Detector::Cypher, has_cypher),
    (Detector::Parameter, |text| PARAMETER.is_match(text)),
    (Detector::PastedOutput, has_pasted_output),
];

/// Every redaction detector that fires on `text`. The URL pass runs first:
/// an allowed documentation link is replaced by the words of its path so
/// the other detectors still see its contents; any other URL is a hit.
pub(super) fn detect(text: &str) -> Vec<Detector> {
    let normalized: String = text.nfkc().collect();
    let (scrubbed, url_hit) = scrub_urls(&normalized);
    let mut hits = Vec::new();
    // The two passes that are not plain predicates over the scrubbed text:
    // the URL pre-pass reports its own hit, and control characters are read
    // from the normalized text before the scrubbing.
    if url_hit {
        hits.push(Detector::Url);
    }
    if has_control_chars(&normalized) {
        hits.push(Detector::ControlChars);
    }
    for (detector, fires) in DETECTORS {
        if fires(&scrubbed) {
            hits.push(detector);
        }
    }
    hits
}

/// Replace every allowed documentation URL by the words of its path and
/// report whether any other URL was present.
fn scrub_urls(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut disallowed = false;
    while let Some(start) = next_url_start(rest) {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let end = candidate.find(is_url_boundary).unwrap_or(candidate.len());
        let raw = &candidate[..end];
        let (url, trailing) = split_trailing_punct(raw);
        if allowed_url(url) {
            out.push_str(&url_words(url));
        } else {
            disallowed = true;
            out.push_str(url);
        }
        out.push_str(trailing);
        rest = &candidate[end..];
    }
    out.push_str(rest);
    (out, disallowed)
}

/// Any `scheme://` spelling, in any case: a candidate goes through
/// [`allowed_url`], which admits only the documented lowercase `https`
/// links, so every other scheme and an uppercase `HTTPS` are hits.
static URL_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[a-z][a-z0-9+.\-]*://").expect("url scheme regex"));

fn next_url_start(text: &str) -> Option<usize> {
    URL_START.find(text).map(|m| m.start())
}

/// Split trailing sentence punctuation off a URL candidate.
fn split_trailing_punct(raw: &str) -> (&str, &str) {
    let trimmed = trim_token_punct(raw);
    (trimmed, &raw[trimmed.len()..])
}

/// The one allowed-link policy: `https` to this project's own documentation
/// (`relatedb.ai/<path>`) and repository (`github.com/relatedb/relate/<path>`),
/// or to the Neo4j Cypher manual (`neo4j.com/docs/...`) — the references a
/// report needs to be actionable — with no userinfo, port, query, fragment
/// or escaped path, and a short plain path. A path is required: a bare host
/// URL with no path (`https://relatedb.ai`, `https://github.com/relatedb/relate`)
/// is refused.
pub(super) fn allowed_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    // No userinfo, port, query, fragment or escaped path.
    if rest
        .bytes()
        .any(|b| matches!(b, b'@' | b'?' | b'#' | b'%' | b':'))
    {
        return false;
    }
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let path = format!("/{path}");
    if path.len() > 160
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        return false;
    }
    match host {
        "relatedb.ai" => true,
        "neo4j.com" => path.starts_with("/docs/"),
        "github.com" => path.starts_with("/relatedb/relate/"),
        _ => false,
    }
}

/// The words of an allowed URL's path, so other detectors keep scanning its
/// contents (e.g. a path segment that looks like a secret) while the URL
/// itself is no longer a URL-shaped token.
fn url_words(url: &str) -> String {
    url.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Control characters other than tab, and Unicode format (Cf) characters
/// such as zero-width and bidi marks — a newline or a bidi override could
/// pose as another consent-dialog line.
fn has_control_chars(text: &str) -> bool {
    text.chars()
        .any(|c| (c.is_control() && c != '\t') || is_format_char(c))
}

fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email detector regex")
});

/// A host is a hit with or without a port: one token pass, one place that
/// decides what a host is. A `host:port` is reduced to its host exactly as
/// [`ip_token`] does, then the host is classified once — a dotted name, or
/// `localhost`, or a single label that has a letter (`box:1234`). A numeric
/// host is left to the IP detector, so times such as `10:30` are not
/// flagged, and a bare host name (`db.prod.internal`) is a hit too.
fn has_host_port(text: &str) -> bool {
    text.split(is_token_boundary).any(host_port_token)
}

fn host_port_token(token: &str) -> bool {
    let token = trim_token_punct(authority(token));
    let (host, port) = match token.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (token, None),
    };
    host_token(host)
        || host.eq_ignore_ascii_case("localhost")
        // A single label is a host only with an explicit port, and only
        // when it has a letter: `10` in `10:30` is a time.
        || (port.is_some() && host_label(host) && host.chars().any(|c| c.is_ascii_alphabetic()))
}

/// A bare host name: two or more dotted labels whose last label is only
/// letters (so a version such as `1.2.3` and a time do not match) and not a
/// known data extension (those are filenames, the data-filename detector's).
/// A label holding `@`, `:` or `/` is not a host, so an email address and a
/// URL token stay with the email and URL detectors.
fn host_token(token: &str) -> bool {
    let token = trim_token_punct(token);
    let Some((_, tld)) = token.rsplit_once('.') else {
        return false;
    };
    tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && !is_data_extension(tld)
        && token.split('.').all(host_label)
}

/// One DNS label: 1-63 alphanumeric/hyphen characters, no leading or
/// trailing hyphen.
fn host_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// A parseable IPv4 or IPv6 address. Candidates are whole words, so an IPv6
/// fragment embedded in a longer word (`std::string`) is skipped; a numeric
/// `host:port` is reduced to its host so the IP detector still sees it.
pub(super) fn has_ip(text: &str) -> bool {
    text.split(is_token_boundary)
        .any(|token| ip_token(trim_token_punct(token)))
}

fn ip_token(token: &str) -> bool {
    let token = authority(token);
    let candidate = match token.rsplit_once(':') {
        Some((prefix, port))
            if prefix.contains('.')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            prefix
        }
        _ => token,
    };
    if !candidate.chars().any(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    candidate.parse::<Ipv4Addr>().is_ok()
        || (candidate.contains(':') && candidate.parse::<Ipv6Addr>().is_ok())
}

fn has_file_path(text: &str) -> bool {
    text.split(is_path_boundary).any(path_token)
}

fn path_token(token: &str) -> bool {
    let token = trim_token_punct(token);
    if token.starts_with("~/") || token.starts_with("./") || token.starts_with("../") {
        return true;
    }
    let bytes = token.as_bytes();
    if bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return true;
    }
    let Some(rest) = token.strip_prefix('/') else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() >= 2
        || segments.last().is_some_and(|last| {
            last.rsplit_once('.')
                .is_some_and(|(base, ext)| !base.is_empty() && !ext.is_empty())
        })
}

/// A data filename's name may itself carry dots (`orders.2024.csv`,
/// `users.prod.csv`), so the name takes any dotted run and the extension is
/// the final one. Without that, the engine's leftmost match stops at the
/// first extension — `orders.2024` — and the real extension is never
/// classified, so a multi-dot data filename passes every detector.
static DATA_FILENAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<name>[A-Za-z0-9_\-]+(?:\.[A-Za-z0-9_\-]+)*)\.(?P<ext>[A-Za-z0-9]{1,10})")
        .expect("data filename regex")
});

/// The data/document extensions that mark a token as a data filename.
const DATA_EXTENSIONS: [&str; 41] = [
    "csv",
    "tsv",
    "json",
    "jsonl",
    "ndjson",
    "cypher",
    "cql",
    "txt",
    "xls",
    "xlsx",
    "xml",
    "yaml",
    "yml",
    "parquet",
    "sql",
    "log",
    "go",
    "java",
    "sh",
    "md",
    "zip",
    "gz",
    "tar",
    "pdf",
    "db",
    "dump",
    "backup",
    "toml",
    "ini",
    "conf",
    "properties",
    "rs",
    "py",
    "js",
    "ts",
    "html",
    "htm",
    "rl",
    "lock",
    "env",
    "bak",
];

/// Well-known product and tool filenames that are safe to mention. Every row
/// is a token [`DATA_FILENAME`] produces: a dotfile or a bare name has no
/// extension to match.
const PRODUCT_FILENAMES: [&str; 12] = [
    "config.json",
    "package.json",
    "Cargo.toml",
    "SKILL.md",
    "README.md",
    "AGENTS.md",
    "CLAUDE.md",
    "install.sh",
    "install.ps1",
    "neo4j.conf",
    "relate.conf",
    "Cargo.lock",
];

fn has_data_filename(text: &str) -> bool {
    DATA_FILENAME.captures_iter(text).any(|cap| {
        if PRODUCT_FILENAMES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&cap[0]))
        {
            return false;
        }
        let ext = cap
            .name("ext")
            .expect("DATA_FILENAME always captures its extension")
            .as_str();
        is_data_extension(ext)
    })
}

/// The one data/document extension predicate: the filename detector rejects
/// such a token, the host detector leaves it to that one.
fn is_data_extension(ext: &str) -> bool {
    DATA_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// The credential keywords, spelled once for both arms below.
const SECRET_KEYWORDS: &str =
    r"pass(word|wd)?|secret|token|api[ _-]?key|access[ _-]?key|private[ _-]?key|auth";

/// A keyword followed by a value: `token=abc123` (`:`/`=`), or the prose
/// spelling `the password hunter2` / `our API key abc123` (whitespace), so a
/// credential written with a natural-language separator is still caught.
/// `api key` and `access key` match with the space, never only `_`/`-`.
///
/// Two arms over that one list. The prose arm keeps its leading word
/// boundary, so ordinary words that merely contain a keyword (`compass
/// direction= north`) stay prose. The assignment arm drops it, so an
/// environment-style name whose keyword is a suffix is caught too —
/// `DATABASE_PASSWORD=…`, `MY_API_KEY=…`, `PGPASSWORD=…` all assign a value
/// to a credential name.
static SECRET_KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b(?:{SECRET_KEYWORDS})\b(?:\s*[:=]\s*|\s+)\S+|(?:{SECRET_KEYWORDS})\s*[:=]\s*\S+"
    ))
    .expect("secret keyword regex")
});

static SECRET_FORMAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:sk-[A-Za-z0-9_-]{8,}|ghp_[A-Za-z0-9]{8,}|xox[abprs]-[A-Za-z0-9-]{8,}|AKIA[0-9A-Z]{12,}|nams_[A-Za-z0-9]{8,})",
    )
    .expect("secret format regex")
});

static LONG_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9]{20,}").expect("long run regex"));

/// A run with at least one ASCII letter and one digit: what makes a long run
/// look like a token rather than prose. The secret and identifier detectors
/// share it, so what counts as a mixed run cannot drift between them.
fn mixed_alphanumeric(run: &str) -> bool {
    run.chars().any(|c| c.is_ascii_alphabetic()) && run.chars().any(|c| c.is_ascii_digit())
}

fn has_secret(text: &str) -> bool {
    SECRET_KEYWORD.is_match(text)
        || SECRET_FORMAT.is_match(text)
        || LONG_RUN
            .captures_iter(text)
            .any(|cap| mixed_alphanumeric(&cap[0]))
}

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .expect("uuid detector regex")
});

static HEX_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9a-fA-F]{8,}").expect("hex run regex"));

/// A UUID, or an 8+ character hex run containing both a letter and a digit.
fn has_identifier(text: &str) -> bool {
    UUID.is_match(text)
        || HEX_RUN
            .captures_iter(text)
            .any(|cap| mixed_alphanumeric(&cap[0]))
}

static CYPHER_KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(MATCH|MERGE|CREATE|RETURN|WHERE|WITH|UNWIND|OPTIONAL\s+MATCH|DETACH\s+DELETE|ORDER\s+BY)\b",
    )
    .expect("cypher keyword regex")
});

static CYPHER_STRUCTURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(\(\s*[A-Za-z_][A-Za-z0-9_]*\s*:\s*[A-Za-z_][A-Za-z0-9_]*\s*(?:\{[^{}]*\})?\s*\))",
        r"|(\[\s*[A-Za-z0-9_]*\s*:\s*[A-Za-z0-9_|]+\s*\])",
        r"|(-\[|\]->|--\(|\(--)",
        r"|(\{[^{}\n]{0,200}\s*:\s*[^{}\n]{0,200}\})",
    ))
    .expect("cypher structure regex")
});

static CYPHER_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$[a-z][A-Za-z0-9_]*").expect("cypher param regex"));

/// Pasted Cypher: two or more distinct query keywords, a node/relationship
/// pattern with a label, an edge, a map literal, or a lower-case `$param`
/// (an all-caps `$ENV_VAR` is not Cypher).
pub(super) fn has_cypher(text: &str) -> bool {
    let keywords = CYPHER_KEYWORD
        .find_iter(text)
        .map(|m| m.as_str().to_ascii_uppercase())
        .collect::<std::collections::HashSet<_>>();
    keywords.len() >= 2 || CYPHER_STRUCTURE.is_match(text) || CYPHER_PARAM.is_match(text)
}

static PARAMETER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)--param\s+\S+=").expect("--param detector regex"));

static PASTED_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\berror:.*\(\s*exit\s+[0-9]+\s*\)").expect("pasted error regex")
});

/// Pasted program output: a JSON object/array, a pipe table, or a captured
/// `Error: … (exit N)` line.
fn has_pasted_output(text: &str) -> bool {
    let trimmed = text.trim();
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        return true;
    }
    if text.lines().any(|line| {
        let line = line.trim();
        line.starts_with('|') && line.ends_with('|') && line.len() > 2
    }) {
        return true;
    }
    PASTED_ERROR.is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every detector runs, and runs exactly once: the table, the two named
    /// pre-passes and the input-shape trio partition `Detector::ALL`. A
    /// variant added to the enum and to none of the three compiles and never
    /// fires — in the redaction path that is text the policy meant to refuse
    /// leaving the machine, with no test noticing.
    #[test]
    fn every_detector_variant_is_run_exactly_once() {
        let mut placed: Vec<Detector> = DETECTORS.iter().map(|(d, _)| *d).collect();
        placed.push(Detector::Url);
        placed.push(Detector::ControlChars);
        placed.extend(Detector::ALL.iter().copied().filter(|d| d.is_input_shape()));

        for detector in Detector::ALL {
            let count = placed.iter().filter(|placed| **placed == detector).count();
            assert_eq!(count, 1, "{detector:?} is placed {count} times");
        }
        assert_eq!(placed.len(), Detector::ALL.len());
    }

    /// The product allow-list holds only tokens the matcher can produce: the
    /// dotfile and the bare name have no `name.ext` shape, so widening the
    /// matcher would be the only way to make them reachable.
    #[test]
    fn the_product_allow_list_holds_only_data_filename_tokens() {
        for token in [".gitignore", "LICENSE"] {
            assert!(!DATA_FILENAME.is_match(token), "{token} has no extension");
        }
        for token in PRODUCT_FILENAMES {
            assert!(DATA_FILENAME.is_match(token), "{token} is exempt");
        }
    }
}

//! Shared `--param`/`--fetch-params` name=value handling for queries and
//! `pull`: assignment splitting and decoding (always compiled — every flag
//! parses in every build, ADR-0104), source classification, and post-parse
//! resolution into query params.

use std::collections::HashMap;
use std::path::PathBuf;

use indexmap::IndexMap;
use relate_model::{serde_value::try_json_to_param_value, CypherValue};

/// One parameter assignment from argv: a parsed `--param` value, or a
/// `--fetch-params` source resolved lazily (after the no-side-effect exits).
pub(crate) enum Assignment {
    Param(CypherValue),
    Source(FetchSource),
}

/// Where a `--fetch-params` source comes from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FetchSource {
    Url(String),
    File(PathBuf),
}

/// Split a `--fetch-params` `NAME=SOURCE` and classify the source. The
/// source may itself contain `=` (query strings), so only the first `=`
/// separates.
pub(crate) fn parse_fetch_assignment(raw: &str) -> Result<(String, FetchSource), String> {
    let (name, source) = split_param_assignment(raw)?;
    if let Some(rest) = source
        .strip_prefix("http://")
        .or_else(|| source.strip_prefix("https://"))
    {
        if rest.is_empty() {
            return Err("empty URL".into());
        }
        return Ok((name.to_string(), FetchSource::Url(source.to_string())));
    }
    if let Some(rest) = source.strip_prefix("file://") {
        if rest.is_empty() {
            return Err("empty file path".into());
        }
        return Ok((name.to_string(), FetchSource::File(PathBuf::from(rest))));
    }
    // Other URL schemes are a usage error, not a filename that happens to
    // contain a colon.
    if source.contains("://") {
        return Err(format!(
            "unsupported source (only http, https, and file paths): {source}"
        ));
    }
    if source.is_empty() {
        return Err("missing source".into());
    }
    Ok((name.to_string(), FetchSource::File(PathBuf::from(source))))
}

/// Resolve assignments into query params. Called only after every
/// no-side-effect exit (`--help`, `--version`, lifecycle) is done, so those
/// paths never fetch. Callers insert into an `IndexMap`, so the last
/// assignment for a name wins (an overwritten `--fetch-params` source is
/// never fetched) and sources resolve in argv order — the first failure is
/// the one reported, as `--fetch-params NAME: ...`.
pub(crate) fn resolve(
    assignments: IndexMap<String, Assignment>,
) -> Result<HashMap<String, CypherValue>, String> {
    let mut params: HashMap<String, CypherValue> = HashMap::new();
    for (name, assignment) in assignments {
        let value = match assignment {
            Assignment::Param(v) => v,
            Assignment::Source(source) => resolve_source(&name, source)?,
        };
        params.insert(name, value);
    }
    Ok(params)
}

/// Resolve a `--fetch-params` source into a bindable `CypherValue`.
#[cfg(feature = "fetch-params")]
fn resolve_source(name: &str, source: FetchSource) -> Result<CypherValue, String> {
    crate::fetch_params::fetch_source(name, &source)
}

/// A `--fetch-params` value parses in every build (ADR-0270); without the
/// `fetch-params` feature resolution fails closed at startup, like
/// `--snapshot-format parquet` without the `parquet` feature (ADR-0105).
#[cfg(not(feature = "fetch-params"))]
fn resolve_source(_name: &str, _source: FetchSource) -> Result<CypherValue, String> {
    Err(
        "--fetch-params is not compiled into this build (enable the \"fetch-params\" feature)"
            .into(),
    )
}

/// Split `NAME=VALUE` on the first `=`; name and value are trimmed.
pub(crate) fn split_param_assignment(raw: &str) -> Result<(&str, &str), String> {
    let Some((name, value)) = raw.split_once('=') else {
        return Err("expected name=value".into());
    };
    let name = name.trim();
    if name.is_empty() {
        return Err("missing parameter name".into());
    }
    Ok((name, value.trim()))
}

/// JSON-parse a pull parameter, else treat its value as a JSON string.
#[cfg(feature = "pull")]
pub(crate) fn parse_param_assignment(raw: &str) -> Result<(String, serde_json::Value), String> {
    use serde_json::Value as Json;

    let (name, trimmed) = split_param_assignment(raw)?;
    let json = match serde_json::from_str::<Json>(trimmed) {
        Ok(v) => v,
        Err(_) => Json::String(trimmed.to_string()),
    };
    Ok((name.to_string(), json))
}

pub(crate) fn parse_param_pair(s: &str) -> Result<(String, CypherValue), String> {
    let (name, value) = split_param_assignment(s)?;
    let parsed = parse_param_value(value).map_err(|e| format!("parameter {name}: {e}"))?;
    Ok((name.to_string(), parsed))
}

/// Decode a `--tx-meta KEY=VALUE` assignment (ADR-0610): the same
/// name=value split and JSON-or-bare-string decode as
/// [`parse_param_pair`] — the two flags share one value grammar, never
/// one error noun.
pub(crate) fn parse_tx_meta_pair(s: &str) -> Result<(String, CypherValue), String> {
    let (name, value) = split_param_assignment(s)?;
    let parsed = parse_param_value(value).map_err(|e| format!("metadata {name}: {e}"))?;
    Ok((name.to_string(), parsed))
}

fn parse_param_value(s: &str) -> Result<CypherValue, String> {
    let trimmed = s.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => try_json_to_param_value(&value).map_err(|e| e.to_string()),
        Err(e)
            if trimmed.starts_with(['{', '[', '"'])
                || trimmed.starts_with(|c: char| c.is_ascii_digit() || c == '-') =>
        {
            Err(format!("invalid parameter JSON: {e}"))
        }
        Err(_) => Ok(CypherValue::Str(trimmed.to_string())),
    }
}

#[cfg(test)]
mod param_decoder_tests {
    use super::*;

    #[test]
    fn parameters_keep_tags_as_map_data_and_reject_unsupported_numbers() {
        let (_, tagged) = parse_param_pair(r#"p={"__type":"point","x":1}"#).unwrap();
        let CypherValue::Map(map) = tagged else {
            panic!("expected map")
        };
        assert_eq!(map["__type"], CypherValue::Str("point".into()));
        assert_eq!(map["x"], CypherValue::Int(1));
        let (_, complete) =
            parse_param_pair(r#"p={"__type":"point","srid":7203,"x":1,"y":2}"#).unwrap();
        assert!(matches!(complete, CypherValue::Map(_)));
        let (_, nested) =
            parse_param_pair(r#"p={"items":[{"__type":"date","value":"bad"}]}"#).unwrap();
        assert!(matches!(nested, CypherValue::Map(_)));
        assert!(parse_param_pair("p=18446744073709551615")
            .unwrap_err()
            .contains("parameter p: $: integer exceeds i64 range"));
        assert!(parse_param_pair("p=1e400")
            .unwrap_err()
            .contains("parameter p: invalid parameter JSON"));
        assert!(parse_param_pair(r#"p={"nested":[18446744073709551615]}"#)
            .unwrap_err()
            .contains("parameter p: $.nested[0]: integer exceeds i64 range"));
        assert_eq!(
            parse_param_pair("p=bare").unwrap().1,
            CypherValue::Str("bare".into())
        );
    }

    #[test]
    fn tx_meta_pairs_share_the_param_decode_with_their_own_noun() {
        // ADR-0610: `--tx-meta` rides `--param`'s split and decode — a
        // bare word is a string, a JSON value stays typed — but its
        // error names the metadata channel, not the parameter.
        let (k, v) = parse_tx_meta_pair("app=loader").unwrap();
        assert_eq!(k, "app");
        assert_eq!(v, CypherValue::Str("loader".into()));
        let (_, n) = parse_tx_meta_pair("run=4711").unwrap();
        assert_eq!(n, CypherValue::Int(4711));
        assert!(parse_tx_meta_pair("novalue")
            .unwrap_err()
            .contains("expected name=value"));
        assert!(
            parse_tx_meta_pair("app=1e400")
                .unwrap_err()
                .contains("metadata app:"),
            "the error names the metadata key, not a parameter"
        );
    }
}

#[cfg(test)]
mod fetch_source_classification_tests {
    use super::*;

    #[test]
    fn url_source_with_query_string() {
        let (name, src) =
            parse_fetch_assignment("value=https://api/x?tags=a,b&site=stackoverflow").unwrap();
        assert_eq!(name, "value");
        assert_eq!(
            src,
            FetchSource::Url("https://api/x?tags=a,b&site=stackoverflow".to_string())
        );
    }

    #[test]
    fn http_bare_path_and_file_url() {
        assert!(matches!(
            parse_fetch_assignment("v=http://localhost:7/x").unwrap().1,
            FetchSource::Url(_)
        ));
        assert_eq!(
            parse_fetch_assignment("v=data/deps.json").unwrap().1,
            FetchSource::File(PathBuf::from("data/deps.json"))
        );
        assert_eq!(
            parse_fetch_assignment("v=file:///tmp/deps.json").unwrap().1,
            FetchSource::File(PathBuf::from("/tmp/deps.json"))
        );
    }

    #[test]
    fn rejects_missing_eq_empty_name_and_other_schemes() {
        // the canonical splitter's message, not a reworded copy
        assert!(parse_fetch_assignment("novalue")
            .unwrap_err()
            .contains("expected name=value"));
        assert!(parse_fetch_assignment("=x").unwrap_err().contains("name"));
        assert!(parse_fetch_assignment("v=ftp://host/x")
            .unwrap_err()
            .contains("unsupported"));
        assert!(parse_fetch_assignment("v=")
            .unwrap_err()
            .contains("missing source"));
        assert!(parse_fetch_assignment("v=https://")
            .unwrap_err()
            .contains("empty URL"));
        assert!(parse_fetch_assignment("v=file://")
            .unwrap_err()
            .contains("empty file path"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "pull")]
    #[test]
    fn json_number_and_bare_string() {
        let (k, v) = parse_param_assignment("x=42").unwrap();
        assert_eq!(k, "x");
        assert_eq!(v, serde_json::json!(42));
        let (_, v) = parse_param_assignment("name=Alice").unwrap();
        assert_eq!(v, serde_json::json!("Alice"));
    }

    #[test]
    fn rejects_missing_eq_and_empty_name() {
        assert!(split_param_assignment("justname")
            .unwrap_err()
            .contains("name=value"));
        assert!(split_param_assignment("=1").unwrap_err().contains("name"));
    }
}

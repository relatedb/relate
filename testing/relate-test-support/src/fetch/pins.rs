//! The `data.sha256` pin table: the line grammar and the lookup.

use std::path::Path;

use super::FetchError;

/// One pinned file: its name and its sha256 — one line of a pin table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The file's name in the cache.
    pub name: String,
    /// Its sha256, 64 lowercase hex characters.
    pub sha256: String,
}

/// A sha256 pin's spelling: 64 lowercase hex characters — the sha256sum
/// spelling the pin table and the verify path share, validated at parse
/// time so a truncated or non-hex pin is a malformed table, never a
/// permanent download mismatch surfacing after the fetch.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse a pin table's text: `#` comment and blank lines aside, every line
/// is `<sha256> <name>` (the sha256sum spelling, whitespace-separated)
/// with the sha256 64 lowercase hex characters — anything else is a
/// malformed table, refused naming `table` and the line. A table naming
/// nothing is refused naming `table` (it has no line to name).
pub fn parse_pins(text: &str, table: &Path) -> Result<Vec<Pin>, FetchError> {
    let mut pins = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line_no = n + 1;
        match line.split_whitespace().collect::<Vec<_>>()[..] {
            [sha256, name] => {
                if !is_sha256_hex(sha256) {
                    return Err(FetchError::PinTable {
                        path: table.to_path_buf(),
                        line: Some(line_no),
                        detail: format!(
                            "a pin's sha256 is 64 lowercase hex characters, got {sha256:?}"
                        ),
                    });
                }
                pins.push(Pin {
                    name: name.to_string(),
                    sha256: sha256.to_string(),
                });
            }
            _ => {
                return Err(FetchError::PinTable {
                    path: table.to_path_buf(),
                    line: Some(line_no),
                    detail: format!("a pin line is `<sha256> <name>`, got {line:?}"),
                })
            }
        }
    }
    if pins.is_empty() {
        return Err(FetchError::PinTable {
            path: table.to_path_buf(),
            line: None,
            detail: "the pin table names no files".to_string(),
        });
    }
    Ok(pins)
}

/// The sha256 a parsed pin table names for `name`: a table that does not
/// name it is its own refusal, naming the table and the file.
pub fn pin_sha256(pins: &[Pin], name: &str, table: &Path) -> Result<String, FetchError> {
    pins.iter()
        .find(|pin| pin.name == name)
        .map(|pin| pin.sha256.clone())
        .ok_or_else(|| FetchError::PinMissing {
            path: table.to_path_buf(),
            name: name.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_tables_parse_their_lines() {
        let dir = tempfile::tempdir().unwrap();
        let table = dir.path().join("data.sha256");
        let text = "# a comment\n\n\
                    c90febb048ce81b459d9132cececaecd0033a9a530d542d890a18c1afeca3d65  stations.csv\n\
                    1006934d2423ec44ac23dfeb66a4eab9edb0195e8f595ebb5958a46cc2d4d151  links.csv\n";
        let pins = parse_pins(text, &table).unwrap();
        assert_eq!(
            pins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["stations.csv", "links.csv"]
        );
        assert!(pins.iter().all(|p| is_sha256_hex(&p.sha256)));
        assert_eq!(
            pin_sha256(&pins, "links.csv", &table).unwrap(),
            "1006934d2423ec44ac23dfeb66a4eab9edb0195e8f595ebb5958a46cc2d4d151"
        );
        let err = pin_sha256(&pins, "absent.csv", &table).unwrap_err();
        assert!(matches!(err, FetchError::PinMissing { .. }), "{err:?}");
        assert!(
            err.to_string().contains("does not name absent.csv"),
            "{err}"
        );

        let err = parse_pins("one-field-only\n", &table).unwrap_err();
        assert!(
            err.to_string().contains("a pin line is `<sha256> <name>`"),
            "{err}"
        );
        assert!(
            err.to_string().contains("data.sha256:1:"),
            "the refusal names the line: {err}"
        );
        // A truncated (or otherwise non-hex) sha256 is a malformed pin,
        // refused at parse time — never a permanent download mismatch.
        let err = parse_pins("c90febb048  stations.csv\n", &table).unwrap_err();
        assert!(
            err.to_string().contains("64 lowercase hex characters"),
            "{err}"
        );
        assert!(err.to_string().contains("data.sha256:1:"), "{err}");
        // A 64-character lowercase but non-hex pin (g–z are lowercase, not
        // hex) is refused too — the predicate admits exactly the alphabet
        // its error text names.
        let err = parse_pins(
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg  stations.csv\n",
            &table,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("64 lowercase hex characters"),
            "{err}"
        );
        let err = parse_pins("# only comments\n\n", &table).unwrap_err();
        assert!(
            err.to_string().contains("the pin table names no files"),
            "{err}"
        );
        assert!(
            err.to_string().contains("data.sha256"),
            "the refusal names the table: {err}"
        );
    }
}

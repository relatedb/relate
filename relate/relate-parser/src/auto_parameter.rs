//! The auto-parameter vocabulary in both directions (ADR-0503's
//! extraction, ADR-0718's substitution): the reserved placeholder prefix
//! and its two named predicates, the name grammar's one writer and its
//! reader, and the authored-spelling pairs the naming path substitutes
//! with. A sibling of `normalize.rs` — the one-pass normalizer produces
//! the spellings, this module owns the vocabulary around them — so the
//! normalizer's own header stays true and the placeholder grammar has
//! one home whichever direction a consumer reads it.

/// The reserved prefix of every auto-parameter name. A statement whose
/// token stream references a parameter with this prefix, or a caller whose
/// parameter map carries a key with it, is [`NormalizeOutcome::NotCacheable`]
/// — the caller can never collide with the engine's own names. Private
/// behind the two named predicates that spell the policy
/// ([`name_is_auto_parameter`] on names, [`column_carries_auto_parameter`]
/// on rendered column text): the prefix itself is not surface.
const AUTO_PARAM_PREFIX: &str = "__AUTO_";

/// Whether a caller's parameter map carries any [`AUTO_PARAM_PREFIX`]-named
/// key — the one spelling of the reserved-prefix collision policy, consulted
/// at the one boundary that decides cache participation — the facade's
/// `Connection::cache_lookup`, before the raw stage — where such a caller
/// disables the cache for the statement entirely. Generic over the value
/// type — the policy is about the names only.
pub fn params_carry_reserved_prefix<V>(params: &std::collections::BTreeMap<String, V>) -> bool {
    params.keys().any(|name| name_is_auto_parameter(name))
}

/// Whether `name` is an auto-parameter name ([`AUTO_PARAM_PREFIX`]) —
/// the one spelling of the prefix test, shared by the caller-map
/// boundary above and the planner's auto-parameter plan predicate (the
/// dispatch epilogue's authored-name restore is keyed on the same
/// prefix, ADR-0663).
pub fn name_is_auto_parameter(name: &str) -> bool {
    name.starts_with(AUTO_PARAM_PREFIX)
}

/// Whether a rendered result column carries the auto-parameter prefix
/// (the compiled plan spells an unaliased auto-extracted item as the
/// placeholder) — the text-level half of the epilogue's re-parse
/// trigger, beside the name-level [`name_is_auto_parameter`]: the two
/// consult one prefix, one on names the plan holds typed, one on the
/// column text the plan rendered.
pub fn column_carries_auto_parameter(column: &str) -> bool {
    column.contains(AUTO_PARAM_PREFIX)
}

/// One extracted literal's authored spelling (ADR-0718): the placeholder
/// name the compiled plan carries, and the authored source text the
/// naming path substitutes it with. Named fields, never a positional
/// pair — which side is the placeholder is a compile-time fact, not a
/// destructuring convention.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthoredSpelling {
    /// The placeholder name (`__AUTO_INT_0__`), the plan's spelling.
    pub placeholder: String,
    /// The authored source text (`1`), what the result column keeps.
    pub authored: String,
}

/// The extracted literals' authored spellings, one per literal, in
/// creation order — the pairs the naming path substitutes with,
/// [`Normalized::values`] zipped with [`Normalized::spellings`], carried
/// beside the values on every cache channel that already carries them.
/// The placeholder's authored text is data, never re-derived by a parse
/// at result time.
pub type AuthoredSpellings = Vec<AuthoredSpelling>;

/// Zip a normalization's [`AuthoredSpellings`] — the one construction,
/// from the two parallel vectors the normalizer produced (generic over
/// the value type: the normalizer's own `NormalizedValue`, the engine's
/// `Value`, whatever channel carries the pairs).
pub fn authored_spellings<V>(values: &[(String, V)], spellings: &[String]) -> AuthoredSpellings {
    // The two parallel vectors are the normalizer's own output shape
    // (one Pass walk pushes both sides together); this is the one
    // boundary where they meet, and a length disagreement is a normalizer
    // bug, stated as the invariant rather than a silent truncation.
    assert_eq!(
        values.len(),
        spellings.len(),
        "the normalizer pushes one spelling per value"
    );
    values
        .iter()
        .zip(spellings)
        .map(|((name, _), spelling)| AuthoredSpelling {
            placeholder: name.clone(),
            authored: spelling.clone(),
        })
        .collect()
}

/// Replace every auto-parameter placeholder in `text` with its authored
/// spelling (ADR-0718) — the one substitution, in the crate that owns the
/// placeholder grammar. Three rules make it safe on rendered column text,
/// which is a verbatim substring of the rewritten statement:
///
/// - **Exact-name match**: a `$__AUTO_…` occurrence substitutes only when
///   the whole parsed name is one of `spellings`' carried names. A
///   non-extracted string literal's content can look like a placeholder
///   (`startsWith(n.x, '$__AUTO_INT_0__')` — the call-argument guard kept
///   the string out of extraction); a lookalike that is not a carried
///   name stays verbatim, exactly what a re-parse of the source would
///   have kept.
/// - **Quote awareness**: the scan tracks string spans by the lexer's own
///   rules (a `'`/`"` opens, the same byte closes, a backslash escapes
///   the next byte), so a string literal that happens to contain a
///   *carried* name verbatim is never touched from inside its quotes —
///   the one case an exact-name match alone cannot refuse.
/// - **Single pass**: the output is never re-scanned, so an authored
///   spelling that itself contains placeholder-lookalike text (an
///   extracted string literal) lands once, verbatim.
///
/// Inter-token spacing inside a substituted name follows the plan the
/// column text came from (ADR-0718 decision 4): a first execution's
/// splice preserves the literal span's trailing separator verbatim, so
/// the name reproduces the source's spacing exactly (`RETURN 1  +  2`
/// answers `1  +  2`); a template hit's plan was compiled from the
/// FIRST statement's rewritten text, so the name inherits that
/// statement's spacing — the one recorded divergence.
pub fn substitute_auto_parameters(text: &str, spellings: &[AuthoredSpelling]) -> String {
    // The fast path: nothing carried, nothing to substitute (the common
    // statement — no literals extracted, no placeholders in any column).
    if spellings.is_empty() {
        return text.to_string();
    }
    // Byte-wise accumulation over valid UTF-8 copied in order and valid
    // UTF-8 spellings spliced in — the result is valid UTF-8 by
    // construction, converted once at the end (a byte-level `String`
    // push would re-encode every multi-byte body byte as mojibake).
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut index = 0;
    // The string span the lexer's grammar would open at this byte: the
    // opening quote while inside, `None` between tokens.
    let mut in_string: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match in_string {
            Some(quote) => {
                out.push(byte);
                if byte == b'\\' {
                    // The escape consumes the next byte (read_string's
                    // own rule); copy both verbatim.
                    if let Some(&escaped) = bytes.get(index + 1) {
                        out.push(escaped);
                        index += 1;
                    }
                } else if byte == quote {
                    in_string = None;
                }
                index += 1;
            }
            None => {
                if byte == b'\'' || byte == b'"' {
                    in_string = Some(byte);
                    out.push(byte);
                    index += 1;
                } else if byte == b'$' {
                    // A placeholder reference renders as `$` + the name;
                    // try to read one whole name and match it exactly.
                    match read_auto_parameter_name(&bytes[index + 1..]) {
                        Some(name) => {
                            match spellings.iter().find(|carried| carried.placeholder == name) {
                                Some(carried) => {
                                    out.extend_from_slice(carried.authored.as_bytes());
                                }
                                // Not a carried name: a lookalike in
                                // ordinary text or a non-extracted
                                // literal's content — verbatim.
                                None => {
                                    out.push(b'$');
                                    out.extend_from_slice(name.as_bytes());
                                }
                            }
                            index += 1 + name.len();
                        }
                        // Not a name at all: an explicit parameter or
                        // ordinary text — copy the `$` and continue.
                        None => {
                            out.push(b'$');
                            index += 1;
                        }
                    }
                } else {
                    out.push(byte);
                    index += 1;
                }
            }
        }
    }
    String::from_utf8(out).expect("copied UTF-8 in order stays valid UTF-8")
}

/// Write one whole auto-parameter name — the grammar
/// [`crate::normalize::Pass::fresh_name`] assigns with (the tag derives
/// from the value it names there, the index from the one creation
/// counter), stated once here so the writer and the reader below can
/// never drift. The reader stays deliberately tolerant of unknown tags
/// (any uppercase run): a stored pair list may outlive a dialect that
/// grew a new value kind.
pub(crate) fn auto_parameter_name(kind: &str, index: usize) -> String {
    format!("{AUTO_PARAM_PREFIX}{kind}_{index}__")
}

/// Read one whole auto-parameter name from the bytes after a `$` —
/// [`AUTO_PARAM_PREFIX`], a kind tag, `_`, digits, and the closing `__`
/// (the grammar [`auto_parameter_name`] writes) — `None` when the bytes
/// do not spell one. Every consumed byte is echoed into the returned
/// name, so the caller advances by `name.len()`: the name IS the
/// consumption record.
fn read_auto_parameter_name(bytes: &[u8]) -> Option<String> {
    let mut rest = bytes.strip_prefix(AUTO_PARAM_PREFIX.as_bytes())?;
    let mut name = AUTO_PARAM_PREFIX.to_string();
    // The kind tag: ASCII uppercase only (`INT`, `FLOAT`, `STR`, `LIST`).
    let tag = rest.iter().take_while(|b| b.is_ascii_uppercase()).count();
    if tag == 0 {
        return None;
    }
    name.push_str(std::str::from_utf8(&rest[..tag]).expect("ASCII uppercase is UTF-8"));
    rest = &rest[tag..];
    // The index separator, then the digits, then the closing `__`. The
    // name's own length is the one advance — the caller moves by what it
    // matched, never by a second counter that could drift from it.
    if rest.first() != Some(&b'_') {
        return None;
    }
    name.push('_');
    rest = &rest[1..];
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    name.push_str(std::str::from_utf8(&rest[..digits]).expect("ASCII digits are UTF-8"));
    rest = &rest[digits..];
    if rest.first() != Some(&b'_') || rest.get(1) != Some(&b'_') {
        return None;
    }
    name.push_str("__");
    Some(name)
}

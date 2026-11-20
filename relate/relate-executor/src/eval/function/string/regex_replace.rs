//! Java `Matcher.appendReplacement`'s replacement grammar — the syntax
//! Neo4j's `string.regexReplace` follows — as one module, stated once
//! here: a backslash escapes the next character (so `\$` is a literal
//! dollar), and an unescaped `$` starts a group reference, `$1` or
//! `${name}`; anything else after the `$` is an illegal group reference,
//! and an unterminated `${name}` is the missing-`}` rejection, fired at
//! exhaustion. A group number past the pattern's group count, and a name
//! the pattern never declared, are rejections; a group that exists — by
//! number or by name — but did not participate in the match expands to
//! the empty string, the one deviation from Java (which appends literal
//! `null` there; Neo4j dev answers `"bc"` for
//! `string.regexReplace('abc', '(a)|(x)', '$2')`, REL8-1061). The regex
//! crate's own replacement syntax differs — there `\$` has no meaning
//! and only `$$` is a literal dollar — so the expansion lives here, not
//! in the crate.

use relate_core::{Error, Result};

/// Applies `replacement` to every match of `pattern` in `input`, in
/// match order — the module's one entry, which `bi_string_regexreplace`
/// calls after charging its allocation bound.
pub(super) fn replace_all_java(
    pattern: &regex::Regex,
    input: &str,
    replacement: &str,
) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    for captures in pattern.captures_iter(input) {
        let whole = captures.get(0).expect("a match always has capture group 0");
        out.push_str(&input[last..whole.start()]);
        expand_java_replacement(replacement, pattern, &captures, &mut out)?;
        last = whole.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

/// One scanned `$`-reference: the digits of `$1` or the name inside
/// `${name}`.
enum GroupRef {
    Numbered(usize),
    Named(String),
}

/// Scan one group reference at the cursor (the `$` already consumed).
fn scan_group_reference(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<GroupRef> {
    match chars.peek().copied() {
        Some('{') => {
            chars.next();
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('}') => return Ok(GroupRef::Named(name)),
                    Some(ch) => name.push(ch),
                    None => {
                        return Err(Error::invalid_argument_value(
                            "named capturing group is missing trailing '}'",
                        ))
                    }
                }
            }
        }
        Some(next) if next.is_ascii_digit() => {
            let digits: String =
                std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_digit())).collect();
            let index: usize = digits.parse().map_err(|_| {
                Error::invalid_argument_value(format!("capturing group too large: {digits}"))
            })?;
            Ok(GroupRef::Numbered(index))
        }
        _ => Err(Error::invalid_argument_value("illegal group reference")),
    }
}

/// The participation policy, stated once for both reference forms: a
/// group that participated contributes its text, one that did not
/// expands to the empty string (the module doc's deviation).
fn group_text(matched: Option<regex::Match<'_>>) -> &str {
    matched.map_or("", |matched| matched.as_str())
}

/// Expand `replacement` against one match's `captures`, appending to
/// `out` — the loop the module doc's grammar describes: escape, literal,
/// or `$`-reference.
fn expand_java_replacement(
    replacement: &str,
    pattern: &regex::Regex,
    captures: &regex::Captures<'_>,
    out: &mut String,
) -> Result<()> {
    let mut chars = replacement.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(escaped) => out.push(escaped),
                None => {
                    return Err(Error::invalid_argument_value(
                        "character to be escaped is missing",
                    ))
                }
            },
            '$' => {
                let text = match scan_group_reference(&mut chars)? {
                    GroupRef::Named(name) => {
                        // The pattern's own name table is the one
                        // authority: a declared name that did not
                        // participate takes the shared empty-string
                        // policy, a name the pattern never declared is
                        // the rejection.
                        if pattern
                            .capture_names()
                            .any(|declared| declared == Some(name.as_str()))
                        {
                            group_text(captures.name(&name))
                        } else {
                            return Err(Error::invalid_argument_value(format!(
                                "no group with name {name}"
                            )));
                        }
                    }
                    GroupRef::Numbered(index) => {
                        if index >= pattern.captures_len() {
                            return Err(Error::invalid_argument_value(format!("no group {index}")));
                        }
                        group_text(captures.get(index))
                    }
                };
                out.push_str(text);
            }
            literal => out.push(literal),
        }
    }
    Ok(())
}

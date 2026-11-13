use super::*;

// TODO(CIP-261): an interpolated literal is scanned three times -- once by
// the lexer (`Lexer::read_interpolation`), once here to split text from
// `{...}` embeds, and once more when each embed is re-tokenized by
// `parse_expression_str_with_options`. Having the lexer emit real tokens for
// the body (an
// open/expression/close triple) would collapse this to a single scan; that
// reshape of the token stream is deferred, not done in this change.
/// Split a CIP-261 interpolation body (the [`TokenType::InterpString`]
/// payload) into text and expression parts. `\{`/`\}` are escaped literal
/// braces; a top-level `{` starts an expression segment that runs to its
/// matching `}` (nesting, inner strings and backtick-quoted identifiers
/// respected), sub-parsed with [`parse_expression_str_with_options`] under
/// the caller's [`crate::lexer::ParseOptions`], so an embed inherits the
/// dialect gates of the parse that lexed the enclosing statement.
///
/// The lexer leaves every escape unresolved in the payload, so literal-text
/// escapes are decoded here -- after the two-byte lookahead that tells `\\`
/// (an escaped backslash) apart from `\{` (an escaped literal brace).
pub(super) fn parse_interp_body(
    body: &str,
    options: crate::lexer::ParseOptions,
) -> Result<Vec<crate::ast::InterpPart>, String> {
    use crate::ast::InterpPart;

    let chars: Vec<char> = body.chars().collect();
    let mut parts: Vec<InterpPart> = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += push_interp_escape(&mut text, &chars, i)?;
            continue;
        }
        if c != '{' {
            text.push(c);
            i += 1;
            continue;
        }
        // Expression segment: find the matching close brace.
        let mut depth = 1usize;
        let mut inner_quote: Option<char> = None;
        let mut in_backtick = false;
        let mut j = i + 1;
        while j < chars.len() {
            let d = chars[j];
            if let Some(q) = inner_quote {
                if d == '\\' {
                    j += 2;
                    continue;
                }
                if d == q {
                    inner_quote = None;
                }
            } else if in_backtick {
                // Inside `...` everything -- braces and quotes included -- is
                // an inert identifier name.
                if d == '`' {
                    in_backtick = false;
                }
            } else {
                match d {
                    '\\' => j += 1,
                    '`' => in_backtick = true,
                    '{' | '[' | '(' => depth += 1,
                    '}' | ']' | ')' => {
                        // A closer with nothing open -- e.g. the `)` in
                        // `{)}` -- is malformed. Guard the decrement so
                        // `depth` can never underflow on hostile input.
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                        // Only the outer `}` reaching depth 0 ends it.
                        if d == '}' && depth == 0 {
                            break;
                        }
                    }
                    '\'' | '"' => inner_quote = Some(d),
                    _ => {}
                }
            }
            j += 1;
        }
        if depth != 0 {
            return Err("unterminated { expression } in interpolated string".into());
        }
        let expr_src: String = chars[i + 1..j].iter().collect();
        let expr = parse_expression_str_with_options(&expr_src, options)
            .map_err(|e| format!("invalid expression in interpolated string: {e}"))?;
        if !text.is_empty() {
            parts.push(InterpPart::Text(std::mem::take(&mut text)));
        }
        parts.push(InterpPart::Interp(Box::new(expr)));
        i = j + 1;
    }
    if !text.is_empty() {
        parts.push(InterpPart::Text(text));
    }
    Ok(parts)
}

/// Decode one escape in a CIP-261 literal-text segment, appending the decoded
/// text to `text` and returning how many chars were consumed (`chars[i]` is
/// the `\`). The lexer leaves escapes unresolved so a later scanner cannot
/// confuse a resolved `\\` with the start of `\{`; this is where they resolve
/// again. `\{`/`\}` become the literal brace, known escapes decode, and an
/// unknown escape keeps its two-character form.
fn push_interp_escape(text: &mut String, chars: &[char], i: usize) -> Result<usize, String> {
    let next = chars[i + 1]; // caller guarantees `i + 1 < chars.len()`
    match next {
        '\\' => {
            text.push('\\');
            Ok(2)
        }
        '{' => {
            text.push('{');
            Ok(2)
        }
        '}' => {
            text.push('}');
            Ok(2)
        }
        'n' => {
            text.push('\n');
            Ok(2)
        }
        't' => {
            text.push('\t');
            Ok(2)
        }
        '\'' => {
            text.push('\'');
            Ok(2)
        }
        '"' => {
            text.push('"');
            Ok(2)
        }
        'u' => {
            if i + 6 > chars.len() {
                return Err("invalid unicode escape sequence".into());
            }
            let hex: String = chars[i + 2..i + 6].iter().collect();
            match u32::from_str_radix(&hex, 16) {
                Ok(cp) => {
                    // A UTF-16 surrogate pair escape (`𐀀`) is one
                    // supplementary code point, same as in a plain string
                    // literal (see the lexer's `surrogate_pair_codepoint`).
                    if let Some(pair) = interp_surrogate_pair(cp, chars, i) {
                        text.push(pair.0);
                        return Ok(pair.1);
                    }
                    if let Some(ch) = char::from_u32(cp) {
                        text.push(ch);
                    }
                    Ok(6)
                }
                Err(_) => Err(format!("invalid unicode escape sequence: \\u{hex}")),
            }
        }
        other => {
            text.push('\\');
            text.push(other);
            Ok(2)
        }
    }
}

/// Decode a high-surrogate escape at `chars[i..]` (the `\`) together with an
/// immediately following low-surrogate escape, returning the combined
/// supplementary code point and the 12 chars the pair occupies.
fn interp_surrogate_pair(cp: u32, chars: &[char], i: usize) -> Option<(char, usize)> {
    if !(0xD800..=0xDBFF).contains(&cp) || i + 12 > chars.len() {
        return None;
    }
    if chars[i + 6] != '\\' || chars[i + 7] != 'u' {
        return None;
    }
    let hex: String = chars[i + 8..i + 12].iter().collect();
    let lo = u32::from_str_radix(&hex, 16).ok()?;
    if !(0xDC00..=0xDFFF).contains(&lo) {
        return None;
    }
    char::from_u32(0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)).map(|ch| (ch, 12))
}

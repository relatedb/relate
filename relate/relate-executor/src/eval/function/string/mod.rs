//! String builtins: case mapping, the trim family, replace/split/substring,
//! `string.*`, `normalize`, and the length variants.

use std::collections::HashSet;
use std::sync::Arc;

use relate_core::{Error, ErrorKind, Result, Value};
use unicode_normalization::UnicodeNormalization;

use crate::overlay::ExecGraph;

use super::super::string::{
    char_range, charge_unicode_expansion, str_unary_charged, string_arg, trim_function,
    trim_spec_function, TrimMode,
};
use super::render_scalar;

pub(super) fn bi_string_interp(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // A lone STRING part (`f"{s}"`) is the result itself, shared.
    if let [Value::String(s)] = vals.as_slice() {
        return Ok(Value::String(Arc::clone(s)));
    }
    let mut out = String::new();
    // The result string is one retaining container; the guard owns its
    // running total (each part is built and retained) and the cap check.
    let mut retained = graph.retain_bytes(0)?;
    for v in &vals {
        // The scalar renderer shares its `Arc<str>` payload; keep the borrow
        // instead of copying it per part.
        let rendered: Arc<str> = match v {
            Value::Null => return Ok(Value::Null),
            Value::Vector { .. } => Arc::from(v.to_display_string()),
            other => {
                let Some(rendered) = render_scalar(other) else {
                    return Err(Error::ty(
                        "InvalidArgumentValue: string interpolation cannot coerce value to STRING",
                    ));
                };
                rendered
            }
        };
        retained.add_allocation(graph, relate_core::str_logical_bytes(&rendered))?;
        out.push_str(&rendered);
    }
    Ok(Value::String(out.into()))
}

pub(super) fn bi_normalize(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok({
        let Some(value) = vals.first() else {
            return Err(Error::semantic("normalize() requires an argument"));
        };
        match value {
            Value::Null => Value::Null,
            Value::String(s) => {
                // ADR-0463: NFKC/NFKD can expand a string by ~12× in bytes,
                // so charge the conservative bound before the allocation.
                charge_unicode_expansion(graph, s)?;
                let form = vals.get(1).and_then(Value::as_string).unwrap_or("NFC");
                let normalized: String = match form.to_ascii_uppercase().as_str() {
                    "NFD" => s.nfd().collect(),
                    "NFKC" => s.nfkc().collect(),
                    "NFKD" => s.nfkd().collect(),
                    _ => s.nfc().collect(),
                };
                Value::String(normalized.into())
            }
            _ => return Err(Error::ty("normalize() requires a string")),
        }
    })
}

pub(super) fn bi_char_length(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::String(s)) => Value::Int(s.chars().count() as i64),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("char_length() needs a string")),
    })
}

pub(super) fn bi_octet_length(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match vals.first() {
        Some(Value::String(s)) => Value::Int(s.len() as i64),
        Some(Value::Null) | None => Value::Null,
        _ => return Err(Error::ty("octet_length() needs a string")),
    })
}

pub(super) fn bi_lower(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    str_unary_charged(graph, &vals, |s| s.to_lowercase())
}

pub(super) fn bi_upper(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    str_unary_charged(graph, &vals, |s| s.to_uppercase())
}

pub(super) fn bi_trim(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_function(&vals, TrimMode::Both, true)
}

pub(super) fn bi_trim_leading(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_function(&vals, TrimMode::Leading, true)
}

pub(super) fn bi_trim_trailing(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    trim_function(&vals, TrimMode::Trailing, true)
}

pub(super) fn bi_trim_spec(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_spec_function(&vals)
}

pub(super) fn bi_btrim(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_function(&vals, TrimMode::Both, false)
}

pub(super) fn bi_ltrim(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_function(&vals, TrimMode::Leading, false)
}

pub(super) fn bi_rtrim(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    trim_function(&vals, TrimMode::Trailing, false)
}

pub(super) fn bi_replace(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok({
        let s = string_arg(&vals, 0)?;
        let from = string_arg(&vals, 1)?;
        let to = string_arg(&vals, 2)?;
        match (s, from, to) {
            (Some(s), Some(f), Some(t)) => {
                let limit = match vals.get(3) {
                    Some(Value::Int(value)) if *value >= 0 => {
                        usize::try_from(*value).map_err(|_| {
                            Error::execution("InvalidArgumentValue: replace limit out of range")
                        })?
                    }
                    Some(Value::Null) => return Ok(Value::Null),
                    Some(Value::Int(_)) => {
                        return Err(Error::execution(
                            "InvalidArgumentValue: replace limit must be non-negative",
                        ))
                    }
                    Some(_) => {
                        return Err(Error::invalid_argument_type(
                            "replace limit must be INTEGER",
                        ))
                    }
                    None => usize::MAX,
                };
                // ADR-0463: count the non-overlapping occurrences first (the
                // O(n) pre-scan), then charge the result string's upper bound
                // — the input plus one replacement's worth per occurrence —
                // before `replace`/`replacen` allocates it.
                let occurrences = s.matches(f).count().min(limit);
                graph.charge_expression(relate_core::str_logical_bytes(s).saturating_add(
                    (occurrences as u64).saturating_mul(t.len().max(f.len()) as u64),
                ))?;
                Value::String(s.replacen(f, t, limit).into())
            }
            _ => Value::Null,
        }
    })
}
/// The split result's pre-allocation charge (ADR-0463): the input's logical
/// string bytes plus a value buffer of one slot per byte plus one — the
/// upper bound on part count at any delimiter shape (a one-character
/// delimiter can split at most once per byte; the empty delimiter yields one
/// part per character, never more than the byte count) — plus the delimiter
/// scan's work.
fn charge_split_bound(graph: &ExecGraph<'_>, source: &str, delimiter_bytes: usize) -> Result<()> {
    // The parts buffer and the split substrings are one built value: compose
    // their canonical figures and charge the sum through the seam once. Each
    // part is its own `Arc<str>` allocation, so every one carries the
    // canonical 16-byte string header (`str_logical_bytes`) on top of its
    // payload — the payload total is bounded by the source length, and the
    // header term is charged per part.
    let parts = source.len().saturating_add(1) as u64;
    // The exact-delimiter rule tests every delimiter at every source
    // position, and each test compares bytes: O(source × Σ delimiter
    // lengths). Charge that CPU amplification beside the allocation bound,
    // so a crafted list of long, shared-prefix delimiters is refused instead
    // of pinning a core while every memory charge passes.
    let delimiter_scan = (source.len() as u64).saturating_mul(delimiter_bytes as u64);
    graph.charge_expression(
        crate::workspace::value_buffer_bytes(source.len().saturating_add(1))
            .saturating_add(parts.saturating_mul(relate_core::str_logical_bytes("")))
            .saturating_add(relate_core::str_logical_bytes(source))
            .saturating_add(delimiter_scan),
    )
}

/// The earliest match of any delimiter at or after `from`: one forward pass
/// from `from` that tests the delimiters at each position. Every position a
/// call examines is consumed by the returned part, so the whole split scans
/// the input once — re-running `find` on the remaining tail per delimiter
/// re-scans the tail for every part and is quadratic on inputs where a
/// delimiter candidate never matches (e.g.
/// `split(repeat('a', n), ['a', 'b'])`).
///
/// A match can only start on a char boundary: UTF-8 is self-synchronizing, so
/// a valid delimiter never matches inside a code point's bytes.
fn earliest_delimiter<'d>(
    graph: &ExecGraph<'_>,
    source: &str,
    from: usize,
    delimiters: &[&'d str],
) -> Result<Option<(usize, &'d str)>> {
    for (index, _) in source[from..].char_indices() {
        // This scan is the split's unbounded work: sample the statement's
        // cancellation token per examined position.
        graph.cancel_tick()?;
        let at = from + index;
        let tail = &source[at..];
        if let Some(delimiter) = delimiters
            .iter()
            .find(|delimiter| tail.starts_with(**delimiter))
        {
            return Ok(Some((at, *delimiter)));
        }
    }
    Ok(None)
}

/// Removes every non-overlapping occurrence of `delimiter` from `source`,
/// sampling the cancellation token per examined position (the same
/// granularity as the delimiter scan) rather than once per `str::replace`
/// pass — a long strip is bounded and interruptible.
fn strip_delimiter(graph: &ExecGraph<'_>, source: &str, delimiter: &str) -> Result<String> {
    let mut out = String::with_capacity(source.len());
    let mut copied = 0;
    let mut pos = 0;
    while pos < source.len() {
        graph.cancel_tick()?;
        if source[pos..].starts_with(delimiter) {
            out.push_str(&source[copied..pos]);
            pos += delimiter.len();
            copied = pos;
        } else {
            let ch = source[pos..]
                .chars()
                .next()
                .expect("pos stays on a character boundary");
            pos += ch.len_utf8();
        }
    }
    out.push_str(&source[copied..]);
    Ok(out)
}

/// Splits `source` into one part per code point: the empty-delimiter split
/// (`'a𐀀b'` is `['a','𐀀','b']`). Every slice is taken on a code-point
/// boundary, so multi-byte characters stay whole.
fn split_per_code_point(graph: &ExecGraph<'_>, source: &str) -> Result<Vec<Value>> {
    let mut parts = Vec::with_capacity(source.len());
    for (i, ch) in source.char_indices() {
        graph.cancel_tick()?;
        parts.push(Value::String(source[i..i + ch.len_utf8()].into()));
    }
    Ok(parts)
}

/// The split rule, selected once at decode time from the delimiter shape
/// (Neo4j's `StringValue.split`):
///
/// - a delimiter equal to the whole (non-empty) input yields two empty
///   parts;
/// - an empty delimiter splits per code point, after every non-empty
///   delimiter has been removed from the input (`'a𐀀b'` with `['b','']`
///   is `['a','𐀀']`);
/// - otherwise the input is cut at the earliest occurrence of any
///   delimiter, keeping empty parts.
///
/// Empty input never takes the whole-input rule: `split('', [''])` and
/// `split('', '')` both yield `[]`.
enum SplitMode<'a> {
    /// The whole input equals one delimiter.
    WholeInput,
    /// An empty delimiter is present. `strip` holds the distinct non-empty
    /// delimiters to remove before the remainder is split per code point
    /// (the empty delimiter itself only selects this mode).
    PerCodePoint { strip: Vec<&'a str> },
    /// Cut at the earliest occurrence of any distinct non-empty delimiter.
    Delimiters(Vec<&'a str>),
}

/// Selects the split rule and normalizes the delimiter list in one place:
/// duplicate delimiters are unobservable in every rule, and the empty
/// delimiter only selects [`SplitMode::PerCodePoint`], so neither reaches a
/// scan loop.
fn split_mode<'a>(source: &str, delimiters: Vec<&'a str>) -> SplitMode<'a> {
    let mut seen = HashSet::with_capacity(delimiters.len());
    let mut distinct = Vec::with_capacity(delimiters.len());
    for delimiter in delimiters {
        if seen.insert(delimiter) {
            distinct.push(delimiter);
        }
    }
    if !source.is_empty() && distinct.contains(&source) {
        SplitMode::WholeInput
    } else if distinct.iter().any(|delimiter| delimiter.is_empty()) {
        SplitMode::PerCodePoint {
            strip: distinct
                .into_iter()
                .filter(|delimiter| !delimiter.is_empty())
                .collect(),
        }
    } else {
        SplitMode::Delimiters(distinct)
    }
}

/// Splits `source` by the rule `mode` selected from its delimiter shape.
fn split_on_delimiters(
    graph: &ExecGraph<'_>,
    source: &str,
    mode: SplitMode<'_>,
) -> Result<Vec<Value>> {
    match mode {
        SplitMode::WholeInput => Ok(vec![Value::String("".into()), Value::String("".into())]),
        SplitMode::PerCodePoint { strip } => {
            // The scalar `split(s, '')` shape has nothing to strip: split the
            // input in place instead of copying it first.
            if strip.is_empty() {
                return split_per_code_point(graph, source);
            }
            let mut reduced = String::from(source);
            for delimiter in strip {
                reduced = strip_delimiter(graph, &reduced, delimiter)?;
            }
            split_per_code_point(graph, &reduced)
        }
        SplitMode::Delimiters(delimiters) => {
            let mut parts = Vec::new();
            let mut offset = 0;
            while let Some((index, delimiter)) =
                earliest_delimiter(graph, source, offset, &delimiters)?
            {
                parts.push(Value::String(source[offset..index].into()));
                offset = index + delimiter.len();
            }
            parts.push(Value::String(source[offset..].into()));
            Ok(parts)
        }
    }
}

pub(super) fn bi_split(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    // The row's `ARITY_EXACTLY_TWO` contract rejected a wrong count before
    // any argument was evaluated.
    let source = &vals[0];
    if source.is_null() {
        return Ok(Value::Null);
    }
    let Value::String(source) = source else {
        return Err(Error::invalid_argument_type("split input must be STRING"));
    };
    // Decode and validate the delimiter shape before charging, so a bad
    // shape never pays for the split bound.
    let delimiters: Vec<&str> = match &vals[1] {
        Value::Null => return Ok(Value::Null),
        Value::String(delimiter) => vec![&**delimiter],
        Value::List(delimiters) => {
            if delimiters.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let mut decoded: Vec<&str> = Vec::with_capacity(delimiters.len());
            for delimiter in delimiters.iter() {
                let Value::String(delimiter) = delimiter else {
                    return Err(Error::invalid_argument_type(
                        "split delimiters must be STRING",
                    ));
                };
                decoded.push(&**delimiter);
            }
            decoded
        }
        _ => return Err(Error::invalid_argument_type("split delimiter")),
    };
    // The decoded total delimiter bytes (duplicates included) is the
    // conservative input to the scan charge; the mode normalizes the list
    // once.
    let delimiter_bytes = delimiters.iter().fold(0usize, |total, delimiter| {
        total.saturating_add(delimiter.len())
    });
    let mode = split_mode(source, delimiters);
    // ADR-0463: at most one part per byte (the empty-delimiter split yields
    // one part per character, never more), so the input length plus one is
    // the bound; charge it before the parts are allocated.
    charge_split_bound(graph, source, delimiter_bytes)?;
    Ok(Value::List(
        split_on_delimiters(graph, source, mode)?.into(),
    ))
}
pub(super) fn bi_string_indexof(
    _graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match (vals.first(), vals.get(1)) {
        (Some(Value::Null), _) | (_, Some(Value::Null)) => Value::Null,
        (Some(Value::String(input)), Some(Value::String(needle))) => {
            // Neo4j counts code points, not bytes: `string.indexOf('a😀b','b')`
            // is 2 even though `b` starts at byte 5.
            Value::Int(
                input
                    .find(&**needle)
                    .map_or(-1, |index| input[..index].chars().count() as i64),
            )
        }
        _ => return Err(Error::invalid_argument_type("string.indexOf")),
    })
}
pub(super) fn bi_string_join(graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    Ok(match (vals.first(), vals.get(1)) {
        (Some(Value::Null), _) | (_, Some(Value::Null)) => Value::Null,
        (Some(Value::List(items)), Some(Value::String(delimiter))) => {
            let strings = items
                .iter()
                .filter(|item| !item.is_null())
                .map(|item| match item {
                    Value::String(value) => Ok(&**value),
                    _ => Err(Error::invalid_argument_type("string.join list item")),
                })
                .collect::<Result<Vec<_>>>()?;
            // ADR-0463: pre-scan the parts' lengths and charge the joined
            // string (its header plus every part and delimiter) before the
            // join allocates it. Delimiters sit between parts only.
            let parts_bytes: usize = strings.iter().map(|part| part.len()).sum();
            let delimiters_bytes = delimiter
                .len()
                .saturating_mul(strings.len().saturating_sub(1));
            graph.charge_expression(
                relate_core::str_logical_bytes("") + parts_bytes as u64 + delimiters_bytes as u64,
            )?;
            Value::String(strings.join(&**delimiter).into())
        }
        _ => return Err(Error::invalid_argument_type("string.join")),
    })
}
pub(super) fn bi_string_regexreplace(
    graph: &ExecGraph<'_>,
    _n: &str,
    vals: Vec<Value>,
) -> Result<Value> {
    Ok(match (vals.first(), vals.get(1), vals.get(2)) {
        (Some(Value::Null), _, _) | (_, Some(Value::Null), _) | (_, _, Some(Value::Null)) => {
            Value::Null
        }
        (
            Some(Value::String(input)),
            Some(Value::String(pattern)),
            Some(Value::String(replacement)),
        ) => {
            // ADR-0463: the replacement allocation happens inside the
            // regex crate and has no interception point, so charge a
            // conservative pre-bound before the call. `(input.len() + 1)`
            // covers an empty-matching pattern (`""`, `a*`), which can
            // match at the end of every input position (n + 1 matches);
            // `(replacement.len() + 1)` covers the inserted text and a
            // separator. The product is saturating, so an extreme pair
            // saturates to the refusal instead of wrapping. This is an
            // upper bound, not an exact size: a pattern that matches
            // nothing still pays it (documented imprecision, ADR-0463).
            graph.charge_expression(
                (input.len() as u64)
                    .saturating_add(1)
                    .saturating_mul((replacement.len() as u64).saturating_add(1)),
            )?;
            let pattern = regex::Regex::new(pattern)
                .map_err(|error| Error::execution(format!("InvalidRegex: {error}")))?;
            Value::String(replace_all_java(&pattern, input, replacement)?.into())
        }
        _ => return Err(Error::invalid_argument_type("string.regexReplace")),
    })
}
/// Applies `replacement` to every match of `pattern` in `input` with Java
/// `Matcher.replaceAll` semantics, which Neo4j's `string.regexReplace` uses.
/// The regex crate's own replacement syntax differs — there `\$` has no
/// meaning and only `$$` is a literal dollar — so expansion is done here.
fn replace_all_java(pattern: &regex::Regex, input: &str, replacement: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    for captures in pattern.captures_iter(input) {
        let whole = captures.get(0).expect("a match always has capture group 0");
        out.push_str(&input[last..whole.start()]);
        expand_java_replacement(replacement, &captures, &mut out)?;
        last = whole.end();
    }
    out.push_str(&input[last..]);
    Ok(out)
}

/// Java `Matcher.appendReplacement`'s replacement syntax: a backslash
/// escapes the next character (so `\$` is a literal dollar), and an
/// unescaped `$` starts a group reference — `$1` or `${name}`. Anything
/// else after `$` is an illegal group reference. A group number past the
/// pattern's group count is an error; a group that exists but did not
/// participate in this match appends Java's literal text `null`.
fn expand_java_replacement(
    replacement: &str,
    captures: &regex::Captures<'_>,
    out: &mut String,
) -> Result<()> {
    let invalid = |reason: String| Error::execution(format!("InvalidArgumentValue: {reason}"));
    let mut chars = replacement.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(escaped) => out.push(escaped),
                None => return Err(invalid("character to be escaped is missing".into())),
            },
            '$' => {
                let group = match chars.peek().copied() {
                    Some('{') => {
                        chars.next();
                        let mut name = String::new();
                        let mut closed = false;
                        for ch in chars.by_ref() {
                            if ch == '}' {
                                closed = true;
                                break;
                            }
                            name.push(ch);
                        }
                        if !closed {
                            return Err(invalid(
                                "named capturing group is missing trailing '}'".into(),
                            ));
                        }
                        captures
                            .name(&name)
                            .map(|matched| matched.as_str())
                            .ok_or_else(|| invalid(format!("no group with name {name}")))?
                    }
                    Some(next) if next.is_ascii_digit() => {
                        let mut digits = String::new();
                        while let Some(next) = chars.peek() {
                            if !next.is_ascii_digit() {
                                break;
                            }
                            digits.push(*next);
                            chars.next();
                        }
                        let index: usize = digits
                            .parse()
                            .map_err(|_| invalid(format!("capturing group too large: {digits}")))?;
                        // `Captures::get` is `None` both for a group number
                        // past the pattern and for a group that merely did
                        // not participate in this match; only the first is
                        // Java's error — the second appends `null`.
                        if index >= captures.len() {
                            return Err(invalid(format!("no group {index}")));
                        }
                        captures
                            .get(index)
                            .map_or("null", |matched| matched.as_str())
                    }
                    _ => return Err(invalid("illegal group reference".into())),
                };
                out.push_str(group);
            }
            literal => out.push(literal),
        }
    }
    Ok(())
}

/// ADR-0175: a `substring()` / `left()` / `right()` start/length argument
/// (Neo4j's `asIntExact(allowFloats=true)`, CypherFunctions.java L1748):
/// an integer passes through; a float converts with Java `(long)`-cast
/// semantics (truncate toward zero, NaN → 0, saturate at the i64 limits);
/// the result must lie in [0, 2147483647] — a negative or beyond-int32
/// value (±Infinity included) raises NumberOutOfRange. A missing or
/// non-numeric argument keeps the historical `default`; a null one never
/// reaches here ([`null_index_arg`] returns the call's null first).
fn string_index_arg(v: Option<&Value>, default: i64, what: &str) -> Result<i64> {
    let Some(n) = super::super::numeric::int_arg(v) else {
        return Ok(default);
    };
    if !(0..=i64::from(i32::MAX)).contains(&n) {
        // ADR-0175's 22N03 mapping: NumberOutOfRange is the ArgumentError
        // class (live 2026.10.1: Neo.ClientError.Statement.ArgumentError).
        return Err(Error::argument_value_kind(
            ErrorKind::NumberOutOfRange,
            what,
        ));
    }
    Ok(n)
}

/// A null start/length argument nulls the whole call *before* a sibling
/// argument's range check, and the string argument's type check still runs
/// first. Live Neo4j 2026.10.1: `left('HELLO', null)`,
/// `substring('HELLO', null, 2^40)` and `substring('HELLO', -1, null)` are
/// all null, while `left(1, null)` is the string type error.
fn null_index_arg(vals: &[Value]) -> bool {
    vals.iter().skip(1).any(Value::is_null)
}

/// The shared prologue of the index-taking string calls — the string
/// argument's extraction (`what` is the call's own non-string type error)
/// then the null prescan: a null start/length nulls the whole call
/// *before* a sibling argument's range check. `Ok(None)` is the nulled
/// call.
fn string_call_arg<'a>(vals: &'a [Value], what: &str) -> Result<Option<&'a str>> {
    let s = match vals.first() {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s,
        _ => return Err(Error::ty(what)),
    };
    if null_index_arg(vals) {
        return Ok(None);
    }
    Ok(Some(s))
}

/// `left()`/`right()`'s shared prefix: the prologue (their string type
/// error is the shared `expected string argument` text) plus the length
/// argument's parse. `Ok(None)` is the nulled call.
fn string_and_length<'a>(vals: &'a [Value], fn_name: &str) -> Result<Option<(&'a str, usize)>> {
    let Some(s) = string_call_arg(vals, "expected string argument")? else {
        return Ok(None);
    };
    let n = string_index_arg(vals.get(1), 0, &format!("{fn_name} length"))? as usize;
    Ok(Some((s, n)))
}

pub(super) fn bi_substring(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    let Some(s) = string_call_arg(&vals, "substring() needs a string")? else {
        return Ok(Value::Null);
    };
    let start = string_index_arg(vals.get(1), 0, "substring() start")? as usize;
    // Without an integer length the substring runs to the end of the string.
    let len = string_index_arg(vals.get(2), i64::MAX, "substring() length")? as usize;
    Ok(Value::String(
        char_range(s, start..start.saturating_add(len)).into(),
    ))
}
pub(super) fn bi_left(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    let Some((s, n)) = string_and_length(&vals, "left()")? else {
        return Ok(Value::Null);
    };
    Ok(Value::String(char_range(s, 0..n).into()))
}
pub(super) fn bi_right(_graph: &ExecGraph<'_>, _n: &str, vals: Vec<Value>) -> Result<Value> {
    let Some((s, n)) = string_and_length(&vals, "right()")? else {
        return Ok(Value::Null);
    };
    Ok(Value::String(
        char_range(s, s.chars().count().saturating_sub(n)..usize::MAX).into(),
    ))
}

#[cfg(test)]
mod tests;

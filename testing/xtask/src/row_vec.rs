//! Guard against new unbounded `Vec<Row>` operators in `relate-executor`
//! (ADR-0555). A non-test fn whose return type contains `Vec<Row>`
//! materializes memory proportional to output cardinality, so every such
//! fn must be named in the committed allowlist with a reason; a stale
//! allowlist entry fails too, so the list only shrinks.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Executor sources scanned, relative to the workspace root.
pub const SCAN_DIR: &str = "relate/relate-executor/src";
/// Committed allowlist, relative to the workspace root.
pub const ALLOWLIST: &str = "testing/xtask/row_vec_allowlist.txt";

/// Blank out comments and string/char literals, keeping length and newlines.
fn mask(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let end = if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |p| i + p)
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            src[i..].find("*/").map_or(b.len(), |p| i + p + 2)
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            (j + 1).min(b.len())
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            i + 3
        } else if b[i] == b'\'' && b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
            i + 4
        } else {
            i += 1;
            continue;
        };
        for c in &mut out[i..end] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
        i = end;
    }
    String::from_utf8(out).expect("masking preserves ASCII structure")
}

/// Index just past the `}` closing the `{` at `open`.
fn skip_block(s: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (i, c) in s[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + i + 1;
                }
            }
            _ => {}
        }
    }
    s.len()
}

/// Remove every `#[cfg(test)]` item (a `mod` or fn body, or a `;` item).
fn strip_cfg_test(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..p]);
        let after = &rest[p + "#[cfg(test)]".len()..];
        let stop = after.find(['{', ';']).unwrap_or(after.len());
        let cut = if after[stop..].starts_with('{') {
            skip_block(after, stop)
        } else {
            (stop + 1).min(after.len())
        };
        rest = &after[cut..];
    }
    out.push_str(rest);
    out
}

/// The whitespace-stripped return type of the signature text after the fn
/// name (generics, parameter list, `-> Ret`, optional `where`), or `None`
/// without one. The parameter list is skipped by paren matching, so an
/// `Fn(..) -> Vec<Row>` bound or parameter is not the fn's own return type.
fn return_type(sig: &str) -> Option<String> {
    let mut angle = 0usize;
    let mut prev = ' ';
    let mut open = None;
    for (i, c) in sig.char_indices() {
        match c {
            '<' => angle += 1,
            '>' if prev != '-' => angle = angle.saturating_sub(1),
            '(' if angle == 0 => {
                open = Some(i);
                break;
            }
            _ => {}
        }
        prev = c;
    }
    let open = open?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in sig[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let rest = sig[close?..].trim_start().strip_prefix("->")?;
    Some(before_where_clause(rest).split_whitespace().collect())
}

/// `ty` cut at a whole-word `where` (preceded by start, whitespace, `>` or
/// `)`, followed by whitespace, `{` or the end), so an identifier that merely
/// contains the letters (`Elsewhere`) is not truncated.
fn before_where_clause(ty: &str) -> &str {
    ty.match_indices("where")
        .find(|&(p, w)| {
            let before = ty[..p].chars().next_back();
            let after = ty[p + w.len()..].chars().next();
            before.is_none_or(|c| c.is_whitespace() || c == '>' || c == ')')
                && after.is_none_or(|c| c.is_whitespace() || c == '{')
        })
        .map_or(ty, |(p, _)| &ty[..p])
}

/// Types that carry an unbounded `Vec<Row>` in a field or variant, so a fn
/// returning one materializes rows exactly as one returning `Vec<Row>` does.
/// A drift test checks each still names `Vec<Row>` in its definition.
pub const ROW_VEC_CARRIERS: [(&str, &str); 2] = [
    ("FrontierRows", "exec/kernels/frontier_run.rs"),
    ("LadderRun", "exec/match_clause.rs"),
];

/// Whether `word` occurs in `ty` as a whole identifier.
fn mentions_word(ty: &str, word: &str) -> bool {
    ty.match_indices(word).any(|(p, w)| {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        !ty[..p].chars().next_back().is_some_and(ident)
            && !ty[p + w.len()..].chars().next().is_some_and(ident)
    })
}

/// Whether a whitespace-stripped type names `Vec<Row>` anywhere (whole-word
/// `Vec`, so `SmallVec<Row>` is not one) or a [`ROW_VEC_CARRIERS`] type.
fn mentions_row_vec(ty: &str) -> bool {
    ty.match_indices("Vec<Row>").any(|(p, _)| {
        !ty[..p]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    }) || ROW_VEC_CARRIERS
        .iter()
        .any(|(name, _)| mentions_word(ty, name))
}

/// Names of fns in `src` (outside `#[cfg(test)]`) whose return type
/// contains `Vec<Row>` anywhere (`Option<Vec<Row>>`, `Result<(Vec<Row>, X)>`, ...) or names a
/// [`ROW_VEC_CARRIERS`] type.
pub fn row_vec_fns(src: &str) -> BTreeSet<String> {
    let code = strip_cfg_test(&mask(src));
    let mut found = BTreeSet::new();
    for (p, _) in code.match_indices("fn ") {
        if code[..p]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let sig = &code[p + 3..];
        let name: String = sig
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some(end) = sig.find(['{', ';']) else {
            continue;
        };
        let Some(ret) = return_type(&sig[name.len()..end]) else {
            continue;
        };
        if mentions_row_vec(&ret) {
            found.insert(name);
        }
    }
    found
}

fn is_test_file(rel: &str) -> bool {
    rel.contains("test_support/")
        || rel
            .rsplit('/')
            .next()
            .is_some_and(|f| f == "tests.rs" || f.ends_with("_tests.rs"))
}

fn collect_files(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_files(&path, root, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            if !is_test_file(&rel) {
                out.push(rel);
            }
        }
    }
    Ok(())
}

/// Every `<file>::<fn>` site under `SCAN_DIR`, file relative to `SCAN_DIR`.
fn scan(root: &Path) -> Result<BTreeSet<String>, String> {
    let base = root.join(SCAN_DIR);
    let mut files = Vec::new();
    collect_files(&base, &base, &mut files)?;
    let mut sites = BTreeSet::new();
    for rel in files {
        let src = fs::read_to_string(base.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        sites.extend(row_vec_fns(&src).into_iter().map(|f| format!("{rel}::{f}")));
    }
    Ok(sites)
}

/// Allowlisted sites: lines of `<file>::<fn> # <reason>`; `#` lines and blanks skipped.
pub fn parse_allowlist(text: &str) -> Result<BTreeSet<String>, String> {
    let mut sites = BTreeSet::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (site, reason) = line
            .split_once(" # ")
            .ok_or_else(|| format!("allowlist line lacks ` # <reason>`: {line}"))?;
        if reason.trim().is_empty() {
            return Err(format!("allowlist line has an empty reason: {line}"));
        }
        sites.insert(site.trim().to_string());
    }
    Ok(sites)
}

/// Violations: found-but-unlisted and listed-but-missing sites.
pub fn diff(found: &BTreeSet<String>, allowed: &BTreeSet<String>) -> Vec<String> {
    let new = found
        .difference(allowed)
        .map(|s| format!("{s}: returns Vec<Row> and is not in {ALLOWLIST} (ADR-0555)"));
    let stale = allowed
        .difference(found)
        .map(|s| format!("{s}: allowlisted in {ALLOWLIST} but no longer exists; remove it"));
    new.chain(stale).collect()
}

/// Run the guard against the workspace at `root`.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let list = fs::read_to_string(root.join(ALLOWLIST)).map_err(|e| format!("{ALLOWLIST}: {e}"))?;
    Ok(diff(&scan(root)?, &parse_allowlist(&list)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "
fn ok() -> usize { 1 }
fn listed(a: u8) -> Result<Vec<Row>> { todo!() }
#[cfg(test)]
mod tests { fn helper() -> Vec<Row> { vec![] } }
";

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowlisted_fns_pass_and_test_fns_are_ignored() {
        let found = row_vec_fns(SRC);
        assert_eq!(found, set(["listed"].as_slice()));
        assert!(diff(&found, &set(["listed"].as_slice())).is_empty());
    }

    #[test]
    fn new_fn_returning_result_vec_row_fails() {
        let src = format!(
            "{SRC}\npub(crate) fn fresh(\n    x: u8,\n) -> Result<Vec<Row>> {{ todo!() }}\n"
        );
        let found = row_vec_fns(&src);
        let v = diff(&found, &set(["listed"].as_slice()));
        assert_eq!(v.len(), 1);
        assert!(v[0].starts_with("fresh:"));
    }

    #[test]
    fn any_return_type_nesting_vec_row_is_flagged() {
        let src = "
fn a() -> Option<Vec<Row>> { todo!() }
fn b() -> Result<Option<Vec<Row>>> { todo!() }
fn c() -> Result<(Vec<Row>, X)> { todo!() }
fn d() -> Result<Vec<Row>, E> { todo!() }
fn e<F: Fn(u8) -> Vec<Row>>(f: F) -> Result<(Vec<Row>, X)> where F: Copy { todo!() }
fn g<T>(x: T) -> Result<Vec<  Row  >> { todo!() }
fn fine1(f: impl Fn() -> Vec<Row>) -> usize { 0 }
fn fine2() -> Vec<RowId> { todo!() }
fn fine3() -> Option<SmallVec<Row>> { todo!() }
fn fine4() -> Result<Vec<Rows>> { todo!() }
";
        assert_eq!(
            row_vec_fns(src),
            set(["a", "b", "c", "d", "e", "g"].as_slice())
        );
    }

    #[test]
    fn carrier_types_are_flagged_whole_word() {
        let src = "
fn a() -> Result<FrontierRows> { todo!() }
fn b() -> Result<LadderRun> { todo!() }
fn fine() -> Result<LadderRunner> { todo!() }
";
        assert_eq!(row_vec_fns(src), set(["a", "b"].as_slice()));
    }

    #[test]
    fn carriers_still_hold_a_row_vec() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (name, file) in ROW_VEC_CARRIERS {
            let src = fs::read_to_string(root.join(SCAN_DIR).join(file)).expect("carrier file");
            let at = src
                .find(&format!("struct {name}"))
                .or_else(|| src.find(&format!("enum {name}")))
                .unwrap_or_else(|| panic!("{name} defined in {file}"));
            let body = &src[at..];
            let body = &body[..body.find("\n}\n").expect("closing brace")];
            assert!(
                body.contains("Vec<Row>"),
                "{name} no longer carries Vec<Row>"
            );
        }
    }

    #[test]
    fn where_cut_is_whole_word_only() {
        let src = "
fn a() -> Elsewhere<Vec<Row>> { todo!() }
fn b<T>() -> Vec<Row> where T: X { todo!() }
fn fine() -> Vec<RowId> where T: Row { todo!() }
";
        assert_eq!(row_vec_fns(src), set(["a", "b"].as_slice()));
    }

    #[test]
    fn stale_allowlist_entry_fails() {
        let v = diff(&row_vec_fns(SRC), &set(["listed", "gone"].as_slice()));
        assert_eq!(v.len(), 1);
        assert!(v[0].starts_with("gone:"));
    }

    #[test]
    fn allowlist_requires_reason() {
        assert!(parse_allowlist("a.rs::f\n").is_err());
        assert_eq!(
            parse_allowlist("# c\na.rs::f # why\n").unwrap(),
            set(["a.rs::f"].as_slice())
        );
    }
}

//! The pure half: input validators and the closed link vocabulary. Every
//! player-typed value passes through here before it reaches a statement
//! parameter, and a link type is only ever one of [`LinkKind::ALL`] —
//! the one value that is spliced into statement text.

/// Longest idea title, in characters.
pub(crate) const MAX_TITLE: usize = 120;
/// Longest tag, in characters.
pub(crate) const MAX_TAG: usize = 32;
/// Most tags one `add` takes.
pub(crate) const MAX_TAGS: usize = 8;
/// Most rows a listing prints; the read asks for one more to say so.
pub(crate) const MAX_ROWS: usize = 200;

/// A title: trimmed, 1..=[`MAX_TITLE`] characters, no control characters
/// (so nothing stored can carry an escape sequence to a terminal).
pub(crate) fn title(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("an idea needs a title".to_string());
    }
    if t.chars().count() > MAX_TITLE {
        return Err(format!("a title is at most {MAX_TITLE} characters"));
    }
    if t.chars().any(char::is_control) {
        return Err("a title cannot contain control characters".to_string());
    }
    Ok(t.to_string())
}

/// A tag: lowercased, 1..=[`MAX_TAG`] characters of `a-z 0-9 - _`.
pub(crate) fn tag(raw: &str) -> Result<String, String> {
    let t = raw.trim().to_ascii_lowercase();
    let ok = !t.is_empty()
        && t.chars().count() <= MAX_TAG
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(t)
    } else {
        Err(format!(
            "a tag is 1-{MAX_TAG} characters of a-z, 0-9, '-' and '_'"
        ))
    }
}

/// The tags of one `add`: each validated, duplicates collapsed in order,
/// at most [`MAX_TAGS`] distinct.
pub(crate) fn tags(raw: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        let t = tag(r)?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > MAX_TAGS {
        return Err(format!("at most {MAX_TAGS} tags per idea"));
    }
    Ok(out)
}

/// The closed relationship vocabulary between two ideas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkKind {
    RelatedTo,
    InspiredBy,
    Complements,
    VariantOf,
    SameData,
}

impl LinkKind {
    pub(crate) const ALL: [LinkKind; 5] = [
        LinkKind::RelatedTo,
        LinkKind::InspiredBy,
        LinkKind::Complements,
        LinkKind::VariantOf,
        LinkKind::SameData,
    ];

    /// The relationship type as stored — the one spelling.
    pub(crate) fn name(self) -> &'static str {
        match self {
            LinkKind::RelatedTo => "RELATED_TO",
            LinkKind::InspiredBy => "INSPIRED_BY",
            LinkKind::Complements => "COMPLEMENTS",
            LinkKind::VariantOf => "VARIANT_OF",
            LinkKind::SameData => "SAME_DATA",
        }
    }

    /// Parse a spelling: case-insensitive, `-` and space read as `_`.
    /// Derived from [`Self::name`], never a second table.
    pub(crate) fn parse(raw: &str) -> Result<LinkKind, String> {
        let norm: String = raw
            .trim()
            .chars()
            .map(|c| match c {
                '-' | ' ' => '_',
                c => c.to_ascii_uppercase(),
            })
            .collect();
        LinkKind::ALL
            .into_iter()
            .find(|k| k.name() == norm)
            .ok_or_else(|| {
                let all: Vec<&str> = LinkKind::ALL.iter().map(|k| k.name()).collect();
                format!("not a link type; use one of: {}", all.join(", "))
            })
    }
}

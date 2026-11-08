//! The embedded seed set: a few dozen of the maintainers' own product
//! ideas (title, one-line summary, category) with the links the source
//! notes drew between them. The asset is data, parsed strictly; a
//! malformed entry fails the seed verb, never a silently shorter set.

use crate::model::LinkKind;
use serde::Deserialize;

const ASSET: &str = include_str!("../seed/ideas.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedIdea {
    pub(crate) name: String,
    pub(crate) summary: String,
    pub(crate) category: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedLinkRaw {
    from: String,
    to: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedRaw {
    ideas: Vec<SeedIdea>,
    links: Vec<SeedLinkRaw>,
}

#[derive(Debug)]
pub(crate) struct SeedLink {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: LinkKind,
}

#[derive(Debug)]
pub(crate) struct Seed {
    pub(crate) ideas: Vec<SeedIdea>,
    pub(crate) links: Vec<SeedLink>,
}

/// Parse and cross-check the embedded asset: every link names two seed
/// ideas and a vocabulary kind, titles are unique.
pub(crate) fn load() -> Result<Seed, String> {
    parse(ASSET)
}

pub(crate) fn parse(text: &str) -> Result<Seed, String> {
    let raw: SeedRaw = serde_json::from_str(text).map_err(|e| format!("the seed asset: {e}"))?;
    let mut seen: Vec<String> = Vec::new();
    let mut ideas = Vec::new();
    for mut idea in raw.ideas {
        // The stored title is the validated, trimmed one — the same
        // canonical form `add` stores.
        idea.name = crate::model::title(&idea.name)?;
        let key = idea.name.to_lowercase();
        if seen.contains(&key) {
            return Err(format!("the seed asset repeats '{}'", idea.name));
        }
        seen.push(key);
        ideas.push(idea);
    }
    let mut links = Vec::new();
    for l in raw.links {
        let from = crate::model::title(&l.from)?;
        let to = crate::model::title(&l.to)?;
        for end in [&from, &to] {
            if !seen.contains(&end.to_lowercase()) {
                return Err(format!("the seed asset links unknown idea '{end}'"));
            }
        }
        let kind = LinkKind::parse(&l.kind)?;
        links.push(SeedLink { from, to, kind });
    }
    Ok(Seed { ideas, links })
}

//! The verbs: `add`, `link`, `related`, `list`, `show`, `seed` — each a
//! few `read_cypher` / `batch_cypher` calls over the host session; the
//! child holds no state. User mistakes are `Err` (the host prints the
//! text and exits 1); results are one summary line plus row lines, which
//! ride the report's `starters` channel because the host prints each
//! starter on its own line while flattening newlines in the summary.

use crate::model::{self, LinkKind, MAX_ROWS};
use crate::seed;
use crate::session::HostSession;
use relate_wire::mcp::{BATCH_CYPHER, READ_CYPHER, REPORT_OUTPUT};
use serde::Deserialize;
use serde_json::{json, Value};

/// The host's request envelope, keyed by the manifest's flag names.
#[derive(Debug, Deserialize)]
pub struct Envelope {
    /// The verb (`--command`).
    pub command: String,
    /// The `IDEA` positional: a title for `add`, the idea for the rest.
    #[serde(default)]
    pub idea: Option<String>,
    /// `--to`: the link target.
    pub to: String,
    /// `--as`: the link type.
    #[serde(rename = "as")]
    pub link_as: String,
    /// `--tag`, one value per occurrence.
    pub tag: Vec<String>,
}

const VERBS: &str = "add, link, related, list, show, seed";

/// One command, one turn.
pub fn run(session: &mut impl HostSession, env: &Envelope) -> Result<(), String> {
    let mut store = Store { session };
    match env.command.trim().to_ascii_lowercase().as_str() {
        "add" => store.add(need(env, "add")?, &env.tag),
        "link" => store.link(need(env, "link")?, &env.to, &env.link_as),
        "related" => store.related(need(env, "related")?),
        "show" => store.show(need(env, "show")?),
        "list" => store.list(&env.tag),
        "seed" => store.seed(),
        other => Err(format!(
            "'{other}' is not an ideas command; use one of: {VERBS}"
        )),
    }
}

fn need<'e>(env: &'e Envelope, verb: &str) -> Result<&'e str, String> {
    env.idea
        .as_deref()
        .ok_or_else(|| format!("{verb} needs an idea (the positional argument)"))
}

struct Store<'a, S: HostSession> {
    session: &'a mut S,
}

fn statement(text: &str, parameters: Value) -> Value {
    json!({"statement": text, "parameters": parameters})
}

fn cell<'v>(row: &'v Value, key: &str) -> Result<&'v str, String> {
    row.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the row answered no string '{key}': {row}"))
}

fn strings(row: &Value, key: &str) -> Result<Vec<String>, String> {
    row.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("the row answered no list '{key}': {row}"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("the list '{key}' holds a non-string: {v}"))
        })
        .collect()
}

impl<S: HostSession> Store<'_, S> {
    fn read(&mut self, text: &str, parameters: Value) -> Result<Vec<Value>, String> {
        let result = self
            .session
            .call(READ_CYPHER, statement(text, parameters))?;
        result
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "read_cypher answered no rows".to_string())
    }

    fn batch(&mut self, statements: Vec<Value>) -> Result<(), String> {
        self.session
            .call(BATCH_CYPHER, json!({"statements": statements}))?;
        Ok(())
    }

    fn report(&mut self, text: &str, lines: &[String]) -> Result<(), String> {
        self.session
            .call(REPORT_OUTPUT, json!({"text": text, "starters": lines}))?;
        Ok(())
    }

    /// The stored title an idea answers to, case-insensitively.
    fn resolve(&mut self, raw: &str) -> Result<Option<String>, String> {
        let wanted = model::title(raw)?;
        let rows = self.read(
            "MATCH (i:Idea) WHERE toLower(i.name) = toLower($name) \
             RETURN i.name AS name LIMIT 1",
            json!({"name": wanted}),
        )?;
        rows.first()
            .map(|r| cell(r, "name").map(str::to_string))
            .transpose()
    }

    fn existing(&mut self, raw: &str) -> Result<String, String> {
        self.resolve(raw)?
            .ok_or_else(|| format!("no idea called '{}'; add it first", raw.trim()))
    }

    fn add(&mut self, raw: &str, raw_tags: &[String]) -> Result<(), String> {
        let tags = model::tags(raw_tags)?;
        let found = self.resolve(raw)?;
        let name = match &found {
            Some(stored) => stored.clone(),
            None => model::title(raw)?,
        };
        let mut statements = Vec::new();
        if found.is_none() {
            statements.push(statement(
                "CREATE (:Idea {name: $name, source: 'user'})",
                json!({"name": name}),
            ));
        }
        if !tags.is_empty() {
            statements.push(statement(
                "MATCH (i:Idea {name: $name}) UNWIND $tags AS t \
                 MERGE (g:Tag {name: t}) MERGE (i)-[:TAGGED]->(g)",
                json!({"name": name, "tags": tags}),
            ));
        }
        if statements.is_empty() {
            return self.report(&format!("'{name}' is already in the store"), &[]);
        }
        self.batch(statements)?;
        let verb = if found.is_some() { "tagged" } else { "added" };
        let suffix = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        self.report(&format!("{verb} '{name}'{suffix}"), &[])
    }

    fn link(&mut self, from: &str, to: &str, kind: &str) -> Result<(), String> {
        if to.trim().is_empty() {
            return Err("link needs a target: --to <idea>".to_string());
        }
        let kind = LinkKind::parse(kind)?;
        let a = self.existing(from)?;
        let b = self.existing(to)?;
        if a == b {
            return Err("an idea cannot link to itself".to_string());
        }
        // The type is spliced into the statement text: only ever one of
        // the closed vocabulary's names, never player text.
        self.batch(vec![statement(
            &format!(
                "MATCH (a:Idea {{name: $a}}), (b:Idea {{name: $b}}) \
                 MERGE (a)-[:{}]->(b)",
                kind.name()
            ),
            json!({"a": a, "b": b}),
        )])?;
        self.report(&format!("linked '{a}' {} '{b}'", kind.name()), &[])
    }

    /// Both directions of an idea's links, as display lines.
    fn link_lines(&mut self, name: &str) -> Result<Vec<String>, String> {
        let out = self.read(
            &format!(
                "MATCH (a:Idea {{name: $name}})-[r]->(b:Idea) \
                 RETURN type(r) AS rel, b.name AS other ORDER BY rel, other LIMIT {}",
                MAX_ROWS + 1
            ),
            json!({"name": name}),
        )?;
        let inn = self.read(
            &format!(
                "MATCH (a:Idea {{name: $name}})<-[r]-(b:Idea) \
                 RETURN type(r) AS rel, b.name AS other ORDER BY rel, other LIMIT {}",
                MAX_ROWS + 1
            ),
            json!({"name": name}),
        )?;
        let mut lines = Vec::new();
        let truncated = out.len() > MAX_ROWS || inn.len() > MAX_ROWS;
        for (arrow, rows) in [("->", out), ("<-", inn)] {
            for row in rows.iter().take(MAX_ROWS) {
                lines.push(format!(
                    "{arrow} {} {}",
                    cell(row, "rel")?,
                    cell(row, "other")?
                ));
            }
        }
        if truncated {
            lines.push(format!("(truncated at {MAX_ROWS} per direction)"));
        }
        Ok(lines)
    }

    fn related(&mut self, raw: &str) -> Result<(), String> {
        let name = self.existing(raw)?;
        let lines = self.link_lines(&name)?;
        let head = if lines.is_empty() {
            format!("'{name}' has no links yet")
        } else {
            format!("'{name}' links:")
        };
        self.report(&head, &lines)
    }

    fn show(&mut self, raw: &str) -> Result<(), String> {
        let name = self.existing(raw)?;
        let rows = self.read(
            "MATCH (i:Idea {name: $name}) OPTIONAL MATCH (i)-[:TAGGED]->(t:Tag) \
             RETURN i.name AS name, coalesce(i.summary, '') AS summary, \
             coalesce(i.category, '') AS category, collect(t.name) AS tags",
            json!({"name": name}),
        )?;
        let row = rows
            .first()
            .ok_or_else(|| format!("'{name}' vanished mid-read"))?;
        let mut lines = Vec::new();
        let summary = cell(row, "summary")?;
        if !summary.is_empty() {
            lines.push(summary.to_string());
        }
        let category = cell(row, "category")?;
        if !category.is_empty() {
            lines.push(format!("category: {category}"));
        }
        let mut tags = strings(row, "tags")?;
        tags.sort();
        if !tags.is_empty() {
            lines.push(format!("tags: {}", tags.join(", ")));
        }
        lines.extend(self.link_lines(&name)?);
        self.report(cell(row, "name")?, &lines)
    }

    fn list(&mut self, raw_tags: &[String]) -> Result<(), String> {
        let tags = model::tags(raw_tags)?;
        if tags.len() > 1 {
            return Err("list takes at most one --tag".to_string());
        }
        let limit = MAX_ROWS + 1;
        let rows = match tags.first() {
            Some(t) => self.read(
                &format!(
                    "MATCH (i:Idea)-[:TAGGED]->(:Tag {{name: $tag}}) \
                     RETURN i.name AS name, coalesce(i.category, '') AS category \
                     ORDER BY name LIMIT {limit}"
                ),
                json!({"tag": t}),
            )?,
            None => self.read(
                &format!(
                    "MATCH (i:Idea) RETURN i.name AS name, \
                     coalesce(i.category, '') AS category ORDER BY name LIMIT {limit}"
                ),
                json!({}),
            )?,
        };
        let mut lines = Vec::new();
        for row in rows.iter().take(MAX_ROWS) {
            let category = cell(row, "category")?;
            lines.push(if category.is_empty() {
                cell(row, "name")?.to_string()
            } else {
                format!("{} ({category})", cell(row, "name")?)
            });
        }
        let head = match (rows.len(), rows.len() > MAX_ROWS) {
            (0, _) => "no ideas yet; try `--command seed`".to_string(),
            (_, true) => format!("first {MAX_ROWS} ideas:"),
            (n, false) => format!("{n} ideas:"),
        };
        self.report(&head, &lines)
    }

    /// Point every seed title at the stored spelling when the store
    /// already holds the idea in another case, so `seed` and `add` agree
    /// on one case-insensitive key and never create a variant twin.
    fn canonicalise_seed(&mut self, seed: &mut seed::Seed) -> Result<(), String> {
        let lowered: Vec<String> = seed.ideas.iter().map(|i| i.name.to_lowercase()).collect();
        let rows = self.read(
            "MATCH (i:Idea) WHERE toLower(i.name) IN $names \
             RETURN i.name AS name ORDER BY name",
            json!({"names": lowered}),
        )?;
        let mut stored: Vec<(String, String)> = Vec::new();
        for row in &rows {
            let name = cell(row, "name")?;
            let key = name.to_lowercase();
            // ORDER BY makes the pick among pre-existing twins stable.
            if !stored.iter().any(|(k, _)| *k == key) {
                stored.push((key, name.to_string()));
            }
        }
        let canon = |name: &str| {
            let key = name.to_lowercase();
            stored
                .iter()
                .find(|(k, _)| *k == key)
                .map_or_else(|| name.to_string(), |(_, n)| n.clone())
        };
        for idea in &mut seed.ideas {
            idea.name = canon(&idea.name);
        }
        for link in &mut seed.links {
            link.from = canon(&link.from);
            link.to = canon(&link.to);
        }
        Ok(())
    }

    fn seed(&mut self) -> Result<(), String> {
        let mut seed = seed::load()?;
        self.canonicalise_seed(&mut seed)?;
        let ideas: Vec<Value> = seed
            .ideas
            .iter()
            .map(|i| json!({"name": i.name, "summary": i.summary, "category": i.category}))
            .collect();
        let mut statements = vec![statement(
            "UNWIND $ideas AS x MERGE (i:Idea {name: x.name}) \
             ON CREATE SET i.source = 'seed' \
             SET i.summary = x.summary, i.category = x.category",
            json!({"ideas": ideas}),
        )];
        for kind in LinkKind::ALL {
            let legs: Vec<Value> = seed
                .links
                .iter()
                .filter(|l| l.kind == kind)
                .map(|l| json!({"from": l.from, "to": l.to}))
                .collect();
            if legs.is_empty() {
                continue;
            }
            statements.push(statement(
                &format!(
                    "UNWIND $links AS l MATCH (a:Idea {{name: l.from}}), \
                     (b:Idea {{name: l.to}}) MERGE (a)-[:{}]->(b)",
                    kind.name()
                ),
                json!({"links": legs}),
            ));
        }
        self.batch(statements)?;
        self.report(
            &format!(
                "seeded {} ideas and {} links (re-running refreshes, never duplicates)",
                seed.ideas.len(),
                seed.links.len()
            ),
            &[],
        )
    }
}

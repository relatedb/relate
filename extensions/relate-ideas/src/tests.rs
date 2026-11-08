//! Unit tests: validators, the link vocabulary, the seed asset, the
//! package manifest, and the verbs through a recording fake host.

use crate::model::{self, LinkKind, MAX_ROWS, MAX_TAGS, MAX_TITLE};
use crate::seed;
use crate::session::HostSession;
use crate::verbs::{run, Envelope};
use serde_json::{json, Value};
use std::collections::VecDeque;

struct FakeHost {
    reads: VecDeque<Value>,
    calls: Vec<(String, Value)>,
}

impl FakeHost {
    fn new(reads: Vec<Vec<Value>>) -> FakeHost {
        FakeHost {
            reads: reads
                .into_iter()
                .map(|rows| json!({"rows": rows}))
                .collect(),
            calls: Vec::new(),
        }
    }
    fn tool(&self, name: &str) -> Vec<&Value> {
        self.calls
            .iter()
            .filter(|(t, _)| t == name)
            .map(|(_, a)| a)
            .collect()
    }
    fn report(&self) -> &Value {
        self.tool("report_output")[0]
    }
}

impl HostSession for FakeHost {
    fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, String> {
        self.calls.push((tool.to_string(), arguments.clone()));
        if tool == "read_cypher" {
            return Ok(self.reads.pop_front().expect("a scripted read"));
        }
        Ok(json!({}))
    }
}

fn env(command: &str, idea: Option<&str>) -> Envelope {
    Envelope {
        command: command.to_string(),
        idea: idea.map(str::to_string),
        to: String::new(),
        link_as: "RELATED_TO".to_string(),
        tag: vec![],
    }
}

fn named(name: &str) -> Vec<Value> {
    vec![json!({"name": name})]
}

// -- validators ----------------------------------------------------------

#[test]
fn titles_are_trimmed_bounded_and_control_free() {
    assert_eq!(
        model::title("  Graph Clipboard ").unwrap(),
        "Graph Clipboard"
    );
    assert!(model::title("   ").is_err());
    assert!(model::title(&"x".repeat(MAX_TITLE + 1)).is_err());
    assert!(model::title(&"x".repeat(MAX_TITLE)).is_ok());
    assert!(model::title("evil\u{1b}[31m").is_err());
    assert!(model::title("two\nlines").is_err());
}

#[test]
fn tags_normalise_and_refuse_odd_input() {
    assert_eq!(model::tag(" Agents ").unwrap(), "agents");
    assert!(model::tag("").is_err());
    assert!(model::tag("a b").is_err());
    assert!(model::tag("a'}) DETACH DELETE (n").is_err());
    let dup = vec!["a".to_string(), "A".to_string(), "b".to_string()];
    assert_eq!(model::tags(&dup).unwrap(), vec!["a", "b"]);
    let many: Vec<String> = (0..=MAX_TAGS).map(|i| format!("t{i}")).collect();
    assert!(model::tags(&many).is_err());
}

#[test]
fn link_kinds_parse_from_the_one_table() {
    for kind in LinkKind::ALL {
        assert_eq!(LinkKind::parse(kind.name()).unwrap(), kind);
    }
    assert_eq!(
        LinkKind::parse("inspired-by").unwrap(),
        LinkKind::InspiredBy
    );
    assert_eq!(LinkKind::parse("variant of").unwrap(), LinkKind::VariantOf);
    let err = LinkKind::parse("KNOWS]->(x) //").unwrap_err();
    assert!(err.contains("RELATED_TO"), "{err}");
}

// -- the seed asset ---------------------------------------------------------

#[test]
fn the_embedded_seed_parses_and_is_clean() {
    let seed = seed::load().expect("the asset parses");
    assert!(
        (25..=40).contains(&seed.ideas.len()),
        "{}",
        seed.ideas.len()
    );
    assert!(!seed.links.is_empty());
    for idea in &seed.ideas {
        assert!(!idea.summary.is_empty() && idea.summary.chars().count() <= 240);
        assert!(!idea.category.is_empty());
        for text in [&idea.name, &idea.summary, &idea.category] {
            assert!(!text.chars().any(char::is_control), "{text}");
        }
    }
}

#[test]
fn a_malformed_seed_fails_loudly() {
    let bad_link = r#"{"ideas":[{"name":"A","summary":"s","category":"c"}],
        "links":[{"from":"A","to":"Z","kind":"RELATED_TO"}]}"#;
    assert!(seed::parse(bad_link).unwrap_err().contains("unknown idea"));
    let bad_kind = r#"{"ideas":[{"name":"A","summary":"s","category":"c"},
        {"name":"B","summary":"s","category":"c"}],
        "links":[{"from":"A","to":"B","kind":"LOVES"}]}"#;
    assert!(seed::parse(bad_kind).is_err());
    let dup = r#"{"ideas":[{"name":"A","summary":"s","category":"c"},
        {"name":"a","summary":"s","category":"c"}],"links":[]}"#;
    assert!(seed::parse(dup).unwrap_err().contains("repeats"));
    assert!(seed::parse(r#"{"ideas":[],"links":[],"x":1}"#).is_err());
}

// -- the package manifest ------------------------------------------------

#[test]
fn the_package_manifest_matches_the_child() {
    let text = include_str!("../package/extension.json");
    let m: Value = serde_json::from_str(text).expect("the manifest is JSON");
    assert_eq!(m["kind"], "command");
    assert_eq!(m["name"], "ideas");
    assert_eq!(m["executable"], "ideas");
    assert_eq!(m["hidden"], true, "an easter egg stays unlisted");
    assert_eq!(m["protocol"], relate_wire::mcp::MCP_PROTOCOL_VERSION);
    // The envelope the host builds from the flag table decodes: every
    // flag at its declared default, the positional absent.
    let mut envelope = serde_json::Map::new();
    for flag in m["flags"].as_array().expect("flags") {
        envelope.insert(
            flag["name"].as_str().unwrap().to_string(),
            flag["default"].clone(),
        );
    }
    let decoded: Envelope = serde_json::from_value(Value::Object(envelope)).expect("decodes");
    assert_eq!(decoded.command, "list");
    assert!(decoded.idea.is_none());
    // The host keys the positional by its declared name, verbatim.
    assert_eq!(m["positional"]["name"], "idea");
    assert!(LinkKind::parse(&decoded.link_as).is_ok());
    let cargo = include_str!("../Cargo.toml");
    assert!(cargo.contains("name = \"ideas\""));
}

// -- the verbs -----------------------------------------------------------

#[test]
fn add_creates_and_tags_in_one_batch() {
    let mut host = FakeHost::new(vec![vec![]]);
    let mut e = env("add", Some("  Graph Clipboard "));
    e.tag = vec!["Sharing".into(), "sharing".into()];
    run(&mut host, &e).unwrap();
    let batch = host.tool("batch_cypher");
    assert_eq!(batch.len(), 1);
    let stmts = batch[0]["statements"].as_array().unwrap();
    assert_eq!(stmts.len(), 2);
    assert_eq!(stmts[0]["parameters"]["name"], "Graph Clipboard");
    assert_eq!(stmts[1]["parameters"]["tags"], json!(["sharing"]));
    assert_eq!(host.report()["text"], "added 'Graph Clipboard' [sharing]");
}

#[test]
fn add_of_an_existing_title_neither_duplicates_nor_rewrites_case() {
    let mut host = FakeHost::new(vec![named("Graph Clipboard")]);
    run(&mut host, &env("add", Some("graph clipboard"))).unwrap();
    assert!(host.tool("batch_cypher").is_empty());
    assert_eq!(
        host.report()["text"],
        "'Graph Clipboard' is already in the store"
    );
}

#[test]
fn add_refuses_bad_input_before_any_call() {
    for (idea, tags) in [
        (None, vec![]),
        (Some("\u{7}"), vec![]),
        (Some("ok"), vec!["a b".to_string()]),
    ] {
        let mut host = FakeHost::new(vec![]);
        let mut e = env("add", idea);
        e.tag = tags;
        assert!(run(&mut host, &e).is_err());
        assert!(host.calls.is_empty());
    }
}

#[test]
fn link_resolves_both_ends_and_splices_only_a_vocabulary_type() {
    let mut host = FakeHost::new(vec![named("A"), named("B")]);
    let mut e = env("link", Some("a"));
    e.to = "b".into();
    e.link_as = "inspired-by".into();
    run(&mut host, &e).unwrap();
    let stmt = &host.tool("batch_cypher")[0]["statements"][0];
    assert!(stmt["statement"]
        .as_str()
        .unwrap()
        .contains("[:INSPIRED_BY]"));
    assert_eq!(stmt["parameters"], json!({"a": "A", "b": "B"}));
    assert_eq!(host.report()["text"], "linked 'A' INSPIRED_BY 'B'");
}

#[test]
fn link_refuses_missing_targets_self_links_and_off_vocabulary_types() {
    let mut host = FakeHost::new(vec![named("A")]);
    assert!(run(&mut host, &env("link", Some("A"))).is_err(), "no --to");

    let mut e = env("link", Some("A"));
    e.to = "A".into();
    let mut host = FakeHost::new(vec![named("A"), named("A")]);
    assert!(run(&mut host, &e).unwrap_err().contains("itself"));

    let mut host = FakeHost::new(vec![named("A"), vec![]]);
    e.to = "Nope".into();
    assert!(run(&mut host, &e).unwrap_err().contains("no idea called"));

    let mut host = FakeHost::new(vec![]);
    e.to = "B".into();
    e.link_as = "X]->() DETACH DELETE".into();
    assert!(run(&mut host, &e).is_err());
    assert!(host.calls.is_empty());
}

#[test]
fn related_lists_both_directions_one_line_each() {
    let mut host = FakeHost::new(vec![
        named("A"),
        vec![json!({"rel": "COMPLEMENTS", "other": "B"})],
        vec![json!({"rel": "RELATED_TO", "other": "C"})],
    ]);
    run(&mut host, &env("related", Some("a"))).unwrap();
    let r = host.report();
    assert_eq!(r["text"], "'A' links:");
    assert_eq!(
        r["starters"],
        json!(["-> COMPLEMENTS B", "<- RELATED_TO C"])
    );
}

#[test]
fn related_with_no_links_says_so() {
    let mut host = FakeHost::new(vec![named("A"), vec![], vec![]]);
    run(&mut host, &env("related", Some("A"))).unwrap();
    assert_eq!(host.report()["text"], "'A' has no links yet");
}

#[test]
fn list_bounds_its_output() {
    let rows: Vec<Value> = (0..=MAX_ROWS)
        .map(|i| json!({"name": format!("i{i}"), "category": ""}))
        .collect();
    let mut host = FakeHost::new(vec![rows]);
    run(&mut host, &env("list", None)).unwrap();
    let r = host.report();
    assert_eq!(r["starters"].as_array().unwrap().len(), MAX_ROWS);
    assert_eq!(r["text"], format!("first {MAX_ROWS} ideas:"));
}

#[test]
fn list_shows_category_and_filters_by_one_tag() {
    let mut host = FakeHost::new(vec![vec![json!({"name": "A", "category": "Tools"})]]);
    let mut e = env("list", None);
    e.tag = vec!["Agents".into()];
    run(&mut host, &e).unwrap();
    let read = host.tool("read_cypher")[0];
    assert_eq!(read["parameters"]["tag"], "agents");
    assert_eq!(host.report()["starters"], json!(["A (Tools)"]));
    assert_eq!(host.report()["text"], "1 ideas:");

    let mut e = env("list", None);
    e.tag = vec!["a".into(), "b".into()];
    assert!(run(&mut FakeHost::new(vec![]), &e).is_err());
}

#[test]
fn show_prints_summary_category_tags_and_links() {
    let mut host = FakeHost::new(vec![
        named("A"),
        vec![json!({"name": "A", "summary": "S", "category": "C", "tags": ["z", "m"]})],
        vec![],
        vec![],
    ]);
    run(&mut host, &env("show", Some("a"))).unwrap();
    let r = host.report();
    assert_eq!(r["text"], "A");
    assert_eq!(r["starters"], json!(["S", "category: C", "tags: m, z"]));
}

#[test]
fn a_malformed_row_fails_the_turn() {
    let mut host = FakeHost::new(vec![vec![json!({"nom": "A"})]]);
    assert!(run(&mut host, &env("show", Some("A"))).is_err());
}

#[test]
fn seed_is_one_atomic_idempotent_batch() {
    let mut host = FakeHost::new(vec![vec![]]);
    run(&mut host, &env("seed", None)).unwrap();
    let batch = host.tool("batch_cypher");
    assert_eq!(batch.len(), 1, "one all-or-nothing call");
    let stmts = batch[0]["statements"].as_array().unwrap();
    for s in stmts {
        assert!(
            s["statement"].as_str().unwrap().contains("MERGE"),
            "re-seed must not duplicate"
        );
    }
    let seed = seed::load().unwrap();
    assert_eq!(
        stmts[0]["parameters"]["ideas"].as_array().unwrap().len(),
        seed.ideas.len()
    );
    assert!(host.report()["text"]
        .as_str()
        .unwrap()
        .starts_with("seeded "));
}

#[test]
fn an_unknown_verb_names_the_vocabulary() {
    let err = run(&mut FakeHost::new(vec![]), &env("frobnicate", None)).unwrap_err();
    assert!(
        err.contains("add, link, related, list, show, seed"),
        "{err}"
    );
}

#[test]
fn seed_adopts_the_stored_spelling_of_a_case_variant_title() {
    let seed = seed::load().unwrap();
    let first = seed.ideas[0].name.clone();
    let variant = first.to_uppercase();
    assert_ne!(first, variant);
    let mut host = FakeHost::new(vec![named(&variant)]);
    run(&mut host, &env("seed", None)).unwrap();
    let probe = host.tool("read_cypher")[0];
    assert!(probe["parameters"]["names"]
        .as_array()
        .unwrap()
        .contains(&json!(first.to_lowercase())));
    let stmts = host.tool("batch_cypher")[0]["statements"]
        .as_array()
        .unwrap()
        .clone();
    let ideas = stmts[0]["parameters"]["ideas"].as_array().unwrap();
    assert_eq!(
        ideas[0]["name"],
        variant.as_str(),
        "no twin under the seed's case"
    );
    assert!(!ideas.iter().any(|i| i["name"] == first.as_str()));
    // Links follow the stored spelling too.
    let linked = stmts[1..].iter().any(|s| {
        s["parameters"]["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["from"] == variant.as_str() || l["to"] == variant.as_str())
    });
    assert!(linked || seed.links.iter().all(|l| l.from != first && l.to != first));
}

#[test]
fn the_seed_stores_trimmed_titles() {
    let text = r#"{"ideas":[{"name":" A ","summary":"s","category":"c"},
        {"name":"B","summary":"s","category":"c"}],
        "links":[{"from":"A","to":" B","kind":"RELATED_TO"}]}"#;
    let seed = seed::parse(text).unwrap();
    assert_eq!(seed.ideas[0].name, "A");
    assert_eq!(seed.links[0].to, "B");
}

#[test]
fn related_says_when_it_truncates() {
    let rows: Vec<Value> = (0..=MAX_ROWS)
        .map(|i| json!({"rel": "RELATED_TO", "other": format!("o{i}")}))
        .collect();
    let mut host = FakeHost::new(vec![named("A"), rows, vec![]]);
    run(&mut host, &env("related", Some("A"))).unwrap();
    let lines = host.report()["starters"].as_array().unwrap().clone();
    assert_eq!(lines.len(), MAX_ROWS + 1);
    assert_eq!(
        lines.last().unwrap(),
        &json!(format!("(truncated at {MAX_ROWS} per direction)"))
    );
}

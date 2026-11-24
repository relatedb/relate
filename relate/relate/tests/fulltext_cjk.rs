//! Chinese and Japanese text is searchable by the words it contains
//! (REL8-2031 #74): the standard tokenizer splits a Han ideograph and a
//! Hiragana character into one token each and keeps a Katakana run whole
//! (UAX #29's shape, live Neo4j 2026.10.2), and the `cjk` analyzer emits
//! overlapping bigrams over those runs. Both entry points agree.

mod common;

use common::connection;
use common::fulltext::fulltext_agree;
use common::fulltext::sorted_names;
use common::run;

/// The issue-74 store: five CJK texts, indexed once with the default
/// analyzer (`ft`) and once with `fulltext.analyzer: 'cjk'` (`fk`).
fn cjk_store() -> relate::Connection {
    let mut c = connection();
    run(
        &mut c,
        "UNWIND [[1, '中文搜索引擎'], [2, '日本語のテキスト'], [3, 'graph データベース'], \
         [4, 'Relate 图数据库'], [5, '한국어 검색']] AS r \
         CREATE (:D {id: r[0], t: r[1]}), (:K {id: r[0], t: r[1]})",
    );
    run(&mut c, "CREATE FULLTEXT INDEX ft FOR (n:D) ON EACH [n.t]");
    run(
        &mut c,
        "CREATE FULLTEXT INDEX fk FOR (n:K) ON EACH [n.t] \
         OPTIONS {indexConfig: {`fulltext.analyzer`: 'cjk'}}",
    );
    run(&mut c, "CALL db.awaitIndexes()");
    c
}

/// One query's hit texts through both entry points, sorted.
fn hits(c: &mut relate::Connection, label: &str, index: &str, query: &str) -> Vec<String> {
    sorted_names(&fulltext_agree(c, label, index, "t", 10, query))
}

/// The issue's word cases under the default analyzer: a word inside a run
/// finds the document, as live's standard tokenizer does.
#[test]
fn cjk_words_are_findable_under_the_default_analyzer() {
    let mut c = cjk_store();
    for (query, expected) in [
        ("中文", vec!["中文搜索引擎"]),
        ("搜索", vec!["中文搜索引擎"]),
        ("日本語", vec!["日本語のテキスト"]),
        ("テキスト", vec!["日本語のテキスト"]),
        ("数据库", vec!["Relate 图数据库"]),
        ("图", vec!["Relate 图数据库"]),
        // Controls: the whole run, a Latin word, space-separated Korean.
        ("中文搜索引擎", vec!["中文搜索引擎"]),
        ("graph", vec!["graph データベース"]),
        ("한국어", vec!["한국어 검색"]),
    ] {
        assert_eq!(hits(&mut c, "D", "ft", query), expected, "{query}");
    }
}

/// The `cjk` analyzer bigrams the same runs: every two-character word
/// matches, and a lone ideograph matches nothing — live's bigrams miss it
/// too, so the control case is the empty answer.
#[test]
fn the_cjk_analyzer_bigrams() {
    let mut c = cjk_store();
    for (query, expected) in [
        ("中文", vec!["中文搜索引擎"]),
        ("搜索", vec!["中文搜索引擎"]),
        ("索引", vec!["中文搜索引擎"]),
        ("日本語", vec!["日本語のテキスト"]),
        ("テキスト", vec!["日本語のテキスト"]),
        ("数据库", vec!["Relate 图数据库"]),
        ("图", vec![]),
        ("中文搜索引擎", vec!["中文搜索引擎"]),
        ("graph", vec!["graph データベース"]),
        ("한국어", vec!["한국어 검색"]),
    ] {
        assert_eq!(hits(&mut c, "K", "fk", query), expected, "{query}");
    }
}

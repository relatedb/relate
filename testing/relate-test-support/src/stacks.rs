//! The sampled-stack model behind the profiling harnesses' `--sample`
//! (ADR-0244; query-latency profile and bulk-loadtest):
//! frames attributed to a group (a crate, `std/core/alloc`, the allocator),
//! collapsed stacks, and the `top.txt` rendering.
//!
//! A frame is attributed by its source file when the line tables name one,
//! else by its symbol name: an inlined frame's name comes from the debug
//! info and is often bare (`match_node`), without its crate path.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// One frame of a sampled stack.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Frame {
    /// The demangled symbol, often bare for an inlined frame.
    pub name: String,
    /// The source file from the line tables, shortened to start at its
    /// crate (`relate-executor/src/exec/expand.rs`), when known.
    pub source: Option<String>,
    /// The group the frame is counted under: a workspace crate
    /// (`relate-executor`), `std/core/alloc`, another crate, or
    /// `allocator`, `native` or `unattributed` for frames the source file
    /// and name do not place in a crate.
    pub group: String,
}

impl Frame {
    /// A frame from its demangled symbol name (a legacy `::h<hash>` suffix
    /// is dropped) and, when the line tables have one, its source path. The
    /// source path attributes the frame to its crate; the name is the
    /// fallback, for frames without debug info.
    pub fn new(name: &str, file: Option<&str>) -> Self {
        let name = clean_symbol(name);
        let source = file.and_then(source_of);
        let group = match &source {
            // The global allocator's entry points (`alloc`, `dealloc`,
            // `realloc`, …) all live in this file; on macOS the system
            // allocator's own frames rarely unwind, so this is where
            // allocation time shows.
            Some(src) if src.short == "alloc/src/alloc.rs" => "allocator".to_string(),
            Some(src) => src.group.clone(),
            None => group_of(&name),
        };
        Self {
            name,
            source: source.map(|src| src.short),
            group,
        }
    }

    /// `name (source)`, or the name alone without a source: how the frame
    /// reads in `top.txt` and `folded.txt`.
    pub fn label(&self) -> String {
        match &self.source {
            Some(src) => format!("{} ({src})", self.name),
            None => self.name.clone(),
        }
    }
}

/// Heaviest first, then by key: the order of every ranked list here.
fn by_count_desc<K: Ord>(a: &(K, u64), b: &(K, u64)) -> Ordering {
    b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

/// Collapsed stacks: each distinct, non-empty stack once, root frame first,
/// with its (non-zero) sample count, heaviest first. [`Stacks::from_pairs`]
/// is the only constructor, so the form holds by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stacks {
    stacks: Vec<(Vec<Frame>, u64)>,
}

impl Stacks {
    /// Build from (stack, count) pairs: identical stacks merge, and empty
    /// stacks and zero counts are dropped.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (Vec<Frame>, u64)>) -> Self {
        let mut merged: HashMap<Vec<Frame>, u64> = HashMap::new();
        for (stack, n) in pairs {
            if !stack.is_empty() && n > 0 {
                *merged.entry(stack).or_default() += n;
            }
        }
        let mut stacks: Vec<_> = merged.into_iter().collect();
        stacks.sort_by(by_count_desc);
        Self { stacks }
    }

    /// The stacks, heaviest first.
    pub fn iter(&self) -> impl Iterator<Item = &(Vec<Frame>, u64)> {
        self.stacks.iter()
    }

    pub fn total(&self) -> u64 {
        self.stacks.iter().map(|(_, n)| n).sum()
    }

    /// The folded format (inferno, speedscope): `root;…;leaf <count>` per
    /// line, heaviest first, each frame as its [`Frame::label`]. A `;`
    /// inside a label becomes `:` so the separator stays unambiguous.
    pub fn to_folded(&self) -> String {
        let mut out = String::new();
        for (stack, n) in &self.stacks {
            let frames: Vec<String> = stack.iter().map(|f| f.label().replace(';', ":")).collect();
            out += &format!("{} {n}\n", frames.join(";"));
        }
        out
    }
}

/// The group of a frame that has no source path, from its name: the
/// workspace crate in its kebab name (`relate-executor`),
/// `std/core/alloc`, another crate's name, `allocator` for an allocator
/// entry point, `unattributed` for a bare Rust name (an inlined generic
/// such as `make_hash<i64, …>`, whose crate the name does not say), or
/// `native` for any other symbol (libc, the OS).
fn group_of(frame: &str) -> String {
    let path = frame.trim_start_matches(['<', '&', '*', ' ']);
    let path = path.strip_prefix("dyn ").unwrap_or(path);
    // The leading path segment, when the name starts with one: the
    // identifier must be followed directly by `::`.
    let ident_end = path
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(path.len());
    if path[ident_end..].starts_with("::") && ident_end > 0 {
        return match &path[..ident_end] {
            "std" | "core" | "alloc" => "std/core/alloc".into(),
            h if h.starts_with("relate") => h.replace('_', "-"),
            h => h.to_string(),
        };
    }
    if is_allocator_symbol(path) {
        "allocator".into()
    } else if path.contains(['<', ':', '{']) {
        "unattributed".into()
    } else {
        "native".into()
    }
}

/// Where a source file lives: its group and its path from the crate down.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    group: String,
    /// The path starting at the crate (`core/src/hash/sip.rs`,
    /// `hashbrown-0.15.2/src/raw/mod.rs`, `relate-executor/src/exec/expand.rs`).
    short: String,
}

/// The [`Source`] of a file path:
///
/// - the Rust standard library (`…/library/core/src/…`): `std/core/alloc`;
/// - a crates.io dependency (`…/registry/src/<index>/hashbrown-0.15.2/…`):
///   the crate name without its version;
/// - a workspace crate (`…/relate-executor/src/…`): the crate's
///   directory name.
///
/// Anything else is `None`, and the frame is grouped by its name.
fn source_of(path: &str) -> Option<Source> {
    let p = path.replace('\\', "/");
    if let Some((_, rest)) = p.split_once("/library/") {
        let krate = rest.split('/').next()?;
        let group = match krate {
            "std" | "core" | "alloc" => "std/core/alloc".to_string(),
            other => other.to_string(),
        };
        return Some(Source {
            group,
            short: rest.to_string(),
        });
    }
    if let Some((_, rest)) = p.split_once("/registry/src/") {
        // <index dir>/<crate>-<version>/…
        let rest = rest.split_once('/')?.1;
        let dir = rest.split('/').next()?;
        let krate = match dir.rsplit_once('-') {
            Some((name, version)) if version.starts_with(|c: char| c.is_ascii_digit()) => name,
            _ => dir,
        };
        return Some(Source {
            group: krate.to_string(),
            short: rest.to_string(),
        });
    }
    let parts: Vec<&str> = p.split('/').collect();
    let src = parts.iter().rposition(|s| *s == "src")?;
    let krate = *parts.get(src.checked_sub(1)?)?;
    krate.starts_with("relate").then(|| Source {
        group: krate.to_string(),
        short: parts[src - 1..].join("/"),
    })
}

/// System and Rust allocator entry points (macOS libmalloc, glibc, the
/// Rust allocator shims).
fn is_allocator_symbol(name: &str) -> bool {
    let n = name.trim_start_matches('_');
    const EXACT: [&str; 8] = [
        "malloc",
        "free",
        "realloc",
        "calloc",
        "cfree",
        "posix_memalign",
        "rust_alloc",
        "rust_dealloc",
    ];
    const PREFIX: [&str; 10] = [
        "malloc_",
        "int_malloc",
        "int_free",
        "int_realloc",
        "xzm_",
        "szone_",
        "nanov2_",
        "tiny_",
        "rust_realloc",
        "rdl_",
    ];
    EXACT.contains(&n) || PREFIX.iter().any(|p| n.starts_with(p)) || n.starts_with("free_")
}

/// Strip a legacy Rust symbol hash (`::h0123456789abcdef`).
fn clean_symbol(name: &str) -> String {
    if let Some((head, hash)) = name.rsplit_once("::h") {
        if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return head.to_string();
        }
    }
    name.to_string()
}

/// `map`'s entries, heaviest first.
fn ranked(map: HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(by_count_desc);
    v
}

/// Render `top.txt`: `header` lines, then the tables, each function table
/// cut to `limit` rows.
pub fn top_text(stacks: &Stacks, header: &[String], limit: usize) -> String {
    let total = stacks.total();
    let mut out = String::new();
    for line in header {
        out += line;
        out.push('\n');
    }
    if total == 0 {
        out += "\nno samples: the loop was too short for the sampling frequency\n";
        return out;
    }
    let pct = |n: u64| 100.0 * n as f64 / total as f64;
    let table = |out: &mut String, title: &str, rows: &[(String, u64)]| {
        out.push('\n');
        out.push_str(title);
        out.push('\n');
        for (name, n) in rows {
            *out += &format!("{:>6.1}%  {:>8}  {name}\n", pct(*n), n);
        }
    };

    let mut self_group: HashMap<String, u64> = HashMap::new();
    let mut incl_crate: HashMap<String, u64> = HashMap::new();
    let mut self_fn: HashMap<String, u64> = HashMap::new();
    let mut incl_fn: HashMap<String, u64> = HashMap::new();
    for (stack, n) in stacks.iter() {
        let leaf = stack.last().expect("Stacks holds no empty stack");
        *self_group.entry(leaf.group.clone()).or_default() += n;
        *self_fn.entry(leaf.label()).or_default() += n;
        let crates: HashSet<&str> = stack
            .iter()
            .map(|f| f.group.as_str())
            .filter(|g| g.starts_with("relate"))
            .collect();
        for c in crates {
            *incl_crate.entry(c.to_string()).or_default() += n;
        }
        let frames: HashSet<String> = stack.iter().map(Frame::label).collect();
        for f in frames {
            *incl_fn.entry(f).or_default() += n;
        }
    }

    // Frames on (nearly) every sample are the thread's entry and the query
    // dispatch; they rank first by construction and say nothing.
    let scaffold = |n: u64| (n as f64) >= 0.99 * total as f64;
    let self_top: Vec<_> = ranked(self_fn).into_iter().take(limit).collect();
    let incl_top: Vec<_> = ranked(incl_fn)
        .into_iter()
        .filter(|(_, n)| !scaffold(*n))
        .take(limit)
        .collect();
    table(
        &mut out,
        "Self time by group (the frame on CPU)",
        &ranked(self_group),
    );
    table(
        &mut out,
        "Inclusive time by engine crate (samples whose stack holds the crate)",
        &ranked(incl_crate),
    );
    table(
        &mut out,
        &format!("Top {limit} functions by self time"),
        &self_top,
    );
    table(
        &mut out,
        &format!(
            "Top {limit} functions by inclusive time (frames on 99%+ of samples, the entry \
             scaffolding, left out)"
        ),
        &incl_top,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(frames: &[&str], n: u64) -> (Vec<Frame>, u64) {
        (frames.iter().map(|f| Frame::new(f, None)).collect(), n)
    }

    fn source(group: &str, short: &str) -> Option<Source> {
        Some(Source {
            group: group.into(),
            short: short.into(),
        })
    }

    #[test]
    fn groups_follow_the_crate_path() {
        assert_eq!(group_of("relate_executor::exec::expand"), "relate-executor");
        assert_eq!(
            group_of("<relate_executor::Scan as core::iter::Iterator>::next"),
            "relate-executor"
        );
        assert_eq!(
            group_of("core::ptr::drop_in_place<relate_core::Value>"),
            "std/core/alloc"
        );
        assert_eq!(group_of("alloc::raw_vec::finish_grow"), "std/core/alloc");
        assert_eq!(group_of("hashbrown::raw::RawTable<T>::find"), "hashbrown");
        assert_eq!(group_of("_xzm_free"), "allocator");
        assert_eq!(group_of("_malloc_zone_malloc"), "allocator");
        assert_eq!(group_of("__rust_alloc"), "allocator");
        assert_eq!(group_of("_int_free"), "allocator");
        assert_eq!(group_of("_platform_memmove"), "native");
        assert_eq!(group_of("_open$NOCANCEL"), "native");
        assert_eq!(group_of("Unknown"), "native");
        // A bare inlined generic: the `::` inside its arguments is not its
        // crate.
        assert_eq!(
            group_of("make_hash<i64, std::hash::random::RandomState>"),
            "unattributed"
        );
        assert_eq!(group_of("{closure#0}"), "unattributed");
    }

    #[test]
    fn symbol_hashes_are_stripped() {
        assert_eq!(Frame::new("a::b::h0123456789abcdef", None).name, "a::b");
        assert_eq!(clean_symbol("a::b::h0123456789abcdef"), "a::b");
        assert_eq!(clean_symbol("a::b::hello"), "a::b::hello");
        assert_eq!(clean_symbol("malloc"), "malloc");
    }

    #[test]
    fn stacks_merge_duplicates_drop_empties_and_fold() {
        let st = Stacks::from_pairs([
            s(&["main", "a"], 2),
            s(&["main", "b;c"], 1),
            s(&["main", "a"], 3),
            s(&[], 7),
            s(&["main", "z"], 0),
        ]);
        assert_eq!(st.total(), 6);
        assert_eq!(st.iter().count(), 2);
        assert_eq!(st.to_folded(), "main;a 5\nmain;b:c 1\n");
    }

    #[test]
    fn source_paths_attribute_frames_to_their_crate() {
        let ws = "/home/u/relate/relate/relate-executor/src/exec/expand.rs";
        assert_eq!(
            source_of(ws),
            source("relate-executor", "relate-executor/src/exec/expand.rs")
        );
        let api = "/home/u/relate/relate/relate/src/connection.rs";
        assert_eq!(source_of(api).unwrap().group, "relate");
        let std = "/rustc/17067e9ac6d7e98f18d4a3b2f1c2a1c7f2d1e0a4/library/core/src/hash/sip.rs";
        assert_eq!(
            source_of(std),
            source("std/core/alloc", "core/src/hash/sip.rs")
        );
        let dep = "/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/hashbrown-0.15.2/src/raw/mod.rs";
        assert_eq!(
            source_of(dep),
            source("hashbrown", "hashbrown-0.15.2/src/raw/mod.rs")
        );
        assert_eq!(source_of("/usr/lib/system/libsystem_malloc.dylib"), None);
    }

    #[test]
    fn a_bare_inlined_name_is_grouped_by_its_source() {
        let f = Frame::new(
            "match_node",
            Some("/w/relate/relate-executor/src/exec/match_pattern.rs"),
        );
        assert_eq!(f.group, "relate-executor");
        assert_eq!(
            f.label(),
            "match_node (relate-executor/src/exec/match_pattern.rs)"
        );
        // Without a source, the name decides.
        assert_eq!(Frame::new("match_node", None).group, "native");
        assert_eq!(Frame::new("_xzm_free", None).group, "allocator");
        // The global allocator's entry points count as the allocator.
        let rustc = "/rustc/17067e9ac6d7e98f18d4a3b2f1c2a1c7f2d1e0a4/library";
        assert_eq!(
            Frame::new("dealloc", Some(&format!("{rustc}/alloc/src/alloc.rs"))).group,
            "allocator"
        );
        assert_eq!(
            Frame::new("push", Some(&format!("{rustc}/alloc/src/vec/mod.rs"))).group,
            "std/core/alloc"
        );
    }

    fn sample_stacks() -> Stacks {
        let thread = "std::sys::thread::start";
        let dispatch = "relate::connection::run_query";
        Stacks::from_pairs([
            s(
                &[
                    thread,
                    dispatch,
                    "relate_executor::expand",
                    "hashbrown::find",
                ],
                50,
            ),
            s(
                &[thread, dispatch, "relate_executor::expand", "_xzm_malloc"],
                30,
            ),
            s(&[thread, dispatch, "relate_planner::plan"], 20),
        ])
    }

    #[test]
    fn top_text_ranks_groups_crates_and_functions() {
        let t = top_text(&sample_stacks(), &["q1 header".to_string()], 10);
        assert!(t.starts_with("q1 header\n"), "{t}");
        assert!(t.contains("  50.0%        50  hashbrown\n"), "{t}");
        assert!(t.contains("  30.0%        30  allocator\n"), "{t}");
        // Inclusive by engine crate: the executor is on 80 of 100 samples.
        assert!(t.contains("  80.0%        80  relate-executor\n"), "{t}");
        assert!(t.contains("  20.0%        20  relate-planner\n"), "{t}");
        // The entry scaffolding (100% of samples) is left out of the
        // inclusive function table.
        let incl = t.split("by inclusive time").nth(1).unwrap();
        assert!(!incl.contains("std::sys::thread::start"), "{incl}");
        assert!(incl.contains("relate_executor::expand"), "{incl}");
    }

    #[test]
    fn top_text_cuts_function_tables_to_the_limit() {
        let t = top_text(&sample_stacks(), &[], 1);
        let self_table = t
            .split("Top 1 functions by self time\n")
            .nth(1)
            .unwrap()
            .split("\n\n")
            .next()
            .unwrap();
        assert_eq!(self_table.lines().count(), 1, "{t}");
        assert!(self_table.contains("hashbrown::find"), "{t}");
    }

    #[test]
    fn top_text_without_samples_says_so() {
        let t = top_text(&Stacks::default(), &[], 10);
        assert!(t.contains("no samples"), "{t}");
    }
}

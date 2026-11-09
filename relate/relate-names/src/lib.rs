//! `SmallName` — the query front-end's name representation (ADR-0509).
//!
//! One tiny leaf crate, dependency-free by contract: seven engine crates
//! consume `SmallName` — `relate-parser`, `relate-ir`,
//! `relate-front`, `relate-ontology`, `relate-planner`,
//! `relate-executor` and `relate` — so anything this crate
//! depended on would land in all of them (ADR-0509 §4; the ADR's
//! stage-0 front door named the parser/IR/front three, the completed
//! migration's final tree carries the seven). Keeping the leaf free of
//! dependencies keeps the name type `Clone` and `'static` with
//! [`SmallName::from_name`] as the single classification point per name
//! (the wired-shape rule, ADR-0509 §1).
//!
//! Shape (per the spike's measured encoding, adopted unchanged):
//!
//! ```text
//! SmallName = Inline(u64)       // <=7 ASCII identifier-shape bytes:
//!                               //   name bytes little-endian in 0..7,
//!                               //   byte length in byte 7 (Pascal tag)
//!           | Id(u32)           // per-store catalog id — constructible
//!           |                   //   only inside this crate until the
//!           |                   //   stage-4 catalog wiring (ADR-0509 §2)
//!           | Spilled(Arc<str>) // >7 bytes, non-ASCII, or quoted-only
//!                               //   shapes (spaces, escapes, hyphens, …)
//! ```
//!
//! Classification is one total function, [`SmallName::from_name`]: a given
//! `&str` always lands in the same arm, so the derived `Eq`/`Hash` are
//! name-equality — the inline encoding is injective (7 payload bytes plus a
//! disjoint length tag; swept exhaustively for 1–2 char names in the tests),
//! `Spilled` compares by `str` value, and no name can reach two arms. The
//! enum's variants are private: the only public constructors are
//! [`SmallName::from_name`] and [`From<&str>`], so no `Id` value can exist
//! outside this crate ahead of the catalog wiring.
//!
//! Recorded cost fact (asserted in the tests on 64-bit targets):
//! `size_of::<SmallName>() == 24 == size_of::<String>()` — the `Arc<str>`
//! `(ptr, len)` pair keeps the enum at `String`'s size, so the measured win
//! (ADR-0509's table: 17.16 → 2.81 allocs/stmt, ~23 → ~13 ns/lookup in the
//! wired shape) is *allocations avoided* (the inline arm needs no heap) and
//! *lookup cost* (hashing/equality over a `u64` instead of str bytes), not
//! struct width.

use std::fmt;
use std::sync::Arc;

/// A per-store catalog token id: the id a store's name interner assigned to
/// a label, relationship type or property key (the id space of
/// [`SmallName::Id`], ADR-0509 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TokenId(pub u32);

/// A name's resolution state against a store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Token {
    /// Planning has not resolved the name: look it up by text.
    #[default]
    Unresolved,
    /// The store has never interned the name, so no stored entity carries it.
    Absent,
    /// The store's id for the name.
    Id(TokenId),
}

/// A name beside its [`Token`]: the entry type of a pattern's label and
/// relationship-type lists.
pub type NamedToken = (SmallName, Token);

impl Token {
    /// The store id when planning resolved one: `Absent` and `Unresolved`
    /// answer `None` without any lookup. One dispatch with
    /// [`Self::id_or_else`].
    pub fn id(self) -> Option<u32> {
        self.id_or_else(|| None)
    }

    /// The store id this token names: `Id` answers it, `Absent` answers
    /// `None` without calling `lookup`, and only `Unresolved` asks `lookup`
    /// (the by-name interner read).
    pub fn id_or_else(self, lookup: impl FnOnce() -> Option<u32>) -> Option<u32> {
        match self {
            Token::Id(id) => Some(id.0),
            Token::Absent => None,
            Token::Unresolved => lookup(),
        }
    }
}

/// Maps a static name to its [`Token`]: [`Token::Absent`] or [`Token::Id`],
/// never [`Token::Unresolved`].
pub trait TokenResolver {
    fn resolve(&self, name: &str) -> Token;
}

/// Longest name text carried inline: 7 payload bytes + the top length tag.
pub const MAX_INLINE_LEN: usize = 7;

/// A name spelled with only identifier bytes: `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Identifier continuation bytes: `[A-Za-z0-9_]`.
pub fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_plain_identifier_shape(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.first() {
        Some(&b) if is_identifier_start(b) => {}
        _ => return false,
    }
    bytes[1..].iter().all(|&b| is_identifier_continue(b))
}

/// The front-end's name representation. See the module docs for the arm
/// contract and the injectivity argument behind the derived `Eq`/`Hash`.
///
/// The variants are private: name text enters through
/// [`SmallName::from_name`] (the one classification point — the wired-shape
/// rule, ADR-0509 §1, forbids re-classifying at every map lookup), and the
/// catalog-id arm is built only by the crate-private [`SmallName::from_id`]
/// until the stage-4 wiring resolves ids at lowering.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SmallName {
    /// Up to 7 ASCII identifier-shape bytes: little-endian `u64` with the
    /// name bytes in `0..7` and the byte length in byte 7 (Pascal-style
    /// tag). Invariant (enforced by [`SmallName::from_name`], the only
    /// constructor of this arm): the tag is `<= MAX_INLINE_LEN` and the
    /// payload is valid ASCII.
    Inline(u64),
    /// Per-store catalog id (label, relationship type, or property key —
    /// the executor interner's id space, ADR-0509 §2). Carries no text
    /// until the stage-4 catalog resolver maps ids back to names; `Display`
    /// and `Debug` render the structured arm form, which that wiring change
    /// audits out of every user-visible rendering.
    Id(u32),
    /// Everything else: `> MAX_INLINE_LEN` bytes, non-ASCII, or names that
    /// only arise backtick-quoted (spaces, escaped backticks, hyphens, …).
    Spilled(Arc<str>),
}

impl SmallName {
    /// Total classification of any `&str` into the three arms — never
    /// panics, never loses bytes. Inline iff the text is at most
    /// [`MAX_INLINE_LEN`] bytes **and** spelled as a plain identifier;
    /// everything else (including empty and quoted-only shapes) spills.
    pub fn from_name(name: &str) -> SmallName {
        if name.len() <= MAX_INLINE_LEN && is_plain_identifier_shape(name) {
            SmallName::Inline(encode_inline(name))
        } else {
            SmallName::Spilled(Arc::from(name))
        }
    }

    /// The catalog-id arm. Crate-private and test-gated: the stage-4
    /// catalog wiring (ADR-0509 §2) is the only future constructor site —
    /// resolving read-position vocabulary at lowering — so until then no
    /// `Id` value exists outside this crate's tests, and the constructor
    /// itself only exists there.
    #[cfg(test)]
    pub(crate) fn from_id(id: u32) -> SmallName {
        SmallName::Id(id)
    }

    /// Visit this name's text without allocating: the inline arm decodes
    /// onto the stack, the spilled arm borrows the `Arc<str>` directly.
    /// The one read bridge for text-domain consumers (error texts, the
    /// `relate-ir` walkers' `String`-keyed sinks, executor row schemas;
    /// the front's own scope maps key on `SmallName` since ADR-0509
    /// stage 2) — unlike [`Display`] it costs no heap traffic, and unlike
    /// `from_name` it re-classifies nothing (the wired-shape rule,
    /// ADR-0509 §1: the value was classified once, at AST construction).
    ///
    /// The `Id` arm carries no text until the stage-4 catalog resolver;
    /// reaching it here is an invariant violation (no `Id` value can exist
    /// outside this crate's tests before that wiring), so it fails loud
    /// with the stated invariant rather than rendering a placeholder into
    /// a text-context read.
    pub fn with_str<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        match self {
            SmallName::Inline(bits) => f(inline_str(&bits.to_le_bytes())),
            SmallName::Id(_) => {
                // Not `unreachable!` beside a handled form: no code path
                // can build this arm before stage 4, so this expect is the
                // loud invariant, per ADR-0509 §2's no-fabricated-text rule.
                panic!(
                    "SmallName::Id carries no text until the stage-4 catalog wiring (ADR-0509 §2)"
                )
            }
            SmallName::Spilled(text) => f(text),
        }
    }
}

/// Name-text order: byte-lexicographic over the name's text, exactly
/// `str`'s `Ord` — so a `SmallName`-keyed scope set's `.min()` / sorted
/// order picks the same name a `String`-keyed one did (the ADR-0505 §4
/// deterministic-offender rule). Consistent with the derived `Eq`: the
/// classification is total and injective, so two names compare `Equal`
/// iff their texts are equal iff they are the same arm and payload.
/// Allocation-free via [`SmallName::with_str`] (and so, like it, loud on
/// the textless `Id` arm until the stage-4 wiring).
impl Ord for SmallName {
    fn cmp(&self, other: &SmallName) -> std::cmp::Ordering {
        self.with_str(|a| other.with_str(|b| a.cmp(b)))
    }
}

impl PartialOrd for SmallName {
    fn partial_cmp(&self, other: &SmallName) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq<str> for SmallName {
    /// Text equality against a literal — the comparison domain of every
    /// consumer that matches a name against a fixed spelling (function
    /// dispatch, keyword recognizers). Allocation-free via [`SmallName::with_str`].
    fn eq(&self, other: &str) -> bool {
        self.with_str(|text| text == other)
    }
}

impl PartialEq<&str> for SmallName {
    fn eq(&self, other: &&str) -> bool {
        self.with_str(|text| text == *other)
    }
}

impl PartialEq<SmallName> for str {
    fn eq(&self, other: &SmallName) -> bool {
        other == self
    }
}

impl PartialEq<SmallName> for &str {
    fn eq(&self, other: &SmallName) -> bool {
        other == *self
    }
}

impl From<&str> for SmallName {
    fn from(name: &str) -> SmallName {
        SmallName::from_name(name)
    }
}

/// The empty name — `String::default()`'s exact standing in the slots it
/// replaces (an optional variable's `unwrap_or_default` placeholder, never
/// a real binding: `""` is not a legal identifier and no name classifies
/// to it from token text, since the empty spelling always spills).
impl Default for SmallName {
    fn default() -> SmallName {
        SmallName::from_name("")
    }
}

/// Pack a `<= MAX_INLINE_LEN` identifier-shape name into the inline arm's
/// `u64`: name bytes little-endian in `0..7`, byte length in byte 7.
fn encode_inline(name: &str) -> u64 {
    let bytes = name.as_bytes();
    debug_assert!(
        bytes.len() <= MAX_INLINE_LEN && is_plain_identifier_shape(name),
        "from_name only inlines short identifier-shape names"
    );
    let mut buf = [0u8; 8];
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[7] = bytes.len() as u8;
    u64::from_le_bytes(buf)
}

/// Decode an inline arm's little-endian bytes (`bits.to_le_bytes()`) to its
/// name text, on the stack — the one spelling of the inline decode, which
/// [`SmallName::with_str`], `Display`/`Debug` and [`decode_inline`] all
/// read. The two `expect`s state the constructor's invariant (tag
/// `<= MAX_INLINE_LEN`, ASCII payload); no other code path builds `Inline`
/// values.
fn inline_str(bytes: &[u8; 8]) -> &str {
    let len = usize::from(bytes[7]);
    let payload = bytes
        .get(..len)
        .expect("inline length tag <= 7 (set by from_name)");
    str::from_utf8(payload).expect("inline payload is ASCII (set by from_name)")
}

/// The owned form of [`inline_str`] — the tests' round-trip probe.
#[cfg(test)]
pub(crate) fn decode_inline(bits: u64) -> String {
    inline_str(&bits.to_le_bytes()).to_owned()
}

impl fmt::Display for SmallName {
    /// Byte-identical to the input name text for the `Inline` and `Spilled`
    /// arms. The `Id` arm has no text until the stage-4 catalog resolver
    /// (ADR-0509 §2) — it renders the same structured form as `Debug`, a
    /// loud placeholder that the stage-4 wiring audits out of every
    /// user-visible rendering; there is deliberately no bare-decimal path.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmallName::Inline(bits) => f.write_str(inline_str(&bits.to_le_bytes())),
            SmallName::Id(id) => write!(f, "SmallName::Id({id})"),
            SmallName::Spilled(text) => f.write_str(text),
        }
    }
}

impl fmt::Debug for SmallName {
    /// Delegates to [`fmt::Display`]: the two traits render one vocabulary —
    /// byte-identical name text for the round-trippable arms (the stage
    /// gates' contract: plan dumps and errors keep names verbatim) and the
    /// structured catalog-id form for internal diagnostics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    //! The spike's edge-case tests, promoted with the type (ADR-0509 §5
    //! stage 0). The suite lives inline so the tests can drive the
    //! crate-private surfaces no external code may touch: the `Id`
    //! constructor ([`SmallName::from_id`]) and the inline decode helper.

    use std::collections::BTreeSet;

    use super::{
        decode_inline, is_identifier_continue, is_identifier_start, SmallName, MAX_INLINE_LEN,
    };

    /// Every one- and two-character plain identifier — the exhaustive
    /// short-name universe the injectivity test sweeps.
    fn short_identifier_alphabet() -> Vec<char> {
        let mut alphabet: Vec<char> = ('a'..='z').chain('A'..='Z').collect();
        alphabet.push('_');
        alphabet
    }

    #[test]
    fn seven_char_identifier_fits_inline() {
        for name in ["a", "ab", "abc", "abcd", "abcde", "abcdef", "abcdefg"] {
            assert!(
                matches!(SmallName::from_name(name), SmallName::Inline(_)),
                "{name:?} should classify Inline"
            );
        }
        // underscores and digits stay identifier-shape
        assert!(matches!(SmallName::from_name("n_1"), SmallName::Inline(_)));
        assert!(matches!(
            SmallName::from_name("_______"),
            SmallName::Inline(_)
        ));
    }

    #[test]
    fn eight_char_identifier_spills() {
        for name in ["abcdefgh", "aaaaaaaa", "n1234567"] {
            assert!(
                matches!(SmallName::from_name(name), SmallName::Spilled(_)),
                "{name:?} should classify Spilled"
            );
        }
    }

    #[test]
    fn non_ascii_spills_even_when_short() {
        for name in ["größe", "日本語", "café", "a\tb", "a\nb"] {
            assert!(
                matches!(SmallName::from_name(name), SmallName::Spilled(_)),
                "{name:?} should classify Spilled"
            );
        }
    }

    #[test]
    fn backtick_only_shapes_spill_even_when_short() {
        // Names that can only be written backtick-quoted: spaces, hyphens,
        // escaped backticks, empty, digits-first, lone punctuation.
        for name in [
            "a b",
            "a`b",
            "x-y",
            "",
            "42",
            "4l",
            "`leading",
            "trailing`",
            "a.b",
            "a:b",
            "a,b",
        ] {
            assert!(
                matches!(SmallName::from_name(name), SmallName::Spilled(_)),
                "{name:?} should classify Spilled"
            );
        }
    }

    #[test]
    fn display_and_debug_round_trip_byte_identically() {
        let names = [
            // inline arm
            "n", "x1", "abc", "label", "abcdefg", "n_1",      // spilled: too long
            "abcdefgh", // spilled: non-ASCII
            "größe",    // spilled: backtick-only shapes
            "a b", "a``b", "x-y", "",
        ];
        for name in names {
            let small = SmallName::from_name(name);
            assert_eq!(small.to_string(), name, "Display round-trip for {name:?}");
            assert_eq!(format!("{small:?}"), name, "Debug round-trip for {name:?}");
        }
    }

    #[test]
    fn inline_encoding_is_injective_over_all_short_names() {
        // Every one- and two-character identifier, exhaustively.
        let alphabet = short_identifier_alphabet();
        let continue_alphabet: Vec<char> = alphabet.iter().copied().chain('0'..='9').collect();
        let mut encoded: BTreeSet<u64> = BTreeSet::new();
        let mut count = 0usize;
        for &a in &alphabet {
            let one = a.to_string();
            let bits = inline_bits(&SmallName::from_name(&one));
            encoded.insert(bits);
            count += 1;
            for &b in &continue_alphabet {
                let two: String = [a, b].iter().collect();
                encoded.insert(inline_bits(&SmallName::from_name(&two)));
                count += 1;
            }
        }
        assert_eq!(encoded.len(), count, "two distinct short names share bits");
    }

    #[test]
    fn inline_length_tag_arithmetic_round_trips() {
        // For every length 1..=MAX_INLINE_LEN the tag occupies byte 7 and the
        // payload bytes 0..len: encode → decode reproduces both the text and
        // the tag exactly, and a same-length sibling differs in the payload
        // region (lengths never encode into the payload bytes).
        for len in 1..=MAX_INLINE_LEN {
            let name = "a".repeat(len);
            let bits = inline_bits(&SmallName::from_name(&name));
            assert_eq!(bits.to_le_bytes()[7] as usize, len, "tag == byte length");
            assert_eq!(decode_inline(bits), name);
            let mut sibling = name.clone();
            sibling.pop();
            sibling.push('b');
            assert_ne!(
                inline_bits(&SmallName::from_name(&sibling)),
                bits,
                "same-length siblings must differ in the payload"
            );
        }
        // the zero-tag encoding decodes to the empty payload
        let zero = u64::from_le_bytes([0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(zero.to_le_bytes()[7], 0);
        assert_eq!(decode_inline(zero), "");
    }

    #[test]
    fn classification_is_total_no_panics_on_any_str() {
        let nasty: Vec<String> = [
            "",
            "\0",
            "a\0b",
            "\u{1F600}",
            "\u{1F600}\u{1F600}",
            "a\u{0301}b",
            "`",
            "``",
            "```",
            " ",
            "  spaced  ",
            "\t",
            "\n\r",
            "0",
            "9",
            "_",
            "________",
            "__________",
            "Ünicode-Name",
            "Λ Ο Γ Ο Σ",
            " mixed`quote",
            "`unterminated",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(["a".repeat(8), "a".repeat(64), "a".repeat(100_000)])
        .collect();
        for name in &nasty {
            let small = SmallName::from_name(name);
            // whatever the arm, rendering never panics and never loses bytes
            let name = name.as_str();
            assert_eq!(small.to_string(), name, "Display round-trip for {name:?}");
            assert_eq!(format!("{small:?}"), name, "Debug round-trip for {name:?}");
        }
    }

    #[test]
    fn id_arm_is_a_distinct_shape() {
        // The Id arm is reachable only through the crate-private from_id;
        // Eq and Hash validate here, the catalog resolution is stage 4.
        assert_eq!(SmallName::from_id(42), SmallName::from_id(42));
        assert_ne!(SmallName::from_id(42), SmallName::from_id(43));
        // Id never equals a name arm — the catalog wiring that would map an
        // id to its name does not exist yet.
        assert_ne!(SmallName::from_id(0), SmallName::from_name("a"));
        assert_ne!(SmallName::from_id(7), SmallName::from_name("abcdefg"));
        // no bare-decimal path (ADR-0509 §2): the spike's pinned placeholder
        // rendering is dropped at promotion; both renderings carry the
        // structured form until the stage-4 resolver replaces them.
        assert_eq!(SmallName::from_id(42).to_string(), "SmallName::Id(42)");
        assert_eq!(format!("{:?}", SmallName::from_id(42)), "SmallName::Id(42)");
        assert_ne!(SmallName::from_id(42).to_string(), "42");
    }

    #[test]
    fn with_str_reads_every_text_arm_without_allocating() {
        for name in ["n", "abcdefg", "n_1", "abcdefgh", "größe", "a b", ""] {
            let small = SmallName::from_name(name);
            // The callback sees exactly the input text, whatever the arm.
            assert_eq!(
                small.with_str(|text| text.to_string()),
                name,
                "with_str round-trip for {name:?}"
            );
            // Text equality against literals holds in both directions,
            // for the inline and the spilled arm alike.
            assert!(small == name, "== &str for {name:?}");
            assert!(name == small, "&str == for {name:?}");
            assert_ne!(small, SmallName::from_name("different"));
        }
    }

    #[test]
    fn identifier_byte_classes_match_the_cypher_identifier_shape() {
        assert!(is_identifier_start(b'a'));
        assert!(is_identifier_start(b'Z'));
        assert!(is_identifier_start(b'_'));
        assert!(!is_identifier_start(b'0'));
        assert!(!is_identifier_start(b'`'));
        assert!(!is_identifier_start(0xC3)); // non-ASCII lead byte
        assert!(is_identifier_continue(b'0'));
        assert!(is_identifier_continue(b'_'));
        assert!(!is_identifier_continue(b'-'));
        assert!(!is_identifier_continue(b' '));
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn recorded_size_fact_matches_string() {
        // The recorded cost fact from the module docs: the Arc<str> payload
        // keeps the enum at String's width — no struct-width win, the wins
        // are allocations avoided and cheaper hashing.
        assert_eq!(
            std::mem::size_of::<SmallName>(),
            std::mem::size_of::<String>()
        );
    }

    #[test]
    fn ord_is_name_text_order_across_arms() {
        // Inline vs inline, inline vs spilled (long, quoted-only, non-ASCII),
        // and the empty name: the sorted SmallNames read exactly as the
        // sorted texts — the ADR-0505 §4 sorted-first-offender rule rides it.
        let texts = [
            "b", "a", "_x", "Z", "abcdefgh", "abc", "a b", "é", "", "abcdefg", "a1",
        ];
        let mut names: Vec<SmallName> = texts.iter().map(|t| SmallName::from_name(t)).collect();
        names.sort();
        let mut sorted = texts.to_vec();
        sorted.sort();
        let rendered: Vec<String> = names.iter().map(ToString::to_string).collect();
        assert_eq!(rendered, sorted);
        assert_eq!(
            names.iter().min().map(ToString::to_string).as_deref(),
            Some("")
        );
        for a in &texts {
            for b in &texts {
                let (x, y) = (SmallName::from_name(a), SmallName::from_name(b));
                assert_eq!(x.cmp(&y), a.cmp(b), "{a:?} vs {b:?}");
                assert_eq!(x.cmp(&y).is_eq(), x == y, "Ord agrees with Eq");
            }
        }
    }

    fn inline_bits(small: &SmallName) -> u64 {
        match small {
            SmallName::Inline(bits) => *bits,
            _ => panic!("test helper: expected the Inline arm, got {small:?}"),
        }
    }
}

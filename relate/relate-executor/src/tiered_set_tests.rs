//! `TieredWordSet` unit tests: the inline→promoted boundary, the
//! refused-charge rollback in every tier state, the len/bytes/charge
//! arithmetic across promotion, both hasher policies, the clone charge
//! contract, and the module's layering guard.

use relate_core::tokens::LedgerGuard;

use crate::test_support::{assert_never_imports_exec_tree, refusing_ledger, roomy_ledger};
use crate::tiered_set::{TieredWordSet, WordHash};
use crate::workspace::{word_insert_delta, word_set_bytes, DISTINCT_PEAK_FAMILY};

fn insert(set: &mut TieredWordSet, word: u64) -> bool {
    set.insert(word, DISTINCT_PEAK_FAMILY)
        .expect("no budget installed refuses")
}

#[test]
fn boundary_pins_at_15_16_17_words() {
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..15 {
        assert!(insert(&mut set, word));
    }
    assert_eq!(set.len(), 15);
    assert_eq!(set.bytes(), word_set_bytes(15));
    assert!(!set.promoted(), "k = 2^4 - 1 stays inline");
    assert!(insert(&mut set, 15));
    assert_eq!(set.len(), 16);
    assert!(!set.promoted(), "k = 2^4 fills T1 exactly");
    // A duplicate offer at len == 16 matches in T1 and never promotes.
    assert!(!insert(&mut set, 7));
    assert!(!set.promoted());
    assert_eq!(set.len(), 16);
    assert!(insert(&mut set, 16));
    assert_eq!(set.len(), 17);
    assert!(set.promoted(), "the 17th distinct word promotes");
    assert_eq!(set.bytes(), word_set_bytes(17));
    for word in 0..17 {
        assert!(!insert(&mut set, word), "{word} stays a member");
    }
    assert_eq!(set.len(), 17);
}

/// Words spread across the u64 range, so the promoted tables' bucket
/// bits see more than the low sequential ids.
fn spread(word: u64) -> u64 {
    word.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

#[test]
fn both_hasher_policies_share_the_boundary() {
    // The tiering is policy-blind: only the promoted table's hasher
    // differs, never a membership or tier decision.
    for hash in [WordHash::Id, WordHash::Value] {
        let mut set = TieredWordSet::new(hash);
        for word in 0..17 {
            assert!(insert(&mut set, spread(word)));
        }
        assert!(set.promoted());
        assert_eq!(set.len(), 17);
        assert!(!insert(&mut set, spread(5)));
    }
}

#[test]
fn summed_deltas_equal_the_bytes_oracle_across_promotion() {
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..40 {
        assert!(insert(&mut set, word));
        assert_eq!(set.charged(), set.bytes(), "after word {word}");
        assert_eq!(set.bytes(), word_set_bytes(set.len()));
    }
    assert!(set.promoted());
}

#[test]
fn promotion_costs_no_delta() {
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = TieredWordSet::new(WordHash::Id);
    for word in 0..16 {
        insert(&mut set, word);
    }
    let inline = set.charged();
    assert_eq!(inline, word_set_bytes(16));
    insert(&mut set, 16);
    assert_eq!(
        set.charged() - inline,
        word_insert_delta(16),
        "the 17th word pays buffer_growth(16, 17, 8) only"
    );
}

#[test]
fn refused_charge_leaves_every_tier_state_unchanged() {
    // From Empty: the first word's charge precedes its write, so a
    // refusal leaves the tier Empty (no stale half-written tier).
    let mut set = TieredWordSet::new(WordHash::Value);
    {
        let mut ledger = refusing_ledger();
        let _guard = LedgerGuard::install(&mut ledger);
        assert!(
            set.insert(0, DISTINCT_PEAK_FAMILY).is_err(),
            "the budget refuses"
        );
    }
    assert_eq!(set.len(), 0);
    assert_eq!(set.bytes(), 0);
    assert!(!set.promoted());
    assert!(insert(&mut set, 0), "the refused offer was rolled back");

    // Mid-T1: a refused fill leaves the held words untouched. The offer
    // lands on a positive delta (the reservation grows 4 -> 8 at the
    // fifth word).
    for word in 1..4 {
        insert(&mut set, word);
    }
    let (len, bytes) = (set.len(), set.bytes());
    {
        let mut ledger = refusing_ledger();
        let _guard = LedgerGuard::install(&mut ledger);
        assert!(set.insert(99, DISTINCT_PEAK_FAMILY).is_err());
    }
    assert_eq!(set.len(), len);
    assert_eq!(set.bytes(), bytes);
    assert!(!insert(&mut set, 3));
    assert!(insert(&mut set, 99));

    // The promoting 17th insert: refused, yet the promotion stays and
    // the moved words are all still members.
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..16 {
        insert(&mut set, word);
    }
    let (len, bytes) = (set.len(), set.bytes());
    {
        let mut ledger = refusing_ledger();
        let _guard = LedgerGuard::install(&mut ledger);
        assert!(set.insert(16, DISTINCT_PEAK_FAMILY).is_err());
    }
    assert!(set.promoted(), "rollback does not undo promotion");
    assert_eq!(set.len(), len);
    assert_eq!(set.bytes(), bytes);
    for word in 0..16 {
        assert!(!insert(&mut set, word), "{word} stayed a member");
    }
    assert!(insert(&mut set, 16));

    // Promoted: a refused table insert removes the word again. The offer
    // again lands on a positive delta (the reservation grows 32 -> 64 at
    // the 33rd word).
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..32 {
        insert(&mut set, word);
    }
    assert!(set.promoted());
    let (len, bytes) = (set.len(), set.bytes());
    {
        let mut ledger = refusing_ledger();
        let _guard = LedgerGuard::install(&mut ledger);
        assert!(set.insert(32, DISTINCT_PEAK_FAMILY).is_err());
    }
    assert_eq!(set.len(), len);
    assert_eq!(set.bytes(), bytes);
    assert!(!insert(&mut set, 16));
    assert!(insert(&mut set, 32));
}

#[test]
fn promote_inline_moves_the_held_words_free() {
    // The caller-forced promotion (the distinct set's tag-switch hook):
    // every held word moves, none is lost, and no charge is paid.
    let mut ledger = roomy_ledger();
    let _guard = LedgerGuard::install(&mut ledger);
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..9 {
        insert(&mut set, word);
    }
    let charged = set.charged();
    set.promote_inline();
    assert!(set.promoted());
    assert_eq!(set.len(), 9);
    assert_eq!(set.bytes(), word_set_bytes(9));
    assert_eq!(
        set.charged(),
        charged,
        "promotion charges nothing (both tiers share the stride-8 figure)"
    );
    for word in 0..9 {
        assert!(!insert(&mut set, word));
    }
    assert!(insert(&mut set, 9));
    // Idempotent on every other state: never moves back, never drops.
    set.promote_inline();
    assert_eq!(set.len(), 10);
    let mut empty = TieredWordSet::new(WordHash::Id);
    empty.promote_inline();
    assert_eq!(empty.len(), 0);
    assert!(!empty.promoted());
}

#[test]
fn clone_charge_semantics() {
    let mut ledger = roomy_ledger();
    let guard = LedgerGuard::install(&mut ledger);
    let empty = TieredWordSet::new(WordHash::Value);
    assert_eq!(
        empty
            .try_clone()
            .expect("an empty set reserves nothing")
            .charged(),
        0,
        "an empty clone reserves nothing"
    );
    let mut set = TieredWordSet::new(WordHash::Value);
    for word in 0..20 {
        insert(&mut set, word);
    }
    let copy = set.try_clone().expect("the roomy ledger admits the copy");
    assert_eq!(
        copy.charged(),
        set.bytes(),
        "a filled clone retains bytes()"
    );
    assert_eq!(copy.len(), set.len());
    drop(guard);
    // A refused retain propagates; the consumer's fallback is the
    // uncharged contents copy.
    let mut refusing = refusing_ledger();
    let _guard = LedgerGuard::install(&mut refusing);
    assert!(set.try_clone().is_err());
    let fallback = set.clone_contents();
    assert_eq!(fallback.charged(), 0);
    assert_eq!(fallback.len(), set.len());
}

#[test]
fn the_module_never_imports_the_exec_tree_or_the_walker() {
    assert_never_imports_exec_tree("tiered_set.rs", include_str!("tiered_set.rs"));
}

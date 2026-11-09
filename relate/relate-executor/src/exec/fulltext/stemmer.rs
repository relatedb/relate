//! The Porter stemmer the `english` search analyzer applies (Lucene's
//! `PorterStemFilter`, the original 1980 algorithm). Its unit test pins the
//! live-calibrated vocabulary (`princesses` → `princess`).

/// Stem one token. Non-ASCII-alphabetic tokens (digits, symbols, carrier
/// returns of the tokenizer) pass through unchanged.
pub(super) fn porter_stem(token: &str) -> String {
    if !token.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return token.to_string();
    }
    let mut word = token.as_bytes().to_vec();
    step_1a(&mut word);
    step_1b(&mut word);
    step_1c(&mut word);
    replace_first_matching(&mut word, STEP_2, 1);
    replace_first_matching(&mut word, STEP_3, 1);
    step_4(&mut word);
    step_5a(&mut word);
    step_5b(&mut word);
    String::from_utf8(word).expect("stemming preserves ASCII bytes")
}

const STEP_2: &[(&str, &str)] = &[
    ("ational", "ate"),
    ("tional", "tion"),
    ("enci", "ence"),
    ("anci", "ance"),
    ("izer", "ize"),
    ("bli", "ble"),
    ("alli", "al"),
    ("entli", "ent"),
    ("eli", "e"),
    ("ousli", "ous"),
    ("ization", "ize"),
    ("ation", "ate"),
    ("ator", "ate"),
    ("alism", "al"),
    ("iveness", "ive"),
    ("fulness", "ful"),
    ("ousness", "ous"),
    ("aliti", "al"),
    ("iviti", "ive"),
    ("biliti", "ble"),
    ("logi", "log"),
];

const STEP_3: &[(&str, &str)] = &[
    ("icate", "ic"),
    ("ative", ""),
    ("alize", "al"),
    ("iciti", "ic"),
    ("ical", "ic"),
    ("ful", ""),
    ("ness", ""),
];

const STEP_4: &[&str] = &[
    "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent", "ion", "ou",
    "ism", "ate", "iti", "ous", "ive", "ize",
];

/// Consonance of every byte position, in one forward pass: a vowel never
/// is, a consonant always is, and `y` is one exactly when the position
/// before it is not (a leading `y` is a vowel — Porter's alternating
/// rule). The one spelling of the rule; the `ends_*` predicates and the
/// `measure`/`contains_vowel` readers each read one profile, so a long
/// `y`-run costs one linear pass, not a recursive frame per byte (the
/// pre-fix spelling overflowed the stack on one).
fn consonant_profile(word: &[u8]) -> Vec<bool> {
    let mut previous = false;
    word.iter()
        .enumerate()
        .map(|(index, &byte)| {
            let current = match byte {
                b'a' | b'e' | b'i' | b'o' | b'u' => false,
                b'y' => index > 0 && !previous,
                _ => true,
            };
            previous = current;
            current
        })
        .collect()
}

/// Porter's `m`: the number of vowel-consonant sequences in the stem —
/// the vowel-run→consonant-run boundaries of the profile.
fn measure(stem: &[u8]) -> usize {
    consonant_profile(stem)
        .windows(2)
        .filter(|pair| !pair[0] && pair[1])
        .count()
}

fn contains_vowel(stem: &[u8]) -> bool {
    consonant_profile(stem).contains(&false)
}

fn ends_double_consonant(word: &[u8]) -> bool {
    word.len() >= 2
        && word[word.len() - 1] == word[word.len() - 2]
        && consonant_profile(word)[word.len() - 1]
}

/// Porter's `*o`: the stem ends consonant-vowel-consonant where the last
/// consonant is not `w`, `x` or `y`.
fn ends_cvc(word: &[u8]) -> bool {
    if word.len() < 3 || matches!(word[word.len() - 1], b'w' | b'x' | b'y') {
        return false;
    }
    let profile = consonant_profile(word);
    let last = word.len() - 1;
    profile[last - 2] && !profile[last - 1] && profile[last]
}

fn ends_with(word: &[u8], suffix: &str) -> bool {
    word.ends_with(suffix.as_bytes())
}

/// Steps 2 and 3 (Lucene's `PorterStemmer`): the FIRST suffix that matches
/// decides — when its measure gate fails the word is left unchanged, never
/// retried against a shorter suffix. (Only `ational`/`tional` and
/// `ization`/`ation` overlap in these tables, and a brute-force sweep of
/// 522k synthetic tokens showed the fall-through alternative converges to
/// the same stems; this is the protocol Lucene implements, pinned anyway.)
fn replace_first_matching(word: &mut Vec<u8>, table: &[(&str, &str)], min_measure: usize) {
    let Some((suffix, replacement)) = table.iter().find(|(suffix, _)| ends_with(word, suffix))
    else {
        return;
    };
    let stem_len = word.len() - suffix.len();
    if measure(&word[..stem_len]) < min_measure {
        return;
    }
    word.truncate(stem_len);
    word.extend_from_slice(replacement.as_bytes());
}

fn step_1a(word: &mut Vec<u8>) {
    if ends_with(word, "sses") || ends_with(word, "ies") {
        word.truncate(word.len() - 2);
    } else if !ends_with(word, "ss") && ends_with(word, "s") {
        word.truncate(word.len() - 1);
    }
}

fn step_1b(word: &mut Vec<u8>) {
    if ends_with(word, "eed") {
        let stem_len = word.len() - 3;
        if measure(&word[..stem_len]) > 0 {
            word.truncate(word.len() - 1);
        }
        return;
    }
    let stem_len = if ends_with(word, "ed") {
        word.len() - 2
    } else if ends_with(word, "ing") {
        word.len() - 3
    } else {
        return;
    };
    if !contains_vowel(&word[..stem_len]) {
        return;
    }
    word.truncate(stem_len);
    if ends_with(word, "at") || ends_with(word, "bl") || ends_with(word, "iz") {
        word.push(b'e');
    } else if ends_double_consonant(word) && !matches!(word[word.len() - 1], b'l' | b's' | b'z') {
        word.pop();
    } else if measure(word) == 1 && ends_cvc(word) {
        word.push(b'e');
    }
}

fn step_1c(word: &mut Vec<u8>) {
    if ends_with(word, "y") {
        let stem_len = word.len() - 1;
        if contains_vowel(&word[..stem_len]) {
            word.truncate(stem_len);
            word.push(b'i');
        }
    }
}

fn step_4(word: &mut Vec<u8>) {
    let Some(suffix) = STEP_4.iter().find(|suffix| ends_with(word, suffix)) else {
        return;
    };
    let stem_len = word.len() - suffix.len();
    if *suffix == "ion" {
        if stem_len > 0
            && matches!(word[stem_len - 1], b's' | b't')
            && measure(&word[..stem_len]) > 1
        {
            word.truncate(stem_len);
        }
        return;
    }
    if measure(&word[..stem_len]) > 1 {
        word.truncate(stem_len);
    }
}

fn step_5a(word: &mut Vec<u8>) {
    if !ends_with(word, "e") {
        return;
    }
    let stem_len = word.len() - 1;
    let stem_measure = measure(&word[..stem_len]);
    if stem_measure > 1 || (stem_measure == 1 && !ends_cvc(&word[..stem_len])) {
        word.truncate(stem_len);
    }
}

fn step_5b(word: &mut Vec<u8>) {
    if measure(word) > 1 && ends_double_consonant(word) && ends_with(word, "l") {
        word.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_stems_the_calibrated_vocabulary() {
        assert_eq!(porter_stem("princesses"), "princess");
        assert_eq!(porter_stem("princess"), "princess");
        assert_eq!(porter_stem("running"), "run");
        assert_eq!(porter_stem("journeys"), "journei");
        assert_eq!(porter_stem("winter"), "winter");
        assert_eq!(porter_stem("caresses"), "caress");
        assert_eq!(porter_stem("ponies"), "poni");
        assert_eq!(porter_stem("agreed"), "agre");
        // The overlapping-suffix edge (step 2's `ization` gate fails, and
        // `ation` must not be retried): Lucene's first-match protocol.
        assert_eq!(porter_stem("blization"), "blizat");
        assert_eq!(porter_stem("formalization"), "formal");
        // Non-alphabetic tokens pass through.
        assert_eq!(porter_stem("42"), "42");
    }

    /// The pre-fix spelling of the `y` rule — recursive, one frame per
    /// preceding `y` — pins the iterative profile's semantics: over a
    /// y-heavy pseudo-random corpus, every position agrees.
    #[test]
    fn y_runs_classify_like_the_recursive_reference() {
        fn reference(word: &[u8], index: usize) -> bool {
            match word[index] {
                b'a' | b'e' | b'i' | b'o' | b'u' => false,
                b'y' => index > 0 && !reference(word, index - 1),
                _ => true,
            }
        }
        let mut rng = relate_test_support::rng::Rng(0x5eed);
        for _ in 0..2_000 {
            let len = 1 + rng.below(24) as usize;
            let mut token = Vec::with_capacity(len);
            for _ in 0..len {
                // Two of three bytes are `y`: the runs are the point.
                token.push(if rng.below(3) == 0 {
                    b'a' + rng.below(26) as u8
                } else {
                    b'y'
                });
            }
            let profile = consonant_profile(&token);
            for (index, actual) in profile.iter().enumerate() {
                assert_eq!(*actual, reference(&token, index));
            }
        }
    }

    /// The rust-review High: a y-run term under `WITH ANALYZER 'english'`
    /// is query-reachable, and the recursive spelling overflowed the
    /// stack on it. A million `y`s stem in one linear pass.
    #[test]
    fn a_million_y_run_stems_without_recursion() {
        let word = "y".repeat(1_000_000);
        assert_eq!(porter_stem(&word), format!("{}i", "y".repeat(999_999)));
    }
}

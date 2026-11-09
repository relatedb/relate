//! The one policy for an egg that says or animates the user's words
//! (`.cowsay`, `.matrix`): join the arguments, drop control characters (so
//! the text can never move the cursor or start an escape), trim, fall back
//! to the egg's default when nothing printable is left, and cut to the
//! egg's cap so output stays bounded.

/// The text for `args` under the shared policy; `cap` counts characters.
pub(crate) fn text_arg(args: &[&str], default: &str, cap: usize) -> String {
    let joined: String = args.join(" ").chars().filter(|c| !c.is_control()).collect();
    let text = match joined.trim() {
        "" => default,
        t => t,
    };
    text.chars().take(cap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_strips_controls_and_trims() {
        assert_eq!(text_arg(&["a", "b"], "d", 10), "a b");
        assert_eq!(text_arg(&["  a\x1b[0m "], "d", 10), "a[0m");
        assert_eq!(text_arg(&["\x1b hi"], "d", 10), "hi");
    }

    #[test]
    fn blank_after_cleaning_falls_back_to_the_default() {
        assert_eq!(text_arg(&[], "d", 10), "d");
        assert_eq!(text_arg(&["  ", "\t"], "d", 10), "d");
        assert_eq!(text_arg(&["\x1b\x07"], "d", 10), "d");
    }

    #[test]
    fn the_cap_counts_characters_and_also_cuts_the_default() {
        assert_eq!(text_arg(&["ééééé"], "d", 3), "ééé");
        assert_eq!(text_arg(&[], "default", 3), "def");
    }
}

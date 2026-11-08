//! `.fortune`: one random line from the editable `assets/fortunes.txt`.

use crate::rng::Rng;
use crate::{Ctx, Refusal};

const FORTUNES: &str = include_str!("../assets/fortunes.txt");

const USAGE: &str = "Usage: .fortune";

/// The fortunes in a file: every line that is neither blank nor a `#`
/// comment, trimmed.
fn parse(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn pick<'a>(fortunes: &[&'a str], rng: &mut Rng) -> &'a str {
    fortunes[rng.below(fortunes.len())]
}

pub(crate) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    if !args.is_empty() {
        return Err(Refusal::Usage(USAGE));
    }
    let fortunes = parse(FORTUNES);
    let line = pick(&fortunes, &mut Rng::from_clock());
    writeln!(ctx.out, "{line}").map_err(|e| Refusal::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blanks() {
        assert_eq!(parse("# c\n\n  a  \nb\n#d\n"), vec!["a", "b"]);
    }

    #[test]
    fn shipped_file_has_plain_single_line_fortunes() {
        let all = parse(FORTUNES);
        assert!(all.len() >= 20);
        assert!(all.iter().all(|l| !l.chars().any(char::is_control)));
    }

    #[test]
    fn pick_returns_a_member_and_varies() {
        let all = parse(FORTUNES);
        let mut rng = Rng::seeded(42);
        let picks: Vec<&str> = (0..50).map(|_| pick(&all, &mut rng)).collect();
        assert!(picks.iter().all(|p| all.contains(p)));
        assert!(picks.iter().any(|p| *p != picks[0]));
    }
}

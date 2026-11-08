//! Archie, the mascot: the `.archie` dot command, the first-run banner art
//! and the opt-in prompt glyph. The sprites are editable text files under
//! `assets/archie/`, embedded at build time and parsed by [`super::sprite`].

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use crate::config_dir::config_dir_with;
use crate::repl::PromptStyle;
use relate_eggs::sprite::{Sprite, BRAND_PALETTE};
use relate_eggs::{Caps, Ctx, Refusal};

const LOGOMARK_L: &str = include_str!("../../assets/archie/logomark-l.txt");
const LOGOMARK_S: &str = include_str!("../../assets/archie/logomark-s.txt");

/// The once-per-install banner marker, in the config directory. Its own
/// file: telemetry's first-run claim is an opt-out away from never firing.
const BANNER_MARKER: &str = "archie-banner-seen";

/// Gap between the art and the banner text.
const BANNER_GAP: &str = "  ";

const USAGE: &str = "Usage: .archie [l|s]";

/// A shipped asset; a parse failure is a build defect the asset test pins.
fn parse_asset(text: &str) -> Sprite {
    Sprite::parse(text).expect("embedded Archie asset parses (pinned by the asset tests)")
}

/// The art as text lines for `caps`, `None` when the locale cannot show it.
/// Colour only when 24-bit colour is on offer; shade glyphs otherwise.
fn render(sprite: &Sprite, caps: Caps) -> Option<(Vec<String>, usize)> {
    if !caps.utf8 {
        return None;
    }
    Some(if caps.truecolor {
        (sprite.render_truecolor(), sprite.width())
    } else {
        (sprite.render_plain(), sprite.width() * 2)
    })
}

/// `.archie [l|s]`: print the large (default) or small logomark.
pub(super) fn run(ctx: &mut Ctx<'_>, args: &[&str]) -> Result<(), Refusal> {
    let text = match args {
        [] | ["l"] => LOGOMARK_L,
        ["s"] => LOGOMARK_S,
        _ => return Err(Refusal::Usage(USAGE)),
    };
    let (lines, _) = render(&parse_asset(text), ctx.caps).ok_or(Refusal::NotUtf8)?;
    let mut out = String::new();
    for line in lines {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    ctx.out
        .write_all(out.as_bytes())
        .map_err(|e| Refusal::Io(e.to_string()))
}

/// `RELATE_BANNER`: `1` forces the banner, `0` suppresses it, unset or
/// empty leaves it to the once-per-install rule.
#[derive(Debug, PartialEq, Eq)]
enum BannerSetting {
    Auto,
    Force,
    Off,
}

impl BannerSetting {
    fn parse(value: Option<String>) -> Result<BannerSetting, String> {
        match value.as_deref().map(str::trim) {
            None | Some("") => Ok(BannerSetting::Auto),
            Some("1") => Ok(BannerSetting::Force),
            Some("0") => Ok(BannerSetting::Off),
            Some(other) => Err(format!(
                "invalid RELATE_BANNER: '{other}' is neither 1 nor 0"
            )),
        }
    }
}

/// The outcome of claiming the banner marker.
#[derive(Debug, PartialEq, Eq)]
enum Claim {
    /// This call created the marker: the first start.
    Fresh,
    /// The marker already existed.
    Seen,
    /// No config directory resolves, or the marker cannot be created: the
    /// install cannot be tracked, so the banner is not shown unforced.
    Untracked,
}

/// Claim the marker atomically (exclusive create): exactly one start wins.
fn claim_marker(dir: Option<&Path>) -> Claim {
    let Some(dir) = dir else {
        return Claim::Untracked;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return Claim::Untracked;
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(BANNER_MARKER))
    {
        Ok(_) => Claim::Fresh,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Claim::Seen,
        Err(_) => Claim::Untracked,
    }
}

/// The show decision. The marker is claimed last, only once everything else
/// says the banner would print, so an unsupported locale never burns it.
fn should_show(setting: &BannerSetting, caps: Caps, claim: impl FnOnce() -> Claim) -> bool {
    if *setting == BannerSetting::Off || !caps.utf8 {
        return false;
    }
    *setting == BannerSetting::Force || claim() == Claim::Fresh
}

/// Archie beside `line`, the text on the art's middle row.
fn banner_lines(sprite: &Sprite, caps: Caps, line: &str) -> Option<Vec<String>> {
    let (art, _) = render(sprite, caps)?;
    let text_row = art.len() / 2;
    Some(
        art.into_iter()
            .enumerate()
            .map(|(i, row)| {
                if i == text_row {
                    format!("{row}{BANNER_GAP}{line}")
                } else {
                    row
                }
            })
            .collect(),
    )
}

/// Print the first-run banner when the rules allow, returning whether it
/// printed (the banner text included).
pub(super) fn print_first_run_banner(
    env: &dyn Fn(&str) -> Option<String>,
    line: &str,
    out: &mut dyn Write,
) -> bool {
    let setting = match BannerSetting::parse(env("RELATE_BANNER")) {
        Ok(s) => s,
        Err(msg) => {
            crate::print_error(format_args!("Warning: {msg}; ignoring it"));
            BannerSetting::Auto
        }
    };
    let caps = Caps::from_env(env);
    let dir = config_dir_with(env);
    if !should_show(&setting, caps, || claim_marker(dir.as_deref())) {
        return false;
    }
    let Some(lines) = banner_lines(&parse_asset(LOGOMARK_L), caps, line) else {
        return false;
    };
    let mut text = lines.join("\n");
    text.push('\n');
    out.write_all(text.as_bytes()).is_ok()
}

/// `RELATE_PROMPT=archie` opts in to the glyph prompt; it falls back to the
/// classic prompts on a non-UTF-8 locale or under `NO_COLOR`. Any other
/// value is reported and ignored.
pub(super) fn prompt_style(env: &dyn Fn(&str) -> Option<String>) -> PromptStyle {
    match env("RELATE_PROMPT").as_deref().map(str::trim) {
        None | Some("") => return PromptStyle::Classic,
        Some("archie") => {}
        Some(other) => {
            crate::print_error(format_args!(
                "Warning: invalid RELATE_PROMPT: '{other}' is not archie; ignoring it"
            ));
            return PromptStyle::Classic;
        }
    }
    let caps = Caps::from_env(env);
    if !caps.utf8 || Caps::no_color(env) {
        return PromptStyle::Classic;
    }
    PromptStyle::Archie {
        tint: caps.truecolor.then_some(prompt_tint()),
    }
}

/// The glyph's colour: the logomark's mid tone, as an SGR parameter string.
fn prompt_tint() -> String {
    let c = BRAND_PALETTE.md;
    format!("38;2;{};{};{}", c.0, c.1, c.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;
    use relate_eggs::sprite::TONES;

    const TRUE: Caps = Caps {
        utf8: true,
        truecolor: true,
    };
    const PLAIN: Caps = Caps {
        utf8: true,
        truecolor: false,
    };
    const ASCII: Caps = Caps {
        utf8: false,
        truecolor: false,
    };

    /// Every shipped asset is rectangular and uses only legend symbols.
    #[test]
    fn every_asset_file_is_rectangular_and_uses_only_legend_symbols() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/archie");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let rows: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
            let width = rows.first().expect("rows").chars().count();
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(row.chars().count(), width, "{path:?} row {}", i + 1);
                for ch in row.chars() {
                    assert!(
                        ch == ' ' || ch == '.' || TONES.iter().any(|t| t.symbol == ch),
                        "{path:?}: symbol {ch:?} is not in the legend"
                    );
                }
            }
            Sprite::parse(&text).unwrap();
            seen += 1;
        }
        assert!(seen >= 2);
    }

    #[test]
    fn logomarks_have_the_shipped_dimensions() {
        let l = parse_asset(LOGOMARK_L);
        assert_eq!((l.width(), l.height()), (12, 10));
        let s = parse_asset(LOGOMARK_S);
        assert_eq!((s.width(), s.height()), (8, 6));
    }

    #[test]
    fn render_refuses_without_utf8_and_honours_colour_caps() {
        let s = parse_asset(LOGOMARK_S);
        assert!(render(&s, ASCII).is_none());
        let (plain, w) = render(&s, PLAIN).unwrap();
        assert_eq!(w, 16);
        assert!(plain.iter().all(|l| !l.contains('\x1b')));
        assert!(plain[0].contains('\u{2593}'));
        let (colour, w) = render(&s, TRUE).unwrap();
        assert_eq!((colour.len(), w), (3, 8));
        assert!(colour[0].contains("\x1b[38;2;"));
    }

    #[test]
    fn banner_keeps_the_text_byte_identical_on_the_middle_row() {
        let text = "Relate 1.0. End statements with ;  (.quit to exit)";
        let lines = banner_lines(&parse_asset(LOGOMARK_L), PLAIN, text).unwrap();
        assert_eq!(lines.len(), 10);
        assert_eq!(lines.iter().filter(|l| l.contains(text)).count(), 1);
        assert!(lines[5].ends_with(text));
        assert!(lines.iter().all(|l| !l.contains('\x1b')));
    }

    #[test]
    fn banner_setting_parses_and_rejects() {
        assert_eq!(BannerSetting::parse(None), Ok(BannerSetting::Auto));
        assert_eq!(
            BannerSetting::parse(Some(" ".into())),
            Ok(BannerSetting::Auto)
        );
        assert_eq!(
            BannerSetting::parse(Some("1".into())),
            Ok(BannerSetting::Force)
        );
        assert_eq!(
            BannerSetting::parse(Some("0".into())),
            Ok(BannerSetting::Off)
        );
        assert!(BannerSetting::parse(Some("yes".into()))
            .unwrap_err()
            .contains("invalid RELATE_BANNER"));
    }

    #[test]
    fn marker_claim_is_once_per_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("relate");
        assert_eq!(claim_marker(Some(&dir)), Claim::Fresh);
        assert_eq!(claim_marker(Some(&dir)), Claim::Seen);
        assert_eq!(claim_marker(None), Claim::Untracked);
    }

    #[test]
    fn show_decision_table() {
        let fresh = || Claim::Fresh;
        let seen = || Claim::Seen;
        assert!(should_show(&BannerSetting::Auto, TRUE, fresh));
        assert!(!should_show(&BannerSetting::Auto, TRUE, seen));
        assert!(!should_show(&BannerSetting::Auto, TRUE, || {
            Claim::Untracked
        }));
        assert!(should_show(&BannerSetting::Force, TRUE, seen));
        assert!(!should_show(&BannerSetting::Off, TRUE, fresh));
        // No UTF-8: never shown, and the marker is not burned.
        assert!(!should_show(&BannerSetting::Force, ASCII, fresh));
        assert!(!should_show(&BannerSetting::Auto, ASCII, || {
            panic!("the marker must not be claimed")
        }));
    }

    #[test]
    fn first_run_banner_prints_once_then_stays_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        let pairs = [("XDG_CONFIG_HOME", home), ("LANG", "en_US.UTF-8")];
        let env = env_of(&pairs);
        let mut out = Vec::new();
        assert!(print_first_run_banner(&env, "hello", &mut out));
        assert!(String::from_utf8(out).unwrap().contains("hello"));
        assert!(!print_first_run_banner(&env, "hello", &mut Vec::new()));
        let forced = [
            ("XDG_CONFIG_HOME", home),
            ("LANG", "en_US.UTF-8"),
            ("RELATE_BANNER", "1"),
        ];
        assert!(print_first_run_banner(
            &env_of(&forced),
            "hello",
            &mut Vec::new()
        ));
        let off = [
            ("XDG_CONFIG_HOME", home),
            ("LANG", "en_US.UTF-8"),
            ("RELATE_BANNER", "0"),
        ];
        assert!(!print_first_run_banner(
            &env_of(&off),
            "hello",
            &mut Vec::new()
        ));
    }

    #[test]
    fn prompt_is_opt_in_and_falls_back() {
        let utf8 = ("LANG", "en_US.UTF-8");
        assert_eq!(prompt_style(&env_of(&[utf8])), PromptStyle::Classic);
        let on = prompt_style(&env_of(&[utf8, ("RELATE_PROMPT", "archie")]));
        assert_eq!(on, PromptStyle::Archie { tint: None });
        let tc = prompt_style(&env_of(&[
            utf8,
            ("RELATE_PROMPT", "archie"),
            ("COLORTERM", "truecolor"),
        ]));
        assert_eq!(
            tc,
            PromptStyle::Archie {
                tint: Some("38;2;10;97;144".into())
            }
        );
        // Non-UTF-8 locale and NO_COLOR both fall back.
        assert_eq!(
            prompt_style(&env_of(&[("LANG", "C"), ("RELATE_PROMPT", "archie")])),
            PromptStyle::Classic
        );
        assert_eq!(
            prompt_style(&env_of(&[
                utf8,
                ("RELATE_PROMPT", "archie"),
                ("NO_COLOR", "1")
            ])),
            PromptStyle::Classic
        );
    }
}

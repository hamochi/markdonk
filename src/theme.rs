//! Colours and small config files. On Omarchy the palette comes from the active
//! theme's `colors.toml`; everywhere else the built-in themes apply.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub bg: u32,
    pub text: u32,
    pub muted: u32,
    pub heading: u32,
    pub link: u32,
    pub border: u32,
    pub code_bg: u32,
    pub inline_bg: u32,
    pub inline_fg: u32,
    pub row_alt: u32,
    pub head_bg: u32,
    pub quote_bar: u32,
    pub select: u32,
    pub match_all: u32,
    pub match_now: u32,
    pub side_bg: u32,
    pub side_now: u32,
    pub bar: u32,
    pub pill_bg: u32,
    pub pill_fg: u32,
    pub alert: u32,
    pub tokens: [u32; 7],
}

pub const DARK: Theme = Theme {
    bg: 0x0f1216,
    text: 0xd5dae1,
    muted: 0x8b95a3,
    heading: 0xf1f4f8,
    link: 0x6cb0ff,
    border: 0x2c333c,
    code_bg: 0x181c22,
    inline_bg: 0x242a32,
    inline_fg: 0xe3e7ec,
    row_alt: 0x151920,
    head_bg: 0x1a1f26,
    quote_bar: 0x3a434e,
    select: 0x3f7fd0,
    match_all: 0xc99a2e,
    match_now: 0xf08a24,
    side_bg: 0x14181d,
    side_now: 0x222a33,
    bar: 0x4a5461,
    pill_bg: 0x262c34,
    pill_fg: 0xe3e7ec,
    alert: 0x6cb0ff,
    tokens: [0xd5dae1, 0x7a8594, 0x9fd28a, 0xe5b567, 0xf08fa0, 0x74cfd9, 0x8fb8ff],
};

pub const LIGHT: Theme = Theme {
    bg: 0xffffff,
    text: 0x24292f,
    muted: 0x626b76,
    heading: 0x11151a,
    link: 0x0a63d1,
    border: 0xd5dae0,
    code_bg: 0xf5f6f8,
    inline_bg: 0xeceef1,
    inline_fg: 0x24292f,
    row_alt: 0xf7f8fa,
    head_bg: 0xf0f2f4,
    quote_bar: 0xcdd3da,
    select: 0x4b93ee,
    match_all: 0xf4d65c,
    match_now: 0xf59a33,
    side_bg: 0xf5f6f8,
    side_now: 0xe3e8ee,
    bar: 0xa8b0ba,
    pill_bg: 0x2c333c,
    pill_fg: 0xf5f6f8,
    alert: 0x0a63d1,
    tokens: [0x24292f, 0x6a737d, 0x1a7f37, 0x9a6700, 0xcf222e, 0x0b7285, 0x6639ba],
};

/// Mixes `a` into `b`; `amount` 1.0 is all `a`.
fn mix(a: u32, b: u32, amount: f32) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f32, ((b >> shift) & 0xff) as f32);
        ((x * amount + y * (1.0 - amount)).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Yields `key = value` pairs from a flat config or TOML file.
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim().trim_matches('"')))
    })
}

/// Builds a theme from `colors.toml` text. Missing keys keep the built-in value.
pub fn from_colors(text: &str) -> Theme {
    let get = |name: &str| {
        pairs(text)
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| u32::from_str_radix(v.strip_prefix('#')?, 16).ok())
    };
    let light = pairs(text).any(|(k, v)| k == "mode" && v == "light");
    let base = if light { LIGHT } else { DARK };
    let or = |name: &str, fallback: u32| get(name).unwrap_or(fallback);

    let bg = or("background", base.bg);
    let text_color = or("foreground", base.text);
    let accent = or("accent", base.link);
    let code_bg = or("dark_background", base.code_bg);
    let raised = or("lighter_background", base.inline_bg);
    // Theme "muted" tones are often too faint for reading, so derive one from the text.
    let muted = mix(text_color, bg, 0.65);
    Theme {
        bg,
        text: text_color,
        muted,
        heading: or("bright_foreground", base.heading),
        link: accent,
        border: raised,
        code_bg,
        inline_bg: raised,
        inline_fg: text_color,
        row_alt: code_bg,
        head_bg: raised,
        quote_bar: or("muted", base.quote_bar),
        select: accent,
        match_all: or("yellow", base.match_all),
        match_now: or("orange", base.match_now),
        side_bg: code_bg,
        side_now: raised,
        bar: or("muted", base.bar),
        pill_bg: raised,
        pill_fg: text_color,
        alert: accent,
        tokens: [
            text_color,
            mix(text_color, bg, 0.55),
            or("green", base.tokens[2]),
            or("orange", base.tokens[3]),
            or("magenta", base.tokens[4]),
            or("cyan", base.tokens[5]),
            or("blue", base.tokens[6]),
        ],
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(home()?.join(".config")),
    }
}

/// Written by `omarchy theme set` after it swaps the theme in, so it's safe to watch.
pub fn omarchy_theme_marker() -> Option<PathBuf> {
    Some(home()?.join(".local/state/omarchy/current/theme.name"))
}

pub fn fontconfig_file() -> Option<PathBuf> {
    Some(config_dir()?.join("fontconfig/fonts.conf"))
}

pub fn load_omarchy() -> Option<Theme> {
    let path = home()?.join(".local/state/omarchy/current/theme/colors.toml");
    Some(from_colors(&std::fs::read_to_string(path).ok()?))
}

/// Reads `background-opacity` from `~/.config/markdonk/config`.
pub fn configured_opacity() -> Option<f32> {
    let text = std::fs::read_to_string(config_dir()?.join("markdonk/config")).ok()?;
    parse_opacity(&text)
}

fn parse_opacity(text: &str) -> Option<f32> {
    let value = pairs(text).find(|(k, _)| *k == "background-opacity")?.1;
    value.parse::<f32>().ok().filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKYO: &str = "mode = \"dark\"\n\naccent = \"#7aa2f7\"\nbackground = \"#1a1b26\"\n\
        foreground = \"#a9b1d6\"\ndark_background = \"#13141c\"\nlighter_background = \"#24283b\"\n\
        bright_foreground = \"#c0caf5\"\ngreen = \"#9ece6a\"\nmagenta = \"#ad8ee6\"\n";

    #[test]
    fn maps_a_theme_file() {
        let t = from_colors(TOKYO);
        assert_eq!(t.bg, 0x1a1b26);
        assert_eq!(t.text, 0xa9b1d6);
        assert_eq!(t.link, 0x7aa2f7);
        assert_eq!(t.heading, 0xc0caf5);
        assert_eq!(t.code_bg, 0x13141c);
        assert_eq!(t.tokens[2], 0x9ece6a);
        assert_eq!(t.tokens[4], 0xad8ee6);
    }

    #[test]
    fn missing_or_bad_values_fall_back_to_the_mode_default() {
        let t = from_colors("mode = \"light\"\nbackground = \"oops\"\n# accent = \"#000000\"\n");
        assert_eq!(t.bg, LIGHT.bg);
        assert_eq!(t.link, LIGHT.link);
        assert_eq!(from_colors("").bg, DARK.bg);
    }

    #[test]
    fn muted_text_sits_between_text_and_background() {
        let t = from_colors(TOKYO);
        let green = |c: u32| (c >> 8) & 0xff;
        assert!(green(t.muted) < green(t.text) && green(t.muted) > green(t.bg));
    }

    #[test]
    fn mix_endpoints_are_exact() {
        assert_eq!(mix(0x102030, 0xa0b0c0, 1.0), 0x102030);
        assert_eq!(mix(0x102030, 0xa0b0c0, 0.0), 0xa0b0c0);
    }

    #[test]
    fn parses_opacity_and_clamps_it() {
        assert_eq!(parse_opacity("# c\nbackground-opacity = 0.9\n"), Some(0.9));
        assert_eq!(parse_opacity("background-opacity = 4"), Some(1.0));
        assert_eq!(parse_opacity("background-opacity = nope"), None);
        assert_eq!(parse_opacity("background-opacity = NaN"), None);
        assert_eq!(parse_opacity("other = 1"), None);
    }
}

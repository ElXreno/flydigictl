//! Colours, taken from whatever the machine already generates.
//!
//! There is no cross-desktop palette to ask for: `org.freedesktop.appearance`
//! offers a light/dark preference, an accent colour and a contrast flag, and
//! nothing else. GTK and Qt applications look themed because their toolkits
//! carry a theme engine, which is not something a window drawn on wgpu has.
//!
//! So the interface reads a file. Its own, written for it and nothing else,
//! which is the only way a colour in it can be argued with individually. And
//! failing that, the files a machine already has: pywal wrote its palette to
//! `~/.cache/wal/colors.json`, wallust inherited both the schema and the
//! ecosystem around it, and between them a great many desktops already have
//! that file. Reading it costs nothing and needs no per-application wiring,
//! on any distribution. A base16 scheme works too, since that is the shape
//! colour scheme generators speak among themselves.
//!
//! Nothing found means nothing invented: the interface falls back to the
//! desktop's light or dark preference.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::theme::Palette;
use iced::{Color, Theme};

use serde::Deserialize;

/// Where to look, most specific first.
fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(explicit) = std::env::var_os("FLYDIGICTL_PALETTE") {
        paths.push(PathBuf::from(explicit));
    }

    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".config")));

    if let Some(config) = config {
        paths.push(config.join("flydigictl/palette.json"));
    }

    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".cache")));

    if let Some(cache) = cache {
        paths.push(cache.join("wallust/colors.json"));
        paths.push(cache.join("wal/colors.json"));
    }

    // pywal hardcoded this one, so it is worth checking even when
    // XDG_CACHE_HOME points elsewhere.
    if let Some(home) = home() {
        paths.push(home.join(".cache/wal/colors.json"));
    }

    paths
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn load() -> Option<Theme> {
    for path in candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        if let Some(palette) = parse(&text) {
            log::info!("colours from {}", path.display());
            return Some(Theme::custom("system".to_string(), palette));
        }

        log::warn!("{} is not a palette this understands", path.display());
    }

    None
}

fn parse(text: &str) -> Option<Palette> {
    // Roles first: it is the form written for this application specifically,
    // so it is the one that means what it says.
    serde_json::from_str::<Roles>(text)
        .ok()
        .and_then(Roles::palette)
        .or_else(|| {
            serde_json::from_str::<Wal>(text)
                .ok()
                .and_then(Wal::palette)
        })
        .or_else(|| {
            serde_json::from_str::<Base16>(text)
                .ok()
                .and_then(Base16::palette)
        })
}

/// The six colours this interface actually uses, named for what they do.
///
/// Anything mapping a scheme onto a window has to make these choices anyway;
/// spelling them out means they can be argued with, one colour at a time.
#[derive(Deserialize)]
struct Roles {
    background: String,
    text: String,
    primary: String,
    success: String,
    warning: String,
    danger: String,
}

impl Roles {
    fn palette(self) -> Option<Palette> {
        Some(Palette {
            background: rgb(&self.background)?,
            text: rgb(&self.text)?,
            primary: rgb(&self.primary)?,
            success: rgb(&self.success)?,
            warning: rgb(&self.warning)?,
            danger: rgb(&self.danger)?,
        })
    }
}

/// pywal and wallust: named terminal colours plus the three special ones.
#[derive(Deserialize)]
struct Wal {
    special: Special,
    colors: HashMap<String, String>,
}

#[derive(Deserialize)]
struct Special {
    background: String,
    foreground: String,
}

impl Wal {
    fn palette(self) -> Option<Palette> {
        let color = |index: usize| {
            self.colors
                .get(&format!("color{index}"))
                .and_then(|c| rgb(c))
        };

        // The ANSI numbering every one of these schemes follows: 1 red,
        // 2 green, 3 yellow, 4 blue. Which makes the mapping onto danger,
        // success, warning and primary the obvious one.
        Some(Palette {
            background: rgb(&self.special.background)?,
            text: rgb(&self.special.foreground)?,
            primary: color(4)?,
            success: color(2)?,
            warning: color(3)?,
            danger: color(1)?,
        })
    }
}

/// A base16 scheme, which is what colour scheme generators pass around.
///
/// The slot names are the scheme's, not this crate's, so they stay as written.
#[derive(Deserialize)]
#[allow(non_snake_case)]
struct Base16 {
    base00: String,
    base05: String,
    base08: String,
    base0A: String,
    base0B: String,
    base0D: String,
}

impl Base16 {
    fn palette(self) -> Option<Palette> {
        Some(Palette {
            background: rgb(&self.base00)?,
            text: rgb(&self.base05)?,
            primary: rgb(&self.base0D)?,
            success: rgb(&self.base0B)?,
            warning: rgb(&self.base0A)?,
            danger: rgb(&self.base08)?,
        })
    }
}

/// `rrggbb`, with or without a leading hash. Longer forms carry an alpha this
/// has no use for, so the first six digits are all that is read.
fn rgb(text: &str) -> Option<Color> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if hex.len() < 6 || !hex.is_char_boundary(6) {
        return None;
    }

    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();

    Some(Color::from_rgb8(byte(0..2)?, byte(2..4)?, byte(4..6)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_wal_cache() {
        let text = r##"{
            "wallpaper": "/home/someone/wall.png",
            "special": { "background": "#1f2430", "foreground": "#cccac2", "cursor": "#ffcc66" },
            "colors": {
                "color0": "#1f2430", "color1": "#f28779", "color2": "#d5ff80",
                "color3": "#ffd173", "color4": "#73d0ff", "color5": "#dfbfff",
                "color6": "#95e6cb", "color7": "#cccac2"
            }
        }"##;

        let palette = parse(text).unwrap();
        assert_eq!(palette.background, Color::from_rgb8(0x1F, 0x24, 0x30));
        assert_eq!(palette.primary, Color::from_rgb8(0x73, 0xD0, 0xFF));
        assert_eq!(palette.danger, Color::from_rgb8(0xF2, 0x87, 0x79));
    }

    #[test]
    fn reads_a_base16_scheme() {
        let text = r##"{
            "base00": "1f2430", "base05": "cccac2", "base08": "f28779",
            "base0A": "ffd173", "base0B": "d5ff80", "base0D": "73d0ff"
        }"##;

        let palette = parse(text).unwrap();
        assert_eq!(palette.text, Color::from_rgb8(0xCC, 0xCA, 0xC2));
        assert_eq!(palette.success, Color::from_rgb8(0xD5, 0xFF, 0x80));
    }

    #[test]
    fn reads_named_roles() {
        let text = r##"{
            "background": "#1f2430", "text": "#cccac2", "primary": "#73d0ff",
            "success": "#d5ff80", "warning": "#ffd173", "danger": "#f28779"
        }"##;

        let palette = parse(text).unwrap();
        assert_eq!(palette.primary, Color::from_rgb8(0x73, 0xD0, 0xFF));
        assert_eq!(palette.warning, Color::from_rgb8(0xFF, 0xD1, 0x73));
    }

    #[test]
    fn ignores_json_that_is_not_a_palette() {
        assert!(parse(r##"{"colors": {"color1": "not a colour"}}"##).is_none());
        assert!(parse("[]").is_none());
    }
}

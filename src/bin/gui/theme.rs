//! Colours, taken from a file so the desktop can decide them.
//!
//! Stylix and friends generate a scheme for the whole machine, and a lone
//! window with its own opinion looks out of place. The file is optional: with
//! nothing to read, the interface falls back to a built-in theme rather than
//! demanding configuration.

use std::path::PathBuf;

use iced::theme::Palette;
use iced::{Color, Theme};

const SYSTEM: &str = "/etc/flydigictl/theme.toml";

/// Where a user's own colours override the machine's.
fn user_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

    Some(base.join("flydigictl/theme.toml"))
}

pub fn load() -> Theme {
    user_path()
        .and_then(|path| read(&path))
        .or_else(|| read(&PathBuf::from(SYSTEM)))
        .unwrap_or(Theme::CatppuccinMacchiato)
}

fn read(path: &std::path::Path) -> Option<Theme> {
    let text = std::fs::read_to_string(path).ok()?;
    let colors: Colors = toml::from_str(&text).ok()?;

    Some(Theme::custom(
        "system".to_string(),
        Palette {
            background: parse(&colors.background)?,
            text: parse(&colors.text)?,
            primary: parse(&colors.primary)?,
            success: parse(&colors.success)?,
            warning: parse(&colors.warning)?,
            danger: parse(&colors.danger)?,
        },
    ))
}

#[derive(serde::Deserialize)]
struct Colors {
    background: String,
    text: String,
    primary: String,
    success: String,
    warning: String,
    danger: String,
}

fn parse(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 {
        return None;
    }

    let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();

    Some(Color::from_rgb8(byte(0..2)?, byte(2..4)?, byte(4..6)?))
}

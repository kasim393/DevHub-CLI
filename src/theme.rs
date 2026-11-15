//! The neon look. One fixed palette; `background = "none"` in the config makes it
//! see-through (for a terminal with acrylic blur). The drawing code reads it via `th()`.

use ratatui::style::Color;
use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Filled behind everything; `None` leaves the terminal's own background showing.
    pub background: Option<Color>,
    pub text: Color,
    pub dim: Color,
    /// Panel titles and section names.
    pub label: Color,
    pub border: Color,
    pub accent: Color,
    pub ok: Color,
    pub busy: Color,
    pub bad: Color,
    /// The selected row; `None` marks it with bold accent text instead of a fill.
    pub selected: Option<Color>,
    /// Log lines matching a search; `None` underlines them instead.
    pub matched: Option<Color>,
}

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const NEON: Theme = Theme {
    background: Some(hex(0x070A09)),
    text: hex(0xC9E9DE),
    dim: hex(0x4F6A61),
    label: hex(0x5B7A70),
    border: hex(0x1D2B27),
    accent: hex(0x7CF2C2),
    ok: hex(0x7CF2C2),
    busy: hex(0xE8C46A),
    bad: hex(0xFF6B6B),
    selected: Some(hex(0x10201B)),
    matched: Some(hex(0x1F3A30)),
};

/// The neon look; see-through when `transparent` (no background, no fills).
pub fn neon(transparent: bool) -> Theme {
    if transparent {
        Theme { background: None, selected: None, matched: None, ..NEON }
    } else {
        NEON
    }
}

/// The escape code that sets the terminal's own default background (OSC 11), which
/// also colours the padding around the grid that devhub cannot draw on. With no solid
/// background, OSC 111 instead: back to the terminal's configured colour.
pub fn terminal_background_code(bg: Option<Color>) -> String {
    match bg {
        Some(Color::Rgb(r, g, b)) => format!("\x1b]11;#{r:02x}{g:02x}{b:02x}\x1b\\"),
        _ => RESET_BACKGROUND.to_string(),
    }
}

/// OSC 111: the terminal's background back to what its settings say.
pub const RESET_BACKGROUND: &str = "\x1b]111\x1b\\";

/// Writes an escape code straight to the terminal.
pub fn emit(code: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(code.as_bytes());
    let _ = out.flush();
}

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(NEON) };
}

/// The theme being drawn with.
pub fn th() -> Theme {
    CURRENT.with(|c| c.get())
}

pub fn set(t: Theme) {
    CURRENT.with(|c| c.set(t));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neon_and_its_see_through_variant() {
        assert!(neon(false).background.is_some());
        let t = neon(true);
        assert!(t.background.is_none() && t.selected.is_none());
        assert_eq!(t.accent, NEON.accent);
    }

    #[test]
    fn terminal_background_codes() {
        assert_eq!(terminal_background_code(Some(Color::Rgb(0x1a, 0x1b, 0x26))), "\x1b]11;#1a1b26\x1b\\");
        assert_eq!(terminal_background_code(None), RESET_BACKGROUND);
        assert_eq!(terminal_background_code(Some(Color::Indexed(236))), RESET_BACKGROUND);
    }
}

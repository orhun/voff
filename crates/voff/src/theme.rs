//! Near-black, blue-toned terminal palette with restrained burnt-orange accents.

use ratatui::style::{Color, Style};

pub const BACKGROUND: Color = Color::Rgb(3, 8, 14);
pub const SURFACE: Color = Color::Rgb(7, 15, 24);
pub const ELEVATED: Color = Color::Rgb(12, 27, 42);
pub const BORDER: Color = Color::Rgb(67, 104, 131);
pub const TEXT: Color = Color::Rgb(230, 241, 249);
pub const MUTED: Color = Color::Rgb(166, 195, 213);
pub const ORANGE: Color = Color::Rgb(192, 93, 43);
pub const YELLOW: Color = Color::Rgb(196, 161, 91);
pub const AQUA: Color = Color::Rgb(103, 165, 164);
pub const GREEN: Color = Color::Rgb(124, 165, 138);
pub const BLUE: Color = Color::Rgb(116, 153, 185);
pub const PURPLE: Color = Color::Rgb(157, 137, 175);
pub const RED: Color = Color::Rgb(194, 105, 104);

pub const fn canvas() -> Style {
    Style::new().fg(TEXT).bg(BACKGROUND)
}

pub const fn surface() -> Style {
    Style::new().fg(TEXT).bg(SURFACE)
}

pub const fn elevated() -> Style {
    Style::new().fg(TEXT).bg(ELEVATED)
}

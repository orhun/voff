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

const AGENT_COLORS: [Color; 32] = [
    ORANGE,
    YELLOW,
    AQUA,
    PURPLE,
    BLUE,
    GREEN,
    RED,
    Color::Rgb(174, 140, 217),
    Color::Rgb(217, 196, 140),
    Color::Rgb(140, 215, 217),
    Color::Rgb(217, 140, 192),
    Color::Rgb(170, 217, 140),
    Color::Rgb(140, 148, 217),
    Color::Rgb(217, 155, 140),
    Color::Rgb(140, 217, 177),
    Color::Rgb(200, 140, 217),
    Color::Rgb(211, 217, 140),
    Color::Rgb(140, 189, 217),
    Color::Rgb(217, 140, 167),
    Color::Rgb(145, 217, 140),
    Color::Rgb(158, 140, 217),
    Color::Rgb(217, 181, 140),
    Color::Rgb(140, 217, 203),
    Color::Rgb(217, 140, 208),
    Color::Rgb(186, 217, 140),
    Color::Rgb(140, 164, 217),
    Color::Rgb(217, 140, 141),
    Color::Rgb(140, 217, 162),
    Color::Rgb(184, 140, 217),
    Color::Rgb(217, 206, 140),
    Color::Rgb(140, 205, 217),
    Color::Rgb(217, 140, 183),
];

/// Assign a stable highlight color by an agent's position in the configured team.
#[must_use]
pub const fn agent_color(index: usize) -> Color {
    AGENT_COLORS[index % AGENT_COLORS.len()]
}

pub const fn canvas() -> Style {
    Style::new().fg(TEXT).bg(BACKGROUND)
}

pub const fn surface() -> Style {
    Style::new().fg(TEXT).bg(SURFACE)
}

pub const fn elevated() -> Style {
    Style::new().fg(TEXT).bg(ELEVATED)
}

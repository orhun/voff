//! High-contrast navy terminal palette with vivid voff-orange and blue accents.

use ratatui::style::{Color, Style};

pub const BACKGROUND: Color = Color::Rgb(8, 12, 23);
pub const SURFACE: Color = Color::Rgb(15, 22, 38);
pub const ELEVATED: Color = Color::Rgb(26, 38, 61);
pub const BORDER: Color = Color::Rgb(67, 111, 158);
pub const TEXT: Color = Color::Rgb(235, 241, 255);
pub const MUTED: Color = Color::Rgb(164, 181, 211);
pub const ORANGE: Color = Color::Rgb(250, 93, 0);
pub const YELLOW: Color = Color::Rgb(255, 202, 93);
pub const AQUA: Color = Color::Rgb(70, 218, 210);
pub const GREEN: Color = Color::Rgb(91, 220, 153);
pub const BLUE: Color = Color::Rgb(116, 177, 255);
pub const PURPLE: Color = Color::Rgb(194, 151, 255);
pub const RED: Color = Color::Rgb(255, 107, 120);

const AGENT_COLORS: [Color; 32] = [
    ORANGE,
    YELLOW,
    AQUA,
    PURPLE,
    BLUE,
    GREEN,
    RED,
    Color::Rgb(194, 151, 255),
    Color::Rgb(255, 202, 93),
    Color::Rgb(70, 218, 210),
    Color::Rgb(255, 126, 198),
    Color::Rgb(145, 235, 111),
    Color::Rgb(139, 159, 255),
    Color::Rgb(255, 145, 113),
    Color::Rgb(91, 220, 153),
    Color::Rgb(218, 145, 255),
    Color::Rgb(237, 224, 105),
    Color::Rgb(116, 195, 255),
    Color::Rgb(255, 130, 167),
    Color::Rgb(163, 235, 107),
    Color::Rgb(171, 145, 255),
    Color::Rgb(255, 186, 119),
    Color::Rgb(100, 230, 199),
    Color::Rgb(244, 146, 226),
    Color::Rgb(194, 235, 107),
    Color::Rgb(145, 179, 255),
    Color::Rgb(255, 134, 139),
    Color::Rgb(104, 230, 164),
    Color::Rgb(206, 145, 255),
    Color::Rgb(255, 216, 105),
    Color::Rgb(105, 211, 235),
    Color::Rgb(255, 145, 184),
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

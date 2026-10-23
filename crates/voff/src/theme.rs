//! Near-black, blue-toned terminal palette with restrained burnt-orange accents.

use ratatui::style::{Color, Style};

// Use the standard xterm palette instead of 24-bit colors. macOS Terminal.app
// only supports 256 colors, while this palette is also understood by modern
// Linux terminals, so both render the same theme without platform checks.
pub const BACKGROUND: Color = Color::Indexed(232);
pub const SURFACE: Color = Color::Indexed(233);
pub const ELEVATED: Color = Color::Indexed(234);
pub const BORDER: Color = Color::Indexed(60);
pub const TEXT: Color = Color::Indexed(255);
pub const MUTED: Color = Color::Indexed(146);
pub const ORANGE: Color = Color::Indexed(130);
pub const YELLOW: Color = Color::Indexed(179);
pub const AQUA: Color = Color::Indexed(73);
pub const GREEN: Color = Color::Indexed(108);
pub const BLUE: Color = Color::Indexed(103);
pub const PURPLE: Color = Color::Indexed(139);
pub const RED: Color = Color::Indexed(131);

const AGENT_COLORS: [Color; 32] = [
    ORANGE,
    YELLOW,
    AQUA,
    PURPLE,
    BLUE,
    GREEN,
    RED,
    Color::Indexed(140),
    Color::Indexed(186),
    Color::Indexed(116),
    Color::Indexed(175),
    Color::Indexed(150),
    Color::Indexed(104),
    Color::Indexed(174),
    Color::Indexed(115),
    Color::Indexed(176),
    Color::Indexed(186),
    Color::Indexed(110),
    Color::Indexed(175),
    Color::Indexed(114),
    Color::Indexed(140),
    Color::Indexed(180),
    Color::Indexed(116),
    Color::Indexed(176),
    Color::Indexed(150),
    Color::Indexed(110),
    Color::Indexed(174),
    Color::Indexed(115),
    Color::Indexed(140),
    Color::Indexed(186),
    Color::Indexed(116),
    Color::Indexed(175),
];

/// Map an RGB color to its nearest entry in the standard xterm 256-color palette.
#[must_use]
pub fn ansi256(red: u8, green: u8, blue: u8) -> Color {
    let mut best_index = 0;
    let mut best_distance = u32::MAX;
    // Entries 0-15 can be redefined by a user's terminal theme. Restrict the
    // result to the fixed xterm color cube and grayscale ramp.
    for index in 16..=255 {
        let (candidate_red, candidate_green, candidate_blue) = ansi256_rgb(index);
        let red_delta = i32::from(red) - i32::from(candidate_red);
        let green_delta = i32::from(green) - i32::from(candidate_green);
        let blue_delta = i32::from(blue) - i32::from(candidate_blue);
        let distance =
            (red_delta * red_delta + green_delta * green_delta + blue_delta * blue_delta) as u32;
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    Color::Indexed(best_index)
}

fn ansi256_rgb(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let index = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (
                component(index / 36),
                component(index % 36 / 6),
                component(index % 6),
            )
        }
        _ => {
            let value = 8 + (index - 232) * 10;
            (value, value, value)
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_colors_map_to_fixed_xterm_entries() {
        assert_eq!(ansi256(0, 0, 0), Color::Indexed(16));
        assert_eq!(ansi256(192, 93, 43), ORANGE);
        assert!(matches!(ansi256(255, 255, 255), Color::Indexed(16..=255)));
    }
}

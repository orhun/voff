//! Near-black, blue-toned terminal palette with restrained burnt-orange accents.
//!
//! Keep emitted colours indexed. Some terminal hosts advertise true-colour but
//! misinterpret `38;2`/`48;2` SGR sequences, turning individual RGB channels
//! into attributes and large saturated backgrounds.

use ratatui::style::{Color, Style};

pub const BACKGROUND: Color = Color::Indexed(232);
pub const SURFACE: Color = Color::Indexed(233);
pub const ELEVATED: Color = Color::Indexed(234);
pub const BORDER: Color = Color::Indexed(24);
pub const TEXT: Color = Color::Indexed(255);
pub const MUTED: Color = Color::Indexed(250);
pub const ORANGE: Color = Color::Indexed(166);
pub const YELLOW: Color = Color::Indexed(143);
pub const AQUA: Color = Color::Indexed(73);
pub const GREEN: Color = Color::Indexed(108);
pub const BLUE: Color = Color::Indexed(67);
pub const PURPLE: Color = Color::Indexed(103);
pub const RED: Color = Color::Indexed(131);

pub(crate) fn indexed_logo_style(mut style: Style) -> Style {
    style.fg = style.fg.map(indexed_logo_color);
    style.bg = style.bg.map(indexed_logo_color);
    style.underline_color = style.underline_color.map(indexed_logo_color);
    style
}

fn indexed_logo_color(color: Color) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    // The artwork is an orange hoodie around a neutral black dog. Restricting
    // it to that palette prevents tiny chromatic RGB variations from becoming
    // saturated red, green, or blue cells in a 256-colour terminal.
    if red > green && green > blue && red.saturating_sub(blue) > 24 {
        let orange = match (u16::from(red) + u16::from(green)) / 2 {
            0..=63 => 52,
            64..=111 => 130,
            112..=159 => 166,
            160..=207 => 208,
            _ => 214,
        };
        Color::Indexed(orange)
    } else {
        let luminance = (u16::from(red) * 3 + u16::from(green) * 6 + u16::from(blue)) / 10;
        Color::Indexed(grayscale_index(luminance as u8))
    }
}

fn grayscale_index(value: u8) -> u8 {
    if value < 8 {
        16
    } else if value > 248 {
        231
    } else {
        232 + ((u16::from(value) - 8) * 24 / 247) as u8
    }
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
    fn logo_palette_contains_only_neutral_and_orange_cells() {
        for (rgb, expected) in [
            ((0, 220, 0), Color::Indexed(244)),
            ((220, 0, 0), Color::Indexed(237)),
            ((240, 140, 20), Color::Indexed(208)),
            ((0, 0, 0), Color::Indexed(16)),
        ] {
            let style = indexed_logo_style(Style::new().fg(Color::Rgb(rgb.0, rgb.1, rgb.2)));
            assert_eq!(style.fg, Some(expected));
        }
    }
}

//! Keep RGB theme colors intact while adapting them for terminals without truecolor.

use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
    style::Color,
};
use std::io::{self, Write};

pub(crate) struct ColorFallbackBackend<W: Write> {
    inner: CrosstermBackend<W>,
    truecolor: bool,
}

impl<W: Write> ColorFallbackBackend<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            truecolor: truecolor_supported(),
        }
    }
}

impl<W: Write> Backend for ColorFallbackBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells = content
            .map(|(x, y, cell)| {
                let mut cell = cell.clone();
                cell.fg = portable_color(cell.fg, self.truecolor);
                cell.bg = portable_color(cell.bg, self.truecolor);
                (x, y, cell)
            })
            .collect::<Vec<_>>();
        self.inner
            .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

fn portable_color(color: Color, truecolor: bool) -> Color {
    match color {
        Color::Rgb(red, green, blue) if !truecolor => ansi256(red, green, blue),
        color => color,
    }
}

fn ansi256(red: u8, green: u8, blue: u8) -> Color {
    let mut best_index = 16;
    let mut best_distance = u32::MAX;
    // Avoid indexes 0-15 because terminals let users redefine those colors.
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
    match index {
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

fn truecolor_supported() -> bool {
    truecolor_supported_by(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
    )
}

fn truecolor_supported_by(
    colorterm: Option<&str>,
    term: Option<&str>,
    term_program: Option<&str>,
) -> bool {
    let declares_truecolor = |value: &str| {
        let value = value.to_ascii_lowercase();
        value.contains("truecolor") || value.contains("24bit") || value.contains("direct")
    };
    colorterm.is_some_and(declares_truecolor)
        || term.is_some_and(declares_truecolor)
        || term_program.is_some_and(|program| {
            matches!(
                program.to_ascii_lowercase().as_str(),
                "ghostty" | "iterm.app" | "kitty" | "wezterm" | "vscode" | "warpterminal"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_detection_keeps_rgb_only_for_capable_terminals() {
        assert!(truecolor_supported_by(
            Some("truecolor"),
            Some("xterm-256color"),
            None
        ));
        assert!(truecolor_supported_by(
            None,
            Some("xterm-256color"),
            Some("iTerm.app")
        ));
        assert!(!truecolor_supported_by(
            None,
            Some("xterm-256color"),
            Some("Apple_Terminal")
        ));
        assert_eq!(
            portable_color(Color::Rgb(12, 34, 56), true),
            Color::Rgb(12, 34, 56)
        );
        assert!(matches!(
            portable_color(Color::Rgb(12, 34, 56), false),
            Color::Indexed(16..=255)
        ));
        assert_eq!(
            portable_color(Color::Indexed(42), false),
            Color::Indexed(42)
        );
    }
}

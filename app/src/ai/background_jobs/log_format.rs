//! Read-only presentation of captured command output, without creating a terminal session.

use std::ops::Range;

use galaxy_core::ui::theme::{AnsiColors, TerminalColors};
use galaxyui::color::ColorU;
use galaxyui::text_layout::TextStyle;
use string_offset::ByteOffset;
use vte::{Params, Parser, Perform};

/// Removes terminal controls and resolves ANSI colors against the current theme.
///
/// The caller supplies the already bounded log tail. Memory grows linearly with
/// that input: cursor movement cannot allocate padding or expand the output.
/// Reformat when the theme changes; returned colors are concrete theme colors.
pub(super) fn format_job_logs(
    raw: &str,
    colors: &TerminalColors,
) -> (String, Vec<(Range<ByteOffset>, TextStyle)>) {
    let mut formatter = LogFormatter {
        colors,
        text: String::with_capacity(raw.len()),
        styles: Vec::new(),
        line: Vec::new(),
        cursor: 0,
        style: TextStyle::default(),
        underline: false,
    };
    let mut parser = Parser::new();
    for byte in raw.bytes() {
        parser.advance(&mut formatter, byte);
    }
    formatter.flush_line();
    (formatter.text, formatter.styles)
}

#[derive(Clone, Copy)]
struct LogCell {
    character: char,
    style: TextStyle,
}

struct LogFormatter<'a> {
    colors: &'a TerminalColors,
    text: String,
    styles: Vec<(Range<ByteOffset>, TextStyle)>,
    line: Vec<LogCell>,
    cursor: usize,
    style: TextStyle,
    underline: bool,
}

impl LogFormatter<'_> {
    fn flush_line(&mut self) {
        for cell in self.line.drain(..) {
            let start = self.text.len();
            self.text.push(cell.character);
            let end = self.text.len();
            if cell.style == TextStyle::default() {
                continue;
            }
            if let Some((range, style)) = self.styles.last_mut() {
                if *style == cell.style && range.end.as_usize() == start {
                    range.end = ByteOffset::from(end);
                    continue;
                }
            }
            self.styles
                .push((ByteOffset::from(start)..ByteOffset::from(end), cell.style));
        }
        self.cursor = 0;
    }

    fn displayed_style(&self) -> TextStyle {
        let mut style = self.style;
        if self.underline {
            style.underline_color = Some(
                style
                    .foreground_color
                    .unwrap_or_else(|| self.colors.normal.white.into()),
            );
        }
        style
    }

    fn set_graphics(&mut self, params: &Params) {
        let params: Vec<_> = params.iter().collect();
        let mut index = 0;
        while let Some(parameter) = params.get(index) {
            match parameter.first().copied().unwrap_or(0) {
                0 => {
                    self.style = TextStyle::default();
                    self.underline = false;
                }
                4 => self.underline = parameter.get(1).copied() != Some(0),
                9 => self.style.show_strikethrough = true,
                24 => self.underline = false,
                29 => self.style.show_strikethrough = false,
                value @ 30..=37 => {
                    self.style.foreground_color = Some(self.indexed_color((value - 30) as u8));
                }
                value @ 40..=47 => {
                    self.style.background_color = Some(self.indexed_color((value - 40) as u8));
                }
                value @ 90..=97 => {
                    self.style.foreground_color = Some(self.indexed_color((value - 90 + 8) as u8));
                }
                value @ 100..=107 => {
                    self.style.background_color = Some(self.indexed_color((value - 100 + 8) as u8));
                }
                39 => self.style.foreground_color = None,
                49 => self.style.background_color = None,
                target @ (38 | 48) => {
                    let (color, consumed) = if parameter.len() > 1 {
                        (self.extended_color(&parameter[1..]), 0)
                    } else {
                        let mode = params
                            .get(index + 1)
                            .and_then(|value| value.first())
                            .copied();
                        let count = match mode {
                            Some(5) => 2,
                            Some(2) => 4,
                            _ => 1,
                        };
                        let values: Vec<_> = params
                            .iter()
                            .skip(index + 1)
                            .take(count)
                            .filter_map(|value| value.first().copied())
                            .collect();
                        (self.extended_color(&values), count)
                    };
                    if let Some(color) = color {
                        if target == 38 {
                            self.style.foreground_color = Some(color);
                        } else {
                            self.style.background_color = Some(color);
                        }
                    }
                    index += consumed;
                }
                // Font weight/italics, cursor controls and other terminal state
                // are not represented by this selectable text-log view.
                _ => {}
            }
            index += 1;
        }
    }

    fn extended_color(&self, values: &[u16]) -> Option<ColorU> {
        match values.first()? {
            5 => Some(self.indexed_color(u8::try_from(*values.get(1)?).ok()?)),
            // Colon notation may include a color-space field before RGB.
            2 if values.len() >= 4 => {
                let rgb = &values[values.len() - 3..];
                Some(ColorU::new(
                    u8::try_from(rgb[0]).ok()?,
                    u8::try_from(rgb[1]).ok()?,
                    u8::try_from(rgb[2]).ok()?,
                    255,
                ))
            }
            _ => None,
        }
    }

    fn indexed_color(&self, index: u8) -> ColorU {
        match index {
            0..=7 => palette_color(&self.colors.normal, index),
            8..=15 => palette_color(&self.colors.bright, index - 8),
            16..=231 => {
                let cube = index - 16;
                let component = |value| if value == 0 { 0 } else { 55 + 40 * value };
                ColorU::new(
                    component(cube / 36),
                    component(cube / 6 % 6),
                    component(cube % 6),
                    255,
                )
            }
            232..=255 => {
                let level = 8 + (index - 232) * 10;
                ColorU::new(level, level, level, 255)
            }
        }
    }
}

fn palette_color(palette: &AnsiColors, index: u8) -> ColorU {
    match index {
        0 => palette.black,
        1 => palette.red,
        2 => palette.green,
        3 => palette.yellow,
        4 => palette.blue,
        5 => palette.magenta,
        6 => palette.cyan,
        _ => palette.white,
    }
    .into()
}

impl Perform for LogFormatter<'_> {
    fn print(&mut self, character: char) {
        let cell = LogCell {
            character,
            style: self.displayed_style(),
        };
        if let Some(existing) = self.line.get_mut(self.cursor) {
            *existing = cell;
        } else {
            self.line.push(cell);
        }
        self.cursor += 1;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.flush_line();
                self.text.push('\n');
            }
            b'\r' => self.cursor = 0,
            b'\t' => self.print('\t'),
            0x08 => self.cursor = self.cursor.saturating_sub(1),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match action {
            'm' => self.set_graphics(params),
            // Support the erase-line controls commonly paired with CR progress
            // output. Other cursor movement is ignored and cannot grow memory.
            'K' => match params
                .iter()
                .next()
                .and_then(|value| value.first())
                .copied()
                .unwrap_or(0)
            {
                0 => self.line.truncate(self.cursor),
                1 => {
                    let count = self.cursor.saturating_add(1).min(self.line.len());
                    self.line[..count].fill(LogCell {
                        character: ' ',
                        style: TextStyle::default(),
                    });
                }
                2 => {
                    self.line.clear();
                    self.cursor = 0;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use galaxy_core::ui::theme::AnsiColor;

    fn colors() -> TerminalColors {
        let palette = |red| {
            AnsiColors::new(
                AnsiColor { r: 0, g: 0, b: 0 },
                AnsiColor { r: red, g: 1, b: 2 },
                AnsiColor { r: 0, g: 180, b: 0 },
                AnsiColor {
                    r: 180,
                    g: 180,
                    b: 0,
                },
                AnsiColor { r: 0, g: 0, b: 180 },
                AnsiColor {
                    r: 180,
                    g: 0,
                    b: 180,
                },
                AnsiColor {
                    r: 0,
                    g: 180,
                    b: 180,
                },
                AnsiColor {
                    r: 230,
                    g: 230,
                    b: 230,
                },
            )
        };
        TerminalColors::new(palette(160), palette(250))
    }

    #[test]
    fn ansi_colors_reset_and_unicode_ranges_use_output_bytes() {
        let colors = colors();
        let (text, styles) =
            format_job_logs("é\x1b[31m红\x1b[0m plain \x1b[91mbright\x1b[39m!", &colors);
        assert_eq!(text, "é红 plain bright!");
        assert_eq!(styles.len(), 2);
        assert_eq!(styles[0].0, ByteOffset::from(2)..ByteOffset::from(5));
        assert_eq!(styles[0].1.foreground_color, Some(colors.normal.red.into()));
        assert_eq!(styles[1].1.foreground_color, Some(colors.bright.red.into()));
        for (range, _) in styles {
            assert!(text
                .get(range.start.as_usize()..range.end.as_usize())
                .is_some());
        }
    }

    #[test]
    fn strips_opaque_controls_and_replaces_progress_lines() {
        let (text, _) = format_job_logs(
            "\x1b]0;hidden title\x07loading 10%\rloading 90%\r\x1b[2Kdone\r\n\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\\tend\x07\x1b[99999C!",
            &colors(),
        );
        assert_eq!(text, "done\nlink\tend!");
    }

    #[test]
    fn supports_indexed_and_rgb_colors_in_both_sgr_notations() {
        let (text, styles) = format_job_logs(
            "\x1b[38;5;196mred\x1b[48;2;1;2;3mBG\x1b[0m\x1b[38:2::4:5:6mRGB\x1b[0m",
            &colors(),
        );
        assert_eq!(text, "redBGRGB");
        assert_eq!(
            styles[0].1.foreground_color,
            Some(ColorU::new(255, 0, 0, 255))
        );
        assert_eq!(
            styles[1].1.background_color,
            Some(ColorU::new(1, 2, 3, 255))
        );
        assert_eq!(
            styles[2].1.foreground_color,
            Some(ColorU::new(4, 5, 6, 255))
        );
    }
}

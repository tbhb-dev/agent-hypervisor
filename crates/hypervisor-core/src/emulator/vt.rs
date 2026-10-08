//! Serialize a grid and its modes to VT bytes that redraw it on a terminal.
//!
//! This replaces ghostty-vt's VT formatter for viewers. The formatter fills a gap between two
//! text runs with spaces under the earlier run's SGR, which restyled 189 cells on spike 2's
//! fixture and up to 1,677 on inline Codex recordings (RFC-36 runs 2 and 3). Here every cell
//! carries its own full SGR state, so a blank gap is drawn under its own style.
//!
//! The output assumes a terminal of the grid's size. It redraws only the active screen: entering
//! the alternate screen leaves the viewer's primary screen blank. Scroll regions, tab stops,

//! charsets, titles, the palette, and kitty keyboard flags are not carried, nor is
//! a pending wrap at the cursor.

use std::fmt::Write as _;

use super::{Attrs, Cell, CellWidth, Color, Cursor, Grid, Modes, MouseFormat, MouseTracking};
use super::{Screen, Underline};

/// VT bytes that redraw `grid` and restore `modes` on a terminal of the same size.
///
/// The bytes select the active screen, soft-reset the terminal (DECSTR),
/// clear it, draw each row from its first column to its last non-blank cell, restore the input
/// modes, and place the cursor. They never set synchronized output (mode 2026), which would hold
/// the viewer's rendering.
#[must_use]
pub fn serialize(grid: &Grid, modes: &Modes) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("\x1b[?2026l");
    out.push_str(if modes.screen == Screen::Alternate {
        "\x1b[?1049h"
    } else {
        "\x1b[?1049l"
    });
    out.push_str("\x1b[!p\x1b[?7h");
    out.push_str(if modes.grapheme_clustering {
        "\x1b[?2027h"
    } else {
        "\x1b[?2027l"
    });
    out.push_str("\x1b[0m\x1b[H\x1b[2J");
    draw_rows(&mut out, grid);
    out.push_str("\x1b[0m");
    restore_modes(&mut out, modes);
    cup(&mut out, grid.cursor());
    out.push_str(if modes.cursor_visible {
        "\x1b[?25h"
    } else {
        "\x1b[?25l"
    });
    out.into_bytes()
}

fn draw_rows(out: &mut String, grid: &Grid) {
    let size = grid.size();
    let cols = usize::from(size.cols());
    let mut pen = Cell::blank();
    // Set when the previous row ended in a spacer head and its wrapped wide character has
    // already been drawn at the start of this row, leaving the cursor after it.
    let mut start = 0;
    for r in 0..size.rows() {
        let row = grid.row(r);
        let next = (r + 1 < size.rows()).then(|| grid.row(r + 1));
        let wraps = row[cols - 1].width == CellWidth::SpacerHead
            && next.is_some_and(|n| n[0].width == CellWidth::Wide);
        let end = if wraps {
            cols - 1
        } else {
            row.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1)
        };
        if start < end {
            if start == 0 {
                cup(out, Cursor { row: r, col: 0 });
            }
            for cell in &row[start..end] {
                if cell.width != CellWidth::SpacerTail {
                    draw(out, &mut pen, cell);
                }
            }
        }
        start = 0;
        if let (true, Some([first, ..])) = (wraps, next) {
            // Drawing the wide character in the last column makes the terminal leave a spacer
            // head there and wrap the character to the next row, as the application's did.
            cup(
                out,
                Cursor {
                    row: r,
                    col: size.cols() - 1,
                },
            );
            draw(out, &mut pen, first);
            start = 2;
        }
    }
    if pen.hyperlink.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
    }
}

fn draw(out: &mut String, pen: &mut Cell, cell: &Cell) {
    if pen.hyperlink != cell.hyperlink {
        out.push_str("\x1b]8;;");
        if let Some(uri) = &cell.hyperlink {
            out.push_str(uri);
        }
        out.push_str("\x1b\\");
        pen.hyperlink.clone_from(&cell.hyperlink);
    }
    if !same_style(pen, cell) {
        sgr(out, cell);
        pen.clone_from(cell);
    }
    if cell.text.is_empty() || cell.width == CellWidth::SpacerHead {
        out.push(' ');
    } else {
        out.push_str(&cell.text);
    }
}

fn same_style(a: &Cell, b: &Cell) -> bool {
    a.fg == b.fg
        && a.bg == b.bg
        && a.underline_color == b.underline_color
        && a.attrs == b.attrs
        && a.underline == b.underline
}

fn sgr(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    for (attr, param) in Attrs::ALL {
        if cell.attrs.contains(attr) {
            let _ = write!(out, ";{param}");
        }
    }
    let underline = match cell.underline {
        Underline::None => "",
        Underline::Single => ";4",
        Underline::Double => ";4:2",
        Underline::Curly => ";4:3",
        Underline::Dotted => ";4:4",
        Underline::Dashed => ";4:5",
    };
    out.push_str(underline);
    color(out, 38, cell.fg);
    color(out, 48, cell.bg);
    color(out, 58, cell.underline_color);
    out.push('m');
}

fn color(out: &mut String, base: u8, color: Color) {
    match color {
        Color::Default => {}
        Color::Palette(i) => {
            let _ = write!(out, ";{base};5;{i}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{base};2;{r};{g};{b}");
        }
    }
}

fn cup(out: &mut String, at: Cursor) {
    let _ = write!(
        out,
        "\x1b[{};{}H",
        u32::from(at.row) + 1,
        u32::from(at.col) + 1
    );
}

fn restore_modes(out: &mut String, modes: &Modes) {
    out.push_str(if modes.application_cursor_keys {
        "\x1b[?1h"
    } else {
        "\x1b[?1l"
    });
    out.push_str(if modes.application_keypad {
        "\x1b="
    } else {
        "\x1b>"
    });
    out.push_str(if modes.wraparound {
        "\x1b[?7h"
    } else {
        "\x1b[?7l"
    });
    // Terminals disagree on mode 1007's default (ghostty sets it, xterm resets it), so it is
    // always written out.
    out.push_str(if modes.alternate_scroll {
        "\x1b[?1007h"
    } else {
        "\x1b[?1007l"
    });
    out.push_str(if modes.focus_events {
        "\x1b[?1004h"
    } else {
        "\x1b[?1004l"
    });
    out.push_str(if modes.bracketed_paste {
        "\x1b[?2004h"
    } else {
        "\x1b[?2004l"
    });
    out.push_str("\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    let tracking = match modes.mouse_tracking {
        MouseTracking::Off => "",
        MouseTracking::X10 => "\x1b[?9h",
        MouseTracking::Normal => "\x1b[?1000h",
        MouseTracking::Button => "\x1b[?1002h",
        MouseTracking::Any => "\x1b[?1003h",
    };
    out.push_str(tracking);
    out.push_str("\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l");
    let format = match modes.mouse_format {
        MouseFormat::X10 => "",
        MouseFormat::Utf8 => "\x1b[?1005h",
        MouseFormat::Sgr => "\x1b[?1006h",
        MouseFormat::Urxvt => "\x1b[?1015h",
        MouseFormat::SgrPixels => "\x1b[?1016h",
    };
    out.push_str(format);
    let _ = write!(out, "\x1b[>{}u", modes.kitty_keyboard_flags);
}

#[cfg(test)]
mod tests {
    use super::serialize;
    use crate::emulator::{
        Attrs, Cell, CellWidth, Color, Cursor, Grid, Modes, MouseFormat, MouseTracking, Screen,
        Size,
    };
    use proptest::prelude::*;

    const PREAMBLE: &str = "\x1b[?2026l\x1b[?1049l\x1b[!p\x1b[?7h\x1b[?2027l\x1b[0m\x1b[H\x1b[2J";

    fn text(s: &str) -> Cell {
        Cell {
            text: s.into(),
            ..Cell::blank()
        }
    }

    fn grid(cols: u16, rows: u16, cells: Vec<Cell>, cursor: Cursor) -> Grid {
        Grid::new(Size::new(cols, rows).unwrap(), cells, cursor).unwrap()
    }

    fn as_text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn blank_grid_is_preamble_reset_and_cursor() {
        let g = Grid::blank(Size::new(3, 2).unwrap());
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(out.starts_with(PREAMBLE), "{out:?}");
        assert!(out.ends_with("\x1b[1;1H\x1b[?25h"), "{out:?}");
    }

    #[test]
    fn draws_up_to_the_last_non_blank_cell_with_style_changes_only() {
        let red = Cell {
            fg: Color::Palette(1),
            attrs: Attrs::BOLD,
            ..text("b")
        };
        let cells = vec![
            text("a"),
            Cell::blank(),
            red.clone(),
            red,
            Cell::blank(),
            Cell::blank(),
        ];
        let g = grid(6, 1, cells, Cursor { row: 0, col: 4 });
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(
            out.starts_with(&format!("{PREAMBLE}\x1b[1;1Ha \x1b[0;1;38;5;1mbb")),
            "{out:?}"
        );
        assert!(out.ends_with("\x1b[1;5H\x1b[?25h"), "{out:?}");
    }

    #[test]
    fn a_styled_blank_gap_keeps_its_own_style() {
        let italic = Cell {
            attrs: Attrs::ITALIC,
            ..text("x")
        };
        let cells = vec![italic.clone(), Cell::blank(), italic];
        let g = grid(3, 1, cells, Cursor::default());
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(out.contains("\x1b[0;3mx\x1b[0m \x1b[0;3mx"), "{out:?}");
    }

    #[test]
    fn wide_characters_skip_their_tail() {
        let wide = Cell {
            width: CellWidth::Wide,
            ..text("\u{4e2d}")
        };
        let tail = Cell {
            width: CellWidth::SpacerTail,
            ..Cell::blank()
        };
        let g = grid(3, 1, vec![wide, tail, text("z")], Cursor::default());
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(out.contains("\x1b[1;1H\u{4e2d}z"), "{out:?}");
    }

    #[test]
    fn a_spacer_head_is_rebuilt_by_wrapping_the_next_rows_wide_character() {
        let head = Cell {
            width: CellWidth::SpacerHead,
            ..Cell::blank()
        };
        let wide = Cell {
            width: CellWidth::Wide,
            ..text("\u{4e2d}")
        };
        let tail = Cell {
            width: CellWidth::SpacerTail,
            ..Cell::blank()
        };
        let cells = vec![text("a"), text("b"), head, wide, tail, text("c")];
        let g = grid(3, 2, cells, Cursor { row: 1, col: 2 });
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(
            out.contains("\x1b[1;1Hab\x1b[1;3H\u{4e2d}c\x1b[0m"),
            "{out:?}"
        );
    }

    #[test]
    fn modes_are_restored_after_the_drawing() {
        let modes = Modes {
            screen: Screen::Alternate,
            cursor_visible: false,
            application_cursor_keys: true,
            bracketed_paste: true,
            focus_events: true,
            mouse_tracking: MouseTracking::Any,
            mouse_format: MouseFormat::Sgr,
            synchronized_output: true,
            ..Modes::default()
        };
        let g = Grid::blank(Size::new(2, 2).unwrap());
        let out = as_text(&serialize(&g, &modes));
        assert!(out.starts_with("\x1b[?2026l\x1b[?1049h\x1b[!p"), "{out:?}");
        assert!(
            out.contains("\x1b[?1h")
                && out.contains("\x1b[?1004h")
                && out.contains("\x1b[?2004h")
                && out.contains("\x1b[?1003h")
                && out.contains("\x1b[?1006h")
                && out.ends_with("\x1b[1;1H\x1b[?25l"),
            "{out:?}"
        );
        assert!(!out.contains("\x1b[?2026h"));
    }

    #[test]
    fn rgb_and_underline_colors_use_direct_color_forms() {
        let cell = Cell {
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Palette(200),
            underline_color: Color::Rgb(9, 8, 7),
            underline: crate::emulator::Underline::Curly,
            ..text("u")
        };
        let g = grid(1, 1, vec![cell], Cursor::default());
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(
            out.contains("\x1b[0;4:3;38;2;1;2;3;48;5;200;58;2;9;8;7mu"),
            "{out:?}"
        );
    }

    #[test]
    fn hyperlink_is_opened_and_closed_around_its_cells() {
        let linked = Cell {
            hyperlink: Some("https://example.test/a".into()),
            ..text("a")
        };
        let g = grid(2, 1, vec![linked, text("b")], Cursor::default());
        let out = as_text(&serialize(&g, &Modes::default()));
        assert!(out.contains("\x1b]8;;https://example.test/a\x1b\\a\x1b]8;;\x1b\\b"));
    }

    proptest! {
        #[test]
        fn ascii_rows_appear_in_order_and_sync_output_never_does(
            lines in proptest::collection::vec("[a-z]{0,8}", 1..6),
            sync in any::<bool>(),
        ) {
            let cols = 8u16;
            let rows = u16::try_from(lines.len()).unwrap();
            let mut cells = Vec::new();
            for line in &lines {
                let mut row: Vec<Cell> = line.chars().map(|c| text(&c.to_string())).collect();
                row.resize(usize::from(cols), Cell::blank());
                cells.extend(row);
            }
            let g = grid(cols, rows, cells, Cursor::default());
            let modes = Modes { synchronized_output: sync, ..Modes::default() };
            let out = as_text(&serialize(&g, &modes));
            prop_assert!(!out.contains("\x1b[?2026h"));
            let mut rest = out.as_str();
            for line in lines.iter().filter(|l| !l.is_empty()) {
                let at = rest.find(line.as_str());
                prop_assert!(at.is_some(), "{line} missing from {out:?}");
                rest = &rest[at.unwrap() + line.len()..];
            }
        }
    }
}

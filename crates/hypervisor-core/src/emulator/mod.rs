//! The terminal emulator a session holder keeps per session (RFC-36 source lines 99 and 100).
//!
//! An [`Emulator`] parses a session's PTY output into a grid. The holder reads the grid to bring
//! a viewer up to date, asks for the modes that decide input routing, and resizes it with the PTY.
//! Implementations live in shell crates, because they wrap a native library or own a parser's
//! state; this module holds the plain values they hand back and the transformations over them.

mod diff;
mod reply;
mod supplement;
mod vt;

pub use diff::{CellDiff, diff};
pub use reply::{
    CapabilityProfile, DeviceAttributes, PROFILE, QueryScanner, ViewerInputFilter, admit_reply,
    is_terminal_reply,
};
pub use supplement::{CsiEffect, TRACKED_MODES, csi_effects};
pub use vt::serialize;

use serde::{Deserialize, Serialize};
use std::fmt;

/// A terminal size in cells. Both dimensions are at least one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    cols: u16,
    rows: u16,
}

/// A size with a zero dimension was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZeroSize;

impl fmt::Display for ZeroSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a terminal needs at least one column and one row")
    }
}

impl std::error::Error for ZeroSize {}

impl Size {
    /// Returns the size, or [`ZeroSize`] when either dimension is zero.
    ///
    /// # Errors
    ///
    /// [`ZeroSize`] when `cols` or `rows` is zero.
    pub const fn new(cols: u16, rows: u16) -> Result<Self, ZeroSize> {
        if cols == 0 || rows == 0 {
            Err(ZeroSize)
        } else {
            Ok(Self { cols, rows })
        }
    }

    /// Columns.
    #[must_use]
    pub const fn cols(self) -> u16 {
        self.cols
    }

    /// Rows.
    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }

    /// Cells in the grid, `cols * rows`.
    #[must_use]
    pub const fn cells(self) -> usize {
        self.cols as usize * self.rows as usize
    }
}

/// A cell color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// An index into the 256-color palette. Indexes 0 to 15 are the named ANSI colors.
    Palette(u8),
    /// A direct color.
    Rgb(u8, u8, u8),
}

/// SGR attributes other than underline and color, as a bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Attrs(u8);

impl Attrs {
    /// No attributes.
    pub const NONE: Self = Self(0);
    /// SGR 1.
    pub const BOLD: Self = Self(1);
    /// SGR 2, also called dim.
    pub const FAINT: Self = Self(1 << 1);
    /// SGR 3.
    pub const ITALIC: Self = Self(1 << 2);
    /// SGR 5.
    pub const BLINK: Self = Self(1 << 3);
    /// SGR 7.
    pub const INVERSE: Self = Self(1 << 4);
    /// SGR 8, also called hidden.
    pub const INVISIBLE: Self = Self(1 << 5);
    /// SGR 9.
    pub const STRIKETHROUGH: Self = Self(1 << 6);
    /// SGR 53.
    pub const OVERLINE: Self = Self(1 << 7);

    /// Every attribute with its SGR parameter, in parameter order.
    pub const ALL: [(Self, u8); 8] = [
        (Self::BOLD, 1),
        (Self::FAINT, 2),
        (Self::ITALIC, 3),
        (Self::BLINK, 5),
        (Self::INVERSE, 7),
        (Self::INVISIBLE, 8),
        (Self::STRIKETHROUGH, 9),
        (Self::OVERLINE, 53),
    ];

    /// The set holding both sets' attributes.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The set without `other`'s attributes.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether every attribute in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Underline style, SGR 4 and its `4:n` forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// `4` or `4:1`.
    Single,
    /// `4:2`, or SGR 21.
    Double,
    /// `4:3`.
    Curly,
    /// `4:4`.
    Dotted,
    /// `4:5`.
    Dashed,
}

/// How much of a wide character a cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CellWidth {
    /// One column.
    #[default]
    Narrow,
    /// The first column of a two-column character.
    Wide,
    /// The second column of a two-column character. Its text is a space.
    SpacerTail,
    /// The last column of a row whose next character was too wide to fit and wrapped.
    SpacerHead,
}

/// One grid cell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The grapheme: a base codepoint and any combining codepoints. An empty cell holds a space.
    pub text: String,
    /// Foreground color.
    pub fg: Color,
    /// Background color, including the color an erase left behind.
    pub bg: Color,
    /// Underline color, SGR 58.
    pub underline_color: Color,
    /// Attributes.
    pub attrs: Attrs,
    /// Underline style.
    pub underline: Underline,
    /// Width class.
    pub width: CellWidth,
    /// The OSC 8 URI, if present. A grid frame assigns its own hyperlink id.
    pub hyperlink: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}

impl Cell {
    /// An erased cell under the default style.
    #[must_use]
    pub fn blank() -> Self {
        Self {
            text: " ".into(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: Color::Default,
            attrs: Attrs::NONE,
            underline: Underline::None,
            width: CellWidth::Narrow,
            hyperlink: None,
        }
    }

    /// Whether the cell looks the same as an erased cell under the default style.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        *self == Self::blank()
    }
}

/// Cursor position, zero-based, on the active screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Cursor {
    /// Row from the top.
    pub row: u16,
    /// Column from the left.
    pub col: u16,
}

/// The active screen's cells, row-major, with the cursor.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Grid {
    size: Size,
    cells: Vec<Cell>,
    cursor: Cursor,
}

/// The cell count or the cursor does not fit the grid size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
    /// The cell vector's length is not `cols * rows`.
    CellCount {
        /// `cols * rows`.
        expected: usize,
        /// The length given.
        found: usize,
    },
    /// The cursor lies outside the grid.
    CursorOutside(Cursor),
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CellCount { expected, found } => {
                write!(f, "grid needs {expected} cells, found {found}")
            }
            Self::CursorOutside(c) => write!(f, "cursor {},{} is outside the grid", c.row, c.col),
        }
    }
}

impl std::error::Error for GridError {}

impl Grid {
    /// Builds a grid from row-major cells.
    ///
    /// # Errors
    ///
    /// [`GridError`] when `cells` does not hold exactly `size.cells()` entries or the cursor
    /// lies outside the grid.
    pub fn new(size: Size, cells: Vec<Cell>, cursor: Cursor) -> Result<Self, GridError> {
        if cells.len() != size.cells() {
            return Err(GridError::CellCount {
                expected: size.cells(),
                found: cells.len(),
            });
        }
        if cursor.row >= size.rows() || cursor.col >= size.cols() {
            return Err(GridError::CursorOutside(cursor));
        }
        Ok(Self {
            size,
            cells,
            cursor,
        })
    }

    /// A grid of blank cells with the cursor at the origin.
    #[must_use]
    pub fn blank(size: Size) -> Self {
        Self {
            size,
            cells: vec![Cell::blank(); size.cells()],
            cursor: Cursor::default(),
        }
    }

    /// The grid size.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    /// The cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// One row's cells.
    ///
    /// # Panics
    ///
    /// When `row` is not below `size().rows()`.
    #[must_use]
    pub fn row(&self, row: u16) -> &[Cell] {
        let cols = usize::from(self.size.cols());
        let start = usize::from(row) * cols;
        &self.cells[start..start + cols]
    }

    /// Every cell, row-major.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Each row's text with spacer cells dropped and trailing spaces trimmed.
    #[must_use]
    pub fn text_rows(&self) -> Vec<String> {
        (0..self.size.rows())
            .map(|r| {
                let line: String = self
                    .row(r)
                    .iter()
                    .filter(|c| matches!(c.width, CellWidth::Narrow | CellWidth::Wide))
                    .map(|c| c.text.as_str())
                    .collect();
                line.trim_end().to_owned()
            })
            .collect()
    }
}

/// Which screen buffer is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Screen {
    /// The primary screen, which holds the scrollback.
    #[default]
    Primary,
    /// The alternate screen, modes 47, 1047, and 1049.
    Alternate,
}

/// Which mouse events the application asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MouseTracking {
    /// No mouse reporting.
    #[default]
    Off,
    /// Mode 9, presses only.
    X10,
    /// Mode 1000, presses and releases.
    Normal,
    /// Mode 1002, plus motion while a button is held.
    Button,
    /// Mode 1003, plus all motion.
    Any,
}

impl MouseTracking {
    /// The tracking mode from the flags of modes 9, 1000, 1002, and 1003, in that order. The
    /// widest mode set wins.
    #[must_use]
    pub const fn from_modes([x10, normal, button, any]: [bool; 4]) -> Self {
        if any {
            Self::Any
        } else if button {
            Self::Button
        } else if normal {
            Self::Normal
        } else if x10 {
            Self::X10
        } else {
            Self::Off
        }
    }
}

/// How mouse reports are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MouseFormat {
    /// The original X10 byte encoding.
    #[default]
    X10,
    /// Mode 1005.
    Utf8,
    /// Mode 1006.
    Sgr,
    /// Mode 1015.
    Urxvt,
    /// Mode 1016.
    SgrPixels,
}

impl MouseFormat {
    /// The format from the flags of modes 1005, 1006, 1015, and 1016, in that order. SGR forms
    /// win over the older encodings.
    #[must_use]
    pub const fn from_modes([utf8, sgr, urxvt, sgr_pixels]: [bool; 4]) -> Self {
        if sgr_pixels {
            Self::SgrPixels
        } else if sgr {
            Self::Sgr
        } else if urxvt {
            Self::Urxvt
        } else if utf8 {
            Self::Utf8
        } else {
            Self::X10
        }
    }
}

/// The modes a holder needs to route input and to bring a viewer up to date.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag mirrors one independent DEC private mode"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modes {
    /// The active screen.
    pub screen: Screen,
    /// Mode 25.
    pub cursor_visible: bool,
    /// Mode 1, application cursor keys.
    pub application_cursor_keys: bool,
    /// Mode 66, application keypad.
    pub application_keypad: bool,
    /// Mode 7.
    pub wraparound: bool,
    /// Modes 9, 1000, 1002, and 1003.
    pub mouse_tracking: MouseTracking,
    /// Modes 1005, 1006, 1015, and 1016.
    pub mouse_format: MouseFormat,
    /// Mode 1007, wheel as arrow keys on the alternate screen.
    pub alternate_scroll: bool,
    /// Mode 1004.
    pub focus_events: bool,
    /// Mode 2004.
    pub bracketed_paste: bool,
    /// Mode 2026. A holder never forwards it to a viewer as set.
    pub synchronized_output: bool,
    /// Mode 2027.
    pub grapheme_clustering: bool,
    /// The kitty keyboard flags the application pushed, 0 when none. The holder never claims the
    /// protocol in a reply, but an application may push flags without asking (Codex does).
    pub kitty_keyboard_flags: u8,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            screen: Screen::Primary,
            cursor_visible: true,
            application_cursor_keys: false,
            application_keypad: false,
            wraparound: true,
            mouse_tracking: MouseTracking::Off,
            mouse_format: MouseFormat::X10,
            alternate_scroll: false,
            focus_events: false,
            bracketed_paste: false,
            synchronized_output: false,
            grapheme_clustering: false,
            kitty_keyboard_flags: 0,
        }
    }
}

/// A terminal emulator holding one session's screen state.
///
/// Implementations are not required to be `Send`. The ghostty-vt implementation is not: the
/// session holder creates it on the session's own thread and never moves it.
pub trait Emulator {
    /// Why an operation failed.
    type Error: std::error::Error;

    /// Parses PTY output and returns the replies the terminal owes the application, in order,
    /// ready to write back to the PTY. Replies come whether or not a viewer is attached, and
    /// they pass [`admit_reply`], so a kitty keyboard query goes unanswered.
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8>;

    /// The current size.
    fn size(&self) -> Size;

    /// Resizes the grid. The primary screen reflows; the alternate screen does not.
    ///
    /// # Errors
    ///
    /// When the implementation cannot allocate the new grid.
    fn resize(&mut self, size: Size) -> Result<(), Self::Error>;

    /// The active screen's grid, read cell by cell.
    fn grid(&self) -> Grid;

    /// The current OSC window title, if one has been set.
    fn title(&self) -> Option<String>;

    /// The modes in force.
    fn modes(&self) -> Modes;

    /// VT bytes that redraw the active screen and restore the modes on a fresh terminal of the
    /// same size. Built from [`Emulator::grid`] and [`Emulator::modes`] by [`serialize`], never
    /// from ghostty-vt's VT formatter, which restyles blank gaps (RFC-36 runs 2 and 3).
    fn serialize_vt(&self) -> Vec<u8> {
        serialize(&self.grid(), &self.modes())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Attrs, Cell, CellWidth, Cursor, Grid, GridError, MouseFormat, MouseTracking, Size, ZeroSize,
    };
    use proptest::prelude::*;

    #[test]
    fn size_rejects_a_zero_dimension() {
        assert_eq!(Size::new(0, 24), Err(ZeroSize));
        assert_eq!(Size::new(80, 0), Err(ZeroSize));
        assert_eq!(Size::new(80, 24).map(Size::cells), Ok(1920));
    }

    #[test]
    fn grid_checks_cell_count_and_cursor() {
        let size = Size::new(2, 2).unwrap();
        assert_eq!(
            Grid::new(size, vec![Cell::blank(); 3], Cursor::default()),
            Err(GridError::CellCount {
                expected: 4,
                found: 3
            })
        );
        let outside = Cursor { row: 2, col: 0 };
        assert_eq!(
            Grid::new(size, vec![Cell::blank(); 4], outside),
            Err(GridError::CursorOutside(outside))
        );
    }

    #[test]
    fn text_rows_drop_spacers_and_trailing_blanks() {
        let size = Size::new(4, 1).unwrap();
        let wide = Cell {
            text: "\u{4e2d}".into(),
            width: CellWidth::Wide,
            ..Cell::blank()
        };
        let tail = Cell {
            width: CellWidth::SpacerTail,
            ..Cell::blank()
        };
        let a = Cell {
            text: "a".into(),
            ..Cell::blank()
        };
        let grid = Grid::new(size, vec![a, wide, tail, Cell::blank()], Cursor::default()).unwrap();
        assert_eq!(grid.text_rows(), vec!["a\u{4e2d}".to_owned()]);
    }

    #[test]
    fn widest_mouse_mode_and_sgr_format_win() {
        assert_eq!(
            MouseTracking::from_modes([true, true, false, true]),
            MouseTracking::Any
        );
        assert_eq!(
            MouseTracking::from_modes([false, false, false, false]),
            MouseTracking::Off
        );
        assert_eq!(
            MouseFormat::from_modes([true, true, false, false]),
            MouseFormat::Sgr
        );
        assert_eq!(
            MouseFormat::from_modes([false, false, true, false]),
            MouseFormat::Urxvt
        );
    }

    proptest! {
        #[test]
        fn attrs_with_then_without_round_trips(a in any::<u8>(), b in any::<u8>()) {
            let (a, b) = (Attrs(a), Attrs(b));
            prop_assert!(a.with(b).contains(a));
            prop_assert!(a.with(b).contains(b));
            prop_assert!(!a.without(b).contains(b) || b == Attrs::NONE);
            prop_assert_eq!(a.without(b).with(b), a.with(b));
        }

        #[test]
        fn size_accepts_every_nonzero_pair(cols in 1u16.., rows in 1u16..) {
            let size = Size::new(cols, rows).unwrap();
            prop_assert_eq!(size.cells(), usize::from(cols) * usize::from(rows));
        }
    }
}

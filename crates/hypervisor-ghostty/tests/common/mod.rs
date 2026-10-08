//! Fixture loading and the `alacritty_terminal` cross-check grid, shared by the test targets.

use std::path::PathBuf;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as AColor, NamedColor, Processor};
use hypervisor_core::emulator::{Attrs, Cell, CellWidth, Color, Cursor, Grid, Size, Underline};
use serde_json::Value;

/// One recorded session and the final screen RFC-36 runs 2, 3, and 5 saw for it.
pub struct Fixture {
    pub bytes: Vec<u8>,
    pub size: Size,
    pub cursor: Cursor,
    pub text: Vec<String>,
    pub alacritty_grid_diffs: usize,
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Loads `tests/fixtures/recordings/<name>.vt` and `tests/fixtures/expected/<name>.json`.
pub fn fixture(name: &'static str) -> Fixture {
    let bytes = std::fs::read(dir().join(format!("recordings/{name}.vt"))).expect("recording");
    let expected: Value = serde_json::from_slice(
        &std::fs::read(dir().join(format!("expected/{name}.json"))).expect("expected screen"),
    )
    .expect("expected screen JSON");
    let num = |k: &str| expected[k].as_u64().expect(k);
    let u16_of = |v: u64| u16::try_from(v).expect("fits u16");
    assert_eq!(num("bytes"), bytes.len() as u64, "{name}: recording length");
    let cursor = expected["cursor"].as_array().expect("cursor");
    Fixture {
        size: Size::new(u16_of(num("cols")), u16_of(num("rows"))).expect("size"),
        cursor: Cursor {
            row: u16_of(cursor[0].as_u64().expect("row")),
            col: u16_of(cursor[1].as_u64().expect("col")),
        },
        text: expected["text"]
            .as_array()
            .expect("text")
            .iter()
            .map(|l| l.as_str().expect("line").to_owned())
            .collect(),
        alacritty_grid_diffs: usize::try_from(num("alacritty_grid_diffs")).expect("count"),
        bytes,
    }
}

/// Every fixture: nine Claude Code and Codex sessions (run 3), six `agy` sessions (run 5), and
/// spike 2's two synthetic streams.
pub const FIXTURES: [&str; 17] = [
    "claude-default-answered",
    "claude-default-ignored",
    "claude-fullscreen-answered",
    "claude-fullscreen-ignored",
    "claude-fullscreen-noalt-answered",
    "codex-auto-answered",
    "codex-auto-ignored",
    "codex-noalt-answered",
    "codex-noalt-ignored",
    "agy-alt-answered",
    "agy-alt-ignored",
    "agy-default-answered",
    "agy-inline-answered",
    "agy-inline-ignored",
    "agy-reviewdefault-nopretool-answered",
    "synthetic-alt",
    "synthetic-exit",
];

struct Dims(usize, usize);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.1
    }
    fn screen_lines(&self) -> usize {
        self.1
    }
    fn columns(&self) -> usize {
        self.0
    }
}

/// The active screen `alacritty_terminal` draws for `bytes`, in the core's terms.
pub fn alacritty_grid(size: Size, bytes: &[u8]) -> Grid {
    let (cols, rows) = (usize::from(size.cols()), usize::from(size.rows()));
    let mut term = Term::new(Config::default(), &Dims(cols, rows), VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, bytes);
    let grid = term.grid();
    let mut cells = Vec::with_capacity(size.cells());
    for y in 0..rows {
        for x in 0..cols {
            let c = &grid[Line(i32::try_from(y).expect("row"))][Column(x)];
            cells.push(alacritty_cell(c));
        }
    }
    let at = grid.cursor.point;
    let cursor = Cursor {
        row: u16::try_from(at.line.0).expect("cursor row"),
        col: u16::try_from(at.column.0).expect("cursor col"),
    };
    Grid::new(size, cells, cursor).expect("alacritty grid")
}

fn alacritty_cell(c: &alacritty_terminal::term::cell::Cell) -> Cell {
    let f = c.flags;
    let spacer = f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
    let mut text = c.c.to_string();
    if let Some(zw) = c.zerowidth() {
        text.extend(zw.iter());
    }
    if spacer {
        text = " ".into();
    }
    let attrs = [
        (Flags::BOLD, Attrs::BOLD),
        (Flags::DIM, Attrs::FAINT),
        (Flags::ITALIC, Attrs::ITALIC),
        (Flags::INVERSE, Attrs::INVERSE),
        (Flags::HIDDEN, Attrs::INVISIBLE),
        (Flags::STRIKEOUT, Attrs::STRIKETHROUGH),
    ]
    .into_iter()
    .filter(|(flag, _)| f.contains(*flag))
    .fold(Attrs::NONE, |acc, (_, a)| acc.with(a));
    let underline = if f.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if f.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if f.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if f.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if f.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    };
    let width = if f.contains(Flags::WIDE_CHAR) {
        CellWidth::Wide
    } else if f.contains(Flags::WIDE_CHAR_SPACER) {
        CellWidth::SpacerTail
    } else if f.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        CellWidth::SpacerHead
    } else {
        CellWidth::Narrow
    };
    Cell {
        text,
        fg: alacritty_color(c.fg, NamedColor::Foreground),
        bg: alacritty_color(c.bg, NamedColor::Background),
        underline_color: c.underline_color().map_or(Color::Default, |u| {
            alacritty_color(u, NamedColor::Foreground)
        }),
        attrs,
        underline,
        width,
    }
}

fn alacritty_color(c: AColor, default: NamedColor) -> Color {
    match c {
        AColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AColor::Indexed(i) => Color::Palette(i),
        AColor::Named(n) if n == default => Color::Default,
        AColor::Named(n) => u8::try_from(n as usize)
            .ok()
            .filter(|i| *i < 16)
            .map_or(Color::Default, Color::Palette),
    }
}

/// `grid` without the attributes `alacritty_terminal` has no flag for, blink and overline.
pub fn comparable(grid: &Grid) -> Grid {
    let cells = grid
        .cells()
        .iter()
        .map(|c| Cell {
            attrs: c.attrs.without(Attrs::BLINK.with(Attrs::OVERLINE)),
            ..c.clone()
        })
        .collect();
    Grid::new(grid.size(), cells, grid.cursor()).expect("same shape")
}

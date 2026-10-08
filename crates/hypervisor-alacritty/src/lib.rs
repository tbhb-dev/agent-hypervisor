//! The [`Emulator`] on `alacritty_terminal` 0.26.0: the fallback for ghostty-vt and the second
//! opinion the golden tests compare it with (RFC-36 source line 101).
//!
//! [`AlacrittyEmulator`] owns a `Term` and its `vte` processor, plus a second `vte` parser that
//! sees the same bytes and hands each CSI to [`csi_effects`]. That parser covers what
//! `alacritty_terminal` does not: it answers DA1, DA2, DA3, and XTVERSION from the core's profile
//! instead of alacritty's hard-coded `CSI ? 6 c`, and it tracks modes 2026 and 2027. The input is
//! cut after each such CSI, so replies come back in the order the queries arrived.
//!
//! # Field mapping
//!
//! - Cell text is the base character plus its zero-width characters; spacer cells read as a space.
//! - Named colors 0 to 15 become palette indexes, the named foreground and background become
//!   [`Color::Default`], and every other named color (the dim and bright variants alacritty keeps
//!   for rendering) also becomes [`Color::Default`].
//! - `Attrs::BLINK` and `Attrs::OVERLINE` are never set: alacritty keeps no flag for SGR 5 or 53.
//! - `Modes::mouse_tracking` never reports [`MouseTracking::X10`], and `Modes::mouse_format` never
//!   reports [`MouseFormat::Urxvt`] or [`MouseFormat::SgrPixels`]: alacritty ignores modes 9, 1015,
//!   and 1016.
//! - Mode 2027 is tracked for [`Modes`], but alacritty still sizes characters one codepoint at a
//!   time, so a regional-indicator pair takes one column per indicator where ghostty-vt takes two.
//!
//! [`AlacrittyEmulator`] is neither `Send` nor `Sync`, like the ghostty implementation: its reply
//! buffer is shared with the terminal's event listener through an `Rc`.
//!
//! ```compile_fail
//! fn needs_send<T: Send>() {}
//! needs_send::<hypervisor_alacritty::AlacrittyEmulator>();
//! ```

use std::cell::RefCell;
use std::convert::Infallible;
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as AlacrittyCell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AlacrittyColor, NamedColor, Processor, Timeout};
use alacritty_terminal::vte::{Params, Parser, Perform};
use hypervisor_core::emulator::{
    Attrs, Cell, CellWidth, Color, CsiEffect, Cursor, Emulator, Grid, Modes, MouseFormat,
    MouseTracking, Screen, Size, Underline, admit_reply, csi_effects,
};

/// alacritty's own DA1 and DA2 replies at 0.26.0. The side parser answers both instead.
const OWN_DEVICE_ATTRIBUTES: [&str; 2] = ["\x1b[?6c", "\x1b[>0;2600;1c"];

/// The terminal's event listener: keeps the replies it writes for the PTY.
#[derive(Clone, Default)]
struct Replies {
    bytes: Rc<RefCell<Vec<u8>>>,
    title: Rc<RefCell<Option<String>>>,
}

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text)
                if !OWN_DEVICE_ATTRIBUTES.contains(&text.as_str())
                    && admit_reply(text.as_bytes()) =>
            {
                self.bytes.borrow_mut().extend_from_slice(text.as_bytes());
            }
            Event::Title(title) => *self.title.borrow_mut() = Some(title),
            Event::ResetTitle => *self.title.borrow_mut() = None,
            _ => {}
        }
    }
}

/// A synchronized-update timer that never runs, so the processor applies bytes as they arrive
/// instead of holding them until the update ends or a wall-clock timeout fires.
#[derive(Default)]
struct Immediate;

impl Timeout for Immediate {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// The side parser's performer: stops the parser after any CSI with an effect.
#[derive(Default)]
struct Side(Vec<CsiEffect>);

impl Perform for Side {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if !ignore {
            let firsts: Vec<u16> = params
                .iter()
                .map(|p| p.first().copied().unwrap_or(0))
                .collect();
            self.0 = csi_effects(intermediates, &firsts, action);
        }
    }

    fn terminated(&self) -> bool {
        !self.0.is_empty()
    }
}

struct Dims(Size);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows())
    }
    fn columns(&self) -> usize {
        usize::from(self.0.cols())
    }
}

/// A terminal emulator backed by `alacritty_terminal` 0.26.0.
pub struct AlacrittyEmulator {
    term: Term<Replies>,
    processor: Processor<Immediate>,
    side: Parser,
    side_effects: Side,
    replies: Replies,
    size: Size,
    synchronized_output: bool,
    grapheme_clustering: bool,
}

impl AlacrittyEmulator {
    /// A blank terminal of `size` that answers queries on its own.
    #[must_use]
    pub fn new(size: Size) -> Self {
        let replies = Replies::default();
        // The kitty keyboard stack is kept so `Modes` can report pushed flags; the reply to its
        // query is dropped by `admit_reply`.
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &Dims(size), replies.clone()),
            processor: Processor::new(),
            side: Parser::new(),
            side_effects: Side::default(),
            replies,
            size,
            synchronized_output: false,
            grapheme_clustering: false,
        }
    }

    fn apply(&mut self, effect: CsiEffect) {
        match effect {
            CsiEffect::Reply(bytes) => self.replies.bytes.borrow_mut().extend_from_slice(&bytes),
            CsiEffect::Mode { mode: 2026, set } => self.synchronized_output = set,
            CsiEffect::Mode { mode: 2027, set } => self.grapheme_clustering = set,
            CsiEffect::Mode { .. } => {}
        }
    }
}

impl Emulator for AlacrittyEmulator {
    type Error = Infallible;

    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = self
                .side
                .advance_until_terminated(&mut self.side_effects, rest);
            self.processor.advance(&mut self.term, &rest[..n]);
            for effect in std::mem::take(&mut self.side_effects.0) {
                self.apply(effect);
            }
            rest = &rest[n..];
        }
        std::mem::take(&mut *self.replies.bytes.borrow_mut())
    }

    fn size(&self) -> Size {
        self.size
    }

    fn resize(&mut self, size: Size) -> Result<(), Infallible> {
        self.term.resize(Dims(size));
        self.size = size;
        Ok(())
    }

    fn grid(&self) -> Grid {
        let grid = self.term.grid();
        let cells = (0..self.size.rows())
            .flat_map(|r| (0..self.size.cols()).map(move |c| (r, c)))
            .map(|(r, c)| cell(&grid[Line(i32::from(r))][Column(usize::from(c))]))
            .collect();
        let at = grid.cursor.point;
        let cursor = Cursor {
            row: u16::try_from(at.line.0).unwrap_or(0),
            col: u16::try_from(at.column.0).unwrap_or(0),
        };
        Grid::new(self.size, cells, cursor).unwrap_or_else(|e| unreachable!("alacritty grid: {e}"))
    }

    fn title(&self) -> Option<String> {
        self.replies.title.borrow().clone()
    }

    fn modes(&self) -> Modes {
        let m = *self.term.mode();
        let on = |flag| m.contains(flag);
        Modes {
            screen: if on(TermMode::ALT_SCREEN) {
                Screen::Alternate
            } else {
                Screen::Primary
            },
            cursor_visible: on(TermMode::SHOW_CURSOR),
            application_cursor_keys: on(TermMode::APP_CURSOR),
            application_keypad: on(TermMode::APP_KEYPAD),
            wraparound: on(TermMode::LINE_WRAP),
            mouse_tracking: MouseTracking::from_modes([
                false,
                on(TermMode::MOUSE_REPORT_CLICK),
                on(TermMode::MOUSE_DRAG),
                on(TermMode::MOUSE_MOTION),
            ]),
            mouse_format: MouseFormat::from_modes([
                on(TermMode::UTF8_MOUSE),
                on(TermMode::SGR_MOUSE),
                false,
                false,
            ]),
            alternate_scroll: on(TermMode::ALTERNATE_SCROLL),
            focus_events: on(TermMode::FOCUS_IN_OUT),
            bracketed_paste: on(TermMode::BRACKETED_PASTE),
            synchronized_output: self.synchronized_output,
            grapheme_clustering: self.grapheme_clustering,
            kitty_keyboard_flags: [
                TermMode::DISAMBIGUATE_ESC_CODES,
                TermMode::REPORT_EVENT_TYPES,
                TermMode::REPORT_ALTERNATE_KEYS,
                TermMode::REPORT_ALL_KEYS_AS_ESC,
                TermMode::REPORT_ASSOCIATED_TEXT,
            ]
            .into_iter()
            .zip(0..)
            .filter(|(flag, _)| on(*flag))
            .fold(0, |acc, (_, bit)| acc | 1 << bit),
        }
    }
}

fn cell(c: &AlacrittyCell) -> Cell {
    let f = c.flags;
    let text = if f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        " ".into()
    } else {
        let mut text = c.c.to_string();
        text.extend(c.zerowidth().into_iter().flatten());
        text
    };
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
    let underline = [
        (Flags::DOUBLE_UNDERLINE, Underline::Double),
        (Flags::UNDERCURL, Underline::Curly),
        (Flags::DOTTED_UNDERLINE, Underline::Dotted),
        (Flags::DASHED_UNDERLINE, Underline::Dashed),
        (Flags::UNDERLINE, Underline::Single),
    ]
    .into_iter()
    .find(|(flag, _)| f.contains(*flag))
    .map_or(Underline::None, |(_, u)| u);
    let width = [
        (Flags::WIDE_CHAR, CellWidth::Wide),
        (Flags::WIDE_CHAR_SPACER, CellWidth::SpacerTail),
        (Flags::LEADING_WIDE_CHAR_SPACER, CellWidth::SpacerHead),
    ]
    .into_iter()
    .find(|(flag, _)| f.contains(*flag))
    .map_or(CellWidth::Narrow, |(_, w)| w);
    Cell {
        text,
        fg: color(c.fg, NamedColor::Foreground),
        bg: color(c.bg, NamedColor::Background),
        underline_color: c
            .underline_color()
            .map_or(Color::Default, |u| color(u, NamedColor::Foreground)),
        attrs,
        underline,
        width,
        hyperlink: c.hyperlink().map(|link| link.uri().to_owned()),
    }
}

fn color(c: AlacrittyColor, default: NamedColor) -> Color {
    match c {
        AlacrittyColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AlacrittyColor::Indexed(i) => Color::Palette(i),
        AlacrittyColor::Named(n) if n == default => Color::Default,
        AlacrittyColor::Named(n) => u8::try_from(n as usize)
            .ok()
            .filter(|i| *i < 16)
            .map_or(Color::Default, Color::Palette),
    }
}
